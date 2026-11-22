use reqwest::header::{self, HeaderName, HeaderValue};
use reqwest::{Client, Method, RequestBuilder, StatusCode, Url};
use serde::Serialize;
use std::path::PathBuf;

use crate::net::Net;
use crate::scope::Scoped;
use crate::session::auth::HostAuth;
use crate::session::{Policy, Request};
use crate::{Category, Error, RequestMeta, Result, filename};

/// What the server told us about a URL, plus where Siler would save it.
#[derive(Debug, Clone, Serialize)]
pub struct ProbeResult {
    pub final_url: String,
    pub total_size: Option<u64>,
    pub resumable: bool,
    pub etag: Option<String>,
    pub last_modified: Option<String>,
    pub filename: String,
    pub content_type: Option<String>,
    pub category: Category,
    /// Suggested folder (filled in by the engine from settings).
    pub dir: PathBuf,
    /// `filename` already exists in `dir` (or another download is about to write it).
    pub exists: bool,
    /// A free name to use instead when `exists` is set, e.g. `name (1).ext`.
    pub suggested: String,
    /// Other addresses of the same file the server named (RFC 6249 `rel=duplicate` links),
    /// best first.
    #[serde(default)]
    pub duplicates: Vec<String>,
    /// The file's checksum, when the server sent a `Digest` or `Repr-Digest` header.
    pub digest: Option<crate::checksum::Checksum>,
    /// The browser's context as it stood after the redirects: what requests to `final_url`
    /// start from. It holds secrets, so the interface never gets it.
    #[serde(skip)]
    pub(crate) final_meta: RequestMeta,
}

pub(crate) fn parse_url(url: &str) -> Result<Url> {
    let url = Url::parse(url.trim()).map_err(|e| Error::InvalidUrl(e.to_string()))?;
    match url.scheme() {
        s if crate::transport::FILE_SCHEMES.contains(&s) => Ok(url),
        s => Err(Error::InvalidUrl(format!("unsupported scheme “{s}”"))),
    }
}

/// Only http(s): for what only makes sense on the web (media, the site grabber).
pub(crate) fn parse_web_url(url: &str) -> Result<Url> {
    let url = parse_url(url)?;
    match url.scheme() {
        "http" | "https" => Ok(url),
        s => Err(Error::InvalidUrl(format!("unsupported scheme “{s}”"))),
    }
}

/// A request carrying what its address may have of the browser context (GET, or the
/// captured form submission). Content encoding is disabled so byte ranges refer to the file itself.
pub(crate) fn build_request(
    client: &Client,
    url: Url,
    scoped: &Scoped,
    default_ua: &str,
    auth: Option<&HostAuth>,
) -> RequestBuilder {
    let meta = scoped.meta();
    let auth = auth.filter(|_| scoped.logins());
    let ua = meta
        .user_agent
        .as_deref()
        .filter(|s| !s.is_empty())
        .unwrap_or(default_ua);
    let method = meta
        .method
        .as_deref()
        .and_then(|m| Method::from_bytes(m.trim().to_ascii_uppercase().as_bytes()).ok())
        .unwrap_or(Method::GET);
    let body = meta.body.clone().filter(|_| method != Method::GET);
    let authorization = auth.and_then(|a| a.header(&method, &url, body.as_deref()));
    let mut rb = client
        .request(method, url)
        .header(header::ACCEPT, "*/*")
        .header(header::ACCEPT_ENCODING, "identity")
        .header(header::USER_AGENT, ua);
    if let Some(r) = meta.referrer.as_deref().filter(|s| !s.is_empty()) {
        rb = rb.header(header::REFERER, r);
    }
    if let Some(c) = meta.cookies.as_deref().filter(|s| !s.is_empty()) {
        rb = rb.header(header::COOKIE, c);
    }
    for (k, v) in &meta.headers {
        let (Ok(name), Ok(value)) = (
            HeaderName::from_bytes(k.as_bytes()),
            HeaderValue::from_str(v),
        ) else {
            continue;
        };
        let reserved = [
            header::RANGE,
            header::ACCEPT_ENCODING,
            header::HOST,
            header::CONTENT_LENGTH,
            header::CONNECTION,
            header::COOKIE,
            header::PROXY_AUTHORIZATION,
            header::REFERER,
            header::USER_AGENT,
        ];
        if !reserved.contains(&name) && !(authorization.is_some() && name == header::AUTHORIZATION)
        {
            rb = rb.header(name, value);
        }
    }
    if let Some(v) = authorization {
        rb = rb.header(header::AUTHORIZATION, v);
    }
    if let Some(body) = body {
        rb = rb.body(body);
    }
    rb
}

