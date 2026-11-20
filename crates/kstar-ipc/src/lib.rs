//! Messages exchanged between the browser extension (via `kstar-nmhost`) and the K-Star app.
//!
//! Framing matches WebExtension native messaging: a 32-bit little-endian length followed
//! by that many bytes of UTF-8 JSON. The native host can therefore relay frames verbatim.

use serde::{Deserialize, Serialize};
use std::io::{self, Read, Write};
use std::path::PathBuf;

/// Upper bound for one message in either direction.
pub const MAX_MESSAGE: usize = 16 * 1024 * 1024;

/// Bumped when the extension and the app stop understanding each other.
/// The extension sends its own value in `Ping` and compares it with the `Pong`.
pub const PROTOCOL: u32 = 2;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Request {
    Ping {
        #[serde(default)]
        extension_version: Option<String>,
        #[serde(default)]
        protocol: Option<u32>,
    },
    AddDownloads {
        items: Vec<BrowserItem>,
        /// Enqueue directly instead of showing the Add dialog.
        #[serde(default)]
        silent: bool,
    },
    GetStatus,
    /// Bring the main window to the front.
    Show,
}

/// A download handed over by the browser, with the context needed to reproduce the request.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct BrowserItem {
    pub url: String,
    pub filename: Option<String>,
    pub referrer: Option<String>,
    pub page_url: Option<String>,
    /// Ready-made `Cookie` header value.
    pub cookies: Option<String>,
    pub user_agent: Option<String>,
    pub headers: Vec<(String, String)>,
    pub size: Option<u64>,
    pub mime: Option<String>,
    /// `POST` for form submissions; GET when absent.
    pub method: Option<String>,
    /// Base64 request body of a form submission.
    pub body: Option<String>,
    /// Pre-selection in the batch dialog ("Download all links"); selected when absent.
    pub selected: Option<bool>,
    /// What the extension found out about a video on the page (protocol 2).
    pub media: Option<MediaHint>,
}

/// A video the extension found on a page, with what the app can't see from the URL alone.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct MediaHint {
    /// The recogniser that found it: `youtube`, `vimeo`, `facebook`, `instagram`, `tiktok`.
    pub site: Option<String>,
    pub title: Option<String>,
    /// A manifest the page gave as text instead of a URL.
    pub manifest: Option<InlineManifest>,
    /// Whole WebVTT files to offer as subtitle tracks.
    pub subtitles: Vec<SubtitleFile>,
    /// Fetch whole-file tracks in ranges of at most this many bytes.
    pub chunk_bytes: Option<u64>,
    /// Don't fetch anything before this time (ms since the epoch).
    pub not_before_ms: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InlineManifest {
    /// `hls` or `dash`.
    pub kind: String,
    pub body: String,
    /// The address relative URLs in the body are resolved against.
    pub base_url: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SubtitleFile {
    pub url: String,
    pub lang: Option<String>,
    pub label: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Response {
    Pong {
        version: String,
        #[serde(default)]
        protocol: u32,
    },
    Accepted {
        count: usize,
    },
    Status {
        active: usize,
        queued: usize,
        speed: u64,
    },
    Ok,
    Error {
        message: String,
    },
}

/// `$XDG_RUNTIME_DIR/kstar/kstar.sock`, falling back to a per-user dir in /tmp.
pub fn socket_path() -> PathBuf {
    let base = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| {
            // SAFETY: getuid has no failure modes.
            PathBuf::from(format!("/tmp/kstar-{}", unsafe { getuid() }))
        });
    base.join("kstar").join("kstar.sock")
}

/// `$XDG_STATE_HOME/kstar/logs` (normally `~/.local/state/kstar/logs`), shared by the app
/// and the native host.
pub fn log_dir() -> PathBuf {
    std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local").join("state")))
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join("kstar")
        .join("logs")
}

unsafe extern "C" {
    fn getuid() -> u32;
}

pub fn read_frame(r: &mut impl Read) -> io::Result<Option<Vec<u8>>> {
    let mut len = [0u8; 4];
    match r.read_exact(&mut len) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e),
    }
    let len = u32::from_le_bytes(len) as usize;
    if len > MAX_MESSAGE {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "message too large",
        ));
    }
    let mut buf = vec![0; len];
    r.read_exact(&mut buf)?;
    Ok(Some(buf))
}

