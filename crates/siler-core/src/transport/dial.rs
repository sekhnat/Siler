//! One way out ([`Exit`]): connections to a server directly or through a proxy of any type,
//! logged in to that proxy. FTP, SFTP and the tunnels reqwest can't make itself use them.

use reqwest::Method;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue, PROXY_AUTHENTICATE};
use std::io;
use std::net::IpAddr;
use std::sync::{Arc, OnceLock};
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;
use tokio_rustls::rustls::{self, ClientConfig, pki_types::ServerName};

use crate::net::Login;
use crate::proxy::{Kind, Proxy, Via};
use crate::resolve::Lookuper;
use crate::session::auth::{HostAuth, Scheme, Windows, parse_challenge_in};
use crate::session::windows;
use crate::{Asked, Error, Result};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(20);
/// For each answer of a proxy.
const ANSWER_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_ATTEMPTS: usize = 4;
const MAX_HEAD: usize = 32 * 1024;
const MAX_REFUSAL_BODY: usize = 256 * 1024;

pub(crate) trait Io: AsyncRead + AsyncWrite + Send + Unpin {}
impl<T: AsyncRead + AsyncWrite + Send + Unpin> Io for T {}

/// A connection, whatever carries it.
pub(crate) type Stream = Box<dyn Io>;

/// The TLS settings for FTPS and HTTPS proxies; one copy, so sessions resume.
pub(crate) fn tls_config() -> Result<Arc<ClientConfig>> {
    static CONFIG: OnceLock<std::result::Result<Arc<ClientConfig>, String>> = OnceLock::new();
    CONFIG
        .get_or_init(|| {
            use rustls_platform_verifier::BuilderVerifierExt;
            let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
            ClientConfig::builder_with_provider(provider)
                .with_safe_default_protocol_versions()
                .and_then(|b| b.with_platform_verifier())
                .map(|b| Arc::new(b.with_no_client_auth()))
                .map_err(|e| e.to_string())
        })
        .clone()
        .map_err(|e| Error::Protocol(format!("TLS can't be set up: {e}")))
}

/// Why a connection couldn't be made.
#[derive(Debug)]
pub(crate) enum Failed {
    /// The proxy itself (or, without one, the server) can't be reached.
    Unreachable(String),
    /// The proxy wants a login, or refused it.
    Login {
        rejected: bool,
        asked: Asked,
        realm: Option<String>,
    },
    /// The name points at a private address, and those aren't allowed here.
    Forbidden(String),
    Other(String),
}

fn other(e: impl std::fmt::Display) -> Failed {
    Failed::Other(format!("proxy: {e}"))
}

/// How reqwest gets out through an [`Exit`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum How {
    Plain,
    /// The proxy takes a Basic login.
    Basic(Login),
    /// The SOCKS login (for version 4 its name).
    Socks(Login),
    /// A private tunnel of Siler's: for logins reqwest can't do, and to see the server's
    /// certificate, which reqwest doesn't show through a proxy.
    Tunnel,
}

/// One way out, with the login its proxy has.
#[derive(Clone)]
pub(crate) struct Exit {
    pub via: Via,
    pub allow_private: bool,
    pub auth: Option<Arc<HostAuth>>,
    /// The login is from the settings, which is then where to put it right.
    own_login: bool,
    lookup: Arc<dyn Lookuper>,
}

impl Exit {
    pub fn new(
        via: Via,
        allow_private: bool,
        auth: Option<Arc<HostAuth>>,
        own_login: bool,
        lookup: Arc<dyn Lookuper>,
    ) -> Exit {
        Exit {
            via,
            allow_private,
            auth,
            own_login,
            lookup,
        }
    }

    pub fn proxy(&self) -> Option<&Proxy> {
        match &self.via {
            Via::Proxy(proxy) => Some(proxy),
            Via::Direct => None,
        }
    }

    /// The proxy reads each request and passes it on (plain http through an HTTP proxy).
    pub fn forwards(&self, https: bool) -> bool {
        !https && self.proxy().is_some_and(|p| p.kind.is_http())
    }

