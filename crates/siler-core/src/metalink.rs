//! Metalink files: one file (or several) with all its mirrors and checksums. Metalink 4
//! (RFC 5854, `.meta4`) and the older 3.0 (`.metalink`) are read; torrents they mention are
//! ignored, and only http(s) addresses are used.

use serde::Serialize;
use std::path::Path;

use crate::checksum::{Algo, Checksum, ChecksumSource, Pieces};
use crate::extras::{Mirror, MirrorOrigin};
use crate::{NewDownload, filename};
use siler_media::ParseLimits;
use siler_media::xml::{self, Node};

/// The largest metalink Siler reads.
pub const MAX_BYTES: u64 = 4 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MetaFile {
    /// The file name, without folders.
    pub name: String,
    /// Folders the file belongs in, below the download folder (already made safe).
    pub folders: Vec<String>,
    pub size: Option<u64>,
    /// Best first.
    pub urls: Vec<String>,
    pub checksum: Option<Checksum>,
    pub pieces: Option<Pieces>,
}

/// Whether a download is a metalink, by its type or name.
pub fn is_metalink(content_type: Option<&str>, name: &str) -> bool {
    let ct = content_type
        .unwrap_or("")
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    if ct == "application/metalink4+xml" || ct == "application/metalink+xml" {
        return true;
    }
    let n = name.to_ascii_lowercase();
    n.ends_with(".meta4") || n.ends_with(".metalink")
}

/// Whether some bytes look like a metalink document.
pub fn looks_like(bytes: &[u8]) -> bool {
    let head = String::from_utf8_lossy(&bytes[..bytes.len().min(4096)]).to_ascii_lowercase();
    head.contains("<metalink")
}

pub fn parse(bytes: &[u8]) -> Result<Vec<MetaFile>, String> {
    let lim = ParseLimits::default();
    let root = xml::parse(bytes, &lim).map_err(|e| format!("The metalink can't be read: {e}"))?;
    if root.name != "metalink" {
        return Err("This isn't a metalink file.".into());
    }
    let v3 =
        root.attr("version").is_some_and(|v| v.starts_with('3')) || root.child("files").is_some();
    let files: Vec<&Node> = if v3 {
        root.child("files")
            .map(|f| f.children_named("file").collect())
            .unwrap_or_default()
    } else {
        root.children_named("file").collect()
    };
    let mut out = Vec::new();
    for f in files {
        if let Some(mf) = if v3 { file_v3(f) } else { file_v4(f) } {
            out.push(mf);
        }
    }
    if out.is_empty() {
        return Err("The metalink lists no file Siler can download (http or https).".into());
    }
    Ok(out)
}

/// Splits a metalink file name into safe folders and a file name. Absolute names and names
/// that climb out of the folder (`..`) are refused.
fn split_name(name: &str) -> Option<(Vec<String>, String)> {
    let name = name.trim();
    if name.is_empty() || name.starts_with('/') || name.starts_with('\\') || name.contains(':') {
        return None;
    }
    let mut parts = Vec::new();
    for c in name.split(['/', '\\']) {
        match c {
            "" | "." => {}
            ".." => return None,
            c => parts.push(filename::sanitize(c)?),
        }
    }
    let file = parts.pop()?;
    Some((parts, file))
}

fn http_url(s: &str) -> Option<String> {
    let u = url::Url::parse(s.trim()).ok()?;
    matches!(u.scheme(), "http" | "https").then(|| u.to_string())
}

fn best_hash<'a>(hashes: impl Iterator<Item = &'a Node>) -> Option<Checksum> {
    hashes
        .filter_map(|h| {
            let algo = Algo::from_name(h.attr("type")?)?;
            Checksum::new(algo, h.text_trimmed(), ChecksumSource::Metalink)
        })
        .max_by_key(|c| Algo::ALL.iter().position(|a| *a == c.algo))
}