/// Parses `bytes <a>-<b>/<total|*>`.
pub(crate) fn parse_content_range(s: &str) -> Option<(u64, u64, Option<u64>)> {
    let s = s.trim().strip_prefix("bytes")?.trim_start();
    let (range, total) = s.split_once('/')?;
    let total = match total.trim() {
        "*" => None,
        t => Some(t.parse().ok()?),
    };
    let (a, b) = range.split_once('-').unwrap_or(("*", "*"));
    Some((
        a.trim().parse().unwrap_or(0),
        b.trim().parse().unwrap_or(0),
        total,
    ))
}

/// Asks for the first byte only: a 206 answer proves the server supports ranges and
/// tells us the full size in one round trip, without downloading the body.
pub(crate) async fn probe(
    net: &Net,
    url: &str,
    meta: &RequestMeta,
    ua: &str,
) -> Result<ProbeResult> {
    let url = parse_url(url)?;
    if crate::transport::kind(&url) != crate::transport::Kind::Http {
        return crate::transport::probe(net, &url).await;
    }
    let meta = meta.clone().bound_to(url.as_str());
    let resp = net
        .send(Request {
            url: &url,
            meta: &meta,
            user_agent: ua,
            range: Some("bytes=0-0"),
            policy: Policy::File,
        })
        .await?;
    let status = resp.status();
    let get = |name: HeaderName| {
        resp.headers()
            .get(name)
            .and_then(|v| v.to_str().ok())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    };
    let content_range = get(header::CONTENT_RANGE).and_then(|cr| parse_content_range(&cr));
    let (total_size, resumable) = match status {
        StatusCode::PARTIAL_CONTENT => match content_range {
            Some((0, _, Some(total))) => (Some(total), true),
            _ => (None, false),
        },
        StatusCode::RANGE_NOT_SATISFIABLE => (content_range.and_then(|r| r.2).or(Some(0)), false),
        s if s.is_success() => (resp.content_length(), false),
        StatusCode::TOO_MANY_REQUESTS | StatusCode::SERVICE_UNAVAILABLE => return Err(resp.busy()),
        s => return Err(Error::Http(s.as_u16())),
    };

    let final_url = resp.url().clone();
    let content_type = get(header::CONTENT_TYPE);
    let name = get(header::CONTENT_DISPOSITION)
        .and_then(|cd| filename::from_content_disposition(&cd))
        .or_else(|| filename::from_url(&final_url))
        .or_else(|| filename::from_url(&url))
        .unwrap_or_else(|| "download".to_string());
    let name = filename::ensure_extension(name, content_type.as_deref());

    let links: Vec<&str> = resp
        .headers()
        .get_all(header::LINK)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .collect();
    let duplicates = duplicate_links(&links, &final_url);
    let digest = crate::checksum::Checksum::from_headers(
        get(HeaderName::from_static("digest")).as_deref(),
        get(HeaderName::from_static("repr-digest")).as_deref(),
    );

    Ok(ProbeResult {
        duplicates,
        digest,
        final_meta: resp.meta.clone(),
        final_url: final_url.to_string(),
        total_size,
        resumable,
        etag: get(header::ETAG),
        last_modified: get(header::LAST_MODIFIED),
        category: Category::from_filename(&name),
        filename: name,
        content_type,
        dir: PathBuf::new(),
        exists: false,
        suggested: String::new(),
    })
}

