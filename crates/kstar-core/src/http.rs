//! HTTP for media. A small body is read into memory (manifests, keys, the first bytes of a
//! source); one object (a segment, or a whole track) is written to a file through a `.part`
//! file and renamed when complete. Both go through [`Net`] (proxy, logins) and the rate
//! limiters, and use the same error classes as file downloads.
//!
//! Redirects are followed here, one hop at a time, so every address a manifest leads to is
//! checked against the URL policy before it is contacted, not just the first one. Cookies and
//! the browser's extra headers go only to the source's own server (and its subdomains or
//! parents), never to the other servers a manifest names.

use futures_util::StreamExt;
use reqwest::header::{CONTENT_RANGE, CONTENT_TYPE, LOCATION, RANGE};
use reqwest::{Response, StatusCode, Url};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use tokio::io::AsyncWriteExt;

use kstar_media::policy;

use crate::error::{MediaError, describe};
use crate::net::{Net, host_key, parse_challenge};
use crate::probe::{build_request, busy, parse_content_range};
use crate::ratelimit::RateLimiter;
use crate::resolve;
use crate::{Error, RequestMeta, Result};

/// Redirects followed for one request.
const MAX_REDIRECTS: usize = 10;

/// The limiters every byte passes through, and the counter it is added to.
pub(crate) struct Limiters<'a> {
    pub global: &'a RateLimiter,
    pub run: &'a RateLimiter,
    /// The download's byte count, updated as data arrives.
    pub count: Option<&'a AtomicU64>,
}

impl Limiters<'_> {
    async fn take(&self, n: u64) {
        self.global.acquire(n).await;
        self.run.acquire(n).await;
        if let Some(c) = self.count {
            c.fetch_add(n, Relaxed);
        }
    }
}

/// Takes back the bytes of an object that wasn't kept (failed, or stopped part way), so the
/// download's count only holds what is on disk.
struct Uncount<'a> {
    count: Option<&'a AtomicU64>,
    n: u64,
}

impl Drop for Uncount<'_> {
    fn drop(&mut self) {
        if let Some(c) = self.count
            && self.n > 0
        {
            let _ = c.try_update(Relaxed, Relaxed, |v| Some(v.saturating_sub(self.n)));
        }
    }
}

/// Decides which addresses may be contacted (media-design §16.2). The URL itself is checked
/// here (scheme, credentials, literal addresses); the addresses a name resolves to are checked
/// by the client's resolver, in the same lookup the connection uses (`resolve::PublicOnly`).
pub(crate) struct Guard {
    pub allow_private: bool,
}

impl Guard {
    pub fn new(allow_private: bool) -> Guard {
        Guard { allow_private }
    }

    pub fn check(&self, url: &Url) -> Result<()> {
        policy::check(url, self.allow_private)
            .map_err(|e| Error::Media(MediaError::UrlForbidden(e.to_string())))
    }
}

/// Everything a media request needs: the network, how the source was requested, and the
/// URL policy.
pub(crate) struct Http {
    pub net: Arc<Net>,
    ua: String,
    /// How the source was requested (method, body, cookies, headers).
    meta: RequestMeta,
    /// The source's host: cookies and extra headers go only to it and its relatives.
    origin: String,
    pub guard: Guard,
}

/// Which request this is: the source itself (sent as the browser sent it), or something the
/// source names (a playlist, a key, a segment: always a plain GET).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Req {
    Source,
    Derived,
}

impl Http {
    /// `allow_private` is the user's setting; a source on a private address may also point
    /// at other private addresses (a media server at home).
    pub fn new(
        net: Arc<Net>,
        source: &Url,
        meta: RequestMeta,
        ua: &str,
        allow_private: bool,
    ) -> Http {
        Http {
            net,
            ua: ua.to_string(),
            meta,
            origin: source.host_str().unwrap_or("").to_ascii_lowercase(),
            guard: Guard::new(allow_private || policy::is_private_source(source)),
        }
    }

    /// The request details for `url`.
    fn meta_for(&self, url: &Url, kind: Req) -> RequestMeta {
        let mut m = RequestMeta {
            referrer: self.meta.referrer.clone(),
            user_agent: self.meta.user_agent.clone(),
            ..Default::default()
        };
        if kind == Req::Source {
            m.method = self.meta.method.clone();
            m.body = self.meta.body.clone();
        }
        if related_hosts(&self.origin, url.host_str().unwrap_or("")) {
            m.cookies = self.meta.cookies.clone();
            m.headers = self.meta.headers.clone();
        }
        m
    }
}

