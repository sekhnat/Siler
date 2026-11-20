//! Working out a good local filename for a download.

use percent_encoding::percent_decode_str;
use reqwest::Url;
use std::path::{Path, PathBuf};

const PART_SUFFIX: &str = ".kstar.part";
const MAX_NAME_BYTES: usize = 240;

/// Extracts the filename from a `Content-Disposition` header value (RFC 6266),
/// preferring the RFC 5987 `filename*` form.
pub fn from_content_disposition(value: &str) -> Option<String> {
    let mut plain = None;
    let mut extended = None;
    for param in split_params(value) {
        let Some((key, val)) = param.split_once('=') else {
            continue;
        };
        match key.trim().to_ascii_lowercase().as_str() {
            "filename*" => extended = decode_ext_value(val.trim()),
            "filename" => plain = Some(maybe_percent_decode(&unquote(val))),
            _ => {}
        }
    }
    extended
        .and_then(|s| sanitize(&s))
        .or_else(|| plain.and_then(|s| sanitize(&s)))
}

/// Uses the last non-empty path segment of a URL.
pub fn from_url(url: &Url) -> Option<String> {
    let seg = url.path_segments()?.rev().find(|s| !s.is_empty())?;
    sanitize(&percent_decode_str(seg).decode_utf8_lossy())
}

/// Makes a name safe to use as a single path component. Returns `None` if nothing usable is left.
/// The file name a URL string points at, if it has one.
pub fn name_from_url(url: &str) -> Option<String> {
    from_url(&Url::parse(url).ok()?)
}

pub fn sanitize(name: &str) -> Option<String> {
    let base = name.rsplit(['/', '\\']).next().unwrap_or("");
    let cleaned: String = base
        .chars()
        .map(|c| if c.is_control() { '_' } else { c })
        .collect();
    let trimmed = cleaned
        .trim()
        .trim_start_matches('.')
        .trim_end_matches(['.', ' '])
        .to_string();
    if trimmed.is_empty() {
        return None;
    }
    Some(truncate_bytes(trimmed, MAX_NAME_BYTES))
}

/// Splits `name` into (stem, extension-with-dot), recognising compound tar extensions.
pub fn split_ext(name: &str) -> (&str, &str) {
    let lower = name.to_ascii_lowercase();
    for multi in [".tar.gz", ".tar.bz2", ".tar.xz", ".tar.zst"] {
        if lower.ends_with(multi) && name.len() > multi.len() {
            let i = name.len() - multi.len();
            return (&name[..i], &name[i..]);
        }
    }
    match name.rfind('.') {
        Some(i) if i > 0 && name.len() - i <= 16 && name.len() - i > 1 => (&name[..i], &name[i..]),
        _ => (name, ""),
    }
}

/// Appends an extension derived from the MIME type when the name has none.
pub fn ensure_extension(name: String, content_type: Option<&str>) -> String {
    if !split_ext(&name).1.is_empty() {
        return name;
    }
    let mime = content_type
        .and_then(|ct| ct.split(';').next())
        .map(|m| m.trim().to_ascii_lowercase())
        .unwrap_or_default();
    let ext = match mime.as_str() {
        "" | "application/octet-stream" | "binary/octet-stream" => None,
        "text/html" => Some("html"),
        "text/plain" => Some("txt"),
        "image/jpeg" => Some("jpg"),
        "video/mp4" => Some("mp4"),
        "audio/mpeg" => Some("mp3"),
        "application/zip" => Some("zip"),
        "application/pdf" => Some("pdf"),
        "application/x-iso9660-image" => Some("iso"),
        m => mime_guess::get_mime_extensions_str(m).and_then(|e| e.first().copied()),
    };
    match ext {
        Some(e) => format!("{name}.{e}"),
        None => name,
    }
}

/// Path of the in-progress file for `name` inside `dir`.
pub fn part_path(dir: &Path, name: &str) -> PathBuf {
    dir.join(format!("{name}{PART_SUFFIX}"))
}

/// Returns `name`, or `name (1).ext`, `name (2).ext`, … — the first one that is not
/// already on disk (finished or partial) and not claimed by `taken`.
pub fn unique_name(dir: &Path, name: &str, taken: impl Fn(&str) -> bool) -> String {
    let free = |n: &str| !taken(n) && !dir.join(n).exists() && !part_path(dir, n).exists();
    if free(name) {
        return name.to_string();
    }
    let (stem, ext) = split_ext(name);
    (1..)
        .map(|i| format!("{stem} ({i}){ext}"))
        .find(|c| free(c))
        .expect("unbounded search always finds a name")
}

