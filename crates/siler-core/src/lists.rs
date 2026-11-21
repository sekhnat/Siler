//! Importing and exporting download lists.
//!
//! Read: a plain list (any http(s) links in the text), Siler's own JSON, a metalink, or an
//! IDM export (`.ef2`). Written: a plain list of addresses, or Siler JSON.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use crate::checksum::{Checksum, Pieces};
use crate::extras::{Mirror, MirrorOrigin};
use crate::metalink;

/// The largest list file Siler reads.
pub const MAX_BYTES: usize = 32 * 1024 * 1024;
/// The most downloads one import gives.
pub const MAX_ITEMS: usize = 10_000;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ImportItem {
    pub url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub filename: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dir: Option<PathBuf>,
    /// A queue name (matched to a queue when the item is added).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub queue: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub checksum: Option<Checksum>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub mirrors: Vec<Mirror>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pieces: Option<Pieces>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub referrer: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_agent: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cookies: Option<String>,
    /// Folders below the download folder (from a metalink).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub folders: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ListFormat {
    Text,
    Json,
    Metalink,
    Ef2,
}

#[derive(Serialize, Deserialize)]
struct JsonList {
    siler: u32,
    downloads: Vec<ImportItem>,
}

/// Reads a list file, telling its format by its content.
pub fn import(bytes: &[u8]) -> Result<(ListFormat, Vec<ImportItem>), String> {
    if bytes.len() > MAX_BYTES {
        return Err("The file is too large to be a download list.".into());
    }
    if metalink::looks_like(bytes) {
        let files = metalink::parse(bytes)?;
        let items = files
            .into_iter()
            .map(|f| ImportItem {
                url: f.urls[0].clone(),
                filename: Some(f.name.clone()),
                size: f.size,
                checksum: f.checksum.clone(),
                mirrors: f.urls[1..]
                    .iter()
                    .map(|u| Mirror::new(u.clone(), MirrorOrigin::Metalink))
                    .collect(),
                pieces: f.pieces.clone(),
                folders: f.folders.clone(),
                ..Default::default()
            })
            .collect();
        return finish(ListFormat::Metalink, items);
    }
    let text = String::from_utf8_lossy(bytes);
    let trimmed = text.trim_start_matches('\u{feff}').trim_start();
    if trimmed.starts_with('{') {
        let list: JsonList = serde_json::from_str(trimmed)
            .map_err(|e| format!("The Siler list can't be read: {e}"))?;
        if list.siler != 1 {
            return Err("This Siler list was written by a newer version.".into());
        }
        let items = list
            .downloads
            .into_iter()
            .filter(|i| is_http(&i.url))
            .map(|mut i| {
                // Mirrors from a file are treated like ones from a metalink.
                for m in &mut i.mirrors {
                    m.origin = MirrorOrigin::Metalink;
                    m.disabled = None;
                    m.bytes = 0;
                }
                i.mirrors.retain(|m| is_http(&m.url));
                i
            })
            .collect();
        return finish(ListFormat::Json, items);
    }
    if text.lines().any(|l| l.trim() == "<") && text.lines().any(|l| l.trim() == ">") {
        return finish(ListFormat::Ef2, ef2(&text));
    }
    finish(ListFormat::Text, links(&text))
}

fn finish(
    format: ListFormat,
    mut items: Vec<ImportItem>,
) -> Result<(ListFormat, Vec<ImportItem>), String> {
    if items.is_empty() {
        return Err("The file has no http or https links.".into());
    }
    if items.len() > MAX_ITEMS {
        return Err(format!(
            "The file lists {} downloads; Siler imports at most {MAX_ITEMS} at a time.",
            items.len()
        ));
    }
    let mut seen = std::collections::HashSet::new();
    items.retain(|i| seen.insert((i.url.clone(), i.filename.clone())));
    Ok((format, items))
}

fn is_http(s: &str) -> bool {
    url::Url::parse(s.trim())
        .is_ok_and(|u| matches!(u.scheme(), "http" | "https") && u.host_str().is_some())
}

/// Every http(s) link in some text.
pub fn links(text: &str) -> Vec<ImportItem> {
    let mut out: Vec<ImportItem> = Vec::new();
    for word in text.split(|c: char| c.is_whitespace() || matches!(c, '"' | '\'' | '<' | '>' | '`'))
    {
        let Some(start) = word.find("http://").or_else(|| word.find("https://")) else {
            continue;
        };
        let cand = word[start..].trim_end_matches(['.', ',', ';', ':', ')', ']', '}', '!', '?']);
        if is_http(cand) && !out.iter().any(|i| i.url == cand) {
            out.push(ImportItem {
                url: cand.to_string(),
                ..Default::default()
            });
        }
    }
    out
}

/// IDM's export: blocks between `<` and `>` lines, the address first, then `referer:`,
/// `User-Agent:`, `cookie:` and similar lines.
fn ef2(text: &str) -> Vec<ImportItem> {
    let mut out = Vec::new();
    let mut cur: Option<ImportItem> = None;
    for line in text.lines().map(str::trim) {
        match line {
            "<" => cur = Some(ImportItem::default()),
            ">" => {
                if let Some(i) = cur.take().filter(|i| is_http(&i.url)) {
                    out.push(i);
                }
            }
            "" => {}
            l => {
                let Some(item) = cur.as_mut() else { continue };
                if item.url.is_empty() {
                    item.url = l.to_string();
                    continue;
                }
                let Some((k, v)) = l.split_once(':') else {
                    continue;
                };
                let v = v.trim().to_string();
                match k.trim().to_ascii_lowercase().as_str() {
                    "referer" | "referrer" => item.referrer = Some(v),
                    "user-agent" => item.user_agent = Some(v),
                    "cookie" => item.cookies = Some(v),
                    "filename" => item.filename = crate::filename::sanitize(&v),
                    _ => {}
                }
            }
        }
    }
    out
}

