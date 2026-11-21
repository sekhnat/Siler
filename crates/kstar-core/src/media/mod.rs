//! Media downloads. An adaptive stream (HLS or DASH) is resolved, the segments of the
//! selected tracks are fetched into a Work Directory, and they are rebuilt into one file
//! with the bundled FFmpeg. A media download is a record like any other: it is queued,
//! paused, resumed, removed and published the same way (media-design.md).

mod ffmpeg;
mod probe;
mod reconstruct;
mod run;

use reqwest::Url;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

use kstar_media::classify::Kind;
use kstar_media::select::{self, ContainerChoice, Output, Preferences, Selection, SubtitleMode};
use kstar_media::{ManifestKind, Presentation, SegmentContainer, Track, TrackId, TrackKind};

use crate::RequestMeta;
use crate::error::MediaError;
use crate::{Error, OnConflict, Result};

pub use ffmpeg::FfmpegStatus;
pub(crate) use ffmpeg::probe as probe_ffmpeg;
pub(crate) use probe::resolve;
pub(crate) use run::drive;

/// What a download is made of.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobKind {
    File,
    Media,
}

impl JobKind {
    pub fn as_str(self) -> &'static str {
        match self {
            JobKind::File => "file",
            JobKind::Media => "media",
        }
    }

    pub fn parse(s: &str) -> JobKind {
        match s {
            "media" => JobKind::Media,
            _ => JobKind::File,
        }
    }
}

/// The stored details of a media download. Holds no cookies or keys: the manifest URL is the
/// download's `url`, and the rest is re-read at every attempt. A manifest the page gave as
/// text (in `hint`) can't be read again, so it is kept.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct MediaRecord {
    pub manifest: ManifestKind,
    pub selection: Selection,
    pub container: ContainerChoice,
    pub output: Output,
    /// Why this container was chosen, in words.
    pub reason: String,
    pub subtitles: SubtitleMode,
    pub title: Option<String>,
    /// The chosen tracks, described for the UI.
    pub labels: Vec<String>,
    /// Hash of the chosen tracks' segments; a changed one needs a restart.
    pub fingerprint: Option<String>,
    /// Subtitle files written beside the output.
    pub sidecars: Vec<String>,
    /// What the browser extension said about the video, when it came from there.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hint: Option<SourceHint>,
}

/// What the browser extension found out about a video on a page: things the app can't see
/// from the URL alone. The same shape as the extension's `media` hint (kstar-ipc).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SourceHint {
    /// The recogniser that found it (`youtube`, `vimeo`, …).
    pub site: Option<String>,
    pub title: Option<String>,
    /// A manifest the page gave as text instead of a URL.
    pub manifest: Option<InlineManifest>,
    /// Whole WebVTT files to offer as subtitle tracks.
    pub subtitles: Vec<HintSubtitle>,
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
pub struct HintSubtitle {
    pub url: String,
    pub lang: Option<String>,
    pub label: Option<String>,
}

/// At most this many subtitle files are taken from a hint.
const MAX_HINT_SUBTITLES: usize = 64;
/// Ranged fetching stays within these bounds, whatever a hint asks for.
const MIN_CHUNK: u64 = 16 << 10;
const MAX_CHUNK: u64 = 256 << 20;
/// A start delay further away than this is ignored.
const MAX_WAIT_MS: u64 = 10 * 60 * 1000;

impl SourceHint {
    /// Checks a hint from the browser and drops what can't be used: it is untrusted input,
    /// like a pasted URL.
    pub(crate) fn checked(mut self, lim: &kstar_media::ParseLimits) -> Result<SourceHint> {
        if let Some(m) = &self.manifest {
            if !matches!(m.kind.as_str(), "hls" | "dash") {
                return Err(Error::Media(MediaError::Unsupported(format!(
                    "a manifest of kind “{}”",
                    m.kind
                ))));
            }
            if m.body.len() > lim.max_manifest_bytes {
                return Err(Error::Media(MediaError::BodyTooLarge(
                    lim.max_manifest_bytes as u64,
                )));
            }
            let base = Url::parse(&m.base_url).map_err(|e| Error::InvalidUrl(e.to_string()))?;
            if !matches!(base.scheme(), "http" | "https") {
                return Err(Error::InvalidUrl(
                    "only http and https pages are accepted".into(),
                ));
            }
        }
        self.subtitles
            .retain(|s| Url::parse(&s.url).is_ok_and(|u| matches!(u.scheme(), "http" | "https")));
        self.subtitles.truncate(MAX_HINT_SUBTITLES);
        self.chunk_bytes = self.chunk_bytes.map(|c| c.clamp(MIN_CHUNK, MAX_CHUNK));
        let now = crate::manager::now_ms() as u64;
        self.not_before_ms = self
            .not_before_ms
            .filter(|t| *t > now && *t - now <= MAX_WAIT_MS);
        self.title = self
            .title
            .map(|t| t.trim().chars().take(300).collect::<String>())
            .filter(|t| !t.is_empty());
        Ok(self)
    }

