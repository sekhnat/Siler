//! Every HTTP request Siler makes. [`Net::send`] follows the redirects itself, so each hop
//! gets only what that address may have (see [`crate::scope`]), is checked against the
//! caller's policy, takes the route the proxy settings give it, and is logged in to the way
//! that server or proxy asks.

pub(crate) mod auth;
mod binding;
mod kerberos;
mod ntlm;
pub(crate) mod tunnel;
pub(crate) mod windows;

use bytes::Bytes;
use futures_util::{Stream, StreamExt};
use reqwest::header::{
    HeaderMap, HeaderName, HeaderValue, LOCATION, PROXY_AUTHENTICATE, PROXY_AUTHORIZATION, RANGE,
    RETRY_AFTER,
};
use reqwest::{Client, Response, StatusCode, Url};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::task::Poll;
use url::Position;

use crate::error::{MediaError, describe, parse_retry_after};
use crate::http::Guard;
use crate::net::{Net, host_key, site_key};
use crate::probe::build_request;
use crate::scope::Scoped;
use crate::transport::dial::{Exit, Failed};
use crate::{Asked, Error, RequestMeta, Result, resolve};
use auth::{HostAuth, Scheme, parse_challenge, parse_challenge_in};
use windows::{Door, Lease, Outcome};

/// Redirects followed for one request.
const MAX_REDIRECTS: usize = 10;
/// Attempts at logging in for one request.
const MAX_LOGINS: usize = 3;
/// Attempts at one hop's proxy, as what it wants is learned.
const MAX_PROXY_TURNS: usize = 4;

/// What a request may carry, and where it may go.
#[derive(Clone, Copy)]
pub(crate) enum Policy<'a> {
    /// A file: the browser's context stays at home, and any address is allowed.
    File,
    /// A stream's: related hosts share the context, and `guard` checks every address.
    /// `source` says it is the stream's own request, not one the manifest names.
    Media { guard: &'a Guard, source: bool },
    /// Siler's own (the update check): these headers, and no context or login.
    Own(&'a [(&'a str, &'a str)]),
}

/// One request.
pub(crate) struct Request<'a> {
    pub url: &'a Url,
    pub meta: &'a RequestMeta,
    pub user_agent: &'a str,
    pub range: Option<&'a str>,
    pub policy: Policy<'a>,
    /// One of several streams of a ranged download: it may go over an HTTP/2 slot.
    pub streams: bool,
}

/// The answer at the end of the redirects.
pub(crate) struct Reply {
    resp: Response,
    /// The seat it came on, for a Windows login.
    lease: Option<Lease>,
    /// The context as it stands there, for further requests to that address.
    pub meta: RequestMeta,
}

impl Reply {
    pub fn status(&self) -> StatusCode {
        self.resp.status()
    }

    pub fn headers(&self) -> &HeaderMap {
        self.resp.headers()
    }

    /// The address that answered, after the redirects.
    pub fn url(&self) -> &Url {
        self.resp.url()
    }

    /// The HTTP version the answer came over.
    pub fn version(&self) -> reqwest::Version {
        self.resp.version()
    }

    pub fn content_length(&self) -> Option<u64> {
        self.resp.content_length()
    }

    /// A 429 or 503 as an error worth retrying, with its `Retry-After`.
    pub fn busy(&self) -> Error {
        let after = self
            .headers()
            .get(RETRY_AFTER)
            .and_then(|v| v.to_str().ok())
            .and_then(parse_retry_after);
        Error::Busy(self.status().as_u16(), after)
    }

    pub fn into_stream(
        self,
    ) -> impl Stream<Item = std::result::Result<Bytes, String>> + Send + Unpin + 'static {
        let mut left = self.resp.content_length();
        let mut lease = self.lease;
        let mut body = self.resp.bytes_stream();
        Box::pin(futures_util::stream::poll_fn(move |cx| {
            let next = std::task::ready!(body.poll_next_unpin(cx));
            let ended = match &next {
                Some(Ok(chunk)) => {
                    left = left.map(|n| n.saturating_sub(chunk.len() as u64));
                    left == Some(0)
                }
                Some(Err(_)) => false,
                None => true,
            };
            if ended && let Some(lease) = lease.as_mut() {
                lease.body_ended();
            }
            Poll::Ready(next.map(|c| c.map_err(|e| describe(&e))))
        }))
    }
}