fn split_params(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let (mut in_quotes, mut escaped) = (false, false);
    for c in s.chars() {
        if escaped {
            cur.push(c);
            escaped = false;
            continue;
        }
        match c {
            '\\' if in_quotes => {
                cur.push(c);
                escaped = true;
            }
            '"' => {
                in_quotes = !in_quotes;
                cur.push(c);
            }
            ';' if !in_quotes => out.push(std::mem::take(&mut cur)),
            _ => cur.push(c),
        }
    }
    out.push(cur);
    out
}

fn unquote(v: &str) -> String {
    let v = v.trim();
    if v.len() >= 2 && v.starts_with('"') && v.ends_with('"') {
        let mut out = String::new();
        let mut escaped = false;
        for c in v[1..v.len() - 1].chars() {
            if escaped {
                out.push(c);
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else {
                out.push(c);
            }
        }
        out
    } else {
        v.to_string()
    }
}

fn decode_ext_value(v: &str) -> Option<String> {
    let v = unquote(v);
    let mut parts = v.splitn(3, '\'');
    let charset = parts.next()?.trim().to_ascii_lowercase();
    let _lang = parts.next()?;
    let bytes: Vec<u8> = percent_decode_str(parts.next()?).collect();
    if charset == "utf-8" || charset.is_empty() {
        String::from_utf8(bytes).ok()
    } else {
        // ISO-8859-1 maps byte-for-byte onto the first 256 code points.
        Some(bytes.into_iter().map(char::from).collect())
    }
}

fn maybe_percent_decode(s: &str) -> String {
    if s.contains('%')
        && let Ok(d) = percent_decode_str(s).decode_utf8()
    {
        return d.into_owned();
    }
    s.to_string()
}

fn truncate_bytes(s: String, max: usize) -> String {
    if s.len() <= max {
        return s;
    }
    let (stem, ext) = split_ext(&s);
    let mut cut = max.saturating_sub(ext.len()).min(stem.len());
    while !stem.is_char_boundary(cut) {
        cut -= 1;
    }
    format!("{}{}", &stem[..cut], ext)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_disposition_variants() {
        assert_eq!(
            from_content_disposition("attachment; filename=\"report final.pdf\"").as_deref(),
            Some("report final.pdf")
        );
        assert_eq!(
            from_content_disposition(
                "attachment; filename=\"fallback.bin\"; filename*=UTF-8''na%C3%AFve%20r%C3%A9sum%C3%A9.bin"
            )
            .as_deref(),
            Some("naïve résumé.bin")
        );
        assert_eq!(
            from_content_disposition("attachment; filename*=iso-8859-1'en'%E9t%E9.txt").as_deref(),
            Some("été.txt")
        );
        assert_eq!(
            from_content_disposition("attachment; filename=\"a;b \\\"q\\\".zip\"").as_deref(),
            Some("a;b \"q\".zip")
        );
        assert_eq!(
            from_content_disposition("inline; filename=../../etc/passwd").as_deref(),
            Some("passwd")
        );
        assert_eq!(from_content_disposition("attachment").as_deref(), None);
    }

    #[test]
    fn url_names() {
        let u = Url::parse("https://x.org/dl/My%20File.tar.gz?token=1").unwrap();
        assert_eq!(from_url(&u).as_deref(), Some("My File.tar.gz"));
        assert_eq!(from_url(&Url::parse("https://x.org/").unwrap()), None);
    }

    #[test]
    fn sanitizing() {
        assert_eq!(sanitize("  .hidden  ").as_deref(), Some("hidden"));
        assert_eq!(sanitize("..."), None);
        assert_eq!(sanitize("a\u{0}b").as_deref(), Some("a_b"));
        let long = format!("{}.iso", "é".repeat(200));
        let s = sanitize(&long).unwrap();
        assert!(s.len() <= MAX_NAME_BYTES && s.ends_with(".iso"));
    }

    #[test]
    fn extensions() {
        assert_eq!(split_ext("x.tar.gz"), ("x", ".tar.gz"));
        assert_eq!(split_ext("noext"), ("noext", ""));
        assert_eq!(split_ext(".bashrc"), (".bashrc", ""));
        assert_eq!(
            ensure_extension("file".into(), Some("application/pdf")),
            "file.pdf"
        );
        assert_eq!(
            ensure_extension("file.bin".into(), Some("application/pdf")),
            "file.bin"
        );
        assert_eq!(
            ensure_extension("file".into(), Some("application/octet-stream")),
            "file"
        );
    }

    #[test]
    fn unique_names() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.zip"), b"").unwrap();
        std::fs::write(part_path(dir.path(), "a (1).zip"), b"").unwrap();
        assert_eq!(unique_name(dir.path(), "a.zip", |_| false), "a (2).zip");
        assert_eq!(
            unique_name(dir.path(), "b.zip", |n| n == "b.zip"),
            "b (1).zip"
        );
    }
}
