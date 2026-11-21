//! Choosing what to download: the default selection, binding a saved selection to a
//! presentation, the output container, size estimates, and the fingerprint that tells
//! whether downloaded segments still belong to the same content (design §7.6, §13.4).
//!
//! A selection is stored by stable [`TrackId`]s, never by URL. Binding it against a fresh
//! presentation (after a signed URL expired) finds the same tracks again.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fmt::Write as _;

use crate::model::{Presentation, Track, TrackId, TrackKind, Variant, without_query};

/// The tracks the user chose. `video` is `None` for an audio-only presentation.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Selection {
    pub video: Option<TrackId>,
    pub audio: Vec<TrackId>,
    pub subtitles: Vec<TrackId>,
}

/// The container the user asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContainerChoice {
    #[default]
    Auto,
    Mp4,
    Mkv,
    /// The segments' own container; only for a single track.
    Original,
}

/// What happens to subtitles.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SubtitleMode {
    /// Written inside the output file.
    #[default]
    Embed,
    /// Written beside the output file.
    Sidecar,
    None,
}

/// What the user prefers, from Settings.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Preferences {
    /// Highest video quality, as a height (360, 720, 1080…); 0 for no limit. Portrait
    /// videos are measured by their shorter side, so a 720×1280 phone video counts as 720p.
    pub max_height: u32,
    pub audio_language: Option<String>,
    pub subtitle_language: Option<String>,
}

/// The file the output is written as.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Output {
    Mp4,
    Mkv,
    /// The segments' container (a single track).
    Original,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SelectError {
    #[error("the chosen video is no longer offered by the server")]
    VideoGone,
    #[error("a chosen audio or subtitle track is no longer offered")]
    TrackGone,
    #[error("the file has no video or audio to download")]
    NothingToDownload,
    #[error("MP4 can't hold {0}; choose MKV or another container")]
    Mp4Codec(String),
    #[error("the original container only works for a single track")]
    OriginalNeedsOneTrack,
}

/// The tracks a selection refers to, found in a presentation.
#[derive(Debug)]
pub struct Resolved<'a> {
    pub variant: Option<&'a Variant>,
    pub video: Option<&'a Track>,
    pub audio: Vec<&'a Track>,
    pub subtitles: Vec<&'a Track>,
}

impl Resolved<'_> {
    /// Every downloaded track, video first.
    pub fn tracks(&self) -> Vec<&Track> {
        self.video
            .into_iter()
            .chain(self.audio.iter().copied())
            .chain(self.subtitles.iter().copied())
            .collect()
    }
}

/// The selection Siler makes when the user accepts the defaults.
pub fn default_selection(p: &Presentation, prefs: &Preferences) -> Selection {
    let usable = |v: &&Variant| v.video.protection.downloadable();
    let variant = pick_variant(p, prefs.max_height, usable);
    let video = variant.map(|v| v.video.id.clone());
    let audio_pool: Vec<&Track> = match variant {
        Some(v) => p.audio.iter().filter(|t| v.audio.contains(&t.id)).collect(),
        None => p.audio.iter().collect(),
    };
    let audio = pick_audio(&audio_pool, prefs.audio_language.as_deref())
        .map(|t| vec![t.id.clone()])
        .unwrap_or_default();
    let subtitles = match (variant, prefs.subtitle_language.as_deref()) {
        (Some(v), Some(lang)) => p
            .subtitles
            .iter()
            .filter(|t| v.subtitles.contains(&t.id))
            .find(|t| t.lang.as_deref().is_some_and(|l| lang_matches(l, lang)))
            .map(|t| vec![t.id.clone()])
            .unwrap_or_default(),
        _ => Vec::new(),
    };
    Selection {
        video,
        audio,
        subtitles,
    }
}

/// A video's quality as people name it ("720p"): the shorter side of its picture.
fn quality(v: &Variant) -> Option<u32> {
    v.video.resolution.map(|(w, h)| w.min(h))
}

/// The highest-bandwidth variant within the quality limit; without one that fits, the
/// lowest one.
fn pick_variant(
    p: &Presentation,
    max_height: u32,
    usable: impl Fn(&&Variant) -> bool,
) -> Option<&Variant> {
    let candidates: Vec<&Variant> = p.variants.iter().filter(usable).collect();
    let fits: Vec<&Variant> = candidates
        .iter()
        .copied()
        .filter(|v| max_height == 0 || quality(v).is_none_or(|q| q <= max_height))
        .collect();
    if !fits.is_empty() {
        fits.iter()
            .copied()
            .max_by_key(|v| v.video.bandwidth.unwrap_or(0))
    } else {
        candidates
            .iter()
            .min_by_key(|v| quality(v).unwrap_or(u32::MAX))
            .copied()
    }
}