pub fn write_frame(w: &mut impl Write, payload: &[u8]) -> io::Result<()> {
    if payload.len() > MAX_MESSAGE {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "message too large",
        ));
    }
    w.write_all(&(payload.len() as u32).to_le_bytes())?;
    w.write_all(payload)?;
    w.flush()
}

#[cfg(feature = "tokio")]
pub mod nonblocking {
    use super::MAX_MESSAGE;
    use std::io;
    use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

    pub async fn read_frame<R: AsyncRead + Unpin>(r: &mut R) -> io::Result<Option<Vec<u8>>> {
        let mut len = [0u8; 4];
        match r.read_exact(&mut len).await {
            Ok(_) => {}
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
            Err(e) => return Err(e),
        }
        let len = u32::from_le_bytes(len) as usize;
        if len > MAX_MESSAGE {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "message too large",
            ));
        }
        let mut buf = vec![0; len];
        r.read_exact(&mut buf).await?;
        Ok(Some(buf))
    }

    pub async fn write_frame<W: AsyncWrite + Unpin>(w: &mut W, payload: &[u8]) -> io::Result<()> {
        if payload.len() > MAX_MESSAGE {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "message too large",
            ));
        }
        w.write_all(&(payload.len() as u32).to_le_bytes()).await?;
        w.write_all(payload).await?;
        w.flush().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wire_format() {
        let req: Request = serde_json::from_str(
            r#"{"type":"add_downloads","items":[{"url":"https://x/y.zip","cookies":"a=b"}]}"#,
        )
        .unwrap();
        match req {
            Request::AddDownloads { items, silent } => {
                assert_eq!(items[0].url, "https://x/y.zip");
                assert_eq!(items[0].cookies.as_deref(), Some("a=b"));
                assert!(!silent);
            }
            _ => panic!(),
        }
        let json = serde_json::to_string(&Response::Accepted { count: 2 }).unwrap();
        assert_eq!(json, r#"{"type":"accepted","count":2}"#);
    }

    #[test]
    fn ping_from_any_extension_version() {
        // Extensions from before the version handshake send a bare ping.
        let old: Request = serde_json::from_str(r#"{"type":"ping"}"#).unwrap();
        assert!(matches!(
            old,
            Request::Ping {
                extension_version: None,
                protocol: None
            }
        ));
        let new: Request =
            serde_json::from_str(r#"{"type":"ping","extension_version":"0.2.0","protocol":1}"#)
                .unwrap();
        assert!(matches!(
            new,
            Request::Ping {
                protocol: Some(1),
                ..
            }
        ));
        let pong = serde_json::to_string(&Response::Pong {
            version: "0.2.0".into(),
            protocol: PROTOCOL,
        })
        .unwrap();
        assert_eq!(pong, r#"{"type":"pong","version":"0.2.0","protocol":2}"#);
    }

    #[test]
    fn items_carry_what_the_page_said_about_a_video() {
        let req: Request = serde_json::from_str(
            r#"{"type":"add_downloads","items":[{"url":"https://www.youtube.com/watch?v=x","media":{"site":"youtube","title":"T","manifest":{"kind":"dash","body":"<MPD/>","base_url":"https://www.youtube.com/"},"subtitles":[{"url":"https://x/a.vtt","lang":"en"}],"chunk_bytes":10485760}}]}"#,
        )
        .unwrap();
        let Request::AddDownloads { items, .. } = req else {
            panic!()
        };
        let m = items[0].media.as_ref().unwrap();
        assert_eq!(m.site.as_deref(), Some("youtube"));
        assert_eq!(m.manifest.as_ref().unwrap().kind, "dash");
        assert_eq!(m.subtitles[0].lang.as_deref(), Some("en"));
        assert_eq!(m.chunk_bytes, Some(10 << 20));
        assert_eq!(m.not_before_ms, None);
        // Items from protocol 1 have none.
        let old: BrowserItem = serde_json::from_str(r#"{"url":"https://x/y"}"#).unwrap();
        assert!(old.media.is_none());
    }

    #[test]
    fn framing_roundtrip() {
        let mut buf = Vec::new();
        write_frame(&mut buf, b"{\"type\":\"ping\"}").unwrap();
        assert_eq!(&buf[..4], &15u32.to_le_bytes());
        let mut r = &buf[..];
        assert_eq!(read_frame(&mut r).unwrap().unwrap(), b"{\"type\":\"ping\"}");
        assert!(read_frame(&mut r).unwrap().is_none());
    }
}
