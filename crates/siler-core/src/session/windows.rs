//! Windows logins: NTLM, and Kerberos inside `Negotiate`. A login belongs to a connection,
//! so each travels on a client of its own with one connection (a seat), which is taken again
//! once its body has ended. Until a login has worked, one request at a time tries it, and a
//! refused one isn't tried again: domain accounts lock after a few wrong passwords.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use hyper_util::client::legacy::connect::HttpInfo;
use reqwest::header::{
    AUTHORIZATION, CONNECTION, HeaderMap, HeaderName, HeaderValue, PROXY_AUTHENTICATE,
    PROXY_AUTHORIZATION, WWW_AUTHENTICATE,
};
use reqwest::{Response, StatusCode, Url};
use std::sync::{Arc, Mutex};

use super::auth::{Challenge, HostAuth, Scheme, Windows, parse_challenge_in};
use super::{Hop, binding, kerberos, ntlm};
use crate::net::{Kept, Login, Net};
use crate::proxy::Via;
use crate::transport::dial::{Exit, How};
use crate::{Asked, Error, Result};

/// Handshakes at once on one site, after the login has worked.
pub(crate) const AT_ONCE: u32 = 4;
/// More of a 401's body than this and its connection isn't kept.
const MAX_REFUSAL_BODY: usize = 1024 * 1024;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum Standing {
    #[default]
    Untried,
    Good,
    Refused,
}

/// A client with one connection, and whose login that connection carries.
struct Seat {
    out: Arc<Kept>,
    via: Via,
    how: How,
    allow_private: bool,
    /// The proxy settings it was built for.
    setup: u64,
    who: Option<Login>,
    /// The channel binding of the certificate last seen on it (`Some(None)`: none exists).
    binding: Option<Option<Vec<u8>>>,
}

impl Seat {
    fn note_certificate(&mut self, resp: &Response) {
        if let Some(info) = resp.extensions().get::<reqwest::tls::TlsInfo>() {
            self.binding = Some(info.peer_certificate().and_then(binding::end_point));
        }
    }

    fn binding(&self) -> Option<&[u8]> {
        self.binding.as_ref()?.as_deref()
    }
}

/// What a site's Windows logins have come to.
pub(crate) struct Logins {
    standing: Mutex<Standing>,
    cred: Mutex<Option<Arc<kerberos::Cred>>>,
    /// All of its permits for an untried login, one for a login that has worked.
    turn: tokio::sync::Semaphore,
    seats: Mutex<Vec<Seat>>,
}

impl Default for Logins {
    fn default() -> Self {
        Logins {
            standing: Mutex::default(),
            cred: Mutex::default(),
            turn: tokio::sync::Semaphore::new(AT_ONCE as usize),
            seats: Mutex::default(),
        }
    }
}

impl Logins {
    /// Another login was given: nothing is known about it.
    pub fn forget(&self) {
        *self.standing.lock().unwrap() = Standing::Untried;
        *self.cred.lock().unwrap() = None;
    }

    fn standing(&self) -> Standing {
        *self.standing.lock().unwrap()
    }

    pub fn is_refused(&self) -> bool {
        self.standing() == Standing::Refused
    }

    /// Waits for this request's turn to log in.
    pub async fn turn(&self) -> Result<tokio::sync::SemaphorePermit<'_>> {
        loop {
            let needs = || match self.standing() {
                Standing::Good => 1,
                Standing::Untried | Standing::Refused => AT_ONCE,
            };
            let wanted = needs();
            let turn = self
                .turn
                .acquire_many(wanted)
                .await
                .map_err(|e| Error::Protocol(e.to_string()))?;
            // While waiting, the login may have worked: then there is room for more.
            if needs() == wanted {
                return Ok(turn);
            }
        }
    }
}

/// Records how a login fared, unless another was given meanwhile.
pub(crate) fn settle(auth: &HostAuth, login: &Login, standing: Standing) {
    if auth.login.lock().unwrap().as_ref() == Some(login) {
        *auth.windows.standing.lock().unwrap() = standing;
    }
}

/// What to ask for after `login` was refused.
pub(crate) fn asked_again(login: &Login) -> Asked {
    if login.is_ticket() {
        Asked::TicketFailed
    } else {
        Asked::Windows
    }
}

/// What follows Kerberos failing: whether another scheme takes over. A refused password is
/// never sent a second way.
pub(crate) fn after_kerberos(
    auth: &HostAuth,
    login: &Login,
    failure: kerberos::Failure,
    fallback: Scheme,
) -> bool {
    if login.is_ticket() {
        return false;
    }
    match failure {
        kerberos::Failure::Refused => {
            settle(auth, login, Standing::Refused);
            false
        }
        kerberos::Failure::Unavailable if matches!(fallback, Scheme::Unknown) => false,
        kerberos::Failure::Unavailable => {
            auth.use_scheme(fallback);
            true
        }
    }
}

