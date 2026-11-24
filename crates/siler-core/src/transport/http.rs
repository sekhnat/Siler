//! One range of a file over HTTP(S), with the browser's context and the server's login.

use reqwest::header::CONTENT_RANGE;
use reqwest::{StatusCode, Url};

use super::{ByteStream, OpenError};
use crate::net::Net;
use crate::probe::parse_content_range;
use crate::session::{Policy, Request};
use crate::{Error, RequestMeta};

/// Opens `url` from `start` to `end` (exclusive). `range` is false when the server can't
/// send parts: then the whole file is asked for and `start` must be 0. With `streams` (one
/// of several ranges of a download) it may go over an HTTP/2 slot. Also says which HTTP
/// version answered: 1 or 2.
pub(crate) async fn open_range(
    net: &Net,
    url: &Url,
    meta: &RequestMeta,
    user_agent: &str,
    range: Option<(u64, u64)>,
    streams: bool,
) -> Result<(ByteStream, u8), OpenError> {
    let range_header = range.map(|(start, end)| format!("bytes={}-{}", start, end - 1));
    let resp = net
        .send(Request {
            url,
            meta,
            user_agent,
            range: range_header.as_deref(),
            policy: Policy::File,
            streams,
        })
        .await
        .map_err(|e| match e {
            Error::Network(m) => OpenError::Transient(m),
            e => OpenError::Fatal(e),
        })?;
    let status = resp.status();
    if status == StatusCode::TOO_MANY_REQUESTS || status == StatusCode::SERVICE_UNAVAILABLE {
        return Err(OpenError::Throttled(
            status.as_u16(),
            resp.busy().retry_after(),
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
    let version = if resp.version() == reqwest::Version::HTTP_2 {
        2
    } else {
        1
    };
    Ok((Box::pin(resp.into_stream()), version))
}