/// One hop of a request, one way out.
pub(crate) struct Hop<'a> {
    req: &'a Request<'a>,
    url: &'a Url,
    scoped: &'a Scoped,
    exit: &'a Exit,
    /// The site's login that goes in a header.
    site: Option<Arc<HostAuth>>,
    /// The last failure was in connecting, so nothing was sent.
    unreached: AtomicBool,
}

impl Hop<'_> {
    /// Sends it, with `token` in place of any header of that name.
    async fn send(
        &self,
        client: &Client,
        token: Option<(HeaderName, HeaderValue)>,
    ) -> Result<Response> {
        let req = self.req;
        let site = self.site.as_deref();
        let mut rb = build_request(client, self.url.clone(), self.scoped, req.user_agent, site);
        if let Policy::Own(headers) = req.policy {
            let mut own = HeaderMap::new();
            for (name, value) in headers {
                if let (Ok(name), Ok(value)) = (
                    HeaderName::from_bytes(name.as_bytes()),
                    HeaderValue::from_str(value),
                ) {
                    own.insert(name, value);
                }
            }
            rb = rb.headers(own);
        }
        if let Some(range) = req.range {
            rb = rb.header(RANGE, range);
        }
        let mut request = rb.build().map_err(|e| Error::Network(describe(&e)))?;
        // A Basic login is the client's to send; a Digest one answers for each address.
        if self.exit.forwards(self.url.scheme() == "https")
            && let Some(auth) = &self.exit.auth
            && matches!(auth.scheme(), Scheme::Digest(_))
        {
            let target = &self.url[..Position::AfterQuery];
            let body = request.body().and_then(|b| b.as_bytes());
            if let Some(login) = auth.answer(request.method(), target, body) {
                request.headers_mut().insert(PROXY_AUTHORIZATION, login);
            }
        }
        if let Some((name, token)) = token {
            // In place of any the page gave.
            request.headers_mut().insert(name, token);
        }
        client.execute(request).await.map_err(|e| {
            self.unreached.store(e.is_connect(), Relaxed);
            match resolve::refused(&e).filter(|_| matches!(req.policy, Policy::Media { .. })) {
                Some(why) => Error::Media(MediaError::UrlForbidden(why)),
                None => Error::Network(describe(&e)),
            }
        })
    }
}

/// An answer, and what went with the request.
struct Answer {
    resp: Response,
    lease: Option<Lease>,
    exit: Exit,
    /// The key of the site's login that was sent.
    sent: Option<String>,
    sent_to_proxy: bool,
}

/// What came of one way out.
enum Step {
    Answer(Box<Answer>),
    /// Kerberos couldn't be used and the site's other scheme takes over.
    SiteFellBack,
    /// The same at the proxy.
    ProxyFellBack,
    /// What the proxy wants is known now: send it again.
    Again,
}

/// Why one way out didn't work.
enum Miss {
    /// Nothing was sent: the next way may be tried.
    Next(Error),
    Stop(Error),
}