    /// How reqwest goes out this way; `binding` says the server's certificate is needed.
    pub fn how(&self, https: bool, binding: bool) -> How {
        let (Some(proxy), Some(auth)) = (self.proxy(), &self.auth) else {
            return How::Plain;
        };
        if https && binding {
            return How::Tunnel;
        }
        let login = auth.login.lock().unwrap().clone();
        let Some(login) = login else {
            return How::Plain;
        };
        if !proxy.kind.is_http() {
            return if login.is_ticket() {
                How::Plain
            } else {
                How::Socks(login)
            };
        }
        match auth.scheme() {
            Scheme::Basic if !login.is_ticket() => How::Basic(login),
            Scheme::Digest(_) | Scheme::Windows(_) if https => How::Tunnel,
            _ => How::Plain,
        }
    }

    pub fn login_error(&self, rejected: bool, asked: Asked, realm: Option<String>) -> Error {
        match self.proxy() {
            Some(proxy) if !self.own_login => Error::AuthRequired {
                host: proxy.login_key(),
                realm,
                rejected,
                asked,
            },
            _ => Error::ProxyAuth,
        }
    }

    pub fn error(&self, failed: Failed) -> Error {
        match failed {
            Failed::Login {
                rejected,
                asked,
                realm,
            } => self.login_error(rejected, asked, realm),
            Failed::Forbidden(why) => Error::Media(crate::MediaError::UrlForbidden(why)),
            Failed::Unreachable(why) | Failed::Other(why) => Error::Network(why),
        }
    }

    /// Whether the proxy can be reached at all.
    pub async fn reach(&self) -> std::result::Result<(), Failed> {
        match self.proxy() {
            Some(proxy) => self.open(proxy).await.map(drop),
            None => Ok(()),
        }
    }

    /// A connection to `host:port` this way.
    pub async fn dial(&self, host: &str, port: u16) -> std::result::Result<Stream, Failed> {
        let host = host.trim_matches(['[', ']']);
        let Some(proxy) = self.proxy() else {
            let tcp = connect(host, port)
                .await
                .map_err(|e| Failed::Unreachable(e.to_string()))?;
            return Ok(Box::new(tcp));
        };
        if proxy.kind.is_http() {
            return self.http(proxy, host, port).await;
        }
        let work = async {
            let mut s = self.open(proxy).await?;
            match proxy.kind {
                Kind::Socks5 => self.socks5(&mut s, host, port).await?,
                kind => self.socks4(&mut s, kind, host, port).await?,
            }
            Ok(s)
        };
        tokio::time::timeout(ANSWER_TIMEOUT, work)
            .await
            .map_err(|_| other("it didn't answer"))?
    }

    async fn open(&self, proxy: &Proxy) -> std::result::Result<Stream, Failed> {
        let away = |e: &dyn std::fmt::Display| {
            Failed::Unreachable(format!("can't reach the proxy {proxy}: {e}"))
        };
        let tcp = connect(&proxy.host, proxy.port)
            .await
            .map_err(|e| away(&e))?;
        if proxy.kind != Kind::Https {
            return Ok(Box::new(tcp));
        }
        let config = tls_config().map_err(other)?;
        let name = ServerName::try_from(proxy.host.clone()).map_err(other)?;
        let tls = tokio::time::timeout(
            CONNECT_TIMEOUT,
            TlsConnector::from(config).connect(name, tcp),
        )
        .await
        .map_err(|e| away(&e))?
        .map_err(|e| away(&e))?;
        Ok(Box::new(tls))
    }

    /// The login when it is a name and password.
    fn password_login(&self) -> Option<Login> {
        self.auth
            .as_ref()
            .and_then(|a| a.login.lock().unwrap().clone())
            .filter(|l| !l.is_ticket())
    }