    /// The subtitle files as the presentation takes them.
    pub(crate) fn subtitle_files(&self) -> Vec<kstar_media::SubtitleFile> {
        self.subtitles
            .iter()
            .filter_map(|s| {
                Some(kstar_media::SubtitleFile {
                    url: Url::parse(&s.url).ok()?,
                    lang: s.lang.clone(),
                    label: s.label.clone(),
                })
            })
            .collect()
    }
}

/// The media details shown in the list and the detail panel.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct MediaInfo {
    pub manifest: ManifestKind,
    pub output: Output,
    pub reason: String,
    pub subtitles: SubtitleMode,
    pub labels: Vec<String>,
    pub sidecars: Vec<String>,
}

/// Where a media download is, for its progress line.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct MediaProgress {
    /// `waiting`, `resolving`, `downloading`, `rebuilding` or `publishing`.
    pub phase: &'static str,
    pub segments_done: u64,
    pub segments_total: u64,
    /// The bytes the pieces add up to, once every piece's size is known (whole files learn
    /// theirs from the first answer; other pieces only when the manifest gives a range).
    pub bytes_total: Option<u64>,
    /// Rebuilding progress, in tenths of a percent.
    pub permille: u32,
}

impl Default for MediaProgress {
    fn default() -> Self {
        MediaProgress {
            phase: "resolving",
            segments_done: 0,
            segments_total: 0,
            bytes_total: None,
            permille: 0,
        }
    }
}

/// How the UI should treat a source.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MediaKind {
    /// A plain file: downloaded as a file.
    Direct,
    Hls,
    Dash,
    /// A web page or something else with no media in it.
    NotMedia,
    Live,
    Drm,
    /// Recognised, but not supported yet (HDS, generic playlists).
    Unsupported,
}

/// A track as the picker shows it.
#[derive(Debug, Clone, Serialize)]
pub struct TrackView {
    pub id: TrackId,
    pub kind: TrackKind,
    pub label: String,
    pub lang: Option<String>,
    pub codecs: Option<String>,
    pub bandwidth: Option<u64>,
    pub resolution: Option<(u32, u32)>,
    pub channels: Option<u32>,
    pub estimated_bytes: Option<u64>,
    pub is_default: bool,
    pub container: SegmentContainer,
}

/// A video choice with the audio and subtitle tracks that go with it.
#[derive(Debug, Clone, Serialize)]
pub struct VariantView {
    pub video: TrackView,
    pub audio: Vec<TrackId>,
    pub subtitles: Vec<TrackId>,
    pub muxed_audio: bool,
}

/// What a probe found. Holds no URLs.
#[derive(Debug, Clone, Serialize)]
pub struct MediaProbe {
    pub kind: MediaKind,
    /// What to tell the user when nothing can be downloaded.
    pub message: Option<String>,
    pub title: Option<String>,
    pub duration: Option<f64>,
    pub variants: Vec<VariantView>,
    pub audio: Vec<TrackView>,
    pub subtitles: Vec<TrackView>,
    pub default: Selection,
    pub warnings: Vec<String>,
    pub dropped_drm: usize,
    /// Whether FFmpeg is available to combine several tracks.
    pub ffmpeg: bool,
}