/// One download to export.
pub struct ExportItem<'a> {
    pub url: &'a str,
    pub filename: Option<&'a str>,
    pub dir: Option<&'a std::path::Path>,
    pub queue: Option<&'a str>,
    pub size: Option<u64>,
    pub checksum: Option<&'a Checksum>,
    pub mirrors: Vec<&'a str>,
    pub referrer: Option<&'a str>,
    /// Only when the user asked to include them.
    pub cookies: Option<&'a str>,
    pub user_agent: Option<&'a str>,
}

pub fn export(items: &[ExportItem], format: ListFormat) -> Result<String, String> {
    match format {
        ListFormat::Text => Ok(items.iter().map(|i| format!("{}\n", i.url)).collect()),
        ListFormat::Json => {
            let list = JsonList {
                siler: 1,
                downloads: items
                    .iter()
                    .map(|i| ImportItem {
                        url: i.url.to_string(),
                        filename: i.filename.map(str::to_string),
                        dir: i.dir.map(|d| d.to_path_buf()),
                        queue: i.queue.map(str::to_string),
                        size: i.size,
                        checksum: i.checksum.cloned(),
                        mirrors: i
                            .mirrors
                            .iter()
                            .map(|u| Mirror::new(u.to_string(), MirrorOrigin::User))
                            .collect(),
                        referrer: i.referrer.map(str::to_string),
                        cookies: i.cookies.map(str::to_string),
                        user_agent: i.user_agent.map(str::to_string),
                        ..Default::default()
                    })
                    .collect(),
            };
            serde_json::to_string_pretty(&list).map_err(|e| e.to_string())
        }
        _ => Err("Siler writes lists as text or Siler JSON.".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_text_finds_links() {
        let (f, items) = import(
            b"See https://a.example/x.zip, and (http://b.example/y.iso).\nftp://c/z\nhttps://a.example/x.zip",
        )
        .unwrap();
        assert_eq!(f, ListFormat::Text);
        let urls: Vec<_> = items.iter().map(|i| i.url.as_str()).collect();
        assert_eq!(urls, ["https://a.example/x.zip", "http://b.example/y.iso"]);
        assert!(import(b"nothing here").is_err());
    }

    #[test]
    fn idm_export() {
        let text = "<\r\nhttps://a.example/f.exe\r\nreferer: https://a.example/\r\nUser-Agent: Mozilla/5.0\r\ncookie: s=1\r\n>\r\n<\r\nhttp://b.example/g.zip\r\n>\r\n";
        let (f, items) = import(text.as_bytes()).unwrap();
        assert_eq!(f, ListFormat::Ef2);
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].referrer.as_deref(), Some("https://a.example/"));
        assert_eq!(items[0].user_agent.as_deref(), Some("Mozilla/5.0"));
        assert_eq!(items[0].cookies.as_deref(), Some("s=1"));
        assert_eq!(items[1].url, "http://b.example/g.zip");
    }

    #[test]
    fn json_round_trip() {
        let c = Checksum::parse("900150983cd24fb0d6963f7d28e17f72").unwrap();
        let dir = std::path::Path::new("/dl");
        let out = export(
            &[ExportItem {
                url: "https://a.example/f.bin",
                filename: Some("f.bin"),
                dir: Some(dir),
                queue: Some("Night"),
                size: Some(3),
                checksum: Some(&c),
                mirrors: vec!["https://m.example/f.bin"],
                referrer: None,
                cookies: None,
                user_agent: None,
            }],
            ListFormat::Json,
        )
        .unwrap();
        assert!(!out.contains("cookies"));
        let (f, items) = import(out.as_bytes()).unwrap();
        assert_eq!(f, ListFormat::Json);
        assert_eq!(items[0].filename.as_deref(), Some("f.bin"));
        assert_eq!(items[0].queue.as_deref(), Some("Night"));
        assert_eq!(items[0].checksum.as_ref(), Some(&c));
        assert_eq!(items[0].mirrors[0].origin, MirrorOrigin::Metalink);
        let text = export(
            &[ExportItem {
                url: "https://a.example/f.bin",
                filename: None,
                dir: None,
                queue: None,
                size: None,
                checksum: None,
                mirrors: vec![],
                referrer: None,
                cookies: None,
                user_agent: None,
            }],
            ListFormat::Text,
        )
        .unwrap();
        assert_eq!(text, "https://a.example/f.bin\n");
        assert!(import(br#"{"siler":2,"downloads":[]}"#).is_err());
    }

    #[test]
    fn metalink_import() {
        let (f, items) = import(
            br#"<metalink xmlns="urn:ietf:params:xml:ns:metalink"><file name="a/b.iso"><url priority="1">https://m1/b.iso</url><url priority="2">https://m2/b.iso</url></file></metalink>"#,
        )
        .unwrap();
        assert_eq!(f, ListFormat::Metalink);
        assert_eq!(items[0].url, "https://m1/b.iso");
        assert_eq!(items[0].folders, ["a"]);
        assert_eq!(items[0].mirrors.len(), 1);
    }
}