fn pieces_of(p: &Node) -> Option<Pieces> {
    let algo = Algo::from_name(p.attr("type")?)?;
    let length: u64 = p.attr("length")?.trim().parse().ok()?;
    let hashes: Vec<String> = p
        .children_named("hash")
        .map(|h| h.text_trimmed().to_ascii_lowercase())
        .collect();
    let valid = length > 0
        && !hashes.is_empty()
        && hashes
            .iter()
            .all(|h| h.len() == algo.digest_len() * 2 && h.bytes().all(|b| b.is_ascii_hexdigit()));
    valid.then_some(Pieces {
        algo,
        length,
        hashes,
    })
}

fn file_v4(f: &Node) -> Option<MetaFile> {
    let (folders, name) = split_name(f.attr("name")?)?;
    let size = f.child("size").and_then(|s| s.text_trimmed().parse().ok());
    let mut urls: Vec<(u32, String)> = f
        .children_named("url")
        .filter_map(|u| {
            let pri = u
                .attr("priority")
                .and_then(|p| p.trim().parse().ok())
                .unwrap_or(999_999);
            Some((pri, http_url(u.text_trimmed())?))
        })
        .collect();
    urls.sort_by_key(|(p, _)| *p);
    let urls = dedup(urls.into_iter().map(|(_, u)| u));
    if urls.is_empty() {
        return None;
    }
    let pieces = f
        .children_named("pieces")
        .filter_map(pieces_of)
        .max_by_key(|p| Algo::ALL.iter().position(|a| *a == p.algo));
    Some(MetaFile {
        name,
        folders,
        size,
        urls,
        checksum: best_hash(f.children_named("hash")),
        pieces: pieces.filter(|p| size.is_none_or(|s| p.fits(s))),
    })
}

fn file_v3(f: &Node) -> Option<MetaFile> {
    let (folders, name) = split_name(f.attr("name")?)?;
    let size = f.child("size").and_then(|s| s.text_trimmed().parse().ok());
    let mut urls: Vec<(u32, String)> = f
        .child("resources")
        .map(|r| {
            r.children_named("url")
                .filter_map(|u| {
                    // Higher preference is better in 3.0.
                    let pref: u32 = u
                        .attr("preference")
                        .and_then(|p| p.trim().parse().ok())
                        .unwrap_or(0);
                    Some((
                        100u32.saturating_sub(pref.min(100)),
                        http_url(u.text_trimmed())?,
                    ))
                })
                .collect()
        })
        .unwrap_or_default();
    urls.sort_by_key(|(p, _)| *p);
    let urls = dedup(urls.into_iter().map(|(_, u)| u));
    if urls.is_empty() {
        return None;
    }
    let verification = f.child("verification");
    let checksum = verification.and_then(|v| best_hash(v.children_named("hash")));
    let pieces = verification
        .and_then(|v| v.children_named("pieces").filter_map(pieces_of).next())
        .filter(|p| size.is_none_or(|s| p.fits(s)));
    Some(MetaFile {
        name,
        folders,
        size,
        urls,
        checksum,
        pieces,
    })
}

fn dedup(urls: impl Iterator<Item = String>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for u in urls {
        if !out.contains(&u) {
            out.push(u);
        }
    }
    out
}