/// A media download to add.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct NewMediaDownload {
    pub url: String,
    pub meta: RequestMeta,
    /// The output name; worked out from the title or the URL when absent.
    pub filename: Option<String>,
    pub dir: Option<PathBuf>,
    /// The tracks to download; the default selection when absent.
    pub selection: Option<Selection>,
    /// The container; the setting when absent.
    pub container: Option<ContainerChoice>,
    /// The subtitle mode; the setting when absent.
    pub subtitles: Option<SubtitleMode>,
    pub start_paused: bool,
    pub on_conflict: Option<OnConflict>,
    pub queue_id: Option<i64>,
    /// What the browser extension found out about the video.
    pub hint: Option<SourceHint>,
}

/// A track's view for the UI.
fn track_view(t: &Track) -> TrackView {
    TrackView {
        id: t.id.clone(),
        kind: t.kind,
        label: describe(t),
        lang: t.lang.clone(),
        codecs: t.codecs.clone(),
        bandwidth: t.bandwidth,
        resolution: t.resolution,
        channels: t.channels,
        estimated_bytes: t.estimated_bytes(),
        is_default: t.is_default,
        container: t.container,
    }
}

/// A short human label for a track: "1280×720 · H.264 · 3.0 Mb/s", "English · 5.1 · AAC",
/// "Deutsch". The codec tells apart choices that are otherwise alike (YouTube offers each
/// size as H.264, VP9 and AV1).
pub(crate) fn describe(t: &Track) -> String {
    let mut parts: Vec<String> = Vec::new();
    let codec = t.codecs.as_deref().and_then(|c| codec_name(c, t.kind));
    match t.kind {
        TrackKind::Video => {
            if let Some((w, h)) = t.resolution {
                parts.push(format!("{w}×{h}"));
            }
            if let Some(c) = codec {
                parts.push(c.into());
            }
            if let Some(bw) = t.bandwidth {
                parts.push(format!("{:.1} Mb/s", bw as f64 / 1e6));
            }
            if parts.is_empty() {
                parts.push("Video".into());
            }
        }
        TrackKind::Audio | TrackKind::Subtitle => {
            let name = t
                .name
                .clone()
                .or_else(|| t.lang.as_deref().map(language_name))
                .unwrap_or_else(|| {
                    if t.kind == TrackKind::Audio {
                        "Audio".into()
                    } else {
                        "Subtitles".into()
                    }
                });
            parts.push(name);
            if let Some(c) = t.channels.filter(|c| *c > 0) {
                parts.push(match c {
                    1 => "mono".into(),
                    2 => "stereo".into(),
                    6 => "5.1".into(),
                    n => format!("{n} channels"),
                });
            }
            if t.kind == TrackKind::Audio {
                if let Some(c) = codec {
                    parts.push(c.into());
                }
                // A bandwidth of 1 marks "not stated" (a bare HLS playlist).
                if let Some(bw) = t.bandwidth.filter(|b| *b > 1) {
                    parts.push(format!("{} kb/s", (bw as f64 / 1000.0).round()));
                }
            }
        }
    }
    parts.join(" · ")
}

/// The everyday name of a track's codec, from an RFC 6381 `codecs` list ("avc1.640028,
/// mp4a.40.2"): the first one of the track's kind.
fn codec_name(codecs: &str, kind: TrackKind) -> Option<&'static str> {
    codecs.split(',').find_map(|c| {
        let c = c.trim().to_ascii_lowercase();
        let family = c.split('.').next().unwrap_or("");
        let video = match family {
            "avc1" | "avc3" => Some("H.264"),
            "hvc1" | "hev1" => Some("HEVC"),
            "vp09" | "vp9" => Some("VP9"),
            "vp08" | "vp8" => Some("VP8"),
            "av01" => Some("AV1"),
            _ => None,
        };
        let audio = match family {
            "mp4a" if c == "mp4a.40.34" || c == "mp4a.6b" => Some("MP3"),
            "mp4a" => Some("AAC"),
            "opus" => Some("Opus"),
            "vorbis" => Some("Vorbis"),
            "flac" | "fla" => Some("FLAC"),
            "ac-3" => Some("Dolby Digital"),
            "ec-3" => Some("Dolby Digital Plus"),
            "mp3" => Some("MP3"),
            _ => None,
        };
        match kind {
            TrackKind::Video => video,
            TrackKind::Audio => audio,
            TrackKind::Subtitle => None,
        }
    })
}

