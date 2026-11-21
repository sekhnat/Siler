//! The parsed form of a media source: what it offers, and how each track is laid out.
//!
//! A [`Presentation`] lists the video choices ([`Variant`]s), the audio and subtitle
//! tracks, and per track a [`SegmentPlan`]. Tracks carry a [`TrackId`] derived from the
//! manifest's own identity (never from a URL), so a selection survives signed URLs
//! expiring and being replaced.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fmt::Write as _;
use url::Url;

use crate::error::ParseError;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ManifestKind {
    Hls,
    Dash,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Liveness {
    Vod,
    Live,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrackKind {
    Video,
    Audio,
    Subtitle,
}

/// How the segments of a track are packaged. Decides how they are muxed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SegmentContainer {
    /// MPEG transport stream.
    Ts,
    /// Fragmented MP4 (CMAF, DASH ISO BMFF), with an initialisation segment.
    Fmp4,
    /// WebM (Matroska), with an initialisation segment.
    Webm,
    Aac,
    Mp3,
    Ac3,
    Vtt,
    Ttml,
    Unknown,
}

impl SegmentContainer {
    /// The file extension for a single-track file in this container.
    pub fn extension(self) -> &'static str {
        match self {
            SegmentContainer::Ts => "ts",
            SegmentContainer::Fmp4 => "mp4",
            SegmentContainer::Webm => "webm",
            SegmentContainer::Aac => "aac",
            SegmentContainer::Mp3 => "mp3",
            SegmentContainer::Ac3 => "ac3",
            SegmentContainer::Vtt => "vtt",
            SegmentContainer::Ttml => "ttml",
            SegmentContainer::Unknown => "bin",
        }
    }
}

/// Whether a track's media is encrypted, and how.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Protection {
    Clear,
    /// AES-128 segment encryption: Siler decrypts it with the key the server gives.
    Aes128,
    /// Encryption Siler doesn't decrypt (SAMPLE-AES with the identity key format).
    Unsupported {
        method: String,
    },
    /// DRM: refused, never downloaded.
    Drm {
        systems: Vec<String>,
    },
}

impl Protection {
    fn rank(&self) -> u8 {
        match self {
            Protection::Clear => 0,
            Protection::Aes128 => 1,
            Protection::Unsupported { .. } => 2,
            Protection::Drm { .. } => 3,
        }
    }

    /// The more restrictive of two protections; DRM systems are merged.
    pub fn worst(self, other: Protection) -> Protection {
        match (self, other) {
            (Protection::Drm { systems: mut a }, Protection::Drm { systems: b }) => {
                for s in b {
                    if !a.contains(&s) {
                        a.push(s);
                    }
                }
                Protection::Drm { systems: a }
            }
            (a, b) => {
                if b.rank() > a.rank() {
                    b
                } else {
                    a
                }
            }
        }
    }

    pub fn is_drm(&self) -> bool {
        matches!(self, Protection::Drm { .. })
    }

    /// Siler can download this track.
    pub fn downloadable(&self) -> bool {
        matches!(self, Protection::Clear | Protection::Aes128)
    }
}

/// A stable identifier for a track: a hash of the manifest's identity for it. Query
/// strings are never part of it.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct TrackId(String);

impl TrackId {
    /// The id for a track with the given identity parts.
    pub fn of(parts: &[&str]) -> TrackId {
        let mut h = Sha256::new();
        for p in parts {
            h.update(p.as_bytes());
            h.update([0x1f]);
        }
        let digest = h.finalize();
        let mut s = String::with_capacity(32);
        for b in &digest[..16] {
            let _ = write!(s, "{b:02x}");
        }
        TrackId(s)
    }

    /// Parses an id received from the UI or from storage. Anything but 32 lower-case hex
    /// digits is refused.
    pub fn parse(s: &str) -> Option<TrackId> {
        let ok = s.len() == 32
            && s.bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
        ok.then(|| TrackId(s.to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A byte range inside a resource (`EXT-X-BYTERANGE`, DASH `mediaRange`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ByteRange {
    pub offset: u64,
    pub length: u64,
}

/// One fetchable resource: a URL, optionally a byte range of it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Object {
    pub url: Url,
    pub range: Option<ByteRange>,
}

/// An AES-128 key for the segments that follow it. `iv: None` means the sequence-derived
/// IV (see `crypto::iv_from_sequence`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Key {
    pub url: Url,
    pub iv: Option<[u8; 16]>,
}

/// An initialisation segment, and the key it is encrypted with (HLS: the `EXT-X-KEY` in
/// effect where its `EXT-X-MAP` appears). The key's IV is always set for an init.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Init {
    pub object: Object,
    pub key: Option<Key>,
}

impl Init {
    /// An initialisation segment that isn't encrypted.
    pub fn plain(object: Object) -> Init {
        Init { object, key: None }
    }
}

/// One media segment of a track.
#[derive(Debug, Clone, PartialEq)]
pub struct Segment {
    /// Sequence number within the track: the HLS media sequence, or the position for DASH.
    pub seq: u64,
    pub object: Object,
    /// Index into [`SegmentPlan::inits`] of the initialisation segment this one needs.
    pub init: Option<usize>,
    pub key: Option<Key>,
    /// Discontinuity counter (segments with a different value start a new stream layout).
    pub discontinuity: u64,
    /// Presentation time of the segment's start, in seconds.
    pub start: f64,
    /// Duration in seconds.
    pub duration: f64,
}

/// The ordered objects that make up a track.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct SegmentPlan {
    pub inits: Vec<Init>,
    pub segments: Vec<Segment>,
    /// The track is one file (DASH `SegmentBase`, or a plain URL): fetched whole, resumable.
    pub whole: bool,
}

impl SegmentPlan {
    /// The total duration of the segments, in seconds.
    pub fn duration(&self) -> f64 {
        self.segments.iter().map(|s| s.duration).sum()
    }

