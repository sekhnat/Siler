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
    /// Something other than Siler's own partial file is where it writes the download: a link,
    /// or a file with other names.
    #[error("{} is a link or has other names pointing at it, so Siler won't write to it", .0.display())]
    UnsafePart(std::path::PathBuf),
    /// The server answered 401: it wants a login, or refused the one sent.
    #[error("{}", auth_message(host, *rejected, *asked))]
    AuthRequired {
        host: String,
        realm: Option<String>,
        rejected: bool,
        asked: crate::Asked,
    },
    #[error("The proxy needs a username and password")]
    ProxyAuth,
    /// The chosen proxy script couldn't answer, and connecting directly would go round it.
    #[error("The proxy script didn't work, so nothing was sent: {0}")]
    Proxy(String),
    /// An FTP server refused a command (a 5xx reply), with its words.
    #[error("{}", ftp_message(*.0, .1))]
    Ftp(u16, String),
    /// An SFTP server's key isn't known yet: the user has to trust it first.
    #[error("{} hasn't been connected to before. Check its key and trust it to continue.", .0.server)]
    HostKeyUnknown(Box<crate::HostKeyRequest>),
    /// An SFTP server's key differs from the one recorded for it.
    #[error(
        "The key of {0} changed since the last connection. This can mean someone is intercepting the connection; Siler won't connect. If the server was reinstalled, remove its old key from ~/.ssh/known_hosts."
    )]
    HostKeyChanged(String),
    /// A media download could not continue (see [`MediaError`]).
    #[error("{0}")]
    Media(MediaError),
    /// The finished file isn't what the checksum says it should be.
    #[error("The file is damaged: its {algo} doesn't match the expected checksum")]
    ChecksumMismatch {
        algo: &'static str,
        expected: String,
        actual: String,
    },
}

/// Why a media download can't be made, in words for the user.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum MediaError {
    #[error("This isn't a video or audio stream Siler can download.")]
    NotMedia,
    #[error("This stream is live, and Siler can't record live streams yet.")]
    LiveUnsupported,
    #[error("This video is protected by DRM. Siler doesn't download DRM-protected media.")]
    Drm,
    #[error("This stream uses something Siler doesn't support yet: {0}")]
    Unsupported(String),
    #[error("The playlist can't be read: {0}")]
    Invalid(String),
    #[error("The playlist or a file in it is larger than Siler accepts ({0} bytes).")]
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
        "FFmpeg is needed to combine these streams, but Siler couldn't find a usable copy. See Settings → Media."
    )]
    FfmpegMissing,
    #[error("FFmpeg couldn't combine the streams: {0}")]
    FfmpegFailed(String),
    #[error(
        "The audio or video can't be joined into one file in this container ({0}). Choose another container or the original format."
    )]
    DiscontinuityIncompatible(String),
    #[error("Siler won't contact that address: {0}")]
    UrlForbidden(String),
    #[error("There isn't enough free disk space for this stream.")]
    DiskSpace,
    #[error(
        "The links from the page have expired. Open the page in the browser and send the video to Siler again."
    )]
    LinksExpired,
}

fn ftp_message(code: u16, text: &str) -> String {
    let what = match code {
        550 => "The file isn't on the FTP server, or can't be read",
        553 => "The FTP server doesn't allow this file name",
        _ => "The FTP server refused",
    };
    if text.is_empty() {
        format!("{what} (FTP {code})")
    } else {
        format!("{what}: {text} (FTP {code})")
    }
}

fn auth_message(host: &str, rejected: bool, asked: crate::Asked) -> String {
    let proxy = host.strip_prefix("proxy+");
    let host = proxy.unwrap_or(host);
    if asked == crate::Asked::TicketFailed {
        format!("{host} didn't accept your Kerberos ticket, or there isn't one")
    } else if proxy.is_some() && rejected {
        format!("The proxy {host} didn't accept the username and password")
    } else if proxy.is_some() {
        format!("The proxy {host} needs a username and password")
    } else if rejected {
        format!("{host} didn't accept the username and password")
    } else {
        format!("{host} needs a username and password")
    }
}

