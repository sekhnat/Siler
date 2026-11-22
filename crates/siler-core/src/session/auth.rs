//! What a server's challenge asks for, and how the login known for that server answers it.

use base64::Engine as _;
use digest_auth::{AuthContext, HttpMethod, WwwAuthenticateHeader};
use reqwest::header::{HeaderMap, HeaderName, HeaderValue, WWW_AUTHENTICATE};
use reqwest::{Method, Url};
use std::sync::Mutex;

use super::{kerberos, windows};
use crate::Asked;
use crate::net::Login;

/// How a Windows login is sent.
#[derive(Debug, Clone)]
pub(crate) enum Windows {
    /// NTLM, under the word the server offered it with (`NTLM` or `Negotiate`).
    Ntlm(&'static str),
    /// Kerberos inside `Negotiate`, and what takes over when it can't be used.
    Kerberos(Box<Scheme>),
}

/// How a server wants to be logged in to, learned from its 401 answer.
#[derive(Debug, Clone, Default)]
pub(crate) enum Scheme {
    /// Not asked yet: no credentials are sent until the server asks.
    #[default]
    Unknown,
    Basic,
    Digest(Box<WwwAuthenticateHeader>),
    Windows(Windows),
}

/// What a 401 answer asked for.
#[derive(Debug, Clone)]
pub(crate) struct Challenge {
    /// The better of Digest and Basic.
    scheme: Scheme,
    negotiate: bool,
    ntlm: bool,
    pub realm: Option<String>,
    /// Digest: the nonce expired, the same login is still good.
    stale: bool,
}

impl Challenge {
    /// Windows logins are on offer, which are kept per scheme, host and port.
    pub fn is_windows(&self) -> bool {
        self.negotiate || self.ntlm
    }

    /// What to ask the user for.
    pub fn asks(&self) -> Asked {
        if self.negotiate && kerberos::available() {
            Asked::Ticket
        } else if self.is_windows() {
            Asked::Windows
        } else {
            Asked::Password
        }
    }

    /// How to send `login`: Negotiate, then NTLM, then Digest, then Basic. A site that has
    /// Negotiate without NTLM and also Digest or Basic is taken to mean Kerberos only.
    fn pick(&self, login: &Login) -> Scheme {
        if login.is_ticket() {
            return if self.negotiate {
                Scheme::Windows(Windows::Kerberos(Box::default()))
            } else {
                Scheme::Unknown
            };
        }
        let without_kerberos = if self.ntlm {
            Scheme::Windows(Windows::Ntlm("NTLM"))
        } else if self.negotiate && matches!(self.scheme, Scheme::Unknown) {
            Scheme::Windows(Windows::Ntlm("Negotiate"))
        } else {
            self.scheme.clone()
        };
        let has_realm = login.username.contains('@') && !login.username.contains('\\');
        if self.negotiate && has_realm && kerberos::available() {
            Scheme::Windows(Windows::Kerberos(Box::new(without_kerberos)))
        } else {
            without_kerberos
        }
    }
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

/// Reads the `WWW-Authenticate` headers: whether Windows logins are offered, and the better
/// of Digest and Basic.
pub(crate) fn parse_challenge(headers: &HeaderMap) -> Option<Challenge> {
    parse_challenge_in(headers, &WWW_AUTHENTICATE)
}

/// The same in the header `name`: a proxy asks in `Proxy-Authenticate`.
pub(crate) fn parse_challenge_in(headers: &HeaderMap, name: &HeaderName) -> Option<Challenge> {
    let mut found = Challenge {
        scheme: Scheme::Unknown,
        negotiate: false,
        ntlm: false,
        realm: None,
        stale: false,
    };
    for v in headers.get_all(name) {
        let Ok(v) = v.to_str() else { continue };
        let lower = v.trim_start().to_ascii_lowercase();
        if lower.starts_with("negotiate") || lower.starts_with("ntlm") {
            // These take no quoted values, so a comma starts the next scheme.
            for part in lower.split(',') {
                match part.split_whitespace().next() {
                    Some("negotiate") => found.negotiate = true,
                    Some("ntlm") => found.ntlm = true,
                    _ => {}
                }
            }
        } else if lower.starts_with("digest") {
            if !matches!(found.scheme, Scheme::Digest(_))
                && let Ok(h) = WwwAuthenticateHeader::parse(v.trim())
            {
                found.realm = Some(h.realm.clone()).filter(|r| !r.is_empty());
                found.stale = h.stale;
                found.scheme = Scheme::Digest(Box::new(h));
            }
        } else if lower.starts_with("basic") && !matches!(found.scheme, Scheme::Digest(_)) {
            found.realm = realm_of(v);
            found.scheme = Scheme::Basic;
        }
    }
    (found.is_windows() || !matches!(found.scheme, Scheme::Unknown)).then_some(found)
}

/// A Basic login's header.
pub(crate) fn basic(login: &Login) -> crate::Result<HeaderValue> {
    let raw = format!("{}:{}", login.username, login.password);
    let b64 = base64::engine::general_purpose::STANDARD.encode(raw);
    let mut v = HeaderValue::from_str(&format!("Basic {b64}"))
        .map_err(|e| crate::Error::Protocol(e.to_string()))?;
    v.set_sensitive(true);
    Ok(v)
}

/// The login state of one server.
#[derive(Default)]
pub(crate) struct HostAuth {
    pub(crate) login: Mutex<Option<Login>>,
    scheme: Mutex<Scheme>,
    pub(crate) windows: windows::Logins,
}

impl HostAuth {
    /// A login is known, and it goes in a header of each request (Basic, Digest).
    pub fn ready(&self) -> bool {
        self.login.lock().unwrap().is_some()
            && matches!(
                *self.scheme.lock().unwrap(),
                Scheme::Basic | Scheme::Digest(_)
            )
    }

