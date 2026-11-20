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
    Some(at.duration_since(SystemTime::now()).unwrap_or(Duration::ZERO))
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