/// A few common language codes spelled out; anything else is shown as given.
fn language_name(code: &str) -> String {
    let base = code.split('-').next().unwrap_or(code).to_ascii_lowercase();
    match base.as_str() {
        "en" => "English".into(),
        "de" => "Deutsch".into(),
        "fr" => "Français".into(),
        "es" => "Español".into(),
        "it" => "Italiano".into(),
        "pt" => "Português".into(),
        "ru" => "Русский".into(),
        "ja" => "日本語".into(),
        "zh" => "中文".into(),
        "nl" => "Nederlands".into(),
        "pl" => "Polski".into(),
        "sv" => "Svenska".into(),
        _ => code.to_string(),
    }
}

/// The default choices from Settings.
pub(crate) fn preferences(s: &crate::Settings) -> Preferences {
    let lang = |l: &str| Some(l.to_string()).filter(|l| !l.is_empty());
    Preferences {
        max_height: s.preferred_max_height,
        audio_language: lang(&s.preferred_audio_language),
        subtitle_language: lang(&s.preferred_subtitle_language),
    }
}

/// The presentation of a source that is a stream K-Star can work with; otherwise why not.
pub(crate) fn presentation_or_error(src: probe::Source) -> Result<Presentation> {
    let kind = src.kind;
    let Some(pres) = src.presentation else {
        return Err(Error::Media(match kind {
            Kind::Hds | Kind::Playlist => {
                MediaError::Unsupported("this playlist format isn't supported yet".into())
            }
            _ => MediaError::NotMedia,
        }));
    };
    if pres.is_live() {
        return Err(Error::Media(MediaError::LiveUnsupported));
    }
    if pres.variants.is_empty() && pres.audio.is_empty() {
        return Err(Error::Media(if pres.dropped_drm > 0 {
            MediaError::Drm
        } else {
            MediaError::NotMedia
        }));
    }
    Ok(pres)
}

/// What a probe shows the user: the picker's choices, or why nothing can be downloaded.
pub(crate) fn probe_view(src: probe::Source, prefs: &Preferences, ffmpeg: bool) -> MediaProbe {
    let kind = src.kind;
    let mut view = MediaProbe {
        kind: MediaKind::NotMedia,
        message: None,
        title: None,
        duration: None,
        variants: Vec::new(),
        audio: Vec::new(),
        subtitles: Vec::new(),
        default: Selection::default(),
        warnings: Vec::new(),
        dropped_drm: 0,
        ffmpeg,
    };
    let Some(p) = src.presentation else {
        match kind {
            Kind::Direct | Kind::File | Kind::WebVtt => view.kind = MediaKind::Direct,
            Kind::Hds | Kind::Playlist => {
                view.kind = MediaKind::Unsupported;
                view.message = Some("This kind of playlist isn't supported yet.".into());
            }
            Kind::Html | Kind::Hls | Kind::Dash => {
                view.message = Some(
                    "This page doesn't contain a video or audio file K-Star can download. Videos on web pages need the K-Star browser extension."
                        .into(),
                );
            }
        }
        return view;
    };
    view.dropped_drm = p.dropped_drm;
    view.title = p.title.clone();
    view.duration = p.duration;
    view.warnings = p.warnings.clone();
    if p.is_live() {
        view.kind = MediaKind::Live;
        view.message = Some("This stream is live. K-Star can't record live streams yet.".into());
        return view;
    }
    if p.variants.is_empty() && p.audio.is_empty() {
        if p.dropped_drm > 0 {
            view.kind = MediaKind::Drm;
            view.message = Some(
                "This video is protected by DRM. K-Star doesn't download DRM-protected media."
                    .into(),
            );
        } else {
            view.message = Some("This stream has no video or audio K-Star can download.".into());
        }
        return view;
    }
    view.kind = match p.kind {
        kstar_media::ManifestKind::Hls => MediaKind::Hls,
        kstar_media::ManifestKind::Dash => MediaKind::Dash,
    };
    view.default = select::default_selection(&p, prefs);
    view.variants = p
        .variants
        .iter()
        .map(|v| VariantView {
            video: track_view(&v.video),
            audio: v.audio.clone(),
            subtitles: v.subtitles.clone(),
            muxed_audio: v.muxed_audio,
        })
        .collect();
    view.audio = p.audio.iter().map(track_view).collect();
    view.subtitles = p.subtitles.iter().map(track_view).collect();
    view
}