impl MetaFile {
    /// The download for this file: its best address, the others as mirrors, saved under
    /// `base` (plus the metalink's folders).
    pub fn to_download(&self, base: Option<&Path>) -> NewDownload {
        let dir = base.map(|b| self.folders.iter().fold(b.to_path_buf(), |d, f| d.join(f)));
        NewDownload {
            url: self.urls[0].clone(),
            filename: Some(self.name.clone()),
            dir,
            checksum: self.checksum.clone(),
            mirrors: self.urls[1..]
                .iter()
                .map(|u| Mirror::new(u.clone(), MirrorOrigin::Metalink))
                .collect(),
            pieces: self.pieces.clone(),
            ..Default::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const V4: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<metalink xmlns="urn:ietf:params:xml:ns:metalink">
  <file name="example.ext">
    <size>14471447</size>
    <hash type="md5">dcb2c6f65f4e1b3a8a3e3c7a3b2d6f0e</hash>
    <hash type="sha-256">f0ad929cd259957e160ea442eb80986b5f01ab2c6ec1ce5e3d3a8d7b4e1a0b6d</hash>
    <pieces length="8388608" type="sha-1">
      <hash>a9993e364706816aba3e25717850c26c9cd0d89d</hash>
      <hash>84983e441c3bd26ebaae4aa1f95129e5e54670f1</hash>
    </pieces>
    <url location="de" priority="2">http://ftp.de.example.com/example.ext</url>
    <url location="fr" priority="1">https://download.fr.example.com/example.ext</url>
    <url priority="3">ftp://ftp.example.com/example.ext</url>
    <metaurl mediatype="torrent" priority="1">http://example.com/example.ext.torrent</metaurl>
  </file>
  <file name="../evil.sh"><url>http://x/evil.sh</url></file>
  <file name="/etc/passwd"><url>http://x/p</url></file>
  <file name="sub/dir/nested.bin"><url>http://x/n</url></file>
  <file name="ftp-only.bin"><url>ftp://x/f</url></file>
</metalink>"#;

    const V3: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<metalink version="3.0" xmlns="http://www.metalinker.org/">
  <files>
    <file name="old.iso">
      <size>20</size>
      <verification>
        <hash type="sha1">a9993e364706816aba3e25717850c26c9cd0d89d</hash>
        <pieces type="sha1" length="10">
          <hash piece="0">a9993e364706816aba3e25717850c26c9cd0d89d</hash>
          <hash piece="1">84983e441c3bd26ebaae4aa1f95129e5e54670f1</hash>
        </pieces>
      </verification>
      <resources>
        <url type="http" preference="10">http://slow.example/old.iso</url>
        <url type="http" preference="90">http://fast.example/old.iso</url>
      </resources>
    </file>
  </files>
</metalink>"#;

    #[test]
    fn reads_metalink_4() {
        let files = parse(V4.as_bytes()).unwrap();
        assert_eq!(
            files.len(),
            2,
            "unsafe names and ftp-only files are left out"
        );
        let f = &files[0];
        assert_eq!(f.name, "example.ext");
        assert_eq!(f.size, Some(14471447));
        assert_eq!(
            f.urls,
            [
                "https://download.fr.example.com/example.ext",
                "http://ftp.de.example.com/example.ext"
            ]
        );
        assert_eq!(f.checksum.as_ref().unwrap().algo, Algo::Sha256);
        let p = f.pieces.as_ref().unwrap();
        assert_eq!((p.algo, p.length, p.hashes.len()), (Algo::Sha1, 8388608, 2));
        assert_eq!(files[1].folders, ["sub", "dir"]);
        assert_eq!(files[1].name, "nested.bin");

        let nd = f.to_download(Some(Path::new("/dl")));
        assert_eq!(nd.url, f.urls[0]);
        assert_eq!(nd.mirrors.len(), 1);
        assert_eq!(nd.mirrors[0].origin, MirrorOrigin::Metalink);
        assert_eq!(nd.dir.as_deref(), Some(Path::new("/dl")));
        let nested = files[1].to_download(Some(Path::new("/dl")));
        assert_eq!(nested.dir.as_deref(), Some(Path::new("/dl/sub/dir")));
    }

    #[test]
    fn reads_metalink_3() {
        let files = parse(V3.as_bytes()).unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(
            files[0].urls,
            ["http://fast.example/old.iso", "http://slow.example/old.iso"]
        );
        assert_eq!(files[0].checksum.as_ref().unwrap().algo, Algo::Sha1);
        assert_eq!(files[0].pieces.as_ref().unwrap().hashes.len(), 2);
    }

    #[test]
    fn refuses_other_documents() {
        assert!(parse(b"<html></html>").is_err());
        assert!(parse(b"not xml").is_err());
        assert!(
            parse(b"<!DOCTYPE x [<!ENTITY a 'b'>]><metalink/>").is_err(),
            "no DTDs"
        );
        assert!(is_metalink(
            Some("application/metalink4+xml; charset=utf-8"),
            "x"
        ));
        assert!(is_metalink(None, "Ubuntu.META4"));
        assert!(!is_metalink(Some("text/xml"), "a.xml"));
    }
}