impl Net {
    /// Sends a request, choosing the way out for each hop, following redirects and
    /// answering logins. A 401 nobody can answer is [`Error::AuthRequired`], and so is a
    /// proxy's 407 unless the settings hold that proxy's login ([`Error::ProxyAuth`]);
    /// every other status is the caller's to read.
    pub(crate) async fn send(&self, req: Request<'_>) -> Result<Reply> {
        let mut target = req.url.clone();
        let mut meta = req.meta.clone();
        let mut hops = 0;
        let mut logins = 0;
        let mut fell_back = false;
        let mut proxy_turns = 0;
        let mut proxy_fell_back = false;
        loop {
            let (scoped, allow_private) = match req.policy {
                Policy::Media { guard, source } => {
                    guard.check(&target)?;
                    (Scoped::media(&meta, &target, source), guard.allow_private)
                }
                Policy::File | Policy::Own(_) => (Scoped::file(&meta, &target), true),
            };
            let may_log_in = scoped.logins() && !matches!(req.policy, Policy::Own(_));
            let origin = site_key(&target);

            let choice = self.router.choose(&target).await?;
            let mut came = None;
            let mut missed = None;
            for via in choice.via {
                let exit = self.exit(via, allow_private);
                match Box::pin(self.over(&req, &target, &scoped, exit, may_log_in)).await {
                    Ok(step) => {
                        came = Some(step);
                        break;
                    }
                    Err(Miss::Next(e)) => missed = Some(e),
                    Err(Miss::Stop(e)) => return Err(e),
                }
            }
            let Some(step) = came else {
                return Err(missed.unwrap_or_else(|| Error::Network("there is no route".into())));
            };
            let answer = match step {
                Step::Answer(answer) => *answer,
                Step::SiteFellBack => {
                    logins += 1;
                    if logins >= MAX_LOGINS {
                        return Err(Error::AuthRequired {
                            host: origin,
                            realm: None,
                            rejected: true,
                            asked: Asked::Windows,
                        });
                    }
                    fell_back = true;
                    continue;
                }
                step @ (Step::ProxyFellBack | Step::Again) => {
                    proxy_turns += 1;
                    if proxy_turns >= MAX_PROXY_TURNS {
                        return Err(Error::Network("the proxy login didn't settle".into()));
                    }
                    proxy_fell_back |= matches!(step, Step::ProxyFellBack);
                    continue;
                }
            };
            let Answer {
                resp,
                lease,
                exit,
                sent,
                sent_to_proxy,
            } = answer;

            let status = resp.status();
            let location = matches!(status.as_u16(), 301 | 302 | 303 | 307 | 308)
                .then(|| resp.headers().get(LOCATION))
                .flatten()
                .and_then(|v| std::str::from_utf8(v.as_bytes()).ok());
            if let Some(location) = location {
                hops += 1;
                if hops > MAX_REDIRECTS {
                    return Err(Error::Protocol(
                        "The server redirected too many times".into(),
                    ));
                }
                let next = target
                    .join(location.trim())
                    .ok()
                    // A guard says itself what is wrong with another scheme.
                    .filter(|u| {
                        matches!(u.scheme(), "http" | "https")
                            || matches!(req.policy, Policy::Media { .. })
                    })
                    .ok_or_else(|| {
                        Error::Protocol(
                            "The server redirected to an address that isn't valid".into(),
                        )
                    })?;
                meta = meta.redirected(status, &next);
                target = next;
                proxy_turns = 0;
                continue;
            }

            match status {
                StatusCode::UNAUTHORIZED if !may_log_in => return Err(Error::Http(401)),
                StatusCode::UNAUTHORIZED => {
                    let challenge = parse_challenge(resp.headers());
                    let windows = challenge.as_ref().is_some_and(|c| c.is_windows());
                    let realm = challenge.as_ref().and_then(|c| c.realm.clone());
                    let asked = challenge.as_ref().map_or(Asked::Unsaid, |c| c.asks());
                    // Windows logins, and those given for exactly this site, keep to it.
                    let exact = windows || self.find(&origin).is_some_and(|a| a.has_password());
                    let key = if exact { origin } else { host_key(&target) };
                    let entry = self.entry(&key);
                    let sent_here = sent.as_ref() == Some(&key);
                    let retry = challenge.is_some_and(|c| entry.learn(c, sent_here))
                        && !(fell_back && sent_here);
                    logins += 1;
                    if retry && logins < MAX_LOGINS {
                        continue;
                    }
                    return Err(Error::AuthRequired {
                        // A login that can't answer counts as refused, or a saved one would
                        // be tried again and again.
                        rejected: entry.has_login(),
                        host: key,
                        realm,
                        asked,
                    });
                }
                StatusCode::PROXY_AUTHENTICATION_REQUIRED => {
                    // Only a proxy passing the request on may ask: not a server over TLS.
                    let asks = exit
                        .auth
                        .as_ref()
                        .filter(|_| exit.forwards(target.scheme() == "https"));
                    let Some(auth) = asks else {
                        return Err(Error::ProxyAuth);
                    };
                    let challenge = parse_challenge_in(resp.headers(), &PROXY_AUTHENTICATE);
                    let realm = challenge.as_ref().and_then(|c| c.realm.clone());
                    let asked = challenge.as_ref().map(|c| c.asks()).unwrap_or_default();
                    let retry = challenge.is_some_and(|c| auth.learn(c, sent_to_proxy))
                        && !(proxy_fell_back && sent_to_proxy);
                    proxy_turns += 1;
                    if retry && proxy_turns < MAX_PROXY_TURNS {
                        continue;
                    }
                    return Err(exit.login_error(auth.has_login(), asked, realm));
                }
                _ => return Ok(Reply { resp, lease, meta }),
            }
        }
    }

