//! Plain playlists: M3U (without HLS tags), PLS and ASX. They are lists of links, not
//! streams: each entry becomes its own download (or stream recording).

use url::Url;

use crate::error::ParseError;
use crate::limits::ParseLimits;

/// One entry of a playlist.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub url: Url,
    pub title: Option<String>,
}

/// Schemes an entry may use: the web, FTP, and the stream protocols Siler records.
const SCHEMES: &[&str] = &[
    "http", "https", "ftp", "ftps", "sftp", "rtmp", "rtmpt", "mms", "mmsh", "mmst",
];

/// At most this many entries are kept.
const MAX_ENTRIES: usize = 5000;

/// Reads a playlist of any of the three formats; `base` resolves relative entries.
pub fn parse(body: &[u8], base: &Url, lim: &ParseLimits) -> Result<Vec<Entry>, ParseError> {
    if body.len() > lim.max_manifest_bytes {
        return Err(ParseError::TooLarge {
            limit: lim.max_manifest_bytes,
        });
    }
    let text = String::from_utf8_lossy(body);
    let text = text.trim_start_matches('\u{feff}');
    let head = text.trim_start();
    let lower: String = head
        .chars()
        .take(512)
        .collect::<String>()
        .to_ascii_lowercase();
    let entries = if lower.starts_with("[playlist]") {
        pls(head, base)
    } else if lower.contains("<asx") {
        asx(body, base, lim)?
    } else {
        m3u(head, base)
    };
    if entries.is_empty() {
        return Err(ParseError::invalid(
            "the playlist has no links Siler can use",
        ));
    }
    Ok(entries)
}

fn entry(raw: &str, base: &Url, title: Option<String>) -> Option<Entry> {
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }
    let url = base.join(raw).ok()?;
    if !SCHEMES.contains(&url.scheme()) || url.host_str().is_none() {
        return None;
    }
    // `mms://` is the old name for MMS over TCP or HTTP; FFmpeg knows mmsh and mmst.
    Some(Entry {
        url,
        title: title
            .map(|t| t.trim().to_string())
            .filter(|t| !t.is_empty()),
    })
}

fn push(out: &mut Vec<Entry>, e: Option<Entry>) {
    if let Some(e) = e
        && out.len() < MAX_ENTRIES
        && !out.iter().any(|o| o.url == e.url)
    {
        out.push(e);
    }
}

/// `#EXTM3U` with `#EXTINF:<seconds>,<title>` lines, or just one link per line.
fn m3u(text: &str, base: &Url) -> Vec<Entry> {
    let mut out = Vec::new();
    let mut title = None;
    for line in text.lines().map(str::trim) {
        if let Some(info) = line.strip_prefix("#EXTINF:") {
            title = info.split_once(',').map(|(_, t)| t.to_string());
        } else if line.starts_with('#') || line.is_empty() {
            continue;
        } else {
            push(&mut out, entry(line, base, title.take()));
        }
    }
    out
}

/// `[playlist]` with `File1=…`, `Title1=…` keys.
fn pls(text: &str, base: &Url) -> Vec<Entry> {
    let mut files: Vec<(u32, String)> = Vec::new();
    let mut titles: Vec<(u32, String)> = Vec::new();
    for line in text.lines().map(str::trim) {
        let Some((k, v)) = line.split_once('=') else {
            continue;
        };
        let k = k.trim().to_ascii_lowercase();
        let num = |prefix: &str| k.strip_prefix(prefix).and_then(|n| n.parse::<u32>().ok());
        if let Some(n) = num("file") {
            files.push((n, v.trim().to_string()));
        } else if let Some(n) = num("title") {
            titles.push((n, v.trim().to_string()));
        }
    }
    files.sort_by_key(|(n, _)| *n);
    let mut out = Vec::new();
    for (n, f) in files {
        let title = titles.iter().find(|(m, _)| *m == n).map(|(_, t)| t.clone());
        push(&mut out, entry(&f, base, title));
    }
    out
}

