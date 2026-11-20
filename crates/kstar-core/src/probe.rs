use reqwest::header::{self, HeaderName, HeaderValue};
use reqwest::{Client, Method, RequestBuilder, StatusCode, Url};
use serde::Serialize;
use std::path::PathBuf;

use crate::error::{describe, parse_retry_after};
use crate::{Category, Error, RequestMeta, Result, filename};

/// What the server told us about a URL, plus where K-Star would save it.
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
}

pub(crate) fn parse_url(url: &str) -> Result<Url> {
    let url = Url::parse(url.trim()).map_err(|e| Error::InvalidUrl(e.to_string()))?;
    match url.scheme() {
        "http" | "https" => Ok(url),
        s => Err(Error::InvalidUrl(format!("unsupported scheme “{s}”"))),
    }
}

/// A request carrying the browser context (GET, or the captured form submission).
/// Content encoding is disabled so byte ranges refer to the file itself.
pub(crate) fn build_request(
    client: &Client,
    url: Url,
    meta: &RequestMeta,
    default_ua: &str,
) -> RequestBuilder {
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
            header::REFERER,
            header::USER_AGENT,
        ];
        if !reserved.contains(&name) {
            rb = rb.header(name, value);
        }
    }
    if let Some(body) = body {
        rb = rb.body(body);
    }
    rb
}

/// The `Retry-After` delay of a 429/503 response, as an error worth retrying.
pub(crate) fn busy(resp: &reqwest::Response) -> Error {
    let after = resp
        .headers()
        .get(header::RETRY_AFTER)
        .and_then(|v| v.to_str().ok())
        .and_then(parse_retry_after);
    Error::Busy(resp.status().as_u16(), after)
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
    client: &Client,
    url: &str,
    meta: &RequestMeta,
    ua: &str,
) -> Result<ProbeResult> {
    let url = parse_url(url)?;
    let resp = build_request(client, url.clone(), meta, ua)
        .header(header::RANGE, "bytes=0-0")
        .send()
        .await
        .map_err(|e| Error::Network(describe(&e)))?;

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
        StatusCode::TOO_MANY_REQUESTS | StatusCode::SERVICE_UNAVAILABLE => return Err(busy(&resp)),
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

    Ok(ProbeResult {
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

#[cfg(test)]
mod tests {
    use super::*;

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
