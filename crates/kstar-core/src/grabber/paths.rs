//! Where a saved site's files go, and how saved pages link to each other.

use std::path::{Component, Path, PathBuf};
use url::Url;

use crate::filename;

/// Extensions of pages (a link to anything else is a file).
const PAGE_EXTENSIONS: &[&str] = &[
    "html", "htm", "xhtml", "shtml", "php", "asp", "aspx", "jsp", "cgi", "pl", "cfm",
];

/// The extension of the last part of a URL's path, in lowercase ("" for none).
pub fn extension(url: &Url) -> String {
    let last = url
        .path_segments()
        .and_then(|mut s| s.next_back())
        .unwrap_or("");
    match last.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() && ext.len() <= 10 => ext.to_ascii_lowercase(),
        _ => String::new(),
    }
}

/// Whether a link looks like a page rather than a file.
pub fn is_page_like(url: &Url) -> bool {
    let ext = extension(url);
    ext.is_empty() || PAGE_EXTENSIONS.contains(&ext.as_str())
}

/// The file name of a URL (decoded), or None for a folder.
pub fn file_name(url: &Url) -> Option<String> {
    let last = url.path_segments().and_then(|mut s| s.next_back())?;
    let decoded = percent_encoding::percent_decode_str(last).decode_utf8_lossy();
    filename::sanitize(&decoded)
}

fn short_hash(s: &str) -> String {
    let mut h = crate::checksum::Algo::Sha1.hasher();
    h.update(s.as_bytes());
    h.finish()[..8].to_string()
}

fn trim_len(s: String) -> String {
    if s.len() <= 120 {
        return s;
    }
    let mut cut = 120;
    while !s.is_char_boundary(cut) {
        cut -= 1;
    }
    s[..cut].to_string()
}

/// Where a URL is saved, relative to the site folder: `<host>/<path>`, `index.html` for a
/// folder, a short hash for a query, and `.html` added to pages without it.
pub fn local_path(url: &Url, html: bool) -> PathBuf {
    let host = match url.port() {
        Some(p) => format!("{}_{p}", url.host_str().unwrap_or("site")),
        None => url.host_str().unwrap_or("site").to_string(),
    };
    let mut out = PathBuf::from(filename::sanitize(&host).unwrap_or_else(|| "site".into()));
    let segs: Vec<&str> = url.path_segments().map(|s| s.collect()).unwrap_or_default();
    let (dirs, last) = match segs.split_last() {
        Some((last, dirs)) => (dirs, *last),
        None => (&[][..], ""),
    };
    for d in dirs {
        let d = percent_encoding::percent_decode_str(d).decode_utf8_lossy();
        if let Some(d) = filename::sanitize(&d).filter(|d| d != "..") {
            out.push(trim_len(d));
        }
    }
    let mut name = percent_encoding::percent_decode_str(last)
        .decode_utf8_lossy()
        .into_owned();
    name = filename::sanitize(&name)
        .filter(|n| n != "..")
        .unwrap_or_else(|| "index.html".into());
    if let Some(q) = url.query() {
        let (stem, ext) = filename::split_ext(&name);
        name = format!("{stem}-{}{ext}", short_hash(q));
    }
    let lower = name.to_ascii_lowercase();
    if html && !(lower.ends_with(".html") || lower.ends_with(".htm") || lower.ends_with(".xhtml")) {
        name.push_str(".html");
    }
    out.push(trim_len(name));
    out
}

/// A relative link from the saved file `from` to the saved file `to` (both relative to the
/// site folder), ready to put in an attribute.
pub fn relative_link(from: &Path, to: &Path) -> String {
    let from_dir: Vec<Component> = from
        .parent()
        .map(|p| p.components().collect())
        .unwrap_or_default();
    let to_parts: Vec<Component> = to.components().collect();
    let common = from_dir
        .iter()
        .zip(&to_parts)
        .take_while(|(a, b)| a == b)
        .count();
    let mut parts: Vec<String> = vec!["..".to_string(); from_dir.len() - common];
    for c in &to_parts[common..] {
        parts.push(encode(&c.as_os_str().to_string_lossy()));
    }
    parts.join("/")
}

fn encode(s: &str) -> String {
    const SET: &percent_encoding::AsciiSet = &percent_encoding::CONTROLS
        .add(b' ')
        .add(b'"')
        .add(b'#')
        .add(b'%')
        .add(b'?')
        .add(b'<')
        .add(b'>')
        .add(b'\'');
    percent_encoding::utf8_percent_encode(s, SET).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn u(s: &str) -> Url {
        Url::parse(s).unwrap()
    }

    #[test]
    fn local_paths() {
        assert_eq!(
            local_path(&u("https://a.example/"), true),
            Path::new("a.example/index.html")
        );
        assert_eq!(
            local_path(&u("https://a.example/docs/"), true),
            Path::new("a.example/docs/index.html")
        );
        assert_eq!(
            local_path(&u("https://a.example/docs/guide"), true),
            Path::new("a.example/docs/guide.html")
        );
        assert_eq!(
            local_path(&u("https://a.example/img/logo.png"), false),
            Path::new("a.example/img/logo.png")
        );
        let q = local_path(&u("https://a.example/view.php?id=3"), true);
        let name = q.file_name().unwrap().to_string_lossy().to_string();
        assert!(
            name.starts_with("view-") && name.ends_with(".php.html"),
            "{name}"
        );
        assert_eq!(
            local_path(&u("http://a.example:8080/x%20y.css"), false),
            Path::new("a.example_8080/x y.css")
        );
        assert_eq!(
            local_path(&u("https://a.example/a/../../b.css"), false),
            Path::new("a.example/b.css")
        );
    }

    #[test]
    fn relative_links() {
        let page = Path::new("a.example/docs/guide.html");
        assert_eq!(
            relative_link(page, Path::new("a.example/docs/x.png")),
            "x.png"
        );
        assert_eq!(
            relative_link(page, Path::new("a.example/img/logo.png")),
            "../img/logo.png"
        );
        assert_eq!(
            relative_link(page, Path::new("cdn.example/s p.css")),
            "../../cdn.example/s%20p.css"
        );
        assert!(is_page_like(&u("https://a/x/")));
        assert!(is_page_like(&u("https://a/x.php?y")));
        assert!(!is_page_like(&u("https://a/x.zip")));
        assert_eq!(
            file_name(&u("https://a/f%20g.zip")).as_deref(),
            Some("f g.zip")
        );
    }
}