/// `<asx><entry><title/><ref href="…"/></entry></asx>`; element names are case-insensitive.
fn asx(body: &[u8], base: &Url, lim: &ParseLimits) -> Result<Vec<Entry>, ParseError> {
    let root = crate::xml::parse(body, lim)?;
    let mut out = Vec::new();
    fn lower(n: &crate::xml::Node) -> String {
        n.name.to_ascii_lowercase()
    }
    fn attr<'a>(n: &'a crate::xml::Node, name: &str) -> Option<&'a str> {
        n.attrs
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
    for e in root.children.iter().filter(|c| lower(c) == "entry") {
        let title = e
            .children
            .iter()
            .find(|c| lower(c) == "title")
            .map(|t| t.text_trimmed().to_string());
        for r in e.children.iter().filter(|c| lower(c) == "ref") {
            if let Some(href) = attr(r, "href") {
                push(&mut out, entry(href, base, title.clone()));
            }
        }
    }
    // Some ASX files put the refs directly under the root.
    for r in root.children.iter().filter(|c| lower(c) == "ref") {
        if let Some(href) = attr(r, "href") {
            push(&mut out, entry(href, base, None));
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> Url {
        Url::parse("http://radio.example/lists/today.m3u").unwrap()
    }

    fn urls(e: &[Entry]) -> Vec<&str> {
        e.iter().map(|e| e.url.as_str()).collect()
    }

    #[test]
    fn m3u_with_and_without_tags() {
        let lim = ParseLimits::default();
        let e = parse(
            b"#EXTM3U\n#EXTINF:123,First song\nsongs/one.mp3\n#EXTINF:-1,Live radio\nmms://media.example/radio\n\n# note\nhttp://other.example/two.ogg\njavascript:alert(1)\n",
            &base(),
            &lim,
        )
        .unwrap();
        assert_eq!(
            urls(&e),
            [
                "http://radio.example/lists/songs/one.mp3",
                "mms://media.example/radio",
                "http://other.example/two.ogg"
            ]
        );
        assert_eq!(e[0].title.as_deref(), Some("First song"));
        assert_eq!(e[2].title, None);
        let plain = parse(
            b"https://a.example/x.mp4\r\nhttps://a.example/x.mp4\r\n",
            &base(),
            &lim,
        )
        .unwrap();
        assert_eq!(plain.len(), 1);
    }

    #[test]
    fn pls() {
        let e = parse(
            b"[playlist]\nNumberOfEntries=2\nFile2=http://b.example/2.mp3\nTitle2=Two\nFile1=http://a.example/1.mp3\nTitle1=One\nLength1=-1\nVersion=2\n",
            &base(),
            &ParseLimits::default(),
        )
        .unwrap();
        assert_eq!(
            urls(&e),
            ["http://a.example/1.mp3", "http://b.example/2.mp3"]
        );
        assert_eq!(e[1].title.as_deref(), Some("Two"));
    }

    #[test]
    fn asx() {
        let e = parse(
            br#"<ASX version="3.0"><Title>Show</Title>
                 <Entry><Title>Part 1</Title><Ref HREF="mms://wm.example/show1.wmv"/><Ref href="http://wm.example/show1.wmv"/></Entry>
                 <ENTRY><REF HREF="rtmp://fl.example/app/stream"/></ENTRY>
               </ASX>"#,
            &base(),
            &ParseLimits::default(),
        )
        .unwrap();
        assert_eq!(
            urls(&e),
            [
                "mms://wm.example/show1.wmv",
                "http://wm.example/show1.wmv",
                "rtmp://fl.example/app/stream"
            ]
        );
        assert_eq!(e[0].title.as_deref(), Some("Part 1"));
    }

    #[test]
    fn empty_is_an_error() {
        assert!(parse(b"#EXTM3U\n# nothing\n", &base(), &ParseLimits::default()).is_err());
    }
}