    /// Adds the segments of `other` after these (a DASH track continuing into the next
    /// period). Initialisation segments are shared when they are equal.
    pub(crate) fn append(&mut self, other: SegmentPlan) {
        let mut map = Vec::with_capacity(other.inits.len());
        for init in other.inits {
            let idx = match self.inits.iter().position(|i| *i == init) {
                Some(i) => i,
                None => {
                    self.inits.push(init);
                    self.inits.len() - 1
                }
            };
            map.push(idx);
        }
        for mut seg in other.segments {
            seg.init = seg.init.and_then(|i| map.get(i).copied());
            self.segments.push(seg);
        }
        self.whole |= other.whole;
    }
}

/// One track of a presentation.
#[derive(Debug, Clone, PartialEq)]
pub struct Track {
    pub id: TrackId,
    pub kind: TrackKind,
    pub lang: Option<String>,
    pub name: Option<String>,
    pub codecs: Option<String>,
    pub container: SegmentContainer,
    pub bandwidth: Option<u64>,
    pub resolution: Option<(u32, u32)>,
    pub channels: Option<u32>,
    /// The manifest marks this track as the default (HLS `DEFAULT=YES`, DASH `Role=main`).
    pub is_default: bool,
    pub protection: Protection,
    pub plan: SegmentPlan,
}

impl Track {
    /// Estimated size in bytes: bandwidth × duration. `None` when the manifest gives no
    /// bandwidth for the track.
    pub fn estimated_bytes(&self) -> Option<u64> {
        let bw = self.bandwidth?;
        let secs = self.plan.duration();
        Some((bw as f64 / 8.0 * secs).round() as u64)
    }
}

/// A video choice: one video track, and the audio and subtitle tracks that go with it.
#[derive(Debug, Clone, PartialEq)]
pub struct Variant {
    pub video: Track,
    /// Audio tracks that go with this video: an HLS audio group, or every DASH audio track.
    pub audio: Vec<TrackId>,
    /// Subtitle tracks that go with this video: an HLS subtitle group.
    pub subtitles: Vec<TrackId>,
    /// The audio is inside the video stream (an HLS variant without an audio group).
    pub muxed_audio: bool,
}

/// What a source offers.
#[derive(Debug, Clone, PartialEq)]
pub struct Presentation {
    pub kind: ManifestKind,
    pub liveness: Liveness,
    /// Total length in seconds, when the manifest says.
    pub duration: Option<f64>,
    pub title: Option<String>,
    pub variants: Vec<Variant>,
    pub audio: Vec<Track>,
    pub subtitles: Vec<Track>,
    /// Things the parser skipped or could not use, for the user.
    pub warnings: Vec<String>,
    /// Tracks refused because they are DRM-protected.
    pub dropped_drm: usize,
}

impl Presentation {
    /// Finds a track of any kind by id.
    pub fn track(&self, id: &TrackId) -> Option<&Track> {
        self.variants
            .iter()
            .map(|v| &v.video)
            .chain(self.audio.iter())
            .chain(self.subtitles.iter())
            .find(|t| &t.id == id)
    }

    pub fn variant(&self, video: &TrackId) -> Option<&Variant> {
        self.variants.iter().find(|v| &v.video.id == video)
    }

    pub fn is_live(&self) -> bool {
        self.liveness == Liveness::Live
    }

    /// Every track Siler would offer (video, audio and subtitles).
    pub fn all_tracks(&self) -> impl Iterator<Item = &Track> {
        self.variants
            .iter()
            .map(|v| &v.video)
            .chain(self.audio.iter())
            .chain(self.subtitles.iter())
    }
}

/// A whole WebVTT file to offer as a subtitle track, from outside the manifest (the browser
/// extension found it on the page).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubtitleFile {
    pub url: Url,
    pub lang: Option<String>,
    pub label: Option<String>,
}