pub(crate) fn ntlm_hello() -> Vec<u8> {
    ntlm::negotiate()
}

pub(crate) fn ntlm_answer(
    hello: &[u8],
    challenge: &[u8],
    login: &Login,
    host: &str,
    binding: Option<&[u8]>,
) -> Result<Vec<u8>> {
    let target = ntlm::Target { host, binding };
    let (user, password) = (&login.username, &login.password);
    ntlm::authenticate(hello, challenge, user, password, random()?, &target).map_err(|e| {
        Error::Protocol(match e {
            ntlm::Unusable::BadChallenge => {
                "The server sent an NTLM challenge that can't be read".into()
            }
            ntlm::Unusable::TooOld => {
                "The server only offers NTLMv1, which is unsafe, so Siler doesn't use it".into()
            }
        })
    })
}

/// A token from the ticket or the password. The library waits for the KDC, so it runs on a
/// thread of its own.
pub(crate) async fn kerberos_token(
    auth: &HostAuth,
    login: &Login,
    host: &str,
    binding: Option<&[u8]>,
) -> Result<Vec<u8>, kerberos::Failure> {
    use kerberos::{Cred, Failure};
    let host = host.to_string();
    let binding = binding.map(<[u8]>::to_vec);
    let given = login.clone();
    let known = auth.windows.cred.lock().unwrap().clone();
    let made = tokio::task::spawn_blocking(move || {
        let binding = binding.as_deref();
        if given.is_ticket() {
            return kerberos::token(&host, None, binding).map(|t| (t, None));
        }
        if let Some(cred) = &known
            && let Ok(token) = kerberos::token(&host, Some(cred), binding)
        {
            return Ok((token, known.clone()));
        }
        // None yet, or its ticket ran out.
        let cred = Cred::with_password(&given.username, &given.password)?;
        let token = kerberos::token(&host, Some(&cred), binding)?;
        Ok((token, Some(cred)))
    })
    .await
    .map_err(|_| Failure::Unavailable)?;
    let (token, cred) = made?;
    if auth.login.lock().unwrap().as_ref() == Some(login) {
        *auth.windows.cred.lock().unwrap() = cred;
    }
    Ok(token)
}

/// A seat while one request uses it; it goes back when this is dropped.
pub(crate) struct Lease {
    auth: Arc<HostAuth>,
    seat: Option<Seat>,
    /// The body ended, so the connection and its login can be used again.
    whole: bool,
}

impl Lease {
    fn take(net: &Net, auth: &Arc<HostAuth>, who: &Login, exit: &Exit, url: &Url) -> Result<Lease> {
        let setup = net.setup();
        let how = exit.how(url.scheme() == "https", true);
        let found = {
            let mut seats = auth.windows.seats.lock().unwrap();
            seats.retain(|s| s.setup == setup);
            let fits = |s: &Seat| {
                s.allow_private == exit.allow_private && s.via == exit.via && s.how == how
            };
            seats
                .iter()
                .rposition(|s| fits(s) && s.who.as_ref() == Some(who))
                .or_else(|| seats.iter().rposition(fits))
                .map(|i| seats.remove(i))
        };
        let seat = match found {
            Some(seat) => seat,
            None => Seat {
                out: net.pinned(exit, &how, url)?,
                via: exit.via.clone(),
                allow_private: exit.allow_private,
                how,
                setup,
                who: None,
                binding: None,
            },
        };
        Ok(Lease {
            auth: auth.clone(),
            seat: Some(seat),
            whole: false,
        })
    }

    fn seat(&mut self) -> &mut Seat {
        self.seat
            .as_mut()
            .expect("a lease has its seat until dropped")
    }

    pub fn body_ended(&mut self) {
        self.whole = true;
    }
}

impl Drop for Lease {
    fn drop(&mut self) {
        if let Some(mut seat) = self.seat.take() {
            if !self.whole {
                seat.who = None;
                seat.binding = None;
            }
            self.auth.windows.seats.lock().unwrap().push(seat);
        }
    }
}

// Moved once, straight to the caller: not worth an allocation per request.
#[expect(clippy::large_enum_variant)]
pub(crate) enum Outcome {
    /// An answer to read: any status but a refusal. A 401 here asks for something other than
    /// a Windows login.
    Answer(Response, Option<Lease>),
    /// Kerberos couldn't be used and the site's other scheme takes over.
    FellBack,
}

/// The challenge that follows `word` in the header `name`.
pub(crate) fn token_after(headers: &HeaderMap, name: &HeaderName, word: &str) -> Option<Vec<u8>> {
    headers
        .get_all(name)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .find_map(|part| {
            let (scheme, token) = part.trim().split_once(' ')?;
            if !scheme.eq_ignore_ascii_case(word) {
                return None;
            }
            B64.decode(token.trim()).ok()
        })
}