/// The preferred language first, then the default track, then the highest bandwidth.
fn pick_audio<'a>(pool: &[&'a Track], lang: Option<&str>) -> Option<&'a Track> {
    let by_lang = lang.and_then(|l| {
        pool.iter()
            .copied()
            .find(|t| t.lang.as_deref().is_some_and(|x| lang_matches(x, l)))
    });
    by_lang
        .or_else(|| pool.iter().copied().find(|t| t.is_default))
        .or_else(|| pool.first().copied())
}

/// `en` matches `en` and `en-US`, case-insensitively.
fn lang_matches(track: &str, wanted: &str) -> bool {
    let t = track.to_ascii_lowercase();
    let w = wanted.trim().to_ascii_lowercase();
    !w.is_empty() && (t == w || t.starts_with(&format!("{w}-")))
}

/// Finds the selected tracks in `p`. Audio and subtitles must belong to the chosen video's
/// group (or the presentation, when there is no video). Refused tracks are not offered.
pub fn resolve<'a>(p: &'a Presentation, sel: &Selection) -> Result<Resolved<'a>, SelectError> {
    let variant = match &sel.video {
        Some(id) => Some(p.variant(id).ok_or(SelectError::VideoGone)?),
        None => None,
    };
    let video = variant.map(|v| &v.video);
    let mut audio = Vec::with_capacity(sel.audio.len());
    for id in &sel.audio {
        let allowed = variant.is_none_or(|v| v.audio.contains(id));
        let t = p
            .audio
            .iter()
            .find(|t| &t.id == id)
            .filter(|_| allowed)
            .ok_or(SelectError::TrackGone)?;
        audio.push(t);
    }
    let mut subtitles = Vec::with_capacity(sel.subtitles.len());
    for id in &sel.subtitles {
        let allowed = variant.is_none_or(|v| v.subtitles.contains(id));
        let t = p
            .subtitles
            .iter()
            .find(|t| &t.id == id)
            .filter(|_| allowed)
            .ok_or(SelectError::TrackGone)?;
        subtitles.push(t);
    }
    let r = Resolved {
        variant,
        video,
        audio,
        subtitles,
    };
    if r.tracks().iter().any(|t| !t.protection.downloadable()) {
        return Err(SelectError::TrackGone);
    }
    if r.tracks().is_empty() {
        return Err(SelectError::NothingToDownload);
    }
    Ok(r)
}

/// Estimated bytes for the selection: bandwidth × duration per track. Tracks without a
/// bandwidth (subtitles) add nothing. `None` when no track has an estimate.
pub fn estimate_bytes(r: &Resolved<'_>) -> Option<u64> {
    let estimates: Vec<u64> = r
        .tracks()
        .iter()
        .filter_map(|t| t.estimated_bytes())
        .collect();
    (!estimates.is_empty()).then(|| estimates.iter().sum())
}

/// A hash of the selected tracks and their segment identities (URLs without query strings,
/// byte ranges, sequence numbers, keys). Two attempts with the same fingerprint can share
/// downloaded segments; a changed one cannot.
pub fn fingerprint(r: &Resolved<'_>) -> String {
    let mut tracks = r.tracks();
    tracks.sort_by(|a, b| a.id.cmp(&b.id));
    let mut h = Sha256::new();
    for t in tracks {
        h.update(t.id.as_str().as_bytes());
        h.update([0x1e]);
        for init in &t.plan.inits {
            update_object(&mut h, &init.object.url, init.object.range);
            // Only an encrypted init adds its key, so a clear stream's fingerprint is the
            // same as in 0.5.0 and its downloads resume.
            if let Some(k) = &init.key {
                h.update([0x1f]);
                h.update(without_query(&k.url).as_bytes());
            }
        }
        h.update([0x1d]);
        for s in &t.plan.segments {
            h.update(s.seq.to_be_bytes());
            update_object(&mut h, &s.object.url, s.object.range);
            if let Some(k) = &s.key {
                h.update(without_query(&k.url).as_bytes());
            }
            h.update([0x1d]);
        }
    }
    let digest = h.finalize();
    let mut out = String::with_capacity(64);
    for b in digest {
        let _ = write!(out, "{b:02x}");
    }
    out
}

