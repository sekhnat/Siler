//! What a source is, from its first bytes, its MIME type and its URL, in that order
//! (design §7.1). CDNs routinely mislabel manifests, so the content wins.

use crate::model::SegmentContainer;

/// The kind of a source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Hls,
    Dash,
    /// A recognised media file (MP4, MPEG-TS, Matroska, audio, …): downloaded as a file.
    Direct,
    /// Adobe HDS (`.f4m`): recognised, not supported yet.
    Hds,
    /// An M3U without HLS tags, PLS or ASX: recognised, not supported yet.
    Playlist,
    WebVtt,
    /// An HTML page: usually an expired token or a login page, not media.
    Html,
    /// Anything else: downloaded as a file.
    File,
}

/// Classifies a source. `head` is its first bytes (at least a few hundred), `content_type`
/// the response header, and `path` the URL's path (lower case is not required).
pub fn classify(head: &[u8], content_type: Option<&str>, path: &str) -> Kind {
    let body = strip_bom(head);
    let trimmed = skip_space(body);
    let lower_prefix = ascii_lower(&trimmed[..trimmed.len().min(512)]);

    if trimmed.starts_with(b"#EXTM3U") {
        return if has_hls_tags(&lower_prefix, trimmed) {
            Kind::Hls
        } else {
            Kind::Playlist
        };
    }
    if contains(&lower_prefix, b"<mpd") || contains(&lower_prefix, b"urn:mpeg:dash:schema:mpd") {
        return Kind::Dash;
    }
    if contains(&lower_prefix, b"<!doctype html") || contains(&lower_prefix, b"<html") {
        return Kind::Html;
    }
    if trimmed.starts_with(b"WEBVTT") {
        return Kind::WebVtt;
    }
    if contains(&lower_prefix, b"<f4m") || contains(&lower_prefix, b"urn:adobe:f4m") {
        return Kind::Hds;
    }
    if trimmed.starts_with(b"[playlist]")
        || trimmed.starts_with(b"[Playlist]")
        || contains(&lower_prefix, b"<asx")
    {
        return Kind::Playlist;
    }
    if is_media_bytes(body) {
        return Kind::Direct;
    }

    let mime = content_type
        .and_then(|c| c.split(';').next())
        .map(|c| c.trim().to_ascii_lowercase())
        .unwrap_or_default();
    match mime.as_str() {
        "application/vnd.apple.mpegurl"
        | "application/x-mpegurl"
        | "audio/mpegurl"
        | "audio/x-mpegurl" => {
            return Kind::Hls;
        }
        "application/dash+xml" => return Kind::Dash,
        "text/vtt" => return Kind::WebVtt,
        "application/f4m+xml" => return Kind::Hds,
        _ => {}
    }
    if mime.starts_with("video/") || mime.starts_with("audio/") {
        return Kind::Direct;
    }

    match extension(path).as_deref() {
        Some("m3u8") => Kind::Hls,
        Some("m3u") | Some("pls") | Some("asx") => Kind::Playlist,
        Some("mpd") => Kind::Dash,
        Some("f4m") => Kind::Hds,
        Some("vtt") => Kind::WebVtt,
        Some(e) if DIRECT_EXTENSIONS.contains(&e) => Kind::Direct,
        _ => Kind::File,
    }
}

/// Extensions of direct media files (lower case, without the dot).
pub const DIRECT_EXTENSIONS: &[&str] = &[
    "3gp", "aac", "asf", "avi", "f4v", "flv", "m4a", "m4v", "mkv", "mov", "mp3", "mp4", "mpeg",
    "mpg", "ogg", "ogv", "qt", "ra", "rm", "ts", "tsa", "tsv", "wav", "webm", "wma", "wmv",
];

