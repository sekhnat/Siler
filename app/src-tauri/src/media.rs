//! Media in the application: where the FFmpeg that combines streams is found, and which
//! links are streams (HLS and DASH manifests) rather than files.

use std::path::{Path, PathBuf};
use tauri::{AppHandle, Manager};

/// The FFmpeg to combine media with: `$SILER_FFMPEG` when set, the copy bundled with the
/// application, one next to the executable, (in a development build) the one in
/// `resources/`, or the first on `PATH`.
pub fn find_ffmpeg(app: &AppHandle) -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("SILER_FFMPEG").filter(|p| !p.is_empty()) {
        return Some(PathBuf::from(p));
    }
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Ok(dir) = app.path().resource_dir() {
        candidates.push(dir.join("ffmpeg"));
    }
    if let Some(dir) = std::env::current_exe()
        .ok()
        .as_deref()
        .and_then(Path::parent)
    {
        candidates.push(dir.join("ffmpeg"));
    }
    // A packaged install (AUR) keeps Siler's FFmpeg out of PATH.
    candidates.push(PathBuf::from("/usr/lib/siler/ffmpeg"));
    if cfg!(debug_assertions) {
        candidates.push(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("resources/ffmpeg"));
    }
    if let Some(found) = candidates.into_iter().find(|p| p.is_file()) {
        return Some(found);
    }
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join("ffmpeg"))
        .find(|p| p.is_file())
}

/// Whether a link is a stream: an HLS, DASH or HDS manifest (by its path, ignoring the query
/// string), or an RTMP or MMS address.
pub fn is_stream(url: &str) -> bool {
    let path = url
        .trim()
        .split(['?', '#'])
        .next()
        .unwrap_or("")
        .to_ascii_lowercase();
    path.ends_with(".m3u8")
        || path.ends_with(".mpd")
        || path.ends_with(".f4m")
        || siler_core::media::stream::is_stream_url(url)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn streams_by_path() {
        assert!(is_stream("https://x/a/master.m3u8?token=1"));
        assert!(is_stream("https://x/a/index.MPD#t=1"));
        assert!(!is_stream("https://x/a/movie.mp4"));
        assert!(!is_stream("https://x/m3u8/page.html"));
    }
}