/// Reads `len` bytes of `url` from `start` with one range request (the way a file download
/// fetches, with the browser's context and logins). The server must answer with exactly that
/// range.
pub(crate) async fn fetch_range(
    net: &Net,
    url: &str,
    meta: &RequestMeta,
    ua: &str,
    start: u64,
    len: u64,
) -> Result<Vec<u8>> {
    use futures_util::StreamExt;
    if len == 0 {
        return Ok(Vec::new());
    }
    let url = parse_url(url)?;
    if crate::transport::kind(&url) != crate::transport::Kind::Http {
        let s = crate::transport::open_range(net, &url, start, Some(start + len))
            .await
            .map_err(|e| match e {
                crate::transport::OpenError::Fatal(e) => e,
                crate::transport::OpenError::Transient(m) => Error::Network(m),
                crate::transport::OpenError::Throttled(c, a) => Error::Busy(c, a),
            })?;
        return crate::transport::collect(s, len).await;
    }
    let range = format!("bytes={}-{}", start, start + len - 1);
    let resp = net
        .send(Request {
            url: &url,
            meta,
            user_agent: ua,
            range: Some(&range),
            policy: Policy::File,
        })
        .await?;
    match resp.status() {
        StatusCode::PARTIAL_CONTENT => {}
        StatusCode::TOO_MANY_REQUESTS | StatusCode::SERVICE_UNAVAILABLE => return Err(resp.busy()),
        s if s.is_success() => {
            return Err(Error::Protocol(
                "The server doesn't send parts of files, which this needs".into(),
            ));
        }
        s => return Err(Error::Http(s.as_u16())),
    }
    let got = resp
        .headers()
        .get(header::CONTENT_RANGE)
        .and_then(|v| v.to_str().ok())
        .and_then(parse_content_range)
        .map(|r| r.0);
    if got != Some(start) {
        return Err(Error::Protocol(
            "The server returned an unexpected byte range".into(),
        ));
    }
    let mut body = Vec::with_capacity(usize::try_from(len.min(1 << 20)).unwrap_or(0));
    let mut stream = resp.into_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(Error::Network)?;
        let room = usize::try_from(len).unwrap_or(usize::MAX) - body.len();
        body.extend_from_slice(&chunk[..chunk.len().min(room)]);
        if body.len() as u64 >= len {
            break;
        }
    }
    if (body.len() as u64) < len {
        return Err(Error::Network("connection closed early".into()));
    }
    Ok(body)
}

/// The `rel=duplicate` targets of `Link` headers (RFC 6249), by `pri` (lowest first). Only
/// http(s) and ftp addresses, at most 10.
pub(crate) fn duplicate_links(values: &[&str], base: &Url) -> Vec<String> {
    let mut found: Vec<(u32, String)> = Vec::new();
    for v in values {
        for link in split_links(v) {
            let Some((target, params)) = link
                .trim()
                .strip_prefix('<')
                .and_then(|l| l.split_once('>'))
            else {
                continue;
            };
            let mut duplicate = false;
            let mut pri = 999_999u32;
            for p in params.split(';') {
                let Some((k, v)) = p.split_once('=') else {
                    continue;
                };
                let v = v.trim().trim_matches('"');
                match k.trim().to_ascii_lowercase().as_str() {
                    "rel" => {
                        duplicate = v
                            .split_whitespace()
                            .any(|r| r.eq_ignore_ascii_case("duplicate"))
                    }
                    "pri" => pri = v.parse().unwrap_or(pri),
                    _ => {}
                }
            }
            if !duplicate {
                continue;
            }
            if let Ok(u) = base.join(target.trim())
                && matches!(u.scheme(), "http" | "https" | "ftp" | "ftps")
                && u != *base
                && !found.iter().any(|(_, f)| *f == u.as_str())
            {
                found.push((pri, u.to_string()));
            }
        }
    }
    found.sort_by_key(|(p, _)| *p);
    found.into_iter().take(10).map(|(_, u)| u).collect()
}

/// Splits a `Link` header at the commas between links (not those inside `<…>` or quotes).
fn split_links(v: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let (mut start, mut in_url, mut in_quote) = (0, false, false);
    for (i, c) in v.char_indices() {
        match c {
            '<' if !in_quote => in_url = true,
            '>' if !in_quote => in_url = false,
            '"' if !in_url => in_quote = !in_quote,
            ',' if !in_url && !in_quote => {
                out.push(&v[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    out.push(&v[start..]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn duplicate_link_headers() {
        let base = Url::parse("https://a.example/f.iso").unwrap();
        let got = duplicate_links(
            &[
                "<https://m2.example/f.iso>; rel=duplicate; pri=2; geo=de, <https://a.example/f.iso.meta4>; rel=describedby; type=\"application/metalink4+xml\"",
                "<http://m1.example/f.iso>; rel=\"duplicate\"; pri=1",
                "<ftp://m3.example/f.iso>; rel=duplicate; pri=3",
                "</f.iso>; rel=duplicate",
            ],
            &base,
        );
        assert_eq!(
            got,
            vec![
                "http://m1.example/f.iso",
                "https://m2.example/f.iso",
                "ftp://m3.example/f.iso"
            ]
        );
    }

    #[test]
    fn content_range() {
        assert_eq!(
            parse_content_range("bytes 0-0/1234"),
            Some((0, 0, Some(1234)))
        );
        assert_eq!(parse_content_range("bytes 5-9/*"), Some((5, 9, None)));
        assert_eq!(parse_content_range("bytes */77"), Some((0, 0, Some(77))));
        assert_eq!(parse_content_range("items 0-1/2"), None);
    }
}
