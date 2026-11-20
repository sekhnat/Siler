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
pub(crate) use probe::source;
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

/// The stored details of a media download. Never holds URLs, cookies or keys: the manifest
/// URL is the download's `url`, and the rest is re-read at every attempt.
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
    /// `resolving`, `downloading`, `rebuilding` or `publishing`.
    pub phase: &'static str,
    pub segments_done: u64,
    pub segments_total: u64,
    /// Rebuilding progress, in tenths of a percent.
    pub permille: u32,
}

impl Default for MediaProgress {
    fn default() -> Self {
        MediaProgress {
            phase: "resolving",
            segments_done: 0,
            segments_total: 0,
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

/// A short human label for a track: "1280×720 · 3.0 Mb/s", "English · 5.1", "Deutsch".
pub(crate) fn describe(t: &Track) -> String {
    let mut parts: Vec<String> = Vec::new();
    match t.kind {
        TrackKind::Video => {
            if let Some((w, h)) = t.resolution {
                parts.push(format!("{w}×{h}"));
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
        }
    }
    parts.join(" · ")
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
    let title = title
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map(str::to_string);
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

/// The file extension of a single-track output, or of the container a selection muxes into.
pub(crate) fn extension(output: Output, track: Option<&Track>) -> &'static str {
    match output {
        Output::Mp4 => "mp4",
        Output::Mkv => "mkv",
        Output::Original => track.map_or("bin", |t| t.container.extension()),
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
    }

    #[test]
    fn workdir_is_hidden_and_beside_the_output() {
        assert_eq!(
            workdir(Path::new("/d"), "a.mkv"),
            PathBuf::from("/d/.a.mkv.kstar.media")
        );
    }

    #[test]
    fn job_kind_round_trips() {
        assert_eq!(JobKind::parse(JobKind::Media.as_str()), JobKind::Media);
        assert_eq!(JobKind::parse("anything else"), JobKind::File);
    }
}
