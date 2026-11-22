//! One range of a file over HTTP(S), with the browser's context and the server's login.

use futures_util::StreamExt;
use reqwest::header::{CONTENT_RANGE, RANGE};
use reqwest::{StatusCode, Url};

use super::{ByteStream, OpenError};
use crate::error::describe;
use crate::net::{Net, host_key, parse_challenge};
use crate::probe::{build_request, busy, parse_content_range};
use crate::{Error, RequestMeta};

/// Opens `url` from `start` to `end` (exclusive). `range` is false when the server can't
/// send parts: then the whole file is asked for and `start` must be 0.
pub(crate) async fn open_range(
    net: &Net,
    url: &Url,
    meta: &RequestMeta,
    user_agent: &str,
    range: Option<(u64, u64)>,
) -> Result<ByteStream, OpenError> {
    let auth = net.auth_for(url);
    let sent = auth.as_ref().is_some_and(|a| a.ready());
    let mut rb = build_request(
        &net.client(),
        url.clone(),
        meta,
        user_agent,
        auth.as_deref(),
    );
    if let Some((start, end)) = range {
        rb = rb.header(RANGE, format!("bytes={}-{}", start, end - 1));
    }
    let resp = rb
        .send()
        .await
        .map_err(|e| OpenError::Transient(describe(&e)))?;
    let status = resp.status();
    if status == StatusCode::UNAUTHORIZED {
        let challenge = parse_challenge(resp.headers());
        let realm = challenge.as_ref().and_then(|c| c.realm.clone());
        let host = host_key(resp.url());
        if let Some(c) = challenge
            && net.entry(&host).learn(c, sent)
        {
            // A stale Digest nonce: the same login works with the new one.
            return Err(OpenError::Transient("login renewed".into()));
        }
        return Err(OpenError::Fatal(Error::AuthRequired {
            host,
            realm,
            rejected: sent,
        }));
    }
    if status == StatusCode::PROXY_AUTHENTICATION_REQUIRED {
        return Err(OpenError::Fatal(Error::ProxyAuth));
    }
    if status == StatusCode::TOO_MANY_REQUESTS || status == StatusCode::SERVICE_UNAVAILABLE {
        return Err(OpenError::Throttled(
            status.as_u16(),
            busy(&resp).retry_after(),
        ));
    }
    if !status.is_success() {
        return Err(
            if status.is_server_error() || status == StatusCode::REQUEST_TIMEOUT {
                OpenError::Transient(format!("HTTP {status}"))
            } else {
                OpenError::Fatal(Error::Http(status.as_u16()))
            },
        );
    }
    if let Some((start, _)) = range {
        if status == StatusCode::PARTIAL_CONTENT {
            let got = resp
                .headers()
                .get(CONTENT_RANGE)
                .and_then(|v| v.to_str().ok())
                .and_then(parse_content_range)
                .map(|r| r.0);
            if got != Some(start) {
                return Err(OpenError::Fatal(Error::Protocol(
                    "The server returned an unexpected byte range".into(),
                )));
            }
        } else if start != 0 {
            return Err(OpenError::Fatal(Error::Protocol(
                "The server stopped accepting range requests".into(),
            )));
        }
    }
    Ok(Box::pin(
        resp.bytes_stream().map(|c| c.map_err(|e| describe(&e))),
    ))
}
