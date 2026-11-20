use std::error::Error as _;
use std::time::{Duration, SystemTime};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("Invalid URL: {0}")]
    InvalidUrl(String),
    #[error("Network error: {0}")]
    Network(String),
    #[error("{}", http_message(*.0))]
    Http(u16),
    /// 429/503: the server is overloaded or rate limiting, possibly saying when to retry.
    #[error("{}", http_message(*.0))]
    Busy(u16, Option<Duration>),
    #[error("{0}")]
    Protocol(String),
    #[error("Not enough disk space")]
    DiskFull,
    #[error("File error: {0}")]
    Io(std::io::Error),
    #[error("Database error: {0}")]
    Db(#[from] rusqlite::Error),
    #[error("Download not found")]
    NotFound,
    /// A refreshed link points to something other than the original download.
    #[error("{0}")]
    LinkMismatch(String),
    #[error("A file named “{0}” already exists")]
    Exists(String),
    #[error("The file was moved or deleted")]
    FileMissing,
    /// The server answered 401: it wants a login, or refused the one sent.
    #[error("{}", auth_message(host, *rejected))]
    AuthRequired {
        host: String,
        realm: Option<String>,
        rejected: bool,
    },
    #[error("The proxy needs a username and password")]
    ProxyAuth,
    /// A media download could not continue (see [`MediaError`]).
    #[error("{0}")]
    Media(MediaError),
}

/// Why a media download can't be made, in words for the user.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum MediaError {
    #[error("This isn't a video or audio stream K-Star can download.")]
    NotMedia,
    #[error("This stream is live, and K-Star can't record live streams yet.")]
    LiveUnsupported,
    #[error("This video is protected by DRM. K-Star doesn't download DRM-protected media.")]
    Drm,
    #[error("This stream uses something K-Star doesn't support yet: {0}")]
    Unsupported(String),
    #[error("The playlist can't be read: {0}")]
    Invalid(String),
    #[error("The playlist or a file in it is larger than K-Star accepts ({0} bytes).")]
    BodyTooLarge(u64),
    #[error("The chosen video or audio is no longer offered; choose again.")]
    SelectionUnavailable,
    #[error(
        "The stream changed since the download started. Restart the download to use the new version."
    )]
    ManifestChanged,
    #[error(
        "The server stopped giving valid pieces of the stream, even after asking for the playlist again."
    )]
    RefreshExhausted,
    #[error("A piece of the stream didn't arrive as video or audio (the link may have expired).")]
    SegmentInvalid,
    #[error("The decryption key couldn't be used: {0}")]
    KeyFetch(String),
    #[error("A piece of the stream couldn't be decrypted (wrong key or damaged data).")]
    Decrypt,
    #[error(
        "FFmpeg is needed to combine these streams, but K-Star couldn't find a usable copy. See Settings → Media."
    )]
    FfmpegMissing,
    #[error("FFmpeg couldn't combine the streams: {0}")]
    FfmpegFailed(String),
    #[error(
        "The audio or video can't be joined into one file in this container ({0}). Choose another container or the original format."
    )]
    DiscontinuityIncompatible(String),
    #[error("K-Star won't contact that address: {0}")]
    UrlForbidden(String),
    #[error("There isn't enough free disk space for this stream.")]
    DiskSpace,
    #[error(
        "The links from the page have expired. Open the page in Firefox and send the video to K-Star again."
    )]
    LinksExpired,
}

fn auth_message(host: &str, rejected: bool) -> String {
    if rejected {
        format!("{host} didn't accept the username and password")
    } else {
        format!("{host} needs a username and password")
    }
}

impl Error {
    /// Failures that may go away by themselves, so retrying is worthwhile.
    pub fn is_transient(&self) -> bool {
        match self {
            Error::Network(_) | Error::Busy(..) => true,
            Error::Http(code) => *code >= 500 || *code == 408,
            _ => false,
        }
    }

    /// The login the server asked for, when this is a 401.
    pub fn login_request(&self) -> Option<crate::LoginRequest> {
        match self {
            Error::AuthRequired {
                host,
                realm,
                rejected,
            } => Some(crate::LoginRequest {
                host: host.clone(),
                realm: realm.clone(),
                rejected: *rejected,
            }),
            _ => None,
        }
    }

    /// How long the server asked us to wait before retrying.
    pub fn retry_after(&self) -> Option<Duration> {
        match self {
            Error::Busy(_, d) => *d,
            _ => None,
        }
    }
}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        // ENOSPC / EDQUOT
        if matches!(e.raw_os_error(), Some(28) | Some(122)) {
            Error::DiskFull
        } else {
            Error::Io(e)
        }
    }
}

impl From<rustix::io::Errno> for Error {
    fn from(e: rustix::io::Errno) -> Self {
        std::io::Error::from(e).into()
    }
}

fn http_message(code: u16) -> String {
    let reason = reqwest::StatusCode::from_u16(code)
        .ok()
        .and_then(|s| s.canonical_reason())
        .unwrap_or("");
    let hint = match code {
        401 => " — the server requires authentication",
        403 => " — access denied (the link may have expired)",
        404 | 410 => " — the file is no longer available",
        429 => " — the server asks to slow down",
        503 => " — the server is busy",
        _ => "",
    };
    format!("Server returned HTTP {code} {reason}{hint}")
}

/// Parses a `Retry-After` value: delay in seconds, or an HTTP date.
pub(crate) fn parse_retry_after(v: &str) -> Option<Duration> {
    let v = v.trim();
    if let Ok(secs) = v.parse::<u64>() {
        return Some(Duration::from_secs(secs));
    }
    let at = httpdate::parse_http_date(v).ok()?;
    Some(
        at.duration_since(SystemTime::now())
            .unwrap_or(Duration::ZERO),
    )
}

/// Flattens a reqwest error and its source chain into one readable line.
pub(crate) fn describe(e: &reqwest::Error) -> String {
    let mut msg = if e.is_timeout() {
        "connection timed out".to_string()
    } else if e.is_connect() {
        "could not connect".to_string()
    } else {
        "connection error".to_string()
    };
    let mut src = e.source();
    let mut last = String::new();
    while let Some(s) = src {
        last = s.to_string();
        src = s.source();
    }
    if !last.is_empty() {
        msg.push_str(": ");
        msg.push_str(&last);
    }
    msg
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retry_after_forms() {
        assert_eq!(parse_retry_after(" 7 "), Some(Duration::from_secs(7)));
        let later = SystemTime::now() + Duration::from_secs(120);
        let d = parse_retry_after(&httpdate::fmt_http_date(later)).unwrap();
        assert!(d > Duration::from_secs(100) && d <= Duration::from_secs(120));
        assert_eq!(
            parse_retry_after("Wed, 21 Oct 2015 07:28:00 GMT"),
            Some(Duration::ZERO)
        );
        assert_eq!(parse_retry_after("soon"), None);
    }

    #[test]
    fn transient_classes() {
        assert!(Error::Network("x".into()).is_transient());
        assert!(Error::Http(502).is_transient());
        assert!(Error::Busy(503, None).is_transient());
        assert!(!Error::Http(404).is_transient());
        assert!(!Error::DiskFull.is_transient());
    }
}