/// What tells one connection from another: its own address, or the tunnel's count.
fn connection(resp: &Response, seat: &Seat) -> (Option<std::net::SocketAddr>, Option<u64>) {
    (
        resp.extensions()
            .get::<HttpInfo>()
            .map(HttpInfo::local_addr),
        seat.out.tunnel.as_ref().map(|t| t.made()),
    )
}

/// Reads a refusal to its end, so that its connection can be used again.
async fn drain(mut resp: Response) -> Result<()> {
    let mut read = 0;
    while let Some(chunk) = resp
        .chunk()
        .await
        .map_err(|e| Error::Network(crate::error::describe(&e)))?
    {
        read += chunk.len();
        if read > MAX_REFUSAL_BODY {
            break;
        }
    }
    Ok(())
}

pub(crate) fn token_header(word: &str, token: &[u8]) -> Result<HeaderValue> {
    let mut v = HeaderValue::from_str(&format!("{word} {}", B64.encode(token)))
        .map_err(|e| Error::Protocol(e.to_string()))?;
    v.set_sensitive(true);
    Ok(v)
}

pub(crate) fn random() -> Result<[u8; 8]> {
    let mut out = [0u8; 8];
    let n = rustix::rand::getrandom(&mut out, rustix::rand::GetRandomFlags::empty())?;
    if n != out.len() {
        return Err(Error::Protocol("No random numbers are available".into()));
    }
    Ok(out)
}

/// Who asks for the login: the site, or the proxy that passes plain http on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Door {
    Site,
    Proxy,
}

impl Door {
    fn status(self) -> StatusCode {
        match self {
            Door::Site => StatusCode::UNAUTHORIZED,
            Door::Proxy => StatusCode::PROXY_AUTHENTICATION_REQUIRED,
        }
    }

    fn asks_in(self) -> HeaderName {
        match self {
            Door::Site => WWW_AUTHENTICATE,
            Door::Proxy => PROXY_AUTHENTICATE,
        }
    }

    fn answers_in(self) -> HeaderName {
        match self {
            Door::Site => AUTHORIZATION,
            Door::Proxy => PROXY_AUTHORIZATION,
        }
    }
}

/// One request behind a Windows login.
pub(crate) struct Attempt<'a> {
    pub net: &'a Net,
    pub hop: &'a Hop<'a>,
    pub door: Door,
    /// What the login is kept under.
    pub key: &'a str,
    pub auth: &'a Arc<HostAuth>,
    pub login: Login,
}