    async fn socks5(
        &self,
        s: &mut Stream,
        host: &str,
        port: u16,
    ) -> std::result::Result<(), Failed> {
        let fail = |m: &str| Failed::Other(format!("SOCKS proxy: {m}"));
        let login = self.password_login();
        let refused = || Failed::Login {
            rejected: login.is_some(),
            asked: Asked::Password,
            realm: None,
        };
        let methods: &[u8] = if login.is_some() { &[0, 2] } else { &[0] };
        let mut hello = vec![5, methods.len() as u8];
        hello.extend_from_slice(methods);
        s.write_all(&hello).await.map_err(other)?;
        let mut reply = [0u8; 2];
        s.read_exact(&mut reply).await.map_err(other)?;
        if reply[0] != 5 {
            return Err(fail("not a SOCKS5 proxy"));
        }
        match (reply[1], &login) {
            (0, _) => {}
            (2, Some(Login { username, password })) => {
                let (Ok(u), Ok(p)) = (u8::try_from(username.len()), u8::try_from(password.len()))
                else {
                    return Err(fail("the username or password is too long"));
                };
                let mut auth = vec![1, u];
                auth.extend_from_slice(username.as_bytes());
                auth.push(p);
                auth.extend_from_slice(password.as_bytes());
                s.write_all(&auth).await.map_err(other)?;
                s.read_exact(&mut reply).await.map_err(other)?;
                if reply[1] != 0 {
                    return Err(refused());
                }
            }
            (2 | 0xff, _) => return Err(refused()),
            _ => return Err(fail("no login method in common")),
        }
        // The proxy resolves the name (socks5h), unless it is an address.
        let mut req = vec![5, 1, 0];
        match host.parse::<IpAddr>() {
            Ok(IpAddr::V4(a)) => {
                req.push(1);
                req.extend_from_slice(&a.octets());
            }
            Ok(IpAddr::V6(a)) => {
                req.push(4);
                req.extend_from_slice(&a.octets());
            }
            Err(_) => {
                let len =
                    u8::try_from(host.len()).map_err(|_| fail("the server name is too long"))?;
                req.extend([3, len]);
                req.extend_from_slice(host.as_bytes());
            }
        }
        req.extend_from_slice(&port.to_be_bytes());
        s.write_all(&req).await.map_err(other)?;
        let mut head = [0u8; 4];
        s.read_exact(&mut head).await.map_err(other)?;
        if head[1] != 0 {
            return Err(fail(match head[1] {
                2 => "the connection isn't allowed",
                3 => "network unreachable",
                4 => "host unreachable",
                5 => "connection refused",
                6 => "timed out",
                _ => "the connection failed",
            }));
        }
        let address = match head[3] {
            1 => 4,
            4 => 16,
            3 => usize::from(s.read_u8().await.map_err(other)?),
            _ => return Err(fail("unexpected answer")),
        };
        let mut rest = vec![0u8; address + 2];
        s.read_exact(&mut rest).await.map_err(other)?;
        Ok(())
    }

    async fn socks4(
        &self,
        s: &mut Stream,
        kind: Kind,
        host: &str,
        port: u16,
    ) -> std::result::Result<(), Failed> {
        let fail = |m: &str| Failed::Other(format!("SOCKS4 proxy: {m}"));
        let (address, name) = match host.parse::<IpAddr>() {
            Ok(IpAddr::V4(a)) => (a, None),
            Ok(IpAddr::V6(_)) => return Err(fail("it can't reach IPv6 addresses")),
            Err(_) if kind == Kind::Socks4a => (std::net::Ipv4Addr::new(0, 0, 0, 1), Some(host)),
            // Version 4 has no names, so the lookup is here, with the media address check.
            Err(_) => {
                let found = self.lookup.lookup(host.to_string()).await.map_err(other)?;
                if !self.allow_private
                    && found
                        .iter()
                        .any(|a| siler_media::policy::is_private(a.ip()))
                {
                    let refused = crate::resolve::PrivateAddress { host: host.into() };
                    return Err(Failed::Forbidden(refused.to_string()));
                }
                let v4 = found.iter().find_map(|a| match a.ip() {
                    IpAddr::V4(v4) => Some(v4),
                    IpAddr::V6(_) => None,
                });
                (
                    v4.ok_or_else(|| fail("the server has no IPv4 address"))?,
                    None,
                )
            }
        };
        let mut req = vec![4, 1];
        req.extend_from_slice(&port.to_be_bytes());
        req.extend_from_slice(&address.octets());
        let user = self
            .password_login()
            .map(|l| l.username)
            .unwrap_or_default();
        if user.contains('\0') || name.is_some_and(|n| n.contains('\0')) {
            return Err(fail("a name has a NUL in it"));
        }
        req.extend_from_slice(user.as_bytes());
        req.push(0);
        if let Some(name) = name {
            req.extend_from_slice(name.as_bytes());
            req.push(0);
        }
        s.write_all(&req).await.map_err(other)?;
        let mut reply = [0u8; 8];
        s.read_exact(&mut reply).await.map_err(other)?;
        match reply[1] {
            90 => Ok(()),
            92 | 93 => Err(Failed::Login {
                rejected: !user.is_empty(),
                asked: Asked::Password,
                realm: None,
            }),
            _ => Err(fail("the connection failed")),
        }
    }

