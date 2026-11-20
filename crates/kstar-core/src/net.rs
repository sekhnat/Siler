//! The HTTP client and what it knows about servers: the proxy, and logins per server.

use base64::Engine as _;
use digest_auth::{AuthContext, HttpMethod, WwwAuthenticateHeader};
use reqwest::header::{HeaderMap, HeaderValue, WWW_AUTHENTICATE};
use reqwest::{Client, Method, Url};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use crate::proxy::{self, ProxyRoute, ProxySettings};
use crate::resolve::{self, Lookuper, PublicOnly};
use crate::{Error, Result};

/// A username and password for one server.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Login {
    pub username: String,
    pub password: String,
}

impl std::fmt::Debug for Login {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Login")
            .field("username", &self.username)
            .finish_non_exhaustive()
    }
}

/// How a server wants to be logged in to, learned from its 401 answer.
#[derive(Debug, Clone, Default)]
pub(crate) enum Scheme {
    /// Not asked yet: no credentials are sent until the server asks.
    #[default]
    Unknown,
    Basic,
    Digest(Box<WwwAuthenticateHeader>),
}

/// What a 401 answer asked for.
#[derive(Debug, Clone)]
pub(crate) struct Challenge {
    scheme: Scheme,
    pub realm: Option<String>,
    /// Digest: the nonce expired, the same login is still good.
    stale: bool,
}

fn realm_of(v: &str) -> Option<String> {
    let i = v.to_ascii_lowercase().find("realm=")?;
    let rest = v[i + 6..].trim_start();
    let realm = match rest.strip_prefix('"') {
        Some(r) => r.split('"').next().unwrap_or(""),
        None => rest.split(',').next().unwrap_or("").trim(),
    };
    (!realm.is_empty()).then(|| realm.to_string())
}

/// Reads the `WWW-Authenticate` headers, preferring Digest over Basic.
pub(crate) fn parse_challenge(headers: &HeaderMap) -> Option<Challenge> {
    let mut basic = None;
    for v in headers.get_all(WWW_AUTHENTICATE) {
        let Ok(v) = v.to_str() else { continue };
        let lower = v.trim_start().to_ascii_lowercase();
        if lower.starts_with("digest")
            && let Ok(h) = WwwAuthenticateHeader::parse(v.trim())
        {
            return Some(Challenge {
                realm: Some(h.realm.clone()).filter(|r| !r.is_empty()),
                stale: h.stale,
                scheme: Scheme::Digest(Box::new(h)),
            });
        }
        if lower.starts_with("basic") {
            basic = Some(Challenge {
                scheme: Scheme::Basic,
                realm: realm_of(v),
                stale: false,
            });
        }
    }
    basic
}

/// The login state of one server.
#[derive(Default)]
pub(crate) struct HostAuth {
    login: Mutex<Option<Login>>,
    scheme: Mutex<Scheme>,
}

impl HostAuth {
    /// A login is known and so is the way to send it.
    pub fn ready(&self) -> bool {
        self.login.lock().unwrap().is_some()
            && !matches!(*self.scheme.lock().unwrap(), Scheme::Unknown)
    }

    /// The `Authorization` header for one request, if there is a login to send.
    pub fn header(&self, method: &Method, url: &Url, body: Option<&[u8]>) -> Option<HeaderValue> {
        let login = self.login.lock().unwrap().clone()?;
        let value = match &mut *self.scheme.lock().unwrap() {
            Scheme::Unknown => return None,
            Scheme::Basic => {
                let raw = format!("{}:{}", login.username, login.password);
                format!(
                    "Basic {}",
                    base64::engine::general_purpose::STANDARD.encode(raw)
                )
            }
            Scheme::Digest(h) => {
                let uri = match url.query() {
                    Some(q) => format!("{}?{q}", url.path()),
                    None => url.path().to_string(),
                };
                let ctx = AuthContext::new_with_method(
                    login.username.as_str(),
                    login.password.as_str(),
                    uri,
                    body,
                    HttpMethod(method.as_str().to_string().into()),
                );
                h.respond(&ctx).ok()?.to_header_string()
            }
        };
        let mut v = HeaderValue::from_str(&value).ok()?;
        v.set_sensitive(true);
        Some(v)
    }

    /// Takes in a 401 answer. Returns whether trying again is worthwhile: the server just
    /// told us how to send a login we have, or a Digest nonce went stale.
    pub fn learn(&self, ch: Challenge, sent: bool) -> bool {
        let has_login = self.login.lock().unwrap().is_some();
        *self.scheme.lock().unwrap() = ch.scheme;
        has_login && (!sent || ch.stale)
    }
}