fn update_object(h: &mut Sha256, url: &url::Url, range: Option<crate::model::ByteRange>) {
    h.update(without_query(url).as_bytes());
    if let Some(r) = range {
        h.update(r.offset.to_be_bytes());
        h.update(r.length.to_be_bytes());
    }
    h.update([0x1f]);
}

/// Whether a codec (one entry of a `CODECS` list) can go in an MP4 file as-is.
pub fn mp4_codec_ok(codec: &str) -> bool {
    let c = codec.trim().to_ascii_lowercase();
    [
        "avc1", "avc3", "hvc1", "hev1", "av01", "mp4a.40.", "mp4a.6b", "mp4a.69", "ac-3", "ec-3",
    ]
    .iter()
    .any(|p| c.starts_with(p))
}

/// Subtitle codecs: converted by FFmpeg, so they don't decide the container.
fn is_text_codec(codec: &str) -> bool {
    let c = codec.trim().to_ascii_lowercase();
    c.starts_with("wvtt") || c.starts_with("stpp") || c.starts_with("tx3g")
}

/// The output container and why (design §13.4): `auto` is MP4 when every codec fits, and
/// MKV otherwise or when subtitles stay as they are.
pub fn resolve_container(
    choice: ContainerChoice,
    r: &Resolved<'_>,
    subtitles: SubtitleMode,
) -> Result<(Output, String), SelectError> {
    let tracks = r.tracks();
    match choice {
        ContainerChoice::Original => {
            if tracks.len() != 1 {
                return Err(SelectError::OriginalNeedsOneTrack);
            }
            Ok((Output::Original, "the track's own container".into()))
        }
        ContainerChoice::Mkv => Ok((Output::Mkv, "as you chose".into())),
        ContainerChoice::Mp4 => {
            for t in &tracks {
                if let Some(c) = offending_codec(t) {
                    return Err(SelectError::Mp4Codec(c));
                }
            }
            Ok((Output::Mp4, "as you chose".into()))
        }
        ContainerChoice::Auto => {
            for t in &tracks {
                if t.kind == TrackKind::Subtitle {
                    continue;
                }
                // An HLS audio rendition names no codecs: the variant's list covers it.
                let covered = t.kind == TrackKind::Audio
                    && r.video.and_then(|v| v.codecs.as_deref()).is_some_and(|c| {
                        c.split(',').any(|x| {
                            let x = x.trim().to_ascii_lowercase();
                            ["mp4a", "ac-3", "ec-3", "opus", "flac", "vorbis", "mp3"]
                                .iter()
                                .any(|p| x.starts_with(p))
                        })
                    });
                match t.codecs.as_deref() {
                    None if covered => {}
                    None => {
                        return Ok((
                            Output::Mkv,
                            "the codecs aren't listed, so MKV is safest".into(),
                        ));
                    }
                    Some(_) => {
                        if let Some(c) = offending_codec(t) {
                            return Ok((Output::Mkv, format!("{c} can't go in MP4")));
                        }
                    }
                }
            }
            if subtitles == SubtitleMode::Embed && !r.subtitles.is_empty() {
                return Ok((Output::Mkv, "MKV keeps the subtitles as they are".into()));
            }
            Ok((Output::Mp4, "every codec fits MP4".into()))
        }
    }
}

/// The first codec of a track that MP4 can't hold. Unknown codecs don't count here.
fn offending_codec(t: &Track) -> Option<String> {
    if t.kind == TrackKind::Subtitle {
        return None;
    }
    t.codecs
        .as_deref()?
        .split(',')
        .filter(|c| !c.trim().is_empty() && !is_text_codec(c))
        .find(|c| !mp4_codec_ok(c))
        .map(|c| codec_name(c.trim()))
}