    /// CONNECT, with whatever login the proxy asks for.
    async fn http(
        &self,
        proxy: &Proxy,
        host: &str,
        port: u16,
    ) -> std::result::Result<Stream, Failed> {
        let target = if host.contains(':') {
            format!("[{host}]:{port}")
        } else {
            format!("{host}:{port}")
        };
        let auth = self.auth.as_ref().ok_or_else(|| other("no login state"))?;
        let mut fell_back = false;
        for attempt in 1..=MAX_ATTEMPTS {
            if let Some(login) = auth.windows_login() {
                match self.windows_tunnel(proxy, auth, &login, &target).await? {
                    Some(stream) => return Ok(stream),
                    None => {
                        fell_back = true;
                        continue;
                    }
                }
            }
            let mut s = self.open(proxy).await?;
            let login = auth.answer(&Method::CONNECT, &target, None);
            let sent = login.is_some();
            let (status, headers) = ask(&mut s, &target, login.as_ref()).await?;
            match status {
                200..=299 => return Ok(s),
                407 => {
                    let challenge = parse_challenge_in(&headers, &PROXY_AUTHENTICATE);
                    let realm = challenge.as_ref().and_then(|c| c.realm.clone());
                    let asked = challenge.as_ref().map(|c| c.asks()).unwrap_or_default();
                    let retry = challenge.is_some_and(|c| auth.learn(c, sent))
                        && !(fell_back && sent)
                        && attempt < MAX_ATTEMPTS;
                    if !retry {
                        return Err(Failed::Login {
                            rejected: auth.has_login(),
                            asked,
                            realm,
                        });
                    }
                }
                _ => return Err(other(format!("it refused the connection (HTTP {status})"))),
            }
        }
        Err(Failed::Login {
            rejected: true,
            asked: Asked::Password,
            realm: None,
        })
    }

    /// CONNECT with NTLM or Kerberos. `None`: another scheme has taken over.
    async fn windows_tunnel(
        &self,
        proxy: &Proxy,
        auth: &Arc<HostAuth>,
        login: &Login,
        target: &str,
    ) -> std::result::Result<Option<Stream>, Failed> {
        let _turn = auth.windows.turn().await.map_err(other)?;
        let refused = || {
            auth.use_scheme(Scheme::Unknown);
            Failed::Login {
                rejected: true,
                asked: windows::asked_again(login),
                realm: None,
            }
        };
        if auth.windows.is_refused() {
            return Err(refused());
        }
        let name = HeaderName::from_static("proxy-authenticate");
        let (status, headers, mut s) = match auth.scheme() {
            Scheme::Windows(Windows::Ntlm(word)) => {
                let mut s = self.open(proxy).await?;
                let hello = windows::ntlm_hello();
                let header = windows::token_header(word, &hello).map_err(other)?;
                let (status, headers) = ask(&mut s, target, Some(&header)).await?;
                if (200..300).contains(&status) {
                    return Ok(Some(s));
                }
                let challenge = windows::token_after(&headers, &name, word);
                let Some(challenge) = challenge.filter(|_| status == 407) else {
                    if status != 407 {
                        return Err(other(format!("it refused the connection (HTTP {status})")));
                    }
                    // It asks for something else now.
                    let changed = parse_challenge_in(&headers, &PROXY_AUTHENTICATE)
                        .is_some_and(|c| !c.is_windows() && auth.learn(c, false));
                    return if changed { Ok(None) } else { Err(refused()) };
                };
                let closes = headers
                    .iter()
                    .filter(|(name, _)| matches!(name.as_str(), "connection" | "proxy-connection"))
                    .any(|(_, v)| v.as_bytes().eq_ignore_ascii_case(b"close"));
                if closes {
                    return Err(other("it closes the connection that an NTLM login needs"));
                }
                drain(&mut s, &headers).await.map_err(other)?;
                let answer = windows::ntlm_answer(&hello, &challenge, login, &proxy.host, None)
                    .map_err(other)?;
                let header = windows::token_header(word, &answer).map_err(other)?;
                let (status, headers) = ask(&mut s, target, Some(&header)).await?;
                (status, headers, s)
            }
            Scheme::Windows(Windows::Kerberos(fallback)) => {
                let token = match windows::kerberos_token(auth, login, &proxy.host, None).await {
                    Ok(token) => token,
                    Err(failure) => {
                        return match windows::after_kerberos(auth, login, failure, *fallback) {
                            true => Ok(None),
                            false => Err(refused()),
                        };
                    }
                };
                let mut s = self.open(proxy).await?;
                let header = windows::token_header("Negotiate", &token).map_err(other)?;
                let (status, headers) = ask(&mut s, target, Some(&header)).await?;
                (status, headers, s)
            }
            _ => return Ok(None),
        };
        match status {
            200..=299 => {
                windows::settle(auth, login, windows::Standing::Good);
                Ok(Some(s))
            }
            407 => {
                windows::settle(auth, login, windows::Standing::Refused);
                tracing::debug!(?headers, "the proxy refused the login");
                let _ = s.shutdown().await;
                Err(refused())
            }
            _ => Err(other(format!("it refused the connection (HTTP {status})"))),
        }
    }
}