/// Whether cookies for host `a` may go to host `b`: the same host, one a subdomain of the
/// other, or both on the same site (`www.example.com` and `cdn.example.com`). Addresses only
/// match themselves.
fn related_hosts(a: &str, b: &str) -> bool {
    let a = a.trim_end_matches('.').to_ascii_lowercase();
    let b = b.trim_end_matches('.').to_ascii_lowercase();
    if a.is_empty() || b.is_empty() {
        return false;
    }
    if a == b {
        return true;
    }
    let is_ip = |h: &str| {
        h.trim_matches(['[', ']'])
            .parse::<std::net::IpAddr>()
            .is_ok()
    };
    if is_ip(&a) || is_ip(&b) {
        return false;
    }
    let under = |h: &str, parent: &str| h.ends_with(&format!(".{parent}"));
    let site = site_of(&a);
    under(&b, &a) || under(&a, &b) || (site != a && (b == site || under(&b, site)))
}

/// The site a host belongs to: the host without its first label, unless that would leave a
/// public suffix (`example.com` stays; `www.example.com` gives `example.com`;
/// `example.co.uk` stays, since `co.uk` is shared by unrelated sites).
fn site_of(host: &str) -> &str {
    let labels: Vec<&str> = host.split('.').collect();
    if labels.len() <= 2 {
        return host;
    }
    let parent = &labels[1..];
    // A short second level under a country code (co.uk, com.au, ne.jp) is a public suffix.
    let country_suffix = parent.len() == 2 && parent[1].len() == 2 && parent[0].len() <= 3;
    if country_suffix {
        return host;
    }
    &host[labels[0].len() + 1..]
}

/// One request, following redirects with every hop checked. A 401 is retried when the
/// server just said how to log in, or a Digest nonce went stale (as in `probe`).
async fn request(http: &Http, url: &Url, mut kind: Req, range: Option<String>) -> Result<Response> {
    let mut target = url.clone();
    let mut hops = 0;
    let mut logins = 0;
    loop {
        http.guard.check(&target)?;
        let meta = http.meta_for(&target, kind);
        let auth = http.net.auth_for(&target);
        let sent = auth.as_ref().is_some_and(|a| a.ready());
        let mut rb = build_request(
            &http.net.manual_client(http.guard.allow_private),
            target.clone(),
            &meta,
            &http.ua,
            auth.as_deref(),
        );
        if let Some(r) = &range {
            rb = rb.header(RANGE, r.as_str());
        }
        let resp = rb.send().await.map_err(|e| match resolve::refused(&e) {
            Some(why) => Error::Media(MediaError::UrlForbidden(why)),
            None => Error::Network(describe(&e)),
        })?;
        let status = resp.status();
        if status.is_redirection()
            && let Some(loc) = resp.headers().get(LOCATION).and_then(|v| v.to_str().ok())
        {
            hops += 1;
            if hops > MAX_REDIRECTS {
                return Err(Error::Protocol(
                    "The server redirected too many times".into(),
                ));
            }
            let next = target.join(loc.trim()).map_err(|_| {
                Error::Protocol("The server redirected to an address that isn't valid".into())
            })?;
            // 303, and 301/302 after anything but GET, continue as a plain GET.
            let not_get = meta
                .method
                .as_deref()
                .is_some_and(|m| !m.eq_ignore_ascii_case("GET"));
            if status == StatusCode::SEE_OTHER
                || (not_get && matches!(status, StatusCode::MOVED_PERMANENTLY | StatusCode::FOUND))
            {
                kind = Req::Derived;
            }
            target = next;
            continue;
        }
        match status {
            StatusCode::UNAUTHORIZED => {
                let challenge = parse_challenge(resp.headers());
                let realm = challenge.as_ref().and_then(|c| c.realm.clone());
                let host = host_key(&target);
                let retry = challenge.is_some_and(|c| http.net.entry(&host).learn(c, sent));
                logins += 1;
                if retry && logins < 3 {
                    continue;
                }
                return Err(Error::AuthRequired {
                    host,
                    realm,
                    rejected: sent,
                });
            }
            StatusCode::PROXY_AUTHENTICATION_REQUIRED => return Err(Error::ProxyAuth),
            StatusCode::TOO_MANY_REQUESTS | StatusCode::SERVICE_UNAVAILABLE => {
                return Err(busy(&resp));
            }
            s if s.is_success() => return Ok(resp),
            s => return Err(Error::Http(s.as_u16())),
        }
    }
}

