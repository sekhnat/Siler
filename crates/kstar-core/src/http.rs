//! Bounded and object-level HTTP for media. A small body is read into memory (manifests,
//! keys, the first bytes of a source); one object (a segment, or a whole track) is written
//! to a file through a `.part` file and renamed when complete. Both go through [`Net`]
//! (proxy, logins) and the rate limiters, and use the same error classes as file downloads.

use futures_util::StreamExt;
use reqwest::header::{CONTENT_RANGE, CONTENT_TYPE, RANGE};
use reqwest::{Response, StatusCode, Url};
use std::path::{Path, PathBuf};
use tokio::io::AsyncWriteExt;

use crate::error::{MediaError, describe};
use crate::net::{Net, host_key, parse_challenge};
use crate::probe::{build_request, busy, parse_content_range};
use crate::ratelimit::RateLimiter;
use crate::{Error, RequestMeta, Result};

/// The limiters every byte passes through: the engine-wide one and the download's own.
pub(crate) struct Limiters<'a> {
    pub global: &'a RateLimiter,
    pub run: &'a RateLimiter,
}

impl Limiters<'_> {
    async fn take(&self, n: u64) {
        self.global.acquire(n).await;
        self.run.acquire(n).await;
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
}

/// One request, with logins and the answers that become errors. A 401 is retried when the
/// server just said how to log in, or a Digest nonce went stale (as in `probe`).
async fn request(
    net: &Net,
    url: &Url,
    meta: &RequestMeta,
    ua: &str,
    range: Option<String>,
) -> Result<Response> {
    let mut target = url.clone();
    let mut attempts = 0;
    loop {
        let auth = net.auth_for(&target);
        let sent = auth.as_ref().is_some_and(|a| a.ready());
        let mut rb = build_request(&net.client(), target.clone(), meta, ua, auth.as_deref());
        if let Some(r) = &range {
            rb = rb.header(RANGE, r.as_str());
        }
        let resp = rb.send().await.map_err(|e| Error::Network(describe(&e)))?;
        match resp.status() {
            StatusCode::UNAUTHORIZED => {
                let at = resp.url().clone();
                let sent_here = sent && host_key(&at) == host_key(&target);
                let challenge = parse_challenge(resp.headers());
                let realm = challenge.as_ref().and_then(|c| c.realm.clone());
                let retry =
                    challenge.is_some_and(|c| net.entry(&host_key(&at)).learn(c, sent_here));
                attempts += 1;
                if retry && attempts < 3 {
                    target = at;
                    continue;
                }
                return Err(Error::AuthRequired {
                    host: host_key(&at),
                    realm,
                    rejected: sent_here,
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

/// Reads a body into memory, up to `max` bytes.
pub(crate) async fn fetch_bounded(
    net: &Net,
    url: &Url,
    meta: &RequestMeta,
    ua: &str,
    max: u64,
    overflow: Overflow,
    lim: &Limiters<'_>,
) -> Result<Fetched> {
    let resp = request(net, url, meta, ua, None).await?;
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
/// With `range`, the server must answer `206` for exactly that byte range. With
/// `resume_from`, the `.part` file (which must be that long) is continued with a range
/// request from that byte. Exactly one GET per call; retries belong to the caller.
/// Returns the size of `dest`.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn fetch_object(
    net: &Net,
    url: &Url,
    meta: &RequestMeta,
    ua: &str,
    dest: &Path,
    range: Option<(u64, u64)>,
    resume_from: Option<u64>,
    lim: &Limiters<'_>,
) -> Result<u64> {
    let part = part_of(dest);
    let start = match (range, resume_from) {
        (Some((a, _)), _) => a,
        (None, Some(n)) => n,
        (None, None) => 0,
    };
    let header = match (range, resume_from) {
        (Some((a, b)), _) => Some(format!("bytes={a}-{b}")),
        (None, Some(n)) if n > 0 => Some(format!("bytes={n}-")),
        _ => None,
    };
    let resp = request(net, url, meta, ua, header.clone()).await?;
    let partial = header.is_some();
    if partial {
        if resp.status() != StatusCode::PARTIAL_CONTENT {
            // The server ignored the range. A resume can't continue from there.
            return Err(Error::Protocol(
                "The server stopped accepting range requests".into(),
            ));
        }
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

    let mut file = if resume_from.is_some() {
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
    let mut written = 0u64;
    let mut stream = resp.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| Error::Network(describe(&e)))?;
        lim.take(chunk.len() as u64).await;
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
    // A range is the whole object; a resumed whole file includes the bytes already there.
    Ok(if range.is_some() {
        written
    } else {
        start + written
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn part_files_sit_beside_their_objects() {
        assert_eq!(
            part_of(Path::new("/w/t/00000001.seg")),
            PathBuf::from("/w/t/00000001.seg.part")
        );
    }
}