/// What an error says, as a stable code and its details, for an app that shows it in the
/// user's language (`error-<code>` in its catalog); `Display` stays the English text.
pub type ErrorArgs = Vec<(&'static str, String)>;

impl MediaError {
    pub fn code_args(&self) -> (&'static str, ErrorArgs) {
        use MediaError::*;
        match self {
            NotMedia => ("media-not-media", vec![]),
            LiveUnsupported => ("media-live-unsupported", vec![]),
            Drm => ("media-drm", vec![]),
            Unsupported(what) => ("media-unsupported", vec![("what", what.clone())]),
            Invalid(why) => ("media-invalid", vec![("why", why.clone())]),
            BodyTooLarge(n) => ("media-body-too-large", vec![("limit", n.to_string())]),
            SelectionUnavailable => ("media-selection-unavailable", vec![]),
            ManifestChanged => ("media-manifest-changed", vec![]),
            RefreshExhausted => ("media-refresh-exhausted", vec![]),
            SegmentInvalid => ("media-segment-invalid", vec![]),
            KeyFetch(why) => ("media-key-fetch", vec![("why", why.clone())]),
            Decrypt => ("media-decrypt", vec![]),
            FfmpegMissing => ("media-ffmpeg-missing", vec![]),
            FfmpegFailed(why) => ("media-ffmpeg-failed", vec![("why", why.clone())]),
            DiscontinuityIncompatible(why) => (
                "media-discontinuity-incompatible",
                vec![("why", why.clone())],
            ),
            UrlForbidden(why) => ("media-url-forbidden", vec![("why", why.clone())]),
            DiskSpace => ("media-disk-space", vec![]),
            LinksExpired => ("media-links-expired", vec![]),
        }
    }
}

impl Error {
    /// The error as a code and its details; see [`ErrorArgs`]. Messages that come from deep
    /// inside (a server's words, a protocol detail) are passed as they are, in English.
    pub fn code_args(&self) -> (&'static str, ErrorArgs) {
        match self {
            Error::InvalidUrl(why) => ("invalid-url", vec![("why", why.clone())]),
            Error::Network(why) => ("network", vec![("why", why.clone())]),
            Error::Http(code) | Error::Busy(code, _) => (
                "http",
                vec![
                    ("code", code.to_string()),
                    (
                        "reason",
                        reqwest::StatusCode::from_u16(*code)
                            .ok()
                            .and_then(|s| s.canonical_reason())
                            .unwrap_or("")
                            .to_string(),
                    ),
                ],
            ),
            Error::Protocol(text) => ("text", vec![("text", text.clone())]),
            Error::DiskFull => ("disk-full", vec![]),
            Error::Io(e) => ("io", vec![("why", e.to_string())]),
            Error::Db(e) => ("db", vec![("why", e.to_string())]),
            Error::NotFound => ("not-found", vec![]),
            Error::LinkMismatch(text) => ("text", vec![("text", text.clone())]),
            Error::Exists(name) => ("exists", vec![("name", name.clone())]),
            Error::FileMissing => ("file-missing", vec![]),
            Error::UnsafePart(path) => ("unsafe-part", vec![("path", path.display().to_string())]),
            Error::AuthRequired {
                host,
                rejected,
                asked,
                ..
            } => {
                let proxy = host.strip_prefix("proxy+");
                let code = match (asked, proxy.is_some(), *rejected) {
                    (crate::Asked::TicketFailed, _, _) => "auth-ticket",
                    (_, true, true) => "proxy-login-rejected",
                    (_, true, false) => "proxy-login-required",
                    (_, false, true) => "auth-rejected",
                    (_, false, false) => "auth-required",
                };
                (code, vec![("host", proxy.unwrap_or(host).to_string())])
            }
            Error::ProxyAuth => ("proxy-auth", vec![]),
            Error::Proxy(why) => ("proxy-script", vec![("why", why.clone())]),
            Error::Ftp(code, text) if text.is_empty() => {
                ("ftp-bare", vec![("code", code.to_string())])
            }
            Error::Ftp(code, text) => (
                "ftp",
                vec![("code", code.to_string()), ("text", text.clone())],
            ),
            Error::HostKeyUnknown(r) => ("host-key-unknown", vec![("server", r.server.clone())]),
            Error::HostKeyChanged(host) => ("host-key-changed", vec![("host", host.clone())]),
            Error::Media(m) => m.code_args(),
            Error::ChecksumMismatch { algo, .. } => {
                ("checksum-mismatch", vec![("algo", algo.to_string())])
            }
        }
    }
}

impl Error {
    /// Failures that may go away by themselves, so retrying is worthwhile.
    pub fn is_transient(&self) -> bool {
        match self {
            Error::Network(_) | Error::Busy(..) => true,
            Error::Http(code) => *code >= 500 || *code == 408,
            Error::Ftp(code, _) => (400..500).contains(code),
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
                asked,
            } => Some(crate::LoginRequest {
                host: host.clone(),
                realm: realm.clone(),
                rejected: *rejected,
                asked: *asked,
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