/// The container of one downloaded segment, read from its bytes. `Unknown` for anything
/// that isn't one of the segment containers.
pub fn sniff_segment(bytes: &[u8]) -> SegmentContainer {
    if is_ts(bytes) {
        return SegmentContainer::Ts;
    }
    let b = skip_space(strip_bom(bytes));
    if b.len() >= 8 && matches!(&b[4..8], b"ftyp" | b"styp" | b"moof" | b"sidx") {
        return SegmentContainer::Fmp4;
    }
    if b.starts_with(b"WEBVTT") {
        return SegmentContainer::Vtt;
    }
    if b.starts_with(b"<?xml") || b.starts_with(b"<tt") {
        return SegmentContainer::Ttml;
    }
    if is_adts(b) {
        return SegmentContainer::Aac;
    }
    if b.starts_with(b"ID3") || is_mpeg_audio(b) {
        return SegmentContainer::Mp3;
    }
    SegmentContainer::Unknown
}

/// Whether a body looks like an HTML page (an expired link or a login screen).
pub fn looks_like_html(bytes: &[u8]) -> bool {
    let head = &bytes[..bytes.len().min(512)];
    let lower = ascii_lower(skip_space(strip_bom(head)));
    lower.starts_with(b"<!doctype html")
        || lower.starts_with(b"<html")
        || lower.starts_with(b"<head")
        || lower.starts_with(b"<body")
}

fn has_hls_tags(lower_prefix: &[u8], full: &[u8]) -> bool {
    let _ = lower_prefix;
    let text = String::from_utf8_lossy(&full[..full.len().min(64 * 1024)]);
    text.lines().any(|l| {
        let l = l.trim_start();
        l.starts_with("#EXT-X-") || l.starts_with("#EXTINF")
    })
}

fn is_media_bytes(b: &[u8]) -> bool {
    if b.len() >= 8 && matches!(&b[4..8], b"ftyp" | b"styp" | b"moov" | b"mdat") {
        return true;
    }
    if is_ts(b) {
        return true;
    }
    if b.starts_with(&[0x1a, 0x45, 0xdf, 0xa3]) || b.starts_with(b"FLV\x01") {
        return true;
    }
    if b.starts_with(b"ID3") || is_adts(b) {
        return true;
    }
    b.starts_with(b"OggS") || b.starts_with(b"RIFF") || b.starts_with(&[0x30, 0x26, 0xb2, 0x75])
}

/// MPEG-TS: sync byte 0x47 every 188 bytes, checked at three positions.
fn is_ts(b: &[u8]) -> bool {
    if b.len() < 3 * 188 + 1 {
        return false;
    }
    (0..3).all(|k| b[k * 188] == 0x47)
}

/// ADTS AAC: 12-bit sync word 0xFFF with layer bits zero.
fn is_adts(b: &[u8]) -> bool {
    b.len() >= 2 && b[0] == 0xff && (b[1] & 0xf6) == 0xf0
}

/// MPEG audio frame sync: 11 set bits and a valid layer.
fn is_mpeg_audio(b: &[u8]) -> bool {
    b.len() >= 2 && b[0] == 0xff && (b[1] & 0xe0) == 0xe0 && (b[1] & 0x06) != 0
}

fn strip_bom(b: &[u8]) -> &[u8] {
    b.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(b)
}

fn skip_space(b: &[u8]) -> &[u8] {
    let n = b.iter().take_while(|c| c.is_ascii_whitespace()).count();
    &b[n..]
}

fn ascii_lower(b: &[u8]) -> Vec<u8> {
    b.to_ascii_lowercase()
}

fn contains(hay: &[u8], needle: &[u8]) -> bool {
    hay.windows(needle.len()).any(|w| w == needle)
}