impl Attempt<'_> {
    fn url(&self) -> &Url {
        self.hop.url
    }

    /// Who the answer is meant for.
    fn host(&self) -> &str {
        let host = match (self.door, self.hop.exit.proxy()) {
            (Door::Proxy, Some(proxy)) => proxy.host.as_str(),
            _ => self.url().host_str().unwrap_or(""),
        };
        host.trim_matches(['[', ']'])
    }

    /// The certificate an answer names; a proxy passing plain http on has none to name.
    fn binding<'s>(&self, seat: &'s Seat) -> Option<&'s [u8]> {
        match self.door {
            Door::Site => seat.binding(),
            Door::Proxy => None,
        }
    }

    fn challenge(&self, resp: &Response) -> Option<Challenge> {
        parse_challenge_in(resp.headers(), &self.door.asks_in())
    }

    fn still_windows(&self, resp: &Response) -> bool {
        self.challenge(resp).is_some_and(|c| c.is_windows())
    }

    fn settle(&self, standing: Standing) {
        settle(self.auth, &self.login, standing);
    }

    fn refused(&self) -> Error {
        // The next request asks plainly first: not every address on the site needs a login.
        self.auth.use_scheme(Scheme::Unknown);
        let asked = asked_again(&self.login);
        match self.door {
            Door::Proxy => self.hop.exit.login_error(true, asked, None),
            Door::Site => Error::AuthRequired {
                host: self.key.to_string(),
                realm: None,
                rejected: true,
                asked,
            },
        }
    }

    async fn get(&self, lease: &mut Lease, token: Option<HeaderValue>) -> Result<Response> {
        let seat = lease.seat();
        let token = token.map(|t| (self.door.answers_in(), t));
        let resp = self.hop.send(&seat.out.client, token).await?;
        seat.note_certificate(&resp);
        Ok(resp)
    }

    fn logged_in(&self, resp: Response, mut lease: Lease) -> Outcome {
        lease.seat().who = Some(self.login.clone());
        Outcome::Answer(resp, Some(lease))
    }

    pub async fn run(&self) -> Result<Outcome> {
        let mut lease = Lease::take(self.net, self.auth, &self.login, self.hop.exit, self.url())?;

        // A connection that is logged in needs nothing more. Kerberos sends its token every
        // time, as many servers check every request.
        if lease.seat().who.as_ref() == Some(&self.login) {
            let sent = match self.auth.scheme() {
                Scheme::Windows(Windows::Kerberos(_)) => {
                    let binding = self.binding(lease.seat());
                    match kerberos_token(self.auth, &self.login, self.host(), binding).await {
                        Ok(t) => Some(token_header("Negotiate", &t)?),
                        Err(_) => None, // the handshake below says what is wrong
                    }
                }
                _ => None,
            };
            let resp = self.get(&mut lease, sent).await?;
            if resp.status() != self.door.status() {
                return Ok(self.logged_in(resp, lease));
            }
            lease.seat().who = None;
            if !self.still_windows(&resp) {
                return Ok(Outcome::Answer(resp, None));
            }
            drain(resp).await?;
        }

        let logins = &self.auth.windows;
        let turn = logins.turn().await?;
        if logins.is_refused() {
            return Err(self.refused());
        }
        let outcome = match self.auth.scheme() {
            Scheme::Windows(Windows::Ntlm(word)) => self.ntlm(lease, word).await,
            Scheme::Windows(Windows::Kerberos(fallback)) => self.kerberos(lease, *fallback).await,
            _ => Ok(Outcome::FellBack),
        };
        drop(turn);
        outcome
    }

    async fn ntlm(&self, mut lease: Lease, word: &str) -> Result<Outcome> {
        let hello = ntlm_hello();
        let resp = self
            .get(&mut lease, Some(token_header(word, &hello)?))
            .await?;
        if resp.status() != self.door.status() {
            // This address needs no login.
            return Ok(Outcome::Answer(resp, Some(lease)));
        }
        let Some(challenge) = token_after(resp.headers(), &self.door.asks_in(), word) else {
            return Ok(Outcome::Answer(resp, None));
        };
        let first = connection(&resp, lease.seat());
        let closes = resp
            .headers()
            .get(CONNECTION)
            .is_some_and(|v| v.as_bytes().eq_ignore_ascii_case(b"close"));
        if closes {
            return Err(Error::Protocol(
                "The server closes the connection that an NTLM login needs".into(),
            ));
        }
        drain(resp).await?;

        let binding = self.binding(lease.seat());
        let answer = ntlm_answer(&hello, &challenge, &self.login, self.host(), binding)?;
        let resp = self
            .get(&mut lease, Some(token_header(word, &answer)?))
            .await?;
        if resp.status() == self.door.status() {
            // An answer that went out on another connection says nothing about the password.
            if connection(&resp, lease.seat()) != first {
                return Err(Error::Network(
                    "the connection closed during the login".into(),
                ));
            }
            self.settle(Standing::Refused);
            return Err(self.refused());
        }
        self.took(resp, lease)
    }

    async fn kerberos(&self, mut lease: Lease, fallback: Scheme) -> Result<Outcome> {
        // Over TLS the token names the certificate, so that has to be seen first.
        let unseen = lease.seat().binding.is_none();
        if self.door == Door::Site && self.url().scheme() == "https" && unseen {
            let resp = self.get(&mut lease, None).await?;
            if resp.status() != self.door.status() {
                return Ok(Outcome::Answer(resp, Some(lease)));
            }
            if !self.still_windows(&resp) {
                return Ok(Outcome::Answer(resp, None));
            }
            drain(resp).await?;
        }
        let binding = self.binding(lease.seat());
        let token = match kerberos_token(self.auth, &self.login, self.host(), binding).await {
            Ok(token) => token,
            Err(failure) => {
                if !after_kerberos(self.auth, &self.login, failure, fallback) {
                    return Err(self.refused());
                }
                return match self.auth.scheme() {
                    Scheme::Windows(Windows::Ntlm(word)) => self.ntlm(lease, word).await,
                    _ => Ok(Outcome::FellBack),
                };
            }
        };
        let resp = self
            .get(&mut lease, Some(token_header("Negotiate", &token)?))
            .await?;
        if resp.status() == self.door.status() {
            if !self.still_windows(&resp) {
                return Ok(Outcome::Answer(resp, None));
            }
            self.settle(Standing::Refused);
            return Err(self.refused());
        }
        self.took(resp, lease)
    }

    fn took(&self, resp: Response, lease: Lease) -> Result<Outcome> {
        let status = resp.status();
        let proxy_objects =
            self.door == Door::Site && status == StatusCode::PROXY_AUTHENTICATION_REQUIRED;
        if !proxy_objects && !status.is_server_error() {
            self.settle(Standing::Good);
        }
        Ok(self.logged_in(resp, lease))
    }
}