/// `host` or `host:port` (port only when it isn't the scheme's default).
pub fn host_key(url: &Url) -> String {
    match (url.host_str(), url.port()) {
        (Some(h), Some(p)) => format!("{h}:{p}"),
        (Some(h), None) => h.to_string(),
        _ => String::new(),
    }
}

pub(crate) struct Net {
    /// Follows redirects itself (file downloads).
    client: RwLock<Client>,
    /// Never follows redirects, so the caller can check every hop, and refuses names that
    /// resolve to private addresses (media).
    manual: RwLock<Client>,
    /// Like `manual`, without the address check (a media download allowed to use private
    /// addresses).
    manual_any: RwLock<Client>,
    lookup: Arc<dyn Lookuper>,
    route: RwLock<ProxyRoute>,
    hosts: Mutex<HashMap<String, Arc<HostAuth>>>,
}

/// How a client follows redirects and resolves names.
#[derive(Clone, Copy)]
enum Kind {
    /// Follows redirects; the system resolver.
    Follow,
    /// Returns redirects; refuses private addresses.
    Public,
    /// Returns redirects; any address.
    Any,
}

fn build_client(
    route: &ProxyRoute,
    login: Option<(&str, &str)>,
    kind: Kind,
    lookup: &Arc<dyn Lookuper>,
) -> Result<Client> {
    let follow = matches!(kind, Kind::Follow);
    let mut b = Client::builder()
        .redirect(if follow {
            reqwest::redirect::Policy::default()
        } else {
            reqwest::redirect::Policy::none()
        })
        .connect_timeout(Duration::from_secs(20))
        .read_timeout(Duration::from_secs(60))
        .pool_idle_timeout(Duration::from_secs(30))
        .no_proxy();
    match kind {
        Kind::Follow => {}
        Kind::Public => {
            let exempt = resolve::proxy_hosts(&[route.http.as_deref(), route.https.as_deref()]);
            b = b.dns_resolver(PublicOnly::new(lookup.clone(), exempt));
        }
        Kind::Any => b = b.dns_resolver(resolve::Any(lookup.clone())),
    }
    let no_proxy = || reqwest::NoProxy::from_string(&route.no_proxy);
    let make = |url: &str, https: bool| {
        let p = if https {
            reqwest::Proxy::https(url)
        } else {
            reqwest::Proxy::http(url)
        };
        let mut p = p
            .map_err(|e| Error::Protocol(format!("Invalid proxy address {url}: {e}")))?
            .no_proxy(no_proxy());
        if let Some((user, pass)) = login {
            p = p.basic_auth(user, pass);
        }
        Ok::<_, Error>(p)
    };
    if let Some(u) = &route.http {
        b = b.proxy(make(u, false)?);
    }
    if let Some(u) = &route.https {
        b = b.proxy(make(u, true)?);
    }
    b.build().map_err(|e| Error::Network(e.to_string()))
}

/// The three clients for one route.
fn build_all(
    route: &ProxyRoute,
    login: Option<(&str, &str)>,
    lookup: &Arc<dyn Lookuper>,
) -> Result<(Client, Client, Client)> {
    Ok((
        build_client(route, login, Kind::Follow, lookup)?,
        build_client(route, login, Kind::Public, lookup)?,
        build_client(route, login, Kind::Any, lookup)?,
    ))
}

impl Net {
    pub fn new(proxy: &ProxySettings, password: Option<&str>) -> Result<Net> {
        Net::with_lookup(proxy, password, Arc::new(resolve::System))
    }

    /// A `Net` whose media clients look names up through `lookup`.
    pub(crate) fn with_lookup(
        proxy: &ProxySettings,
        password: Option<&str>,
        lookup: Arc<dyn Lookuper>,
    ) -> Result<Net> {
        let (client, manual, manual_any) = build_all(&ProxyRoute::default(), None, &lookup)?;
        let net = Net {
            client: RwLock::new(client),
            manual: RwLock::new(manual),
            manual_any: RwLock::new(manual_any),
            lookup,
            route: RwLock::new(ProxyRoute::default()),
            hosts: Mutex::new(HashMap::new()),
        };
        net.configure(proxy, password);
        Ok(net)
    }