/// The folder a media download is assembled in, next to its output.
pub(crate) fn workdir(dir: &Path, name: &str) -> PathBuf {
    dir.join(format!(".{name}.kstar.media"))
}

/// Path segments that say nothing about the title: then the parent segment is used.
const GENERIC: &[&str] = &[
    "master",
    "index",
    "playlist",
    "manifest",
    "chunklist",
    "stream",
    "video",
    "audio",
    "vod",
    "hls",
    "dash",
    "main",
    "file",
    "out",
];

/// A name for the output when the user gave none: the stream's title, else its URL's name,
/// else the host. The extension is added and the whole name is sanitised by the caller.
pub(crate) fn default_name(title: Option<&str>, url: &Url) -> String {
    // A title is a name, not a path: its slashes become "⧸" (as yt-dlp writes them), so
    // "http://cnn.it/x | CNN" isn't cut down to "x | CNN" by the sanitiser.
    let title = title
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map(|t| t.replace(['/', '\\'], "⧸"));
    title
        .or_else(|| name_from_path(url))
        .unwrap_or_else(|| format!("{} video", url.host_str().unwrap_or("media")))
}

fn name_from_path(url: &Url) -> Option<String> {
    let segs: Vec<String> = url
        .path_segments()?
        .filter(|s| !s.is_empty())
        .map(|s| {
            percent_encoding::percent_decode_str(s)
                .decode_utf8_lossy()
                .into_owned()
        })
        .collect();
    let last = segs.last()?;
    let pick = if is_generic(last) {
        segs.get(segs.len().checked_sub(2)?)?
    } else {
        last
    };
    let stem = match pick.rsplit_once('.') {
        Some((stem, _)) if !stem.is_empty() => stem,
        _ => pick.as_str(),
    };
    (!stem.is_empty()).then(|| stem.to_string())
}

fn is_generic(segment: &str) -> bool {
    let stem = segment.rsplit_once('.').map_or(segment, |(s, _)| s);
    GENERIC.contains(&stem.to_ascii_lowercase().as_str())
}

/// Extensions a media output can have; a name given with one has it replaced by the
/// output's own.
const MEDIA_EXTENSIONS: &[&str] = &[
    "mp4", "mkv", "m4a", "m4v", "mov", "webm", "ts", "aac", "mp3", "ac3", "vtt", "ttml", "bin",
];

/// `name` without a media file extension.
pub(crate) fn strip_media_extension(name: &str) -> &str {
    match name.rsplit_once('.') {
        Some((stem, ext))
            if !stem.is_empty()
                && MEDIA_EXTENSIONS.contains(&ext.to_ascii_lowercase().as_str()) =>
        {
            stem
        }
        _ => name,
    }
}

