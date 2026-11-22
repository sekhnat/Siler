//! How bytes are fetched, per URL scheme. The scheduling around it (segments, connection
//! limits, retries, mirrors) is the same for all of them; a transport only answers two
//! questions: what is this file (`probe`), and give me its bytes from here to there
//! (`open_range`).

pub(crate) mod dial;
pub(crate) mod ftp;
pub(crate) mod http;
pub(crate) mod sftp;

use bytes::Bytes;
use futures_util::Stream;
use std::pin::Pin;
use std::time::Duration;
use url::Url;

use crate::net::Net;
use crate::probe::ProbeResult;
use crate::{Error, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    Http,
    Ftp,
    Sftp,
}

pub(crate) fn kind(url: &Url) -> Kind {
    match url.scheme() {
        "ftp" | "ftps" => Kind::Ftp,
        "sftp" => Kind::Sftp,
        _ => Kind::Http,
    }
}

/// Schemes a file download can use.
pub(crate) const FILE_SCHEMES: &[&str] = &["http", "https", "ftp", "ftps", "sftp"];

/// The bytes of one range, as they arrive. An error ends the stream and is worth a retry.
pub(crate) type ByteStream = Pin<Box<dyn Stream<Item = std::result::Result<Bytes, String>> + Send>>;

/// Why a range couldn't be opened.
pub(crate) enum OpenError {
    /// Worth retrying (dropped connection, server error, timeout).
    Transient(String),
    /// The server is limiting connections, possibly saying when to retry.
    Throttled(u16, Option<Duration>),
    Fatal(Error),
}

impl From<Error> for OpenError {
    fn from(e: Error) -> Self {
        match e {
            Error::Busy(code, after) => OpenError::Throttled(code, after),
            e if e.is_transient() => OpenError::Transient(e.to_string()),
            e => OpenError::Fatal(e),
        }
    }
}

/// What the server says about a file reached over FTP or SFTP.
pub(crate) async fn probe(net: &Net, url: &Url) -> Result<ProbeResult> {
    match kind(url) {
        Kind::Ftp => ftp::probe(net, url).await,
        Kind::Sftp => sftp::probe(net, url).await,
        Kind::Http => Err(Error::Protocol("not an FTP or SFTP address".into())),
    }
}

/// The bytes of an FTP or SFTP file from `start` up to `end` (exclusive; `None` = to the end).
pub(crate) async fn open_range(
    net: &Net,
    url: &Url,
    start: u64,
    end: Option<u64>,
) -> std::result::Result<ByteStream, OpenError> {
    match kind(url) {
        Kind::Ftp => ftp::open_range(net, url, start, end).await,
        Kind::Sftp => sftp::open_range(net, url, start, end).await,
        Kind::Http => Err(OpenError::Fatal(Error::Protocol(
            "not an FTP or SFTP address".into(),
        ))),
    }
}

/// The key logins are kept under: `host[:port]` for the web (unchanged from before FTP),
/// `scheme://host[:port]` for FTP and SFTP, so a site's web and FTP logins never mix.
pub fn login_key(url: &Url) -> String {
    let host = crate::net::host_key(url);
    match kind(url) {
        Kind::Http => host,
        // ftp and ftps share logins: they are the same server account.
        Kind::Ftp => format!("ftp://{host}"),
        Kind::Sftp => format!("sftp://{host}"),
    }
}

/// The decoded path of an FTP or SFTP URL.
pub(crate) fn decoded_path(url: &Url) -> String {
    let path = url.path();
    // RFC 1738 `;type=i` suffix.
    let path = path.split(";type=").next().unwrap_or(path);
    percent_encoding::percent_decode_str(path)
        .decode_utf8_lossy()
        .into_owned()
}

/// Reads a whole stream into memory, up to `len` bytes (for small ranges).
pub(crate) async fn collect(mut s: ByteStream, len: u64) -> Result<Vec<u8>> {
    use futures_util::StreamExt;
    let mut body = Vec::with_capacity(usize::try_from(len.min(1 << 20)).unwrap_or(0));
    while let Some(chunk) = s.next().await {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn login_keys() {
        let k = |s: &str| login_key(&Url::parse(s).unwrap());
        assert_eq!(k("https://a.example/x"), "a.example");
        assert_eq!(k("ftp://a.example/x"), "ftp://a.example");
        assert_eq!(k("ftps://a.example:990/x"), "ftp://a.example:990");
        assert_eq!(k("sftp://a.example:2222/x"), "sftp://a.example:2222");
        assert_eq!(
            decoded_path(&Url::parse("ftp://h/pub/a%20b.iso;type=i").unwrap()),
            "/pub/a b.iso"
        );
    }
}
