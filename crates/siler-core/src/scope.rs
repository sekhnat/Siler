//! Where the browser's context may go. Cookies, extra headers, a form's body and the page's
//! address belong to the site they were given for ([`RequestMeta::origin`]). A request is
//! only built from a [`Scoped`] context, so every one of them passes through here.

use reqwest::header::CONTENT_TYPE;
use reqwest::{StatusCode, Url};

use crate::RequestMeta;

/// The browser's context, cut down to what one address may have.
pub(crate) struct Scoped {
    meta: RequestMeta,
    logins: bool,
}

impl Scoped {
    /// For a file: everything at home (the same origin), next to nothing anywhere else.
    pub fn file(meta: &RequestMeta, url: &Url) -> Scoped {
        let home = meta.is_home(url);
        Scoped::cut(meta, url, home, home)
    }

    /// For a stream: its segments often come from a sibling host that needs the cookies, so
    /// those and the headers go to the same site too. `source` says this is the stream's own
    /// request, whose form body goes only home.
    pub fn media(meta: &RequestMeta, url: &Url, source: bool) -> Scoped {
        let home = meta.is_home(url);
        let related = home
            || meta.home().is_some_and(|h| {
                !downgraded(&h, url)
                    && related_hosts(h.host_str().unwrap_or(""), url.host_str().unwrap_or(""))
            });
        Scoped::cut(meta, url, related, home && source)
    }

    fn cut(meta: &RequestMeta, url: &Url, context: bool, body: bool) -> Scoped {
        let mut out = RequestMeta {
            user_agent: meta.user_agent.clone(),
            ..Default::default()
        };
        if context {
            out.referrer = meta.referrer.clone();
            out.cookies = meta.cookies.clone();
            out.headers = meta.headers.clone();
        } else {
            out.referrer = meta.referrer.as_deref().and_then(|r| origin_only(r, url));
        }
        if body {
            out.method = meta.method.clone();
            out.body = meta.body.clone();
        }
        // A login given for https isn't repeated in clear text to the same server.
        let logins = !meta
            .home()
            .is_some_and(|h| downgraded(&h, url) && h.host_str() == url.host_str());
        Scoped { meta: out, logins }
    }

    pub fn meta(&self) -> &RequestMeta {
        &self.meta
    }

    /// The server's saved login may be sent with this request.
    pub fn logins(&self) -> bool {
        self.logins
    }
}

impl RequestMeta {
    /// Ties the context to the site of `url`, unless it already belongs to one. An address
    /// that can't be read leaves it tied to none, so it goes nowhere.
    pub fn bound_to(mut self, url: &str) -> RequestMeta {
        if self.origin.is_none() {
            let origin = Url::parse(url.trim()).ok().and_then(|u| origin_of(&u));
            self.origin = Some(origin.unwrap_or_default());
        }
        self
    }

    fn home(&self) -> Option<Url> {
        Url::parse(self.origin.as_deref()?).ok()
    }

    /// The same scheme, host and port, or https on the standard port of an http site's host.
    /// Without an origin nothing is home.
    pub(crate) fn is_home(&self, url: &Url) -> bool {
        let Some(home) = self.home() else {
            return false;
        };
        if home.host_str() != url.host_str() {
            return false;
        }
        let same = home.scheme() == url.scheme()
            && home.port_or_known_default() == url.port_or_known_default();
        let upgrade = home.scheme() == "http"
            && url.scheme() == "https"
            && home.port().is_none()
            && url.port().is_none();
        same || upgrade
    }

    /// What is left after a redirect to `next`: a form becomes a plain GET when the answer
    /// says so (301, 302, 303) or when it leads off the site, and stays one from then on.
    pub(crate) fn redirected(mut self, status: StatusCode, next: &Url) -> RequestMeta {
        let posting = self
            .method
            .as_deref()
            .is_some_and(|m| !m.trim().eq_ignore_ascii_case("GET"));
        let becomes_get = matches!(
            status,
            StatusCode::MOVED_PERMANENTLY | StatusCode::FOUND | StatusCode::SEE_OTHER
        );
        if posting && (becomes_get || !self.is_home(next)) {
            self.method = None;
            self.body = None;
            self.headers
                .retain(|(name, _)| !name.eq_ignore_ascii_case(CONTENT_TYPE.as_str()));
        }
        self
    }
}

fn origin_of(url: &Url) -> Option<String> {
    let host = url.host_str()?;
    Some(match url.port() {
        Some(port) => format!("{}://{host}:{port}", url.scheme()),
        None => format!("{}://{host}", url.scheme()),
    })
}