    /// The login, when the site wants it the Windows way.
    pub fn windows_login(&self) -> Option<Login> {
        if !matches!(*self.scheme.lock().unwrap(), Scheme::Windows(_)) {
            return None;
        }
        self.login.lock().unwrap().clone()
    }

    pub fn has_login(&self) -> bool {
        self.login.lock().unwrap().is_some()
    }

    /// A login with a password, which Basic and Digest need.
    pub fn has_password(&self) -> bool {
        self.login
            .lock()
            .unwrap()
            .as_ref()
            .is_some_and(|l| !l.is_ticket())
    }

    pub fn scheme(&self) -> Scheme {
        self.scheme.lock().unwrap().clone()
    }

    pub fn use_scheme(&self, scheme: Scheme) {
        *self.scheme.lock().unwrap() = scheme;
    }

    pub fn set(&self, login: Option<Login>) {
        *self.login.lock().unwrap() = login;
        self.windows.forget();
        // How a Windows login is sent depends on the login, so it is chosen again.
        let mut scheme = self.scheme.lock().unwrap();
        if matches!(*scheme, Scheme::Windows(_)) {
            *scheme = Scheme::Unknown;
        }
    }

    /// The `Authorization` header for one request, if there is a login to send.
    pub fn header(&self, method: &Method, url: &Url, body: Option<&[u8]>) -> Option<HeaderValue> {
        let target = match url.query() {
            Some(q) => format!("{}?{q}", url.path()),
            None => url.path().to_string(),
        };
        self.answer(method, &target, body)
    }

    /// The same for a request target as it goes out: a proxy sees `host:port` or a whole
    /// address.
    pub fn answer(
        &self,
        method: &Method,
        target: &str,
        body: Option<&[u8]>,
    ) -> Option<HeaderValue> {
        let login = self.login.lock().unwrap().clone()?;
        let value = match &mut *self.scheme.lock().unwrap() {
            Scheme::Unknown | Scheme::Windows(_) => return None,
            _ if login.is_ticket() => return None,
            Scheme::Basic => return basic(&login).ok(),
            Scheme::Digest(h) => {
                let ctx = AuthContext::new_with_method(
                    login.username.as_str(),
                    login.password.as_str(),
                    target,
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
        let login = self.login.lock().unwrap().clone();
        let windows = ch.is_windows();
        let scheme = match &login {
            Some(login) if windows || login.is_ticket() => ch.pick(login),
            // Not a hint to send a password readably where Windows logins are on offer.
            None if windows => Scheme::Unknown,
            _ => ch.scheme,
        };
        let usable = login.is_some() && !matches!(scheme, Scheme::Unknown);
        // A password sent another way says nothing about a Windows login.
        let another_way = matches!(scheme, Scheme::Windows(_));
        *self.scheme.lock().unwrap() = scheme;
        usable && (!sent || ch.stale || another_way)
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
    fn the_order() {
        let pick = |offers: &[&str], user: &str| {
            let login = Login {
                username: user.into(),
                password: "p".into(),
            };
            parse_challenge(&headers(offers)).unwrap().pick(&login)
        };
        let both = ["Basic realm=\"x\"", "Negotiate, NTLM"];
        assert!(matches!(
            pick(&both, "CORP\\jo"),
            Scheme::Windows(Windows::Ntlm("NTLM"))
        ));
        if kerberos::available() {
            assert!(matches!(
                pick(&both, "jo@corp.example"),
                Scheme::Windows(Windows::Kerberos(_))
            ));
        }
        assert!(matches!(
            pick(&["Negotiate"], "jo"),
            Scheme::Windows(Windows::Ntlm("Negotiate"))
        ));
        // Kerberos only, with Basic for those without a ticket.
        let apache = ["Negotiate", "Basic realm=\"x\""];
        assert!(matches!(pick(&apache, "jo"), Scheme::Basic));
        let ticket = parse_challenge(&headers(&["NTLM"])).unwrap();
        assert!(matches!(
            ticket.pick(&Login::kerberos_ticket()),
            Scheme::Unknown
        ));
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
}