/// The file extension of a single-track output, or of the container a selection muxes into.
pub(crate) fn extension(output: Output, track: Option<&Track>) -> &'static str {
    match output {
        Output::Mp4 => "mp4",
        Output::Mkv => "mkv",
        // A subtitle track on its own is written as one WebVTT file.
        Output::Original => track.map_or("bin", |t| {
            if t.kind == TrackKind::Subtitle {
                "vtt"
            } else {
                t.container.extension()
            }
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_come_from_the_title_or_the_url() {
        let u = |s: &str| Url::parse(s).expect("URL");
        assert_eq!(
            default_name(Some(" Dune: Part Two "), &u("https://x/a/master.m3u8")),
            "Dune: Part Two"
        );
        assert_eq!(
            default_name(None, &u("https://cdn.example/shows/Episode%201.m3u8")),
            "Episode 1"
        );
        assert_eq!(
            default_name(None, &u("https://cdn.example/shows/42/master.m3u8")),
            "42"
        );
        assert_eq!(
            default_name(None, &u("https://cdn.example/index.mpd")),
            "cdn.example video"
        );
        assert_eq!(
            default_name(None, &u("https://cdn.example/")),
            "cdn.example video"
        );
        assert_eq!(
            default_name(
                Some("Vote http://cnn.it/2eERh5f | CNN"),
                &u("https://x/a.mpd")
            ),
            "Vote http:⧸⧸cnn.it⧸2eERh5f | CNN"
        );
    }

    #[test]
    fn labels_name_the_codec_so_alike_choices_differ() {
        let t = |kind, codecs: &str, res, bw, lang: Option<&str>, ch| Track {
            id: TrackId::of(&[codecs]),
            kind,
            lang: lang.map(str::to_string),
            name: None,
            codecs: Some(codecs.into()),
            container: SegmentContainer::Fmp4,
            bandwidth: bw,
            resolution: res,
            channels: ch,
            is_default: false,
            protection: kstar_media::Protection::Clear,
            plan: Default::default(),
        };
        assert_eq!(
            describe(&t(
                TrackKind::Video,
                "avc1.640028,mp4a.40.2",
                Some((1920, 1080)),
                Some(3_000_000),
                None,
                None
            )),
            "1920×1080 · H.264 · 3.0 Mb/s"
        );
        assert_eq!(
            describe(&t(
                TrackKind::Video,
                "av01.0.08M.08",
                Some((1920, 1080)),
                Some(2_000_000),
                None,
                None
            )),
            "1920×1080 · AV1 · 2.0 Mb/s"
        );
        assert_eq!(
            describe(&t(
                TrackKind::Audio,
                "opus",
                None,
                Some(129_000),
                Some("en"),
                Some(2)
            )),
            "English · stereo · Opus · 129 kb/s"
        );
        assert_eq!(
            describe(&t(TrackKind::Audio, "mp4a.40.2", None, Some(1), None, None)),
            "Audio · AAC"
        );
        assert_eq!(
            describe(&t(
                TrackKind::Subtitle,
                "wvtt",
                None,
                None,
                Some("de"),
                None
            )),
            "Deutsch"
        );
    }

    #[test]
    fn a_given_media_extension_is_replaced() {
        assert_eq!(strip_media_extension("Movie.mp4"), "Movie");
        assert_eq!(strip_media_extension("Movie.MKV"), "Movie");
        assert_eq!(strip_media_extension("Dr. Who"), "Dr. Who");
        assert_eq!(strip_media_extension("talk.v2"), "talk.v2");
        assert_eq!(strip_media_extension(".mp4"), ".mp4");
    }

    #[test]
    fn workdir_is_hidden_and_beside_the_output() {
        assert_eq!(
            workdir(Path::new("/d"), "a.mkv"),
            PathBuf::from("/d/.a.mkv.kstar.media")
        );
    }

    #[test]
    fn hints_from_the_browser_are_checked() {
        let lim = kstar_media::ParseLimits::default();
        let now = crate::manager::now_ms() as u64;
        let dash = |kind: &str, base: &str| InlineManifest {
            kind: kind.into(),
            body: "<MPD/>".into(),
            base_url: base.into(),
        };
        let h = SourceHint {
            title: Some("  ".into()),
            manifest: Some(dash("dash", "https://www.youtube.com/watch?v=x")),
            subtitles: vec![
                HintSubtitle {
                    url: "file:///etc/passwd".into(),
                    ..Default::default()
                },
                HintSubtitle {
                    url: "https://x/a.vtt".into(),
                    ..Default::default()
                },
            ],
            chunk_bytes: Some(1),
            not_before_ms: Some(now + 3_600_000),
            ..Default::default()
        }
        .checked(&lim)
        .expect("accepted");
        assert_eq!(h.title, None);
        assert_eq!(h.subtitles.len(), 1, "only http(s) subtitles");
        assert_eq!(h.chunk_bytes, Some(MIN_CHUNK));
        assert_eq!(h.not_before_ms, None, "an hour's wait is ignored");
        let soon = SourceHint {
            not_before_ms: Some(now + 5_000),
            ..Default::default()
        };
        assert!(soon.checked(&lim).expect("ok").not_before_ms.is_some());
        for bad in [dash("smil", "https://x/"), dash("dash", "file:///x")] {
            let h = SourceHint {
                manifest: Some(bad),
                ..Default::default()
            };
            assert!(h.checked(&lim).is_err());
        }
    }

    #[test]
    fn job_kind_round_trips() {
        assert_eq!(JobKind::parse(JobKind::Media.as_str()), JobKind::Media);
        assert_eq!(JobKind::parse("anything else"), JobKind::File);
    }
}