/// What to do with a body longer than the cap.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Overflow {
    /// Refuse it: the whole body is needed (a manifest).
    Refuse,
    /// Keep the first `max` bytes and stop reading (a sniff of the start of a file).
    Truncate,
}

/// A body read into memory.
#[derive(Debug, Clone)]
pub(crate) struct Fetched {
    /// After redirects: the base for relative references.
    pub final_url: Url,
    pub content_type: Option<String>,
    pub body: Vec<u8>,
    /// The whole body was read (it wasn't cut at the cap).
    pub complete: bool,
}

/// Reads a body into memory, up to `max` bytes.
pub(crate) async fn fetch_bounded(
    http: &Http,
    url: &Url,
    kind: Req,
    max: u64,
    overflow: Overflow,
    lim: &Limiters<'_>,
) -> Result<Fetched> {
    let resp = request(http, url, kind, None).await?;
    let final_url = resp.url().clone();
    let content_type = resp
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    if overflow == Overflow::Refuse && resp.content_length().is_some_and(|n| n > max) {
        return Err(Error::Media(MediaError::BodyTooLarge(max)));
    }
    let mut body = Vec::new();
    let mut complete = true;
    let mut stream = resp.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| Error::Network(describe(&e)))?;
        lim.take(chunk.len() as u64).await;
        let room = max.saturating_sub(body.len() as u64) as usize;
        if chunk.len() > room {
            match overflow {
                Overflow::Refuse => return Err(Error::Media(MediaError::BodyTooLarge(max))),
                Overflow::Truncate => {
                    body.extend_from_slice(&chunk[..room]);
                    complete = false;
                    break;
                }
            }
        }
        body.extend_from_slice(&chunk);
    }
    Ok(Fetched {
        final_url,
        content_type,
        body,
        complete,
    })
}

/// The `.part` file an object is written to until it is complete.
pub(crate) fn part_of(dest: &Path) -> PathBuf {
    let mut s = dest.as_os_str().to_owned();
    s.push(".part");
    PathBuf::from(s)
}

/// Fetches one object into `dest`, through `dest.part`, which is renamed on success.
///
/// With `range`, the server must answer `206` for exactly that byte range. A `whole` file
/// (a track that is one file) keeps its `.part` file between attempts and is continued from
/// where it stopped; a server that answers with the whole file starts it over. Exactly one
/// GET per call; retries belong to the caller. Returns the size of `dest`.
pub(crate) async fn fetch_object(
    http: &Http,
    url: &Url,
    dest: &Path,
    range: Option<(u64, u64)>,
    whole: bool,
    lim: &Limiters<'_>,
) -> Result<u64> {
    let part = part_of(dest);
    let resume_from = if whole && range.is_none() {
        tokio::fs::metadata(&part)
            .await
            .ok()
            .map(|m| m.len())
            .filter(|n| *n > 0)
    } else {
        None
    };
    let header = match (range, resume_from) {
        (Some((a, b)), _) => Some(format!("bytes={a}-{b}")),
        (None, Some(n)) => Some(format!("bytes={n}-")),
        (None, None) => None,
    };
    let resp = request(http, url, Req::Derived, header.clone()).await?;
    let mut start = range.map_or(resume_from.unwrap_or(0), |(a, _)| a);
    let mut appending = resume_from.is_some();
    if header.is_some() {
        if resp.status() != StatusCode::PARTIAL_CONTENT {
            if range.is_none() {
                // The server sent the whole file instead of the rest: start it over.
                appending = false;
                start = 0;
            } else {
                return Err(Error::Protocol(
                    "The server doesn't send parts of this file, which this stream needs".into(),
                ));
            }
        } else {
            let got = resp
                .headers()
                .get(CONTENT_RANGE)
                .and_then(|v| v.to_str().ok())
                .and_then(parse_content_range)
                .map(|r| r.0);
            if got != Some(start) {
                return Err(Error::Protocol(
                    "The server returned an unexpected byte range".into(),
                ));
            }
        }
    }

    let mut file = if appending {
        let len = tokio::fs::metadata(&part)
            .await
            .map(|m| m.len())
            .unwrap_or(0);
        if Some(len) != resume_from {
            return Err(Error::Protocol("The partial file changed on disk".into()));
        }
        tokio::fs::OpenOptions::new()
            .append(true)
            .open(&part)
            .await?
    } else {
        tokio::fs::File::create(&part).await?
    };
    // Until a piece is complete, its bytes are taken back if this stops (a failure, a pause
    // that drops this future): only a finished piece counts. A whole file keeps its part.
    let mut taken = Uncount {
        count: lim.count.filter(|_| !whole),
        n: 0,
    };
    let mut written = 0u64;
    let mut stream = resp.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| Error::Network(describe(&e)))?;
        lim.take(chunk.len() as u64).await;
        taken.n += chunk.len() as u64;
        file.write_all(&chunk).await?;
        written += chunk.len() as u64;
    }
    file.flush().await?;
    drop(file);
    if let Some((a, b)) = range
        && written != b - a + 1
    {
        return Err(Error::Network("connection closed early".into()));
    }
    tokio::fs::rename(&part, dest).await?;
    taken.n = 0;
    // A range is the whole object; a resumed whole file includes the bytes already there.
    Ok(if range.is_some() {
        written
    } else {
        start + written
    })
}