impl Presentation {
    /// Adds whole WebVTT files as subtitle tracks that go with every video. A file already
    /// offered (same address without its query, language and label) isn't added twice.
    pub fn add_subtitle_files(&mut self, files: &[SubtitleFile]) {
        let duration = self.duration.unwrap_or(0.0);
        for f in files {
            let lang = language(f.lang.as_deref());
            let label = f
                .label
                .as_deref()
                .map(str::trim)
                .filter(|l| !l.is_empty())
                .map(str::to_string);
            let id = TrackId::of(&[
                "file",
                "subtitle",
                &without_query(&f.url),
                lang.as_deref().unwrap_or(""),
                label.as_deref().unwrap_or(""),
            ]);
            if self.track(&id).is_some() {
                continue;
            }
            for v in &mut self.variants {
                v.subtitles.push(id.clone());
            }
            self.subtitles.push(Track {
                id,
                kind: TrackKind::Subtitle,
                lang,
                name: label,
                codecs: None,
                container: SegmentContainer::Vtt,
                bandwidth: None,
                resolution: None,
                channels: None,
                is_default: false,
                protection: Protection::Clear,
                plan: SegmentPlan {
                    inits: Vec::new(),
                    segments: vec![Segment {
                        seq: 0,
                        object: Object {
                            url: f.url.clone(),
                            range: None,
                        },
                        init: None,
                        key: None,
                        discontinuity: 0,
                        start: 0.0,
                        duration,
                    }],
                    whole: true,
                },
            });
        }
    }
}

/// Resolves `uri` against `base`. Refuses empty and unparseable references.
pub(crate) fn resolve(base: &Url, uri: &str) -> Result<Url, ParseError> {
    let uri = uri.trim();
    if uri.is_empty() {
        return Err(ParseError::invalid("a reference is empty"));
    }
    base.join(uri)
        .map_err(|_| ParseError::invalid("a reference can't be understood as a URL"))
}

/// A language tag from a manifest, or `None` for none: empty, or `und` (undetermined).
pub(crate) fn language(tag: Option<&str>) -> Option<String> {
    let t = tag?.trim();
    (!t.is_empty() && !t.eq_ignore_ascii_case("und")).then(|| t.to_string())
}

/// Whether a subtitle track can be downloaded: WebVTT or TTML files, or carried inside MP4
/// (`wvtt`, `stpp`). Subtitles inside WebM can't be yet.
pub(crate) fn subtitle_supported(t: &Track) -> bool {
    t.container != SegmentContainer::Webm
}

/// The warning for subtitles that were left out.
pub(crate) const SUBTITLES_IN_MP4: &str =
    "subtitles carried inside WebM aren't supported yet and were left out";

/// The URL without its query and fragment: the identity used for [`TrackId`]s and the
/// segment fingerprint, which must not change when signed query strings rotate.
pub fn without_query(url: &Url) -> String {
    let mut u = url.clone();
    u.set_query(None);
    u.set_fragment(None);
    u.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dash() -> Presentation {
        let mpd = br#"<MPD mediaPresentationDuration="PT10S"><Period><AdaptationSet mimeType="video/mp4" codecs="avc1"><Representation id="v1" bandwidth="1000" width="640" height="360"><BaseURL>v1.mp4</BaseURL><SegmentBase indexRange="0-9"/></Representation><Representation id="v2" bandwidth="2000" width="1280" height="720"><BaseURL>v2.mp4</BaseURL><SegmentBase indexRange="0-9"/></Representation></AdaptationSet></Period></MPD>"#;
        let base = Url::parse("https://example.com/a/").unwrap();
        crate::dash::parse(mpd, &base, &crate::ParseLimits::default()).unwrap()
    }

    #[test]
    fn subtitle_files_go_with_every_video() {
        let mut p = dash();
        let file = |q: &str, lang: &str, label: &str| SubtitleFile {
            url: Url::parse(&format!("https://example.com/api/timedtext?v=x&lang={q}")).unwrap(),
            lang: Some(lang.into()),
            label: Some(label.into()),
        };
        p.add_subtitle_files(&[
            file("en", "en", "English"),
            file("en-auto", "en", "English (auto-generated)"),
            file("de", "de", "Deutsch"),
        ]);
        // The same file again (a fresh query) isn't added twice.
        p.add_subtitle_files(&[file("en2", "en", "English")]);
        assert_eq!(p.subtitles.len(), 3);
        for v in &p.variants {
            assert_eq!(v.subtitles.len(), 3);
        }
        let t = &p.subtitles[0];
        assert!(t.plan.whole);
        assert_eq!(t.container, SegmentContainer::Vtt);
        assert_eq!(t.plan.duration(), 10.0);
        assert_eq!(t.name.as_deref(), Some("English"));
        let again = {
            let mut q = dash();
            q.add_subtitle_files(&[file("other", "en", "English")]);
            q.subtitles[0].id.clone()
        };
        assert_eq!(again, t.id, "ids don't depend on the query");
    }
}