    /// One hop, one way out.
    async fn over(
        &self,
        req: &Request<'_>,
        url: &Url,
        scoped: &Scoped,
        exit: Exit,
        may_log_in: bool,
    ) -> std::result::Result<Step, Miss> {
        let https = url.scheme() == "https";
        let origin = site_key(url);
        let forwards = exit.forwards(https);
        let site_windows = self
            .find(&origin)
            .filter(|_| may_log_in)
            .and_then(|auth| auth.windows_login().map(|login| (auth, login)));
        let proxy_windows = exit
            .auth
            .clone()
            .filter(|_| forwards)
            .and_then(|auth| auth.windows_login().map(|login| (auth, login)));
        if site_windows.is_some() && proxy_windows.is_some() {
            return Err(Miss::Stop(Error::Protocol(
                "The proxy and the site both want a Windows login, which only works over https"
                    .into(),
            )));
        }

        let mut sent = None;
        let mut site = None;
        if may_log_in && site_windows.is_none() {
            // The login for exactly this scheme, host and port, else the host's.
            let found = self
                .find(&origin)
                .filter(|a| a.ready() && a.has_password())
                .map(|a| (origin.clone(), a))
                .or_else(|| {
                    let key = host_key(url);
                    self.find(&key).map(|a| (key, a))
                });
            if let Some((key, auth)) = found {
                if auth.ready() {
                    sent = Some(key);
                }
                site = Some(auth);
            }
        }
        let sent_to_proxy = forwards
            && exit
                .auth
                .as_ref()
                .is_some_and(|a| a.ready() && a.has_password());
        let binding = site_windows.is_some();
        let how = exit.how(https, binding);

        let hop = Hop {
            req,
            url,
            scoped,
            exit: &exit,
            site,
            unreached: AtomicBool::new(false),
        };
        let behind = match (proxy_windows, site_windows) {
            (Some((auth, login)), _) => Some((Door::Proxy, auth, login)),
            (None, Some((auth, login))) => Some((Door::Site, auth, login)),
            (None, None) => None,
        };
        let result = match &behind {
            Some((door, auth, login)) => {
                let attempt = windows::Attempt {
                    net: self,
                    hop: &hop,
                    door: *door,
                    key: &origin,
                    auth,
                    login: login.clone(),
                };
                attempt.run().await.map(|outcome| match outcome {
                    Outcome::Answer(resp, lease) => Ok((resp, lease)),
                    Outcome::FellBack => Err(match door {
                        Door::Site => Step::SiteFellBack,
                        Door::Proxy => Step::ProxyFellBack,
                    }),
                })
            }
            None => match self.client(
                &exit,
                &how,
                url,
                (req.streams && https).then(|| self.next_slot()).flatten(),
            ) {
                Ok(out) => {
                    let first = out.first_turn(url).await;
                    let sent = hop.send(&out.client, None).await;
                    if sent.is_ok() {
                        out.answered(url);
                    }
                    drop(first);
                    sent.map(|resp| Ok((resp, None)))
                }
                Err(e) => Err(e),
            },
        };
        let error = match result {
            Ok(Ok((resp, lease))) => {
                return Ok(Step::Answer(Box::new(Answer {
                    resp,
                    lease,
                    exit,
                    sent,
                    sent_to_proxy,
                })));
            }
            Ok(Err(step)) => return Ok(step),
            Err(e) => e,
        };
        if !matches!(error, Error::Network(_)) || !hop.unreached.load(Relaxed) {
            return Err(Miss::Stop(error));
        }

        // No connection: find out whether the proxy is to blame, or what it wants.
        let Some(proxy) = exit.proxy() else {
            return Err(Miss::Next(error));
        };
        let checked = if forwards {
            exit.reach().await
        } else {
            let host = url.host_str().unwrap_or("");
            let port = url.port_or_known_default().unwrap_or(80);
            exit.dial(host, port).await.map(drop)
        };
        match checked {
            Ok(()) if exit.how(https, binding) != how => Ok(Step::Again),
            Ok(()) => Err(Miss::Stop(error)),
            Err(Failed::Unreachable(why)) => {
                self.router.mark_failed(proxy);
                Err(Miss::Next(Error::Network(why)))
            }
            Err(failed) => Err(Miss::Stop(exit.error(failed))),
        }
    }
}