/// Fetches a whole file in ranges of at most `chunk` bytes, appended to `dest.part`, which is
/// renamed on success. Some servers (YouTube's) slow down or refuse long single requests. A
/// partial file is resumed from its size; a server that ignores ranges sends the whole file.
pub(crate) async fn fetch_whole_chunked(
    http: &Http,
    url: &Url,
    dest: &Path,
    chunk: u64,
    lim: &Limiters<'_>,
) -> Result<u64> {
    let part = part_of(dest);
    let mut have = tokio::fs::metadata(&part).await.map_or(0, |m| m.len());
    let mut total: Option<u64> = None;
    let chunk = chunk.max(1);
    loop {
        if total.is_some_and(|t| have >= t) {
            break;
        }
        let end = have + chunk - 1;
        let resp = match request(http, url, Req::Derived, Some(format!("bytes={have}-{end}"))).await
        {
            // Everything is already there (a resumed file that was complete).
            Err(Error::Http(416)) if have > 0 => break,
            r => r?,
        };
        let ranged = resp.status() == StatusCode::PARTIAL_CONTENT;
        let (mut file, expect) = if ranged {
            let (start, last, size) = resp
                .headers()
                .get(CONTENT_RANGE)
                .and_then(|v| v.to_str().ok())
                .and_then(parse_content_range)
                .filter(|(a, b, _)| *a == have && b >= a)
                .ok_or_else(|| {
                    Error::Protocol("The server returned an unexpected byte range".into())
                })?;
            total = size.or(total);
            let file = tokio::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&part)
                .await?;
            (file, Some(last - start + 1))
        } else {
            // The whole file instead of a range: start over and take all of it.
            have = 0;
            (tokio::fs::File::create(&part).await?, None)
        };
        let mut written = 0u64;
        let mut stream = resp.bytes_stream();
        while let Some(bytes) = stream.next().await {
            let bytes = bytes.map_err(|e| Error::Network(describe(&e)))?;
            lim.take(bytes.len() as u64).await;
            file.write_all(&bytes).await?;
            written += bytes.len() as u64;
        }
        file.flush().await?;
        drop(file);
        have += written;
        if !ranged {
            total = Some(have);
            break;
        }
        if expect.is_some_and(|n| written != n) {
            return Err(Error::Network("connection closed early".into()));
        }
        if total.is_none() && expect.is_some_and(|n| n < chunk) {
            // No size given, and the server sent less than asked: that was the end.
            break;
        }
        if written == 0 {
            return Err(Error::Network("the server sent nothing".into()));
        }
    }
    tokio::fs::rename(&part, dest).await?;
    Ok(total.unwrap_or(have))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[test]
    fn part_files_sit_beside_their_objects() {
        assert_eq!(
            part_of(Path::new("/w/t/00000001.seg")),
            PathBuf::from("/w/t/00000001.seg.part")
        );
    }

    #[test]
    fn cookies_stay_with_the_source_site() {
        assert!(related_hosts("example.com", "example.com"));
        assert!(related_hosts("example.com", "cdn.example.com"));
        assert!(related_hosts("www.example.com", "example.com"));
        assert!(related_hosts("www.example.com", "cdn.example.com"));
        assert!(related_hosts("www.example.com", "WWW.EXAMPLE.COM"));
        assert!(related_hosts("www.bbc.co.uk", "media.bbc.co.uk"));
        assert!(!related_hosts("example.com", "evil-example.com"));
        assert!(!related_hosts("example.com", "cdn.other.net"));
        assert!(!related_hosts("example.co.uk", "evil.co.uk"));
        assert!(!related_hosts("127.0.0.1", "1.0.0.1"));
        assert!(!related_hosts("", "example.com"));
    }

    #[test]
    fn derived_requests_are_plain_gets_and_keep_cookies_at_home() {
        let net = Arc::new(Net::new(&Default::default(), None).expect("net"));
        let src = Url::parse("https://www.example.com/v/master.m3u8").expect("URL");
        let meta = RequestMeta {
            referrer: Some("https://www.example.com/watch".into()),
            cookies: Some("session=1".into()),
            headers: vec![("X-Token".into(), "t".into())],
            method: Some("POST".into()),
            body: Some(b"a=1".to_vec()),
            ..Default::default()
        };
        let http = Http::new(net, &src, meta, "ua", false);
        let home = http.meta_for(&src, Req::Source);
        assert_eq!(home.method.as_deref(), Some("POST"));
        assert_eq!(home.cookies.as_deref(), Some("session=1"));
        let seg = http.meta_for(
            &Url::parse("https://cdn.example.com/s1.ts").expect("URL"),
            Req::Derived,
        );
        assert_eq!(seg.method, None);
        assert_eq!(seg.body, None);
        assert_eq!(seg.cookies.as_deref(), Some("session=1"));
        let elsewhere = http.meta_for(
            &Url::parse("https://cdn.other.net/s1.ts").expect("URL"),
            Req::Derived,
        );
        assert_eq!(elsewhere.cookies, None);
        assert!(elsewhere.headers.is_empty());
        assert_eq!(
            elsewhere.referrer.as_deref(),
            Some("https://www.example.com/watch")
        );
    }

    #[test]
    fn the_guard_refuses_private_targets_from_a_public_source() {
        let g = Guard::new(false);
        for u in [
            "http://127.0.0.1/a",
            "http://169.254.169.254/latest",
            "http://localhost/x",
            "file:///etc/passwd",
        ] {
            let url = Url::parse(u).expect("URL");
            assert!(
                matches!(
                    g.check(&url),
                    Err(Error::Media(MediaError::UrlForbidden(_)))
                ),
                "{u}"
            );
        }
        assert!(
            Guard::new(true)
                .check(&Url::parse("http://127.0.0.1/a").expect("URL"))
                .is_ok()
        );
    }

    /// A name that resolves to this computer is refused from a public source when the
    /// connection is made, not only when the name is first checked.
    #[tokio::test]
    async fn a_name_resolving_to_this_computer_is_refused_at_connect() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let port = listener.local_addr().expect("addr").port();
        let app = axum::Router::new().route("/a", axum::routing::get(|| async { "ok" }));
        tokio::spawn(async move { axum::serve(listener, app).await });
        let local = vec![vec![std::net::SocketAddr::from(([127, 0, 0, 1], 0))]];
        let table = crate::resolve::Table(Mutex::new(
            [("rebind.test".to_string(), local)].into_iter().collect(),
        ));
        let net =
            Arc::new(Net::with_lookup(&Default::default(), None, Arc::new(table)).expect("net"));
        let url = Url::parse(&format!("http://rebind.test:{port}/a")).expect("URL");
        let limit = RateLimiter::new(0);
        let get = |allow| {
            let http = Http::new(net.clone(), &url, RequestMeta::default(), "ua", allow);
            let url = url.clone();
            let limit = &limit;
            async move {
                let lim = Limiters {
                    global: limit,
                    run: limit,
                    count: None,
                };
                fetch_bounded(&http, &url, Req::Source, 1024, Overflow::Refuse, &lim).await
            }
        };
        match get(false).await {
            Err(Error::Media(MediaError::UrlForbidden(m))) => {
                assert!(m.contains("rebind.test"), "{m}")
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(get(true).await.expect("allowed").body, b"ok");
    }

    /// A server for a 100 000-byte file that honours ranges (or not), and the ranges asked.
    async fn ranged_server(honour: bool) -> (Url, Arc<Mutex<Vec<String>>>, Vec<u8>) {
        use axum::http::{HeaderMap, StatusCode as S, header};
        let body: Vec<u8> = (0..100_000u32).map(|i| (i % 251) as u8).collect();
        let asked = Arc::new(Mutex::new(Vec::new()));
        let (b, a) = (body.clone(), asked.clone());
        let app = axum::Router::new().route(
            "/f",
            axum::routing::get(move |h: HeaderMap| {
                let (b, a) = (b.clone(), a.clone());
                async move {
                    let range = h
                        .get(header::RANGE)
                        .and_then(|v| v.to_str().ok())
                        .map(str::to_string);
                    let Some(r) = range.filter(|_| honour) else {
                        return (S::OK, HeaderMap::new(), b);
                    };
                    a.lock().unwrap().push(r.clone());
                    let (x, y) = r.trim_start_matches("bytes=").split_once('-').unwrap();
                    let x: usize = x.parse().unwrap();
                    let len = b.len();
                    let mut out = HeaderMap::new();
                    if x >= len {
                        out.insert(
                            header::CONTENT_RANGE,
                            format!("bytes */{len}").parse().unwrap(),
                        );
                        return (S::RANGE_NOT_SATISFIABLE, out, Vec::new());
                    }
                    let y = y.parse::<usize>().map_or(len - 1, |y| y.min(len - 1));
                    out.insert(
                        header::CONTENT_RANGE,
                        format!("bytes {x}-{y}/{len}").parse().unwrap(),
                    );
                    (S::PARTIAL_CONTENT, out, b[x..=y].to_vec())
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let port = listener.local_addr().expect("addr").port();
        tokio::spawn(async move { axum::serve(listener, app).await });
        (
            Url::parse(&format!("http://127.0.0.1:{port}/f")).expect("URL"),
            asked,
            body,
        )
    }

    #[tokio::test]
    async fn a_whole_file_is_fetched_in_ranges_and_resumed_from_its_part() {
        let (url, asked, body) = ranged_server(true).await;
        let net = Arc::new(Net::new(&Default::default(), None).expect("net"));
        let http = Http::new(net, &url, RequestMeta::default(), "ua", true);
        let limit = RateLimiter::new(0);
        let lim = Limiters {
            global: &limit,
            run: &limit,
            count: None,
        };
        let t = tempfile::tempdir().expect("tempdir");
        let dest = t.path().join("whole.bin");
        // An earlier attempt got this far.
        std::fs::write(part_of(&dest), &body[..20_000]).expect("part");
        let n = fetch_whole_chunked(&http, &url, &dest, 32_768, &lim)
            .await
            .expect("fetched");
        assert_eq!(n, 100_000);
        assert_eq!(std::fs::read(&dest).expect("read"), body);
        assert!(!part_of(&dest).exists());
        assert_eq!(
            *asked.lock().unwrap(),
            [
                "bytes=20000-52767",
                "bytes=52768-85535",
                "bytes=85536-118303"
            ]
        );

        // A part that is already complete: the server says there's nothing more.
        asked.lock().unwrap().clear();
        std::fs::write(part_of(&dest), &body).expect("part");
        fetch_whole_chunked(&http, &url, &dest, 32_768, &lim)
            .await
            .expect("done");
        assert_eq!(std::fs::read(&dest).expect("read"), body);
        assert_eq!(*asked.lock().unwrap(), ["bytes=100000-132767"]);
    }

    #[tokio::test]
    async fn a_server_without_ranges_sends_the_whole_file_once() {
        let (url, _asked, body) = ranged_server(false).await;
        let net = Arc::new(Net::new(&Default::default(), None).expect("net"));
        let http = Http::new(net, &url, RequestMeta::default(), "ua", true);
        let limit = RateLimiter::new(0);
        let lim = Limiters {
            global: &limit,
            run: &limit,
            count: None,
        };
        let t = tempfile::tempdir().expect("tempdir");
        let dest = t.path().join("whole.bin");
        std::fs::write(part_of(&dest), b"stale bytes").expect("part");
        let n = fetch_whole_chunked(&http, &url, &dest, 32_768, &lim)
            .await
            .expect("fetched");
        assert_eq!(n, 100_000);
        assert_eq!(std::fs::read(&dest).expect("read"), body);
    }
}