    /// Applies new proxy settings. A broken proxy address falls back to direct connections
    /// with an explanation in the route.
    pub fn configure(&self, proxy: &ProxySettings, password: Option<&str>) {
        let mut route = proxy::route(proxy);
        let login = proxy
            .needs_password()
            .then(|| (proxy.username.trim(), password.unwrap_or("")));
        let built = build_all(&route, login, &self.lookup);
        let (client, manual, manual_any) = match built {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!("{e}");
                route = ProxyRoute {
                    source: route.source,
                    note: Some(format!("{e}; connecting directly")),
                    ..Default::default()
                };
                match build_all(&route, None, &self.lookup) {
                    Ok(c) => c,
                    Err(e) => {
                        tracing::error!("can't create the HTTP client: {e}");
                        return;
                    }
                }
            }
        };
        tracing::info!("proxy: {}", route.summary());
        *self.client.write().unwrap() = client;
        *self.manual.write().unwrap() = manual;
        *self.manual_any.write().unwrap() = manual_any;
        *self.route.write().unwrap() = route;
    }

    pub fn client(&self) -> Client {
        self.client.read().unwrap().clone()
    }

    /// A client that returns redirects instead of following them. Unless `allow_private`,
    /// it refuses names that resolve to this computer or the local network.
    pub fn manual_client(&self, allow_private: bool) -> Client {
        if allow_private {
            self.manual_any.read().unwrap().clone()
        } else {
            self.manual.read().unwrap().clone()
        }
    }

    pub fn route(&self) -> ProxyRoute {
        self.route.read().unwrap().clone()
    }

    pub fn auth_for(&self, url: &Url) -> Option<Arc<HostAuth>> {
        self.hosts.lock().unwrap().get(&host_key(url)).cloned()
    }

    pub fn entry(&self, host: &str) -> Arc<HostAuth> {
        self.hosts
            .lock()
            .unwrap()
            .entry(host.to_string())
            .or_default()
            .clone()
    }

    pub fn set_login(&self, host: &str, login: Option<Login>) {
        *self.entry(host).login.lock().unwrap() = login;
    }

    /// Moves `user:password@` out of a URL into the login for its server.
    pub fn take_userinfo(&self, url: &mut Url) {
        if url.username().is_empty() && url.password().is_none() {
            return;
        }
        let decode = |s: &str| {
            percent_encoding::percent_decode_str(s)
                .decode_utf8_lossy()
                .into_owned()
        };
        let login = Login {
            username: decode(url.username()),
            password: decode(url.password().unwrap_or("")),
        };
        self.set_login(&host_key(url), Some(login));
        let _ = url.set_username("");
        let _ = url.set_password(None);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(values: &[&str]) -> HeaderMap {
        let mut h = HeaderMap::new();
        for v in values {
            h.append(WWW_AUTHENTICATE, HeaderValue::from_str(v).unwrap());
        }
        h
    }

    #[test]
    fn challenges() {
        assert!(parse_challenge(&HeaderMap::new()).is_none());
        let c =
            parse_challenge(&headers(&["Basic realm=\"NAS files\", charset=\"UTF-8\""])).unwrap();
        assert!(matches!(c.scheme, Scheme::Basic));
        assert_eq!(c.realm.as_deref(), Some("NAS files"));
        let c = parse_challenge(&headers(&[
            "Basic realm=\"x\"",
            "Digest realm=\"box\", nonce=\"abc\", qop=\"auth\", stale=true",
        ]))
        .unwrap();
        assert!(matches!(c.scheme, Scheme::Digest(_)));
        assert_eq!(c.realm.as_deref(), Some("box"));
        assert!(c.stale);
    }

    #[test]
    fn basic_header_and_learning() {
        let a = HostAuth::default();
        let url = Url::parse("http://h/f").unwrap();
        assert!(a.header(&Method::GET, &url, None).is_none());
        let ch = parse_challenge(&headers(&["Basic realm=\"r\""])).unwrap();
        // No login yet: remember the scheme, nothing to retry with.
        assert!(!a.learn(ch.clone(), false));
        *a.login.lock().unwrap() = Some(Login {
            username: "Aladdin".into(),
            password: "open sesame".into(),
        });
        assert!(a.ready());
        let v = a.header(&Method::GET, &url, None).unwrap();
        assert_eq!(v.to_str().unwrap(), "Basic QWxhZGRpbjpvcGVuIHNlc2FtZQ==");
        // Sent and refused: the login is wrong.
        assert!(!a.learn(ch, true));
    }

    #[test]
    fn userinfo_is_moved_out() {
        let net = Net::new(&ProxySettings::default(), None).unwrap();
        let mut url = Url::parse("https://me%40x:p%3Ass@files.example:8443/a.iso").unwrap();
        net.take_userinfo(&mut url);
        assert_eq!(url.as_str(), "https://files.example:8443/a.iso");
        let a = net.auth_for(&url).unwrap();
        let login = a.login.lock().unwrap().clone().unwrap();
        assert_eq!(login.username, "me@x");
        assert_eq!(login.password, "p:ss");
        assert_eq!(host_key(&Url::parse("https://a.b:443/").unwrap()), "a.b");
    }
}
