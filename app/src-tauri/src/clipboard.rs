//! Clipboard monitoring: copying a link to a downloadable file offers it in the Add
//! window. On Wayland this reads the clipboard through the data-control protocol (KDE,
//! wlroots), which works without focus; elsewhere through X11.

use kstar_ipc::BrowserItem;
use std::collections::HashSet;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::time::Duration;
use tauri::Url;
use tauri::{AppHandle, Emitter, Manager};

use crate::AppState;
use crate::ipc_server::AddOptions;

/// Ignore clipboard text longer than this (not a list of links).
const MAX_TEXT: usize = 64 * 1024;
const MAX_LINKS: usize = 100;

/// Extensions that make a copied link worth offering (lower case, without the dot).
pub fn extensions(s: &kstar_core::Settings) -> HashSet<String> {
    s.categories
        .iter()
        .flat_map(|c| c.extensions.iter().cloned())
        .chain(["m3u8", "mpd"].map(String::from))
        .collect()
}

/// The links to offer from copied text: the text must be nothing but http(s) links, one
/// per line (not a paragraph with a link in it), and only links to files with a known
/// extension count.
pub fn links_in(text: &str, exts: &HashSet<String>) -> Vec<String> {
    if text.len() > MAX_TEXT {
        return Vec::new();
    }
    let lines: Vec<&str> = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    if lines.is_empty() || lines.len() > MAX_LINKS {
        return Vec::new();
    }
    let mut out = Vec::new();
    for line in lines {
        if line.contains(char::is_whitespace) {
            return Vec::new();
        }
        let Ok(url) = Url::parse(line) else {
            return Vec::new();
        };
        if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
            return Vec::new();
        }
        let name = url
            .path_segments()
            .and_then(|mut s| s.next_back())
            .map(|n| {
                percent_encoding::percent_decode_str(n)
                    .decode_utf8_lossy()
                    .to_lowercase()
            })
            .unwrap_or_default();
        let known = exts.iter().any(|e| {
            name.len() > e.len() + 1
                && name.ends_with(e.as_str())
                && name.as_bytes()[name.len() - e.len() - 1] == b'.'
        });
        if known && !out.iter().any(|u| u == line) {
            out.push(line.to_string());
        }
    }
    out
}

fn hash(text: &str) -> u64 {
    let mut h = DefaultHasher::new();
    text.hash(&mut h);
    h.finish()
}

/// One of K-Star's windows is on screen and focused (a copy is then K-Star's own, like
/// "Copy link"). Hidden windows can report focus, so visibility is checked too.
fn has_focus(app: &AppHandle) -> bool {
    app.webview_windows()
        .values()
        .any(|w| w.is_visible().unwrap_or(false) && w.is_focused().unwrap_or(false))
}

fn offer(app: &AppHandle, links: Vec<String>) {
    let Some(state) = app.try_state::<AppState>() else {
        return;
    };
    tracing::info!(count = links.len(), "links copied to the clipboard");
    let items = links
        .into_iter()
        .map(|url| BrowserItem {
            url,
            ..Default::default()
        })
        .collect();
    state.push_pending(
        items,
        &AddOptions {
            origin: crate::ipc_server::Origin::Clipboard,
            ..Default::default()
        },
    );
    let handle = app.clone();
    let _ = app.run_on_main_thread(move || {
        let _ = handle.emit("pending-changed", ());
        crate::open_add_window(&handle);
    });
}

/// Watches the clipboard while the setting is on.
pub fn spawn(app: AppHandle) {
    std::thread::Builder::new()
        .name("clipboard".into())
        .spawn(move || {
            let mut clip: Option<arboard::Clipboard> = None;
            let mut last: Option<u64> = None;
            let mut warned = false;
            loop {
                std::thread::sleep(Duration::from_secs(1));
                let Some(state) = app.try_state::<AppState>() else {
                    continue;
                };
                let settings = state.engine.settings();
                if !settings.clipboard_monitor {
                    // Whatever is copied while it's off isn't offered when it's turned on.
                    clip = None;
                    last = None;
                    continue;
                }
                if clip.is_none() {
                    match arboard::Clipboard::new() {
                        Ok(c) => clip = Some(c),
                        Err(e) => {
                            if !warned {
                                tracing::warn!("can't read the clipboard: {e}");
                                warned = true;
                            }
                            std::thread::sleep(Duration::from_secs(30));
                            continue;
                        }
                    }
                }
                let Some(c) = clip.as_mut() else { continue };
                let text = match c.get_text() {
                    Ok(t) => t,
                    Err(arboard::Error::ContentNotAvailable) => String::new(),
                    Err(e) => {
                        tracing::debug!("clipboard read failed: {e}");
                        clip = None;
                        continue;
                    }
                };
                let h = hash(&text);
                let first = last.is_none();
                if last.replace(h) == Some(h) || first {
                    continue;
                }
                // Copied inside K-Star ("Copy link"): not a new download.
                if has_focus(&app) {
                    tracing::debug!("clipboard changed while K-Star has the focus");
                    continue;
                }
                let links = links_in(&text, &extensions(&settings));
                tracing::debug!(len = text.len(), links = links.len(), "clipboard changed");
                if !links.is_empty() {
                    offer(&app, links);
                }
            }
        })
        .expect("spawn the clipboard thread");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn copied_links() {
        let exts: HashSet<String> = ["iso", "zip", "tar.gz", "mkv"]
            .into_iter()
            .map(String::from)
            .collect();
        assert_eq!(
            links_in("  https://x.org/a/Debian.ISO \n", &exts),
            ["https://x.org/a/Debian.ISO"]
        );
        assert_eq!(
            links_in(
                "https://x.org/a.zip\n\nhttp://y.org/src.tar.gz?sig=1\nhttps://x.org/page.html\nhttps://x.org/a.zip",
                &exts
            ),
            ["https://x.org/a.zip", "http://y.org/src.tar.gz?sig=1"]
        );
        assert_eq!(
            links_in("https://x.org/My%20Film.mkv", &exts),
            ["https://x.org/My%20Film.mkv"]
        );
        // Prose, other schemes and pages aren't offered.
        assert!(links_in("see https://x.org/a.zip for details", &exts).is_empty());
        assert!(links_in("ftp://x.org/a.zip", &exts).is_empty());
        assert!(links_in("https://x.org/", &exts).is_empty());
        assert!(links_in("https://x.org/zip", &exts).is_empty());
        assert!(links_in("https://x.org/a.gz", &exts).is_empty());
        assert!(links_in("", &exts).is_empty());
        assert!(links_in(&"https://x.org/a.zip\n".repeat(200), &exts).is_empty());
    }
}