fn codec_name(c: &str) -> String {
    let l = c.to_ascii_lowercase();
    if l.starts_with("opus") {
        "Opus audio".into()
    } else if l.starts_with("vp09") || l.starts_with("vp8") {
        "VP9 or VP8 video".into()
    } else if l.starts_with("vorbis") {
        "Vorbis audio".into()
    } else if l.starts_with("flac") {
        "FLAC audio".into()
    } else {
        format!("the codec {c}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{
        Init, Key, Liveness, ManifestKind, Object, Protection, Segment, SegmentContainer,
        SegmentPlan,
    };
    use url::Url;

    fn track(
        id: &str,
        kind: TrackKind,
        codecs: Option<&str>,
        bw: Option<u64>,
        res: Option<(u32, u32)>,
        lang: Option<&str>,
        default: bool,
    ) -> Track {
        let url = Url::parse(&format!("https://cdn.example/{id}/s.ts?sig=1")).expect("URL");
        Track {
            id: TrackId::of(&[id]),
            kind,
            lang: lang.map(str::to_string),
            name: None,
            codecs: codecs.map(str::to_string),
            container: SegmentContainer::Ts,
            bandwidth: bw,
            resolution: res,
            channels: None,
            is_default: default,
            protection: Protection::Clear,
            plan: SegmentPlan {
                inits: vec![],
                segments: vec![Segment {
                    seq: 0,
                    object: Object { url, range: None },
                    init: None,
                    key: None,
                    discontinuity: 0,
                    start: 0.0,
                    duration: 10.0,
                }],
                whole: false,
            },
        }
    }

    fn presentation() -> Presentation {
        let lo = track(
            "lo",
            TrackKind::Video,
            Some("avc1.42e01e,mp4a.40.2"),
            Some(500_000),
            Some((640, 360)),
            None,
            false,
        );
        let hi = track(
            "hi",
            TrackKind::Video,
            Some("avc1.640028,mp4a.40.2"),
            Some(3_000_000),
            Some((1920, 1080)),
            None,
            false,
        );
        let mid = track(
            "mid",
            TrackKind::Video,
            Some("avc1.4d401f"),
            Some(1_200_000),
            Some((1280, 720)),
            None,
            false,
        );
        let en = track(
            "en",
            TrackKind::Audio,
            Some("mp4a.40.2"),
            Some(128_000),
            None,
            Some("en"),
            true,
        );
        let de = track(
            "de",
            TrackKind::Audio,
            Some("opus"),
            Some(96_000),
            None,
            Some("de"),
            false,
        );
        let sub = track(
            "sub",
            TrackKind::Subtitle,
            Some("wvtt"),
            None,
            None,
            Some("en-US"),
            false,
        );
        let audio_ids = vec![en.id.clone(), de.id.clone()];
        let sub_ids = vec![sub.id.clone()];
        let variants = [lo, hi, mid]
            .into_iter()
            .map(|v| Variant {
                video: v,
                audio: audio_ids.clone(),
                subtitles: sub_ids.clone(),
                muxed_audio: false,
            })
            .collect();
        Presentation {
            kind: ManifestKind::Hls,
            liveness: Liveness::Vod,
            duration: Some(10.0),
            title: None,
            variants,
            audio: vec![en, de],
            subtitles: vec![sub],
            warnings: vec![],
            dropped_drm: 0,
        }
    }

    #[test]
    fn default_picks_the_best_variant_within_the_limit() {
        let p = presentation();
        let unlimited = default_selection(&p, &Preferences::default());
        assert_eq!(unlimited.video, Some(TrackId::of(&["hi"])));
        assert_eq!(unlimited.audio, vec![TrackId::of(&["en"])]);
        assert!(unlimited.subtitles.is_empty());

        let capped = default_selection(
            &p,
            &Preferences {
                max_height: 720,
                audio_language: Some("DE".into()),
                subtitle_language: Some("en".into()),
            },
        );
        assert_eq!(capped.video, Some(TrackId::of(&["mid"])));
        assert_eq!(capped.audio, vec![TrackId::of(&["de"])]);
        assert_eq!(capped.subtitles, vec![TrackId::of(&["sub"])]);

        // Nothing fits under 100 p: the lowest is used.
        let tiny = default_selection(
            &p,
            &Preferences {
                max_height: 100,
                ..Default::default()
            },
        );
        assert_eq!(tiny.video, Some(TrackId::of(&["lo"])));
    }

    #[test]
    fn portrait_videos_are_measured_by_their_shorter_side() {
        let mut p = presentation();
        // The same ladder turned upright, like a phone video.
        for v in &mut p.variants {
            v.video.resolution = v.video.resolution.map(|(w, h)| (h, w));
        }
        let capped = default_selection(
            &p,
            &Preferences {
                max_height: 720,
                ..Default::default()
            },
        );
        assert_eq!(capped.video, Some(TrackId::of(&["mid"])));
    }

    #[test]
    fn selections_survive_signed_url_rotation() {
        let p = presentation();
        let sel = default_selection(&p, &Preferences::default());
        let r = resolve(&p, &sel).expect("resolves");
        let fp = fingerprint(&r);
        // Same tracks, new query strings: same fingerprint.
        let mut rotated = presentation();
        for v in &mut rotated.variants {
            for s in &mut v.video.plan.segments {
                s.object.url.set_query(Some("sig=new&exp=99"));
            }
        }
        let r2 = resolve(&rotated, &sel).expect("resolves");
        assert_eq!(fingerprint(&r2), fp);
        // A different segment list: different fingerprint.
        let mut changed = presentation();
        changed.variants[1].video.plan.segments[0].duration = 9.0;
        changed.variants[1].video.plan.segments[0].object.url =
            Url::parse("https://cdn.example/other.ts").expect("URL");
        let r3 = resolve(&changed, &sel).expect("resolves");
        assert_ne!(fingerprint(&r3), fp);
    }

    #[test]
    fn a_clear_init_keeps_the_0_5_0_fingerprint_and_a_key_changes_it() {
        let with_init = |key: Option<Key>| {
            let mut p = presentation();
            for v in &mut p.variants {
                v.video.plan.inits.push(Init {
                    object: Object {
                        url: Url::parse("https://cdn.example/init.mp4?sig=2").expect("URL"),
                        range: None,
                    },
                    key: key.clone(),
                });
            }
            let sel = default_selection(&p, &Preferences::default());
            fingerprint(&resolve(&p, &sel).expect("resolves"))
        };
        // Computed by 0.5.0 for the same presentation: downloads started there resume.
        assert_eq!(
            with_init(None),
            "f5cd0ab441a19be4edb9108c2e374c4bc9829138dfda340dab0816ce32fc8f1b"
        );
        let key = Key {
            url: Url::parse("https://cdn.example/k.bin").expect("URL"),
            iv: Some([0; 16]),
        };
        assert_ne!(with_init(Some(key)), with_init(None));
    }

    #[test]
    fn selection_binding_refuses_missing_tracks() {
        let p = presentation();
        let gone = Selection {
            video: Some(TrackId::of(&["nope"])),
            ..Default::default()
        };
        assert_eq!(resolve(&p, &gone).err(), Some(SelectError::VideoGone));
        let wrong_group = Selection {
            video: Some(TrackId::of(&["lo"])),
            subtitles: vec![TrackId::of(&["other"])],
            ..Default::default()
        };
        assert_eq!(
            resolve(&p, &wrong_group).err(),
            Some(SelectError::TrackGone)
        );
        assert_eq!(
            resolve(&p, &Selection::default()).err(),
            Some(SelectError::NothingToDownload)
        );
    }

    #[test]
    fn drm_tracks_are_never_selected() {
        let mut p = presentation();
        p.audio[0].protection = Protection::Drm {
            systems: vec!["Widevine".into()],
        };
        let sel = Selection {
            video: Some(TrackId::of(&["lo"])),
            audio: vec![TrackId::of(&["en"])],
            ..Default::default()
        };
        assert_eq!(resolve(&p, &sel).err(), Some(SelectError::TrackGone));
    }

    #[test]
    fn container_auto_rules() {
        let p = presentation();
        let r = resolve(
            &p,
            &Selection {
                video: Some(TrackId::of(&["hi"])),
                audio: vec![TrackId::of(&["en"])],
                subtitles: vec![],
            },
        )
        .expect("resolves");
        let (out, why) =
            resolve_container(ContainerChoice::Auto, &r, SubtitleMode::None).expect("ok");
        assert_eq!(out, Output::Mp4);
        assert_eq!(why, "every codec fits MP4");

        // Opus audio: MKV, and the reason says why.
        let r_opus = resolve(
            &p,
            &Selection {
                video: Some(TrackId::of(&["hi"])),
                audio: vec![TrackId::of(&["de"])],
                subtitles: vec![],
            },
        )
        .expect("resolves");
        let (out, why) =
            resolve_container(ContainerChoice::Auto, &r_opus, SubtitleMode::None).expect("ok");
        assert_eq!(out, Output::Mkv);
        assert_eq!(why, "Opus audio can't go in MP4");

        // Embedded subtitles favour MKV (they stream-copy).
        let with_subs = resolve(
            &p,
            &Selection {
                video: Some(TrackId::of(&["hi"])),
                audio: vec![TrackId::of(&["en"])],
                subtitles: vec![TrackId::of(&["sub"])],
            },
        )
        .expect("resolves");
        assert_eq!(
            resolve_container(ContainerChoice::Auto, &with_subs, SubtitleMode::Embed)
                .expect("ok")
                .0,
            Output::Mkv
        );
        assert_eq!(
            resolve_container(ContainerChoice::Auto, &with_subs, SubtitleMode::Sidecar)
                .expect("ok")
                .0,
            Output::Mp4
        );

        // Explicit MP4 with Opus is rejected up front.
        assert!(matches!(
            resolve_container(ContainerChoice::Mp4, &r_opus, SubtitleMode::None),
            Err(SelectError::Mp4Codec(c)) if c == "Opus audio"
        ));
        // Original needs exactly one track.
        assert_eq!(
            resolve_container(ContainerChoice::Original, &r, SubtitleMode::None).err(),
            Some(SelectError::OriginalNeedsOneTrack)
        );
        let one = resolve(
            &p,
            &Selection {
                video: Some(TrackId::of(&["hi"])),
                ..Default::default()
            },
        )
        .expect("resolves");
        assert_eq!(
            resolve_container(ContainerChoice::Original, &one, SubtitleMode::None)
                .expect("ok")
                .0,
            Output::Original
        );
    }

    #[test]
    fn hls_audio_renditions_take_their_codecs_from_the_variant() {
        let mut p = presentation();
        p.audio[0].codecs = None;
        // "lo" lists mp4a.40.2 for its audio: the rendition fits MP4.
        let lo = p.variants[0].video.id.clone();
        let sel = Selection {
            video: Some(lo),
            audio: vec![p.audio[0].id.clone()],
            subtitles: vec![],
        };
        let r = resolve(&p, &sel).expect("resolves");
        assert_eq!(
            resolve_container(ContainerChoice::Auto, &r, SubtitleMode::Embed)
                .expect("ok")
                .0,
            Output::Mp4
        );
        // "mid" lists only its video codec: the audio's codec is unknown, so MKV.
        let mid = p.variants[2].video.id.clone();
        let sel = Selection {
            video: Some(mid),
            ..sel
        };
        let r = resolve(&p, &sel).expect("resolves");
        assert_eq!(
            resolve_container(ContainerChoice::Auto, &r, SubtitleMode::Embed)
                .expect("ok")
                .0,
            Output::Mkv
        );
    }

    #[test]
    fn unknown_codecs_go_to_mkv() {
        let mut p = presentation();
        p.variants[0].video.codecs = None;
        let r = resolve(
            &p,
            &Selection {
                video: Some(TrackId::of(&["lo"])),
                ..Default::default()
            },
        )
        .expect("resolves");
        let (out, why) =
            resolve_container(ContainerChoice::Auto, &r, SubtitleMode::None).expect("ok");
        assert_eq!(out, Output::Mkv);
        assert!(why.contains("aren't listed"));
    }

    #[test]
    fn estimates_add_up() {
        let p = presentation();
        let r = resolve(
            &p,
            &Selection {
                video: Some(TrackId::of(&["lo"])),
                audio: vec![TrackId::of(&["en"])],
                subtitles: vec![TrackId::of(&["sub"])],
            },
        )
        .expect("resolves");
        // 500 kb/s and 128 kb/s for 10 s, the subtitle adds nothing.
        assert_eq!(estimate_bytes(&r), Some(625_000 + 160_000));
    }

    #[test]
    fn mp4_codec_table() {
        for ok in [
            "avc1.640028",
            "hvc1.1.6",
            "av01.0.04M.08",
            "mp4a.40.2",
            "mp4a.6b",
            "ac-3",
            "ec-3",
            "wvtt",
        ] {
            assert!(mp4_codec_ok(ok) || is_text_codec(ok), "{ok}");
        }
        for bad in ["opus", "vp09.00.10", "vorbis", "flac", "mp4a.a5", ""] {
            assert!(!mp4_codec_ok(bad), "{bad}");
        }
    }

    #[test]
    fn selection_round_trips_through_json() {
        let sel = Selection {
            video: Some(TrackId::of(&["hi"])),
            audio: vec![TrackId::of(&["en"])],
            subtitles: vec![],
        };
        let json = serde_json::to_string(&sel).unwrap_or_default();
        assert!(json.contains(TrackId::of(&["hi"]).as_str()));
        assert_eq!(
            TrackId::parse(TrackId::of(&["hi"]).as_str()),
            Some(TrackId::of(&["hi"]))
        );
        assert_eq!(TrackId::parse("not hex"), None);
        assert_eq!(TrackId::parse(&"A".repeat(32)), None);
    }
}