/// The lower-case extension of the last path segment.
pub(crate) fn extension(path: &str) -> Option<String> {
    let last = path.rsplit('/').next()?;
    let (_, ext) = last.rsplit_once('.')?;
    Some(ext.to_ascii_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ts_bytes() -> Vec<u8> {
        let mut v = vec![0u8; 3 * 188 + 1];
        for k in 0..3 {
            v[k * 188] = 0x47;
        }
        v
    }

    #[test]
    fn content_beats_mime_and_extension() {
        assert_eq!(
            classify(b"#EXTM3U\n#EXT-X-VERSION:3\n", Some("text/plain"), "/x.bin"),
            Kind::Hls
        );
        assert_eq!(
            classify(b"#EXTM3U\n#EXT-X-VERSION:3\n", Some("text/html"), "/x"),
            Kind::Hls
        );
        assert_eq!(
            classify(
                b"<html><body>expired</body></html>",
                Some("video/mp4"),
                "/a.mp4"
            ),
            Kind::Html
        );
        assert_eq!(
            classify(
                b"\xef\xbb\xbf  <?xml?><MPD xmlns=\"urn:mpeg:dash:schema:mpd:2011\">",
                None,
                "/x"
            ),
            Kind::Dash
        );
        assert_eq!(
            classify(b"WEBVTT\n\n", Some("application/octet-stream"), "/s"),
            Kind::WebVtt
        );
        assert_eq!(
            classify(&ts_bytes(), Some("text/plain"), "/s"),
            Kind::Direct
        );
        assert_eq!(
            classify(b"#EXTM3U\n#EXTINF:1,\na.ts\n", None, "/p"),
            Kind::Hls
        );
    }

    #[test]
    fn mime_then_extension() {
        assert_eq!(
            classify(
                b"?",
                Some("application/vnd.apple.mpegurl; charset=utf-8"),
                "/a"
            ),
            Kind::Hls
        );
        assert_eq!(
            classify(b"?", Some("application/dash+xml"), "/a"),
            Kind::Dash
        );
        assert_eq!(classify(b"?", Some("video/x-foo"), "/a"), Kind::Direct);
        assert_eq!(
            classify(b"?", Some("application/octet-stream"), "/a/Master.M3U8"),
            Kind::Hls
        );
        assert_eq!(classify(b"?", Some("text/plain"), "/a/x.mpd"), Kind::Dash);
        assert_eq!(classify(b"?", None, "/a/movie.mkv"), Kind::Direct);
        assert_eq!(classify(b"?", None, "/a/page.html"), Kind::File);
        assert_eq!(classify(b"?", None, "/a/Debian.iso"), Kind::File);
        assert_eq!(classify(b"?", None, "/a/list.m3u"), Kind::Playlist);
        assert_eq!(classify(b"?", None, "/a/x.f4m"), Kind::Hds);
    }

    #[test]
    fn m3u_without_hls_tags_is_a_playlist() {
        assert_eq!(
            classify(b"#EXTM3U\nhttp://x/a.mp3\n", None, "/p"),
            Kind::Playlist
        );
        assert_eq!(
            classify(b"[playlist]\nNumberOfEntries=1\n", None, "/p"),
            Kind::Playlist
        );
    }

    #[test]
    fn segment_sniffing() {
        assert_eq!(sniff_segment(&ts_bytes()), SegmentContainer::Ts);
        let mut boxed = vec![0, 0, 0, 24];
        boxed.extend_from_slice(b"ftypisom");
        assert_eq!(sniff_segment(&boxed), SegmentContainer::Fmp4);
        assert_eq!(sniff_segment(b"WEBVTT\n"), SegmentContainer::Vtt);
        assert_eq!(
            sniff_segment(&[0xff, 0xf1, 0x50, 0x80, 0, 0x1f]),
            SegmentContainer::Aac
        );
        assert_eq!(sniff_segment(b"ID3\x03\x00"), SegmentContainer::Mp3);
        assert_eq!(sniff_segment(b"<html>"), SegmentContainer::Unknown);
        assert!(looks_like_html(b"  <!DOCTYPE html>"));
        assert!(!looks_like_html(&ts_bytes()));
    }
}