async fn connect(host: &str, port: u16) -> io::Result<TcpStream> {
    let tcp = tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect((host, port)))
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "connection timed out"))??;
    let _ = tcp.set_nodelay(true);
    Ok(tcp)
}

/// One CONNECT and the head of its answer, with nothing read beyond it.
async fn ask(
    s: &mut Stream,
    target: &str,
    login: Option<&HeaderValue>,
) -> std::result::Result<(u16, HeaderMap), Failed> {
    let mut req = format!("CONNECT {target} HTTP/1.1\r\nHost: {target}\r\n").into_bytes();
    if let Some(login) = login {
        req.extend_from_slice(b"Proxy-Authorization: ");
        req.extend_from_slice(login.as_bytes());
        req.extend_from_slice(b"\r\n");
    }
    req.extend_from_slice(b"\r\n");
    let work = async {
        s.write_all(&req).await?;
        s.flush().await?;
        let mut head = Vec::new();
        while !head.ends_with(b"\r\n\r\n") {
            if head.len() > MAX_HEAD {
                return Err(io::Error::other("the answer is too long"));
            }
            head.push(s.read_u8().await?);
        }
        Ok(head)
    };
    let head = tokio::time::timeout(ANSWER_TIMEOUT, work)
        .await
        .map_err(|_| other("it didn't answer"))?
        .map_err(other)?;
    let text = String::from_utf8_lossy(&head);
    let mut lines = text.split("\r\n");
    let status = lines
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|code| code.parse().ok())
        .ok_or_else(|| other("the answer isn't HTTP"))?;
    let mut headers = HeaderMap::new();
    for line in lines {
        if let Some((name, value)) = line.split_once(':')
            && let Ok(name) = HeaderName::from_bytes(name.trim().as_bytes())
            && let Ok(value) = HeaderValue::from_str(value.trim())
        {
            headers.append(name, value);
        }
    }
    Ok((status, headers))
}

/// Reads a 407's body, so that its connection can carry the login's next message.
async fn drain(s: &mut Stream, headers: &HeaderMap) -> io::Result<()> {
    let text = |name: &str| headers.get(name).and_then(|v| v.to_str().ok());
    let too_long = || io::Error::other("the refusal is too long");
    let work = async {
        if text("transfer-encoding").is_some_and(|v| v.eq_ignore_ascii_case("chunked")) {
            let mut total = 0;
            loop {
                let mut line = Vec::new();
                while !line.ends_with(b"\r\n") {
                    if line.len() > 128 {
                        return Err(too_long());
                    }
                    line.push(s.read_u8().await?);
                }
                let line = String::from_utf8_lossy(&line);
                let size = line.trim().split(';').next().unwrap_or("").trim();
                let size = usize::from_str_radix(size, 16)
                    .map_err(|_| io::Error::other("the refusal can't be read"))?;
                if size > MAX_REFUSAL_BODY - total {
                    return Err(too_long());
                }
                total += size;
                // The last chunk is empty and, without trailers, ends in the same line break.
                let mut chunk = vec![0; size + 2];
                s.read_exact(&mut chunk).await?;
                if size == 0 {
                    return Ok(());
                }
            }
        }
        let len: usize = text("content-length")
            .and_then(|v| v.parse().ok())
            .unwrap_or(0);
        if len > MAX_REFUSAL_BODY {
            return Err(too_long());
        }
        s.read_exact(&mut vec![0; len]).await.map(drop)
    };
    tokio::time::timeout(ANSWER_TIMEOUT, work)
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "it didn't answer"))?
}
