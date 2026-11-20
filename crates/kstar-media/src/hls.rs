//! HLS (RFC 8216 with the common extensions): master playlists, media playlists, and the
//! assembly of a [`Presentation`] from a master and the media playlists it references.
//!
//! Parsing is two-phase so the caller can fetch the playlists in between: [`parse_master`],
//! then [`playlist_urls`] to learn which media playlists to fetch, then [`parse_media`] for
//! each, then [`assemble`].

use std::collections::HashMap;
use url::Url;

use crate::attrs::{self, get};
use crate::classify::extension;
use crate::error::ParseError;
use crate::limits::ParseLimits;
use crate::model::{
    ByteRange, Key, Liveness, ManifestKind, Object, Presentation, Protection, Segment,
    SegmentContainer, SegmentPlan, Track, TrackId, TrackKind, Variant, resolve, without_query,
};
use crate::protection;

/// A key tag as written in the playlist, for protection assessment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyTag {
    /// Upper-case method: `NONE`, `AES-128`, `SAMPLE-AES`, `SAMPLE-AES-CTR`, or other.
    pub method: String,
    pub format: Option<String>,
    /// The key URI, with DEFINE variables substituted.
    pub uri: Option<String>,
}

/// A master playlist: the video choices and the audio and subtitle renditions.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Master {
    pub variants: Vec<MasterVariant>,
    pub renditions: Vec<Rendition>,
    pub session_keys: Vec<KeyTag>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MasterVariant {
    pub uri: Url,
    pub bandwidth: u64,
    pub codecs: Option<String>,
    pub resolution: Option<(u32, u32)>,
    pub audio_group: Option<String>,
    pub subtitle_group: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RenditionKind {
    Audio,
    Subtitles,
    ClosedCaptions,
}

/// An `EXT-X-MEDIA` entry.
#[derive(Debug, Clone, PartialEq)]
pub struct Rendition {
    pub kind: RenditionKind,
    pub group: String,
    pub name: String,
    pub language: Option<String>,
    pub default: bool,
    pub uri: Option<Url>,
    pub channels: Option<u32>,
}

/// A media playlist: one track's segments.
#[derive(Debug, Clone, PartialEq)]
pub struct Media {
    pub target_duration: Option<f64>,
    pub media_sequence: u64,
    /// Ended (`EXT-X-ENDLIST`) or declared VOD: the content is complete.
    pub vod: bool,
    /// Every key tag in the playlist, for protection assessment.
    pub keys: Vec<KeyTag>,
    pub inits: Vec<Object>,
    pub segments: Vec<Segment>,
    /// Segments marked `EXT-X-GAP`: skipped, and counted for the user.
    pub gaps: usize,
}

/// Whether a playlist body is a media playlist (it has segments) rather than a master.
pub fn is_media_playlist(body: &[u8]) -> bool {
    String::from_utf8_lossy(body)
        .lines()
        .any(|l| l.trim_start().starts_with("#EXTINF"))
}

fn decode(body: &[u8], lim: &ParseLimits) -> Result<String, ParseError> {
    if body.len() > lim.max_manifest_bytes {
        return Err(ParseError::TooLarge {
            limit: lim.max_manifest_bytes,
        });
    }
    let body = body.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(body);
    let text = String::from_utf8_lossy(body).into_owned();
    if !text.trim_start().starts_with("#EXTM3U") {
        return Err(ParseError::invalid(
            "the playlist doesn't start with #EXTM3U",
        ));
    }
    Ok(text)
}

fn lines(text: &str) -> impl Iterator<Item = &str> {
    text.lines().map(str::trim).filter(|l| !l.is_empty())
}

fn define(rest: &str, defines: &mut HashMap<String, String>) -> Result<(), ParseError> {
    let a = attrs::parse(rest);
    if get(&a, "IMPORT").is_some() || get(&a, "QUERYPARAM").is_some() {
        return Err(ParseError::unsupported(
            "EXT-X-DEFINE with IMPORT or QUERYPARAM",
        ));
    }
    match (get(&a, "NAME"), get(&a, "VALUE")) {
        (Some(n), Some(v)) => {
            defines.insert(n.to_string(), v.to_string());
            Ok(())
        }
        _ => Err(ParseError::invalid("EXT-X-DEFINE without NAME and VALUE")),
    }
}

/// Replaces `{$NAME}` with the values from EXT-X-DEFINE.
fn subst(s: &str, defines: &HashMap<String, String>) -> Result<String, ParseError> {
    if !s.contains("{$") {
        return Ok(s.to_string());
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find("{$") {
        out.push_str(&rest[..i]);
        let after = &rest[i + 2..];
        let end = after
            .find('}')
            .ok_or_else(|| ParseError::invalid("a {$ variable is never closed"))?;
        let name = &after[..end];
        let value = defines
            .get(name)
            .ok_or_else(|| ParseError::invalid("a playlist uses a variable that isn't defined"))?;
        out.push_str(value);
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    Ok(out)
}

fn uri_of(s: &str, base: &Url, defines: &HashMap<String, String>) -> Result<Url, ParseError> {
    resolve(base, &subst(s, defines)?)
}

fn key_tag(
    a: &[(String, String)],
    defines: &HashMap<String, String>,
) -> Result<KeyTag, ParseError> {
    let method = get(a, "METHOD")
        .ok_or_else(|| ParseError::invalid("an EXT-X-KEY has no METHOD"))?
        .trim()
        .to_ascii_uppercase();
    let uri = match get(a, "URI") {
        Some(u) => Some(subst(u, defines)?),
        None => None,
    };
    Ok(KeyTag {
        method,
        format: get(a, "KEYFORMAT").map(str::to_string),
        uri,
    })
}

fn parse_resolution(s: &str) -> Option<(u32, u32)> {
    let (w, h) = s.split_once('x')?;
    Some((w.trim().parse().ok()?, h.trim().parse().ok()?))
}

/// `n` or `n@o`: a byte length and an optional offset.
fn parse_byterange(s: &str) -> Result<(u64, Option<u64>), ParseError> {
    let bad = || ParseError::invalid("a byte range isn't a number");
    match s.trim().split_once('@') {
        Some((n, o)) => Ok((
            n.trim().parse().map_err(|_| bad())?,
            Some(o.trim().parse().map_err(|_| bad())?),
        )),
        None => Ok((s.trim().parse().map_err(|_| bad())?, None)),
    }
}

fn iv_of(s: Option<&str>) -> Result<Option<[u8; 16]>, ParseError> {
    let Some(s) = s else { return Ok(None) };
    let hex = s
        .trim()
        .strip_prefix("0x")
        .or_else(|| s.trim().strip_prefix("0X"))
        .ok_or_else(|| ParseError::invalid("an IV doesn't start with 0x"))?;
    if hex.len() != 32 || !hex.is_ascii() {
        return Err(ParseError::invalid("an IV isn't 128 bits"));
    }
    let mut iv = [0u8; 16];
    for (i, byte) in iv.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&hex[2 * i..2 * i + 2], 16)
            .map_err(|_| ParseError::invalid("an IV isn't hex"))?;
    }
    Ok(Some(iv))
}

/// Parses a master playlist.
pub fn parse_master(body: &[u8], base: &Url, lim: &ParseLimits) -> Result<Master, ParseError> {
    let text = decode(body, lim)?;
    let mut defines = HashMap::new();
    let mut master = Master::default();
    let mut pending: Option<Vec<(String, String)>> = None;
    for line in lines(&text) {
        if let Some(rest) = line.strip_prefix("#EXT-X-STREAM-INF:") {
            if pending.is_some() {
                return Err(ParseError::invalid(
                    "#EXT-X-STREAM-INF isn't followed by a URI",
                ));
            }
            pending = Some(attrs::parse(rest));
        } else if let Some(rest) = line.strip_prefix("#EXT-X-DEFINE:") {
            define(rest, &mut defines)?;
        } else if let Some(rest) = line.strip_prefix("#EXT-X-MEDIA:") {
            let a = attrs::parse(rest);
            if let Some(r) = rendition(&a, base, &defines)? {
                if master.renditions.len() >= lim.max_tracks {
                    return Err(ParseError::TooMany {
                        what: "audio and subtitle renditions",
                        limit: lim.max_tracks,
                    });
                }
                master.renditions.push(r);
            }
        } else if let Some(rest) = line.strip_prefix("#EXT-X-SESSION-KEY:") {
            master
                .session_keys
                .push(key_tag(&attrs::parse(rest), &defines)?);
        } else if line.starts_with('#') {
            continue;
        } else if let Some(a) = pending.take() {
            if master.variants.len() >= lim.max_variants {
                return Err(ParseError::TooMany {
                    what: "video variants",
                    limit: lim.max_variants,
                });
            }
            master.variants.push(variant(&a, line, base, &defines)?);
        }
    }
    if pending.is_some() {
        return Err(ParseError::invalid("the last #EXT-X-STREAM-INF has no URI"));
    }
    if master.variants.is_empty() {
        return Err(ParseError::invalid("the master playlist lists no variants"));
    }
    Ok(master)
}

fn rendition(
    a: &[(String, String)],
    base: &Url,
    defines: &HashMap<String, String>,
) -> Result<Option<Rendition>, ParseError> {
    let kind = match get(a, "TYPE").unwrap_or("") {
        "AUDIO" => RenditionKind::Audio,
        "SUBTITLES" => RenditionKind::Subtitles,
        "CLOSED-CAPTIONS" => RenditionKind::ClosedCaptions,
        _ => return Ok(None),
    };
    let uri = match get(a, "URI") {
        Some(u) => Some(uri_of(u, base, defines)?),
        None => None,
    };
    if uri.is_none() && kind != RenditionKind::ClosedCaptions {
        // Without a playlist there is nothing to download.
        return Ok(None);
    }
    let language = get(a, "LANGUAGE")
        .filter(|l| !l.is_empty())
        .map(str::to_string);
    Ok(Some(Rendition {
        kind,
        group: get(a, "GROUP-ID").unwrap_or("").to_string(),
        name: get(a, "NAME").unwrap_or("").to_string(),
        language,
        default: get(a, "DEFAULT") == Some("YES"),
        uri,
        channels: get(a, "CHANNELS")
            .and_then(|c| c.split('/').next())
            .and_then(|c| c.trim().parse().ok()),
    }))
}

fn variant(
    a: &[(String, String)],
    line: &str,
    base: &Url,
    defines: &HashMap<String, String>,
) -> Result<MasterVariant, ParseError> {
    let bandwidth = get(a, "BANDWIDTH")
        .or_else(|| get(a, "AVERAGE-BANDWIDTH"))
        .and_then(|v| v.trim().parse::<u64>().ok())
        .ok_or_else(|| ParseError::invalid("a variant has no bandwidth"))?;
    Ok(MasterVariant {
        uri: uri_of(line, base, defines)?,
        bandwidth,
        codecs: get(a, "CODECS").map(str::to_string),
        resolution: get(a, "RESOLUTION").and_then(parse_resolution),
        audio_group: get(a, "AUDIO")
            .filter(|g| !g.is_empty())
            .map(str::to_string),
        subtitle_group: get(a, "SUBTITLES")
            .filter(|g| !g.is_empty())
            .map(str::to_string),
    })
}

/// The media playlists a master refers to, each once, for the caller to fetch.
pub fn playlist_urls(master: &Master, lim: &ParseLimits) -> Result<Vec<Url>, ParseError> {
    let mut urls: Vec<Url> = Vec::new();
    let refs = master.variants.iter().map(|v| &v.uri).chain(
        master
            .renditions
            .iter()
            .filter(|r| r.kind != RenditionKind::ClosedCaptions)
            .filter_map(|r| r.uri.as_ref()),
    );
    for u in refs {
        if !urls.contains(u) {
            urls.push(u.clone());
        }
    }
    if urls.len() > lim.max_media_playlists {
        return Err(ParseError::TooMany {
            what: "media playlists",
            limit: lim.max_media_playlists,
        });
    }
    Ok(urls)
}

/// Parses a media playlist.
pub fn parse_media(body: &[u8], base: &Url, lim: &ParseLimits) -> Result<Media, ParseError> {
    let text = decode(body, lim)?;
    let mut defines = HashMap::new();
    let mut m = Media {
        target_duration: None,
        media_sequence: 0,
        vod: false,
        keys: Vec::new(),
        inits: Vec::new(),
        segments: Vec::new(),
        gaps: 0,
    };
    let mut discontinuity_sequence = 0u64;
    let mut discontinuities = 0u64;
    let mut key: Option<Key> = None;
    let mut init: Option<usize> = None;
    let mut extinf: Option<f64> = None;
    let mut byterange: Option<(u64, Option<u64>)> = None;
    let mut gap = false;
    // The end of the previous segment's byte range, for ranges without an explicit offset.
    let mut last_end: Option<(Url, u64)> = None;
    let mut start = 0.0f64;
    let mut index = 0u64;

    for line in lines(&text) {
        if let Some(rest) = line.strip_prefix("#EXTINF:") {
            if extinf.is_some() {
                return Err(ParseError::invalid("#EXTINF isn't followed by a URI"));
            }
            let d = rest
                .split(',')
                .next()
                .unwrap_or("")
                .trim()
                .parse::<f64>()
                .ok()
                .filter(|d| d.is_finite() && *d >= 0.0)
                .ok_or_else(|| ParseError::invalid("a segment duration isn't a number"))?;
            extinf = Some(d);
        } else if let Some(rest) = line.strip_prefix("#EXT-X-TARGETDURATION:") {
            m.target_duration = rest.trim().parse().ok();
        } else if let Some(rest) = line.strip_prefix("#EXT-X-MEDIA-SEQUENCE:") {
            m.media_sequence = rest
                .trim()
                .parse()
                .map_err(|_| ParseError::invalid("EXT-X-MEDIA-SEQUENCE isn't a number"))?;
        } else if let Some(rest) = line.strip_prefix("#EXT-X-DISCONTINUITY-SEQUENCE:") {
            discontinuity_sequence = rest
                .trim()
                .parse()
                .map_err(|_| ParseError::invalid("EXT-X-DISCONTINUITY-SEQUENCE isn't a number"))?;
        } else if line == "#EXT-X-DISCONTINUITY" {
            discontinuities += 1;
        } else if let Some(rest) = line.strip_prefix("#EXT-X-KEY:") {
            let kt = key_tag(&attrs::parse(rest), &defines)?;
            key = match kt.method.as_str() {
                "NONE" => None,
                "AES-128" => {
                    let uri = kt
                        .uri
                        .as_deref()
                        .ok_or_else(|| ParseError::invalid("an AES-128 key has no URI"))?;
                    let iv = iv_of(get(&attrs::parse(rest), "IV"))?;
                    Some(Key {
                        url: resolve(base, uri)?,
                        iv,
                    })
                }
                _ => None,
            };
            if !m.keys.contains(&kt) {
                m.keys.push(kt);
            }
        } else if let Some(rest) = line.strip_prefix("#EXT-X-MAP:") {
            let a = attrs::parse(rest);
            let uri = get(&a, "URI").ok_or_else(|| ParseError::invalid("EXT-X-MAP has no URI"))?;
            let range = match get(&a, "BYTERANGE") {
                Some(r) => {
                    let (len, off) = parse_byterange(r)?;
                    Some(ByteRange {
                        offset: off.unwrap_or(0),
                        length: len,
                    })
                }
                None => None,
            };
            let obj = Object {
                url: uri_of(uri, base, &defines)?,
                range,
            };
            let idx = match m.inits.iter().position(|o| *o == obj) {
                Some(i) => i,
                None => {
                    if m.inits.len() >= lim.max_tracks {
                        return Err(ParseError::TooMany {
                            what: "initialisation segments",
                            limit: lim.max_tracks,
                        });
                    }
                    m.inits.push(obj);
                    m.inits.len() - 1
                }
            };
            init = Some(idx);
        } else if let Some(rest) = line.strip_prefix("#EXT-X-BYTERANGE:") {
            byterange = Some(parse_byterange(rest)?);
        } else if line == "#EXT-X-GAP" {
            gap = true;
        } else if let Some(rest) = line.strip_prefix("#EXT-X-PLAYLIST-TYPE:") {
            if rest.trim().eq_ignore_ascii_case("VOD") {
                m.vod = true;
            }
        } else if line == "#EXT-X-ENDLIST" {
            m.vod = true;
        } else if let Some(rest) = line.strip_prefix("#EXT-X-DEFINE:") {
            define(rest, &mut defines)?;
        } else if line.starts_with("#EXT-X-SKIP") {
            return Err(ParseError::unsupported("playlists that skip old segments"));
        } else if line == "#EXT-X-I-FRAMES-ONLY" {
            return Err(ParseError::unsupported("I-frame-only playlists"));
        } else if line.starts_with('#') {
            continue;
        } else {
            let d = extinf
                .take()
                .ok_or_else(|| ParseError::invalid("a segment URI has no #EXTINF"))?;
            let url = uri_of(line, base, &defines)?;
            let range = match byterange.take() {
                None => None,
                Some((len, Some(offset))) => Some(ByteRange {
                    offset,
                    length: len,
                }),
                Some((len, None)) => {
                    let offset = match &last_end {
                        Some((u, end)) if *u == url => *end,
                        _ => {
                            return Err(ParseError::invalid(
                                "a byte range without an offset doesn't follow a range of the same file",
                            ));
                        }
                    };
                    Some(ByteRange {
                        offset,
                        length: len,
                    })
                }
            };
            last_end = range.map(|r| (url.clone(), r.offset + r.length));
            let seq = m.media_sequence + index;
            index += 1;
            if gap {
                m.gaps += 1;
            } else {
                if m.segments.len() >= lim.max_segments_per_track {
                    return Err(ParseError::TooMany {
                        what: "segments",
                        limit: lim.max_segments_per_track,
                    });
                }
                m.segments.push(Segment {
                    seq,
                    object: Object { url, range },
                    init,
                    key: key.clone(),
                    discontinuity: discontinuity_sequence + discontinuities,
                    start,
                    duration: d,
                });
            }
            start += d;
            gap = false;
        }
    }
    if extinf.is_some() {
        return Err(ParseError::invalid("#EXTINF isn't followed by a URI"));
    }
    if m.segments.is_empty() && m.gaps == 0 {
        return Err(ParseError::invalid("the media playlist has no segments"));
    }
    Ok(m)
}

/// Builds the presentation from a master and the media playlists it refers to (keyed by the
/// URL they were fetched from). Variants whose media is DRM-protected are dropped and
/// counted in [`Presentation::dropped_drm`].
pub fn assemble(
    master: &Master,
    lists: &HashMap<Url, Media>,
    lim: &ParseLimits,
) -> Result<Presentation, ParseError> {
    let session = master
        .session_keys
        .iter()
        .map(|k| protection::hls_key(&k.method, k.format.as_deref(), k.uri.as_deref()))
        .fold(Protection::Clear, Protection::worst);
    let mut warnings = Vec::new();
    let mut dropped_drm = 0usize;
    let mut live = false;
    let mut count = 0usize;
    let mut audio: Vec<Track> = Vec::new();
    let mut subtitles: Vec<Track> = Vec::new();
    let mut groups: HashMap<(RenditionKind, String), Vec<TrackId>> = HashMap::new();

    for r in &master.renditions {
        let (Some(uri), false) = (&r.uri, r.kind == RenditionKind::ClosedCaptions) else {
            // Captions are embedded in the video; they are not a separate download.
            continue;
        };
        let Some(media) = lists.get(uri) else {
            warnings.push(format!("the playlist for “{}” wasn't read", r.name));
            continue;
        };
        let protection = media_protection(media).worst(session.clone());
        if protection.is_drm() {
            dropped_drm += 1;
            continue;
        }
        let (kind, label) = match r.kind {
            RenditionKind::Audio => (TrackKind::Audio, "audio"),
            _ => (TrackKind::Subtitle, "subtitles"),
        };
        let id = TrackId::of(&[
            label,
            &r.group,
            &r.name,
            r.language.as_deref().unwrap_or(""),
        ]);
        groups
            .entry((r.kind, r.group.clone()))
            .or_default()
            .push(id.clone());
        let list = if kind == TrackKind::Audio {
            &mut audio
        } else {
            &mut subtitles
        };
        if list.iter().any(|t| t.id == id) {
            continue;
        }
        count += 1;
        check_count(count, lim)?;
        live |= !media.vod;
        list.push(Track {
            id,
            kind,
            lang: r.language.clone(),
            name: Some(r.name.clone()).filter(|n| !n.is_empty()),
            codecs: None,
            container: container_of(media),
            bandwidth: None,
            resolution: None,
            channels: r.channels,
            is_default: r.default,
            protection,
            plan: plan_of(media),
        });
    }

    let mut variants: Vec<Variant> = Vec::new();
    let mut seen: Vec<TrackId> = Vec::new();
    for v in &master.variants {
        let Some(media) = lists.get(&v.uri) else {
            warnings.push("the playlist for a video choice wasn't read".into());
            continue;
        };
        let protection = media_protection(media).worst(session.clone());
        if protection.is_drm() {
            dropped_drm += 1;
            continue;
        }
        if is_audio_only(v.codecs.as_deref(), v.resolution) {
            warnings.push(
                "an audio-only variant was skipped; it isn't offered as a video choice".into(),
            );
            continue;
        }
        let codecs = v.codecs.clone();
        let res = v.resolution.map(|(w, h)| format!("{w}x{h}"));
        let id = TrackId::of(&[
            "hls",
            "video",
            &v.bandwidth.to_string(),
            res.as_deref().unwrap_or(""),
            codecs.as_deref().unwrap_or(""),
            &without_query(&v.uri),
        ]);
        if seen.contains(&id) {
            continue;
        }
        seen.push(id.clone());
        count += 1;
        check_count(count, lim)?;
        live |= !media.vod;
        if media.gaps > 0 {
            warnings.push(format!(
                "{} segments are marked as gaps and skipped",
                media.gaps
            ));
        }
        let video = Track {
            id,
            kind: TrackKind::Video,
            lang: None,
            name: None,
            codecs,
            container: container_of(media),
            bandwidth: Some(v.bandwidth),
            resolution: v.resolution,
            channels: None,
            is_default: false,
            protection,
            plan: plan_of(media),
        };
        let audio_ids = v
            .audio_group
            .as_ref()
            .and_then(|g| groups.get(&(RenditionKind::Audio, g.clone())))
            .cloned()
            .unwrap_or_default();
        let subtitle_ids = v
            .subtitle_group
            .as_ref()
            .and_then(|g| groups.get(&(RenditionKind::Subtitles, g.clone())))
            .cloned()
            .unwrap_or_default();
        variants.push(Variant {
            video,
            audio: audio_ids,
            subtitles: subtitle_ids,
            muxed_audio: v.audio_group.is_none(),
        });
    }

    if variants.is_empty() && audio.is_empty() && dropped_drm == 0 {
        return Err(ParseError::invalid(
            "no variant of this playlist could be read",
        ));
    }
    let duration = variants.first().map(|v| v.video.plan.duration());
    Ok(Presentation {
        kind: ManifestKind::Hls,
        liveness: if live { Liveness::Live } else { Liveness::Vod },
        duration,
        title: None,
        variants,
        audio,
        subtitles,
        warnings,
        dropped_drm,
    })
}

fn check_count(count: usize, lim: &ParseLimits) -> Result<(), ParseError> {
    if count > lim.max_tracks {
        return Err(ParseError::TooMany {
            what: "tracks",
            limit: lim.max_tracks,
        });
    }
    Ok(())
}

fn media_protection(media: &Media) -> Protection {
    media
        .keys
        .iter()
        .map(|k| protection::hls_key(&k.method, k.format.as_deref(), k.uri.as_deref()))
        .fold(Protection::Clear, Protection::worst)
}

fn plan_of(media: &Media) -> SegmentPlan {
    SegmentPlan {
        inits: media.inits.clone(),
        segments: media.segments.clone(),
        whole: false,
    }
}

/// The container of a media playlist: from the segment extension, else from the presence
/// of an initialisation segment.
fn container_of(media: &Media) -> SegmentContainer {
    let by_ext = media
        .segments
        .first()
        .and_then(|s| container_of_ext(s.object.url.path()));
    by_ext.unwrap_or(if media.inits.is_empty() {
        SegmentContainer::Unknown
    } else {
        SegmentContainer::Fmp4
    })
}

/// The container a segment file extension implies.
pub(crate) fn container_of_ext(path: &str) -> Option<SegmentContainer> {
    match extension(path)?.as_str() {
        "ts" | "tsa" | "tsv" => Some(SegmentContainer::Ts),
        "m4s" | "mp4" | "m4v" | "m4a" | "cmfv" | "cmfa" | "cmft" => Some(SegmentContainer::Fmp4),
        "aac" => Some(SegmentContainer::Aac),
        "mp3" => Some(SegmentContainer::Mp3),
        "ac3" | "ec3" => Some(SegmentContainer::Ac3),
        "vtt" | "webvtt" => Some(SegmentContainer::Vtt),
        "ttml" | "dfxp" | "xml" => Some(SegmentContainer::Ttml),
        _ => None,
    }
}

fn is_audio_only(codecs: Option<&str>, resolution: Option<(u32, u32)>) -> bool {
    resolution.is_none()
        && codecs.is_some_and(|c| {
            c.split(',').all(|t| {
                let t = t.trim();
                ["mp4a", "ac-3", "ec-3", "opus", "flac", "vorbis", "mp3"]
                    .iter()
                    .any(|p| t.starts_with(p))
            })
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> Url {
        Url::parse("https://cdn.example/show/master.m3u8").expect("URL")
    }

    fn lim() -> ParseLimits {
        ParseLimits::default()
    }

    const MASTER: &str = "#EXTM3U\n\
        #EXT-X-DEFINE:NAME=\"host\",VALUE=\"https://cdn.example/show\"\n\
        #EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"aud\",NAME=\"English\",LANGUAGE=\"en\",DEFAULT=YES,AUTOSELECT=YES,CHANNELS=\"2\",URI=\"{$host}/en.m3u8\"\n\
        #EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"aud\",NAME=\"Deutsch\",LANGUAGE=\"de\",URI=\"de/index.m3u8\"\n\
        #EXT-X-MEDIA:TYPE=SUBTITLES,GROUP-ID=\"subs\",NAME=\"English\",LANGUAGE=\"en\",URI=\"subs/en.m3u8\"\n\
        #EXT-X-MEDIA:TYPE=CLOSED-CAPTIONS,GROUP-ID=\"cc\",NAME=\"CC1\",INSTREAM-ID=\"CC1\"\n\
        #EXT-X-STREAM-INF:BANDWIDTH=2000000,CODECS=\"avc1.4d401f,mp4a.40.2\",RESOLUTION=1280x720,AUDIO=\"aud\",SUBTITLES=\"subs\"\n\
        720p/index.m3u8\n\
        #EXT-X-STREAM-INF:BANDWIDTH=800000,RESOLUTION=640x360,AUDIO=\"aud\"\n\
        360p/index.m3u8\n\
        #EXT-X-STREAM-INF:BANDWIDTH=64000,CODECS=\"mp4a.40.2\"\n\
        audio-only.m3u8\n";

    const MEDIA: &str = "#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-TARGETDURATION:4\n#EXT-X-MEDIA-SEQUENCE:10\n\
        #EXT-X-KEY:METHOD=AES-128,URI=\"keys/k.bin\",IV=0x000102030405060708090a0b0c0d0e0f\n\
        #EXT-X-MAP:URI=\"init.mp4\",BYTERANGE=\"720@0\"\n\
        #EXTINF:4.0,\n#EXT-X-BYTERANGE:1000@720\nmedia.mp4\n\
        #EXTINF:4.0,\n#EXT-X-BYTERANGE:1000\nmedia.mp4\n\
        #EXT-X-DISCONTINUITY\n\
        #EXT-X-KEY:METHOD=NONE\n\
        #EXTINF:2.0,\nad.ts\n\
        #EXT-X-GAP\n#EXTINF:3.0,\ngone.ts\n\
        #EXTINF:4.0,\n../seg/last.ts?sig=abc\n\
        #EXT-X-ENDLIST\n";

    #[test]
    fn master_variants_and_renditions() {
        let m = parse_master(MASTER.as_bytes(), &base(), &lim()).expect("parses");
        assert_eq!(m.variants.len(), 3);
        assert_eq!(m.variants[0].bandwidth, 2_000_000);
        assert_eq!(m.variants[0].resolution, Some((1280, 720)));
        assert_eq!(
            m.variants[0].uri.as_str(),
            "https://cdn.example/show/720p/index.m3u8"
        );
        // The DEFINE variable is substituted and resolved.
        let en = &m.renditions[0];
        assert_eq!(
            en.uri.as_ref().map(Url::as_str),
            Some("https://cdn.example/show/en.m3u8")
        );
        assert!(en.default);
        assert_eq!(en.channels, Some(2));
        // Closed captions have no playlist of their own; they are not fetched (see playlist_urls).
        assert_eq!(
            m.renditions
                .iter()
                .filter(|r| r.kind == RenditionKind::ClosedCaptions)
                .count(),
            1
        );
        let urls = playlist_urls(&m, &lim()).expect("within limits");
        assert_eq!(urls.len(), 6);
    }

    #[test]
    fn media_playlist_details() {
        let m = parse_media(MEDIA.as_bytes(), &base(), &lim()).expect("parses");
        assert!(m.vod);
        assert_eq!(m.media_sequence, 10);
        assert_eq!(m.gaps, 1);
        assert_eq!(m.inits.len(), 1);
        assert_eq!(
            m.inits[0].range,
            Some(ByteRange {
                offset: 0,
                length: 720
            })
        );
        assert_eq!(m.segments.len(), 4);
        // Sequence numbers count the gap too.
        assert_eq!(
            m.segments.iter().map(|s| s.seq).collect::<Vec<_>>(),
            [10, 11, 12, 14]
        );
        // Implicit offset continues the previous range of the same file.
        assert_eq!(
            m.segments[1].object.range,
            Some(ByteRange {
                offset: 1720,
                length: 1000
            })
        );
        // Relative and query-carrying URIs resolve against the playlist URL.
        assert_eq!(
            m.segments[3].object.url.as_str(),
            "https://cdn.example/seg/last.ts?sig=abc"
        );
        assert_eq!(m.segments[0].init, Some(0));
        let key = m.segments[0].key.as_ref().expect("AES key");
        assert_eq!(key.url.as_str(), "https://cdn.example/show/keys/k.bin");
        assert_eq!(key.iv.expect("explicit IV")[15], 0x0f);
        // Discontinuity counts move on after the tag; the key reset applies to the ad segment.
        assert_eq!(m.segments[0].discontinuity, 0);
        assert_eq!(m.segments[2].discontinuity, 1);
        assert!(m.segments[2].key.is_none());
        assert_eq!(m.segments[3].start, 4.0 + 4.0 + 2.0 + 3.0);
        assert!(m.keys.iter().any(|k| k.method == "AES-128"));
    }

    #[test]
    fn sequence_iv_when_none_is_given() {
        let body =
            "#EXTM3U\n#EXT-X-KEY:METHOD=AES-128,URI=\"k\"\n#EXTINF:1,\na.ts\n#EXT-X-ENDLIST\n";
        let m = parse_media(body.as_bytes(), &base(), &lim()).expect("parses");
        assert_eq!(m.segments[0].key.as_ref().map(|k| k.iv), Some(None));
    }

    #[test]
    fn assembles_presentation_and_drops_drm() {
        let master = parse_master(MASTER.as_bytes(), &base(), &lim()).expect("parses");
        let mut lists = HashMap::new();
        let clear = parse_media(
            b"#EXTM3U\n#EXTINF:4,\na.ts\n#EXT-X-ENDLIST\n",
            &base(),
            &lim(),
        )
        .expect("parses");
        let drm = parse_media(
            b"#EXTM3U\n#EXT-X-KEY:METHOD=SAMPLE-AES,KEYFORMAT=\"com.apple.streamingkeydelivery\",URI=\"skd://x\"\n#EXTINF:4,\na.ts\n#EXT-X-ENDLIST\n",
            &base(),
            &lim(),
        )
        .expect("parses");
        for u in playlist_urls(&master, &lim()).expect("ok") {
            lists.insert(u.clone(), clear.clone());
        }
        // The 360p variant is DRM-protected.
        lists.insert(
            Url::parse("https://cdn.example/show/360p/index.m3u8").expect("URL"),
            drm,
        );
        let p = assemble(&master, &lists, &lim()).expect("assembles");
        assert_eq!(p.kind, ManifestKind::Hls);
        assert_eq!(p.variants.len(), 1);
        assert_eq!(p.dropped_drm, 1);
        assert_eq!(p.variants[0].video.resolution, Some((1280, 720)));
        assert_eq!(p.variants[0].audio.len(), 2);
        assert_eq!(p.subtitles.len(), 1);
        assert!(!p.variants[0].muxed_audio);
        assert_eq!(p.liveness, Liveness::Vod);
        assert_eq!(p.duration, Some(4.0));
        let en = p
            .audio
            .iter()
            .find(|t| t.lang.as_deref() == Some("en"))
            .expect("en");
        assert!(en.is_default);
        assert_eq!(en.channels, Some(2));
        assert!(p.variants[0].audio.contains(&en.id));
    }

    #[test]
    fn all_variants_drm_is_reported_not_hidden() {
        let master = parse_master(
            b"#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=1\nv.m3u8\n",
            &base(),
            &lim(),
        )
        .expect("parses");
        let drm = parse_media(
            b"#EXTM3U\n#EXT-X-KEY:METHOD=SAMPLE-AES,KEYFORMAT=\"urn:uuid:edef8ba9-79d6-4ace-a3c8-27dcd51d21ed\",URI=\"x\"\n#EXTINF:4,\na.ts\n#EXT-X-ENDLIST\n",
            &base(),
            &lim(),
        )
        .expect("parses");
        let mut lists = HashMap::new();
        lists.insert(
            Url::parse("https://cdn.example/show/v.m3u8").expect("URL"),
            drm,
        );
        let p = assemble(&master, &lists, &lim()).expect("assembles");
        assert!(p.variants.is_empty());
        assert_eq!(p.dropped_drm, 1);
    }

    #[test]
    fn malformed_playlists_are_typed_errors() {
        let b = base();
        let l = lim();
        let cases: &[&str] = &[
            "not a playlist",
            "#EXTM3U\n#EXTINF:4,\n",
            "#EXTM3U\n#EXTINF:nan,\na.ts\n",
            "#EXTM3U\n#EXTINF:-1,\na.ts\n",
            "#EXTM3U\n#EXT-X-DEFINE:NAME=\"x\",VALUE=\"y\",IMPORT=\"z\"\n#EXTINF:1,\na.ts\n",
            "#EXTM3U\n#EXT-X-DEFINE:NAME=\"x\",VALUE=\"y\"\n#EXTINF:1,\n{$nope}.ts\n",
            "#EXTM3U\n#EXTINF:1,\n{$unclosed.ts\n",
            "#EXTM3U\n#EXT-X-KEY:METHOD=AES-128,IV=0x01,URI=\"k\"\n#EXTINF:1,\na.ts\n",
            "#EXTM3U\n#EXT-X-KEY:METHOD=AES-128\n#EXTINF:1,\na.ts\n",
            "#EXTM3U\n#EXT-X-SKIP:SKIPPED-SEGMENTS=3\n#EXTINF:1,\na.ts\n",
            "#EXTM3U\n#EXT-X-BYTERANGE:100\n#EXTINF:1,\na.ts\n",
            "#EXTM3U\n#EXTINF:1,\na.ts\n#EXT-X-BYTERANGE:10\n#EXTINF:1,\nb.ts\n",
            "#EXTM3U\n#EXT-X-ENDLIST\n",
            "#EXTM3U\n#EXTINF:1,\n",
        ];
        for c in cases {
            assert!(parse_media(c.as_bytes(), &b, &l).is_err(), "{c:?}");
        }
        assert!(
            parse_master(
                b"#EXTM3U\n#EXT-X-STREAM-INF:RESOLUTION=1x1\nv.m3u8\n",
                &b,
                &l
            )
            .is_err()
        );
        assert!(parse_master(b"#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=1\n", &b, &l).is_err());
        assert!(parse_master(b"#EXTM3U\n#EXT-X-VERSION:3\n", &b, &l).is_err());
    }

    #[test]
    fn every_limit_is_enforced() {
        let b = base();
        let body = format!(
            "#EXTM3U\n{}#EXT-X-ENDLIST\n",
            "#EXTINF:1,\na.ts\n".repeat(5)
        );
        let small = ParseLimits {
            max_segments_per_track: 4,
            ..lim()
        };
        assert!(matches!(
            parse_media(body.as_bytes(), &b, &small),
            Err(ParseError::TooMany {
                what: "segments",
                ..
            })
        ));
        let tiny = ParseLimits {
            max_manifest_bytes: 10,
            ..lim()
        };
        assert!(matches!(
            parse_media(body.as_bytes(), &b, &tiny),
            Err(ParseError::TooLarge { .. })
        ));
        let mut master = String::from("#EXTM3U\n");
        for i in 0..3 {
            master.push_str(&format!("#EXT-X-STREAM-INF:BANDWIDTH={i}\nv{i}.m3u8\n"));
        }
        let two = ParseLimits {
            max_variants: 2,
            ..lim()
        };
        assert!(matches!(
            parse_master(master.as_bytes(), &b, &two),
            Err(ParseError::TooMany {
                what: "video variants",
                ..
            })
        ));
        let one = ParseLimits {
            max_media_playlists: 1,
            ..lim()
        };
        let m = parse_master(master.as_bytes(), &b, &lim()).expect("parses");
        assert!(matches!(
            playlist_urls(&m, &one),
            Err(ParseError::TooMany { .. })
        ));
    }

    #[test]
    fn hostile_references_are_resolved_not_fetched() {
        // A file: or javascript: reference resolves to a URL the policy later refuses.
        let m = parse_media(
            b"#EXTM3U\n#EXTINF:1,\nfile:///etc/passwd\n#EXT-X-ENDLIST\n",
            &base(),
            &lim(),
        );
        assert!(m.is_ok());
        assert!(parse_media(b"#EXTM3U\n#EXTINF:1,\n\n#EXT-X-ENDLIST\n", &base(), &lim()).is_err());
    }
}