fn downgraded(from: &Url, to: &Url) -> bool {
    from.scheme() == "https" && to.scheme() == "http"
}

/// A page's address as other sites get it: its origin, and nothing from https to http.
fn origin_only(referrer: &str, to: &Url) -> Option<String> {
    let page = Url::parse(referrer).ok()?;
    if !matches!(page.scheme(), "http" | "https") || downgraded(&page, to) {
        return None;
    }
    origin_of(&page).map(|o| format!("{o}/"))
}

fn is_address(host: &str) -> bool {
    host.trim_matches(['[', ']'])
        .parse::<std::net::IpAddr>()
        .is_ok()
}

/// Whether cookies for host `a` may go to host `b`: the same host, one a subdomain of the
/// other, or both on the same site (`www.example.com` and `cdn.example.com`), and never
/// across a public suffix (`a.github.io` and `b.github.io`). Addresses only match themselves.
pub(crate) fn related_hosts(a: &str, b: &str) -> bool {
    let a = a.trim_end_matches('.').to_ascii_lowercase();
    let b = b.trim_end_matches('.').to_ascii_lowercase();
    if a.is_empty() || b.is_empty() {
        return false;
    }
    if a == b {
        return true;
    }
    if is_address(&a) || is_address(&b) {
        return false;
    }
    match (psl::domain_str(&a), psl::domain_str(&b)) {
        (Some(x), Some(y)) if x == y => {}
        _ => return false,
    }
    let under = |h: &str, parent: &str| h.ends_with(&format!(".{parent}"));
    let site = site_of(&a);
    under(&b, &a) || under(&a, &b) || b == site || under(&b, site)
}

/// The site a host belongs to: the host without its first label, but never more than the
/// domain someone registered (`example.co.uk` and `alice.github.io` stay as they are).
pub fn site_of(host: &str) -> &str {
    if is_address(host) {
        return host;
    }
    let Some(registered) = psl::domain_str(host) else {
        return host;
    };
    if registered.len() >= host.len() {
        return host;
    }
    host.split_once('.').map_or(host, |(_, parent)| parent)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn url(s: &str) -> Url {
        Url::parse(s).expect("URL")
    }

    fn meta(origin: &str) -> RequestMeta {
        RequestMeta {
            referrer: Some("https://page.example/private/path?q=1".into()),
            cookies: Some("s=1".into()),
            ..Default::default()
        }
        .bound_to(origin)
    }

    #[test]
    fn home_is_the_origin_or_its_https_upgrade() {
        let m = meta("http://h.example/a?token=x");
        assert_eq!(m.origin.as_deref(), Some("http://h.example"));
        assert!(m.is_home(&url("http://h.example:80/b")));
        assert!(m.is_home(&url("https://h.example/b")));
        assert!(!m.is_home(&url("https://h.example:8443/b")));
        assert!(!m.is_home(&url("http://cdn.h.example/b")));
        assert!(!meta("https://h.example/a").is_home(&url("http://h.example/a")));
        assert!(!RequestMeta::default().is_home(&url("http://h.example/a")));
    }

    #[test]
    fn binding_happens_once() {
        let m = meta("https://first.example/").bound_to("https://second.example/");
        assert_eq!(m.origin.as_deref(), Some("https://first.example"));
    }

    #[test]
    fn strangers_get_the_pages_origin_and_nothing_over_clear_text() {
        let m = meta("https://h.example/a");
        let there = Scoped::file(&m, &url("https://cdn.other/f"));
        assert_eq!(there.meta().cookies, None);
        assert_eq!(
            there.meta().referrer.as_deref(),
            Some("https://page.example/")
        );
        assert_eq!(
            Scoped::file(&m, &url("http://cdn.other/f")).meta().referrer,
            None
        );
        let clear = url("http://h.example/a");
        assert!(!Scoped::file(&m, &clear).logins());
        assert_eq!(Scoped::media(&m, &clear, false).meta().cookies, None);
    }

    #[test]
    fn sites() {
        assert_eq!(site_of("www.example.com"), "example.com");
        assert_eq!(site_of("a.b.example.com"), "b.example.com");
        assert_eq!(site_of("example.co.uk"), "example.co.uk");
        assert_eq!(site_of("alice.github.io"), "alice.github.io");
        assert_eq!(site_of("192.168.1.2"), "192.168.1.2");
        assert_eq!(site_of("localhost"), "localhost");
    }
}
