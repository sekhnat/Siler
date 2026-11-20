//! DASH static MPDs (ISO/IEC 23009-1): periods, adaptation sets and representations, and
//! the segment addressing K-Star downloads: `SegmentTemplate` (with `SegmentTimeline`),
//! `SegmentList`, and `SegmentBase` as one whole file.
//!
//! Inheritance follows the spec: a representation's attributes override its adaptation
//! set's, which override the period's. Representations are joined across periods by their
//! adaptation set and representation ids, so a track keeps its id through period changes.

use std::collections::HashMap;
use url::Url;

use crate::error::ParseError;
use crate::limits::ParseLimits;
use crate::model::{
    ByteRange, Liveness, ManifestKind, Object, Presentation, Protection, SUBTITLES_IN_MP4, Segment,
    SegmentContainer, SegmentPlan, Track, TrackId, TrackKind, Variant, language, resolve,
    subtitle_supported,
};
use crate::protection;
use crate::xml::{self, Node};

/// Parses a static MPD. Dynamic (live) manifests parse, and report [`Liveness::Live`].
pub fn parse(body: &[u8], base: &Url, lim: &ParseLimits) -> Result<Presentation, ParseError> {
    if body.len() > lim.max_manifest_bytes {
        return Err(ParseError::TooLarge {
            limit: lim.max_manifest_bytes,
        });
    }
    let root = xml::parse(body, lim)?;
    if root.name != "MPD" {
        return Err(ParseError::invalid("the root element isn't MPD"));
    }
    let dynamic = root.attr("type") == Some("dynamic");
    let mpd_duration = match root.attr("mediaPresentationDuration") {
        Some(s) => Some(iso_duration(s)?),
        None => None,
    };
    let mpd_base = child_base(&root, base)?;
    let periods: Vec<&Node> = root.children_named("Period").collect();
    if periods.is_empty() {
        return Err(ParseError::invalid("the MPD has no periods"));
    }
    if periods.len() > lim.max_periods {
        return Err(ParseError::TooMany {
            what: "periods",
            limit: lim.max_periods,
        });
    }
    let timeline = period_timeline(&periods, mpd_duration)?;

    let mut accs: Vec<Acc> = Vec::new();
    let mut index: HashMap<String, usize> = HashMap::new();
    let mut warnings: Vec<String> = Vec::new();
    for (period, (p_start, p_dur)) in periods.iter().zip(timeline.iter().copied()) {
        let period_base = child_base(period, &mpd_base)?;
        for (ai, aset) in period.children_named("AdaptationSet").enumerate() {
            let as_base = child_base(aset, &period_base)?;
            let as_key = aset
                .attr("id")
                .map(str::to_string)
                .unwrap_or_else(|| format!("#{ai}"));
            for (ri, rep) in aset.children_named("Representation").enumerate() {
                let rep_id = rep
                    .attr("id")
                    .map(str::to_string)
                    .unwrap_or_else(|| format!("#{ri}"));
                let get = |n: &str| rep.attr(n).or_else(|| aset.attr(n));
                let mime = get("mimeType");
                let codecs = get("codecs");
                let Some(kind) = kind_of(get("contentType"), mime, codecs) else {
                    // Images (thumbnails) and anything unknown are not media tracks.
                    continue;
                };
                let rep_base = child_base(rep, &as_base)?;
                let bandwidth = rep
                    .attr("bandwidth")
                    .and_then(|b| b.trim().parse::<u64>().ok());
                let resolution = match (get("width"), get("height")) {
                    (Some(w), Some(h)) => {
                        match (w.trim().parse::<u32>(), h.trim().parse::<u32>()) {
                            (Ok(w), Ok(h)) => Some((w, h)),
                            _ => None,
                        }
                    }
                    _ => None,
                };
                let lang = language(get("lang"));
                let role_main = aset
                    .children_named("Role")
                    .chain(rep.children_named("Role"))
                    .any(|r| r.attr("value") == Some("main"));
                let name = aset
                    .child("Label")
                    .map(|l| l.text_trimmed().to_string())
                    .filter(|s| !s.is_empty());
                let channels = rep
                    .child("AudioChannelConfiguration")
                    .or_else(|| aset.child("AudioChannelConfiguration"))
                    .and_then(|c| c.attr("value"))
                    .and_then(|v| v.trim().parse::<u32>().ok());
                let protection = protections(aset, rep);
                let Some(plan) = plan_for(
                    rep, aset, period, &rep_base, &rep_id, bandwidth, p_start, p_dur, lim,
                )?
                else {
                    let w = "a representation that doesn't say where its segments are was skipped";
                    if !warnings.iter().any(|x| x == w) {
                        warnings.push(w.into());
                    }
                    continue;
                };
                let container = container_for(mime, codecs, &plan);
                let kind_label = match kind {
                    TrackKind::Video => "video",
                    TrackKind::Audio => "audio",
                    TrackKind::Subtitle => "subtitle",
                };
                let key = format!("{kind_label}\u{1f}{as_key}\u{1f}{rep_id}");
                match index.get(&key) {
                    Some(&i) => {
                        let acc = &mut accs[i];
                        acc.plan.append(plan);
                        acc.protection = std::mem::replace(&mut acc.protection, Protection::Clear)
                            .worst(protection);
                    }
                    None => {
                        index.insert(key, accs.len());
                        accs.push(Acc {
                            id: TrackId::of(&[
                                "dash",
                                kind_label,
                                &as_key,
                                lang.as_deref().unwrap_or(""),
                                if role_main { "main" } else { "" },
                                &rep_id,
                            ]),
                            kind,
                            lang,
                            name,
                            codecs: codecs.map(str::to_string),
                            container,
                            bandwidth,
                            resolution,
                            channels,
                            is_default: role_main,
                            protection,
                            plan,
                        });
                    }
                }
                if accs.len() > lim.max_tracks {
                    return Err(ParseError::TooMany {
                        what: "tracks",
                        limit: lim.max_tracks,
                    });
                }
            }
        }
    }
    if accs.is_empty() {
        return Err(ParseError::invalid(
            "the MPD has no audio, video or subtitle representations",
        ));
    }

    let mut dropped_drm = 0usize;
    let mut video = Vec::new();
    let mut audio = Vec::new();
    let mut subtitles = Vec::new();
    let mut subtitles_dropped = false;
    for acc in accs {
        let track = acc.into_track();
        if track.protection.is_drm() {
            dropped_drm += 1;
            continue;
        }
        if track.kind == TrackKind::Subtitle && !subtitle_supported(&track) {
            subtitles_dropped = true;
            continue;
        }
        match track.kind {
            TrackKind::Video => video.push(track),
            TrackKind::Audio => audio.push(track),
            TrackKind::Subtitle => subtitles.push(track),
        }
    }
    if subtitles_dropped {
        warnings.push(SUBTITLES_IN_MP4.into());
    }
    let audio_ids: Vec<TrackId> = audio.iter().map(|t| t.id.clone()).collect();
    let subtitle_ids: Vec<TrackId> = subtitles.iter().map(|t| t.id.clone()).collect();
    let variants = video
        .into_iter()
        .map(|v| Variant {
            video: v,
            audio: audio_ids.clone(),
            subtitles: subtitle_ids.clone(),
            muxed_audio: false,
        })
        .collect();
    let total: f64 = timeline.iter().filter_map(|t| t.1).sum();
    let duration = mpd_duration.or(if total > 0.0 { Some(total) } else { None });
    let title = root
        .child("ProgramInformation")
        .and_then(|p| p.child("Title"))
        .map(|t| t.text_trimmed().to_string())
        .filter(|t| !t.is_empty());
    Ok(Presentation {
        kind: ManifestKind::Dash,
        liveness: if dynamic {
            Liveness::Live
        } else {
            Liveness::Vod
        },
        duration,
        title,
        variants,
        audio,
        subtitles,
        warnings,
        dropped_drm,
    })
}

/// A track while its periods are being read.
struct Acc {
    id: TrackId,
    kind: TrackKind,
    lang: Option<String>,
    name: Option<String>,
    codecs: Option<String>,
    container: SegmentContainer,
    bandwidth: Option<u64>,
    resolution: Option<(u32, u32)>,
    channels: Option<u32>,
    is_default: bool,
    protection: Protection,
    plan: SegmentPlan,
}

impl Acc {
    fn into_track(self) -> Track {
        Track {
            id: self.id,
            kind: self.kind,
            lang: self.lang,
            name: self.name,
            codecs: self.codecs,
            container: self.container,
            bandwidth: self.bandwidth,
            resolution: self.resolution,
            channels: self.channels,
            is_default: self.is_default,
            protection: self.protection,
            plan: self.plan,
        }
    }
}

fn child_base(node: &Node, parent: &Url) -> Result<Url, ParseError> {
    match node.child("BaseURL") {
        Some(b) => resolve(parent, b.text_trimmed()),
        None => Ok(parent.clone()),
    }
}

/// Each period's start and duration in seconds. Missing values come from the next period's
/// start, or from the MPD's total duration for the last one.
fn period_timeline(
    periods: &[&Node],
    mpd: Option<f64>,
) -> Result<Vec<(f64, Option<f64>)>, ParseError> {
    let mut explicit_start = Vec::with_capacity(periods.len());
    let mut explicit_dur = Vec::with_capacity(periods.len());
    for p in periods {
        explicit_start.push(p.attr("start").map(iso_duration).transpose()?);
        explicit_dur.push(p.attr("duration").map(iso_duration).transpose()?);
    }
    let mut starts = Vec::with_capacity(periods.len());
    let mut cursor = 0.0f64;
    for i in 0..periods.len() {
        let s = explicit_start[i].unwrap_or(cursor);
        starts.push(s);
        cursor = s + explicit_dur[i].unwrap_or(0.0);
    }
    let mut out = Vec::with_capacity(periods.len());
    for i in 0..periods.len() {
        let dur = explicit_dur[i]
            .or_else(|| starts.get(i + 1).map(|next| (next - starts[i]).max(0.0)))
            .or_else(|| mpd.map(|m| (m - starts[i]).max(0.0)));
        out.push((starts[i], dur));
    }
    Ok(out)
}

fn kind_of(
    content_type: Option<&str>,
    mime: Option<&str>,
    codecs: Option<&str>,
) -> Option<TrackKind> {
    let mime_l = mime
        .map(|m| m.trim().to_ascii_lowercase())
        .unwrap_or_default();
    if mime_l == "application/ttml+xml" || mime_l == "text/vtt" {
        return Some(TrackKind::Subtitle);
    }
    let codecs_l = codecs.map(|c| c.to_ascii_lowercase()).unwrap_or_default();
    let by_codec = |c: &str| {
        if ["avc", "hvc", "hev", "av01", "vp09", "vp08"]
            .iter()
            .any(|p| c.starts_with(p))
        {
            Some(TrackKind::Video)
        } else if [
            "mp4a", "ac-3", "ec-3", "opus", "flac", "vorbis", "mp3", "dtsc",
        ]
        .iter()
        .any(|p| c.starts_with(p))
        {
            Some(TrackKind::Audio)
        } else if ["stpp", "wvtt"].iter().any(|p| c.starts_with(p)) {
            Some(TrackKind::Subtitle)
        } else {
            None
        }
    };
    let major = content_type
        .map(str::to_string)
        .or_else(|| mime.and_then(|m| m.split('/').next()).map(str::to_string))
        .unwrap_or_default()
        .to_ascii_lowercase();
    match major.as_str() {
        "video" => Some(TrackKind::Video),
        "audio" => Some(TrackKind::Audio),
        "text" => Some(TrackKind::Subtitle),
        "image" => None,
        _ => codecs_l.split(',').map(str::trim).find_map(by_codec),
    }
}

fn container_for(mime: Option<&str>, codecs: Option<&str>, plan: &SegmentPlan) -> SegmentContainer {
    match mime.unwrap_or("").trim().to_ascii_lowercase().as_str() {
        "video/mp2t" | "audio/mp2t" => SegmentContainer::Ts,
        "audio/aac" => SegmentContainer::Aac,
        "audio/mpeg" => SegmentContainer::Mp3,
        "text/vtt" => SegmentContainer::Vtt,
        "application/ttml+xml" => SegmentContainer::Ttml,
        "video/mp4" | "audio/mp4" | "application/mp4" => SegmentContainer::Fmp4,
        "video/webm" | "audio/webm" => SegmentContainer::Webm,
        _ => {
            let c = codecs.unwrap_or("").to_ascii_lowercase();
            if !plan.inits.is_empty() || c.contains("stpp") || c.contains("wvtt") {
                SegmentContainer::Fmp4
            } else {
                SegmentContainer::Unknown
            }
        }
    }
}

fn protections(aset: &Node, rep: &Node) -> Protection {
    aset.children_named("ContentProtection")
        .chain(rep.children_named("ContentProtection"))
        .filter_map(|c| protection::dash_scheme(c.attr("schemeIdUri").unwrap_or("")))
        .fold(Protection::Clear, Protection::worst)
}

/// Where a representation's segments are, from the nearest level that says: the
/// representation, then its adaptation set, then the period. `None` when none says.
#[allow(clippy::too_many_arguments)]
fn plan_for(
    rep: &Node,
    aset: &Node,
    period: &Node,
    base: &Url,
    rep_id: &str,
    bandwidth: Option<u64>,
    p_start: f64,
    p_dur: Option<f64>,
    lim: &ParseLimits,
) -> Result<Option<SegmentPlan>, ParseError> {
    let levels = [rep, aset, period];
    let templates: Vec<&Node> = levels
        .iter()
        .filter_map(|n| n.child("SegmentTemplate"))
        .collect();
    if !templates.is_empty() {
        let t = Layered(templates);
        return template_plan(&t, base, rep_id, bandwidth, p_start, p_dur, lim).map(Some);
    }
    if let Some(l) = levels.iter().find_map(|n| n.child("SegmentList")) {
        return list_plan(l, base, p_start, lim).map(Some);
    }
    if levels.iter().any(|n| n.child("SegmentBase").is_some()) || rep.child("BaseURL").is_some() {
        // One file: fetched whole, and resumed if interrupted.
        return Ok(Some(whole_plan(base, p_start, p_dur)));
    }
    Ok(None)
}

/// A `SegmentTemplate` with its inherited attributes: each one comes from the nearest level
/// that has it, as does the `SegmentTimeline`.
struct Layered<'a>(Vec<&'a Node>);

impl Layered<'_> {
    fn attr(&self, name: &str) -> Option<&str> {
        self.0.iter().find_map(|n| n.attr(name))
    }

    fn timeline(&self) -> Option<&Node> {
        self.0.iter().find_map(|n| n.child("SegmentTimeline"))
    }

    fn u64(&self, name: &str) -> Result<Option<u64>, ParseError> {
        match self.attr(name) {
            None => Ok(None),
            Some(v) => {
                v.trim().parse::<u64>().map(Some).map_err(|_| {
                    ParseError::invalid(format!("the attribute {name} isn't a number"))
                })
            }
        }
    }
}

fn whole_plan(base: &Url, start: f64, dur: Option<f64>) -> SegmentPlan {
    SegmentPlan {
        inits: Vec::new(),
        segments: vec![Segment {
            seq: 0,
            object: Object {
                url: base.clone(),
                range: None,
            },
            init: None,
            key: None,
            discontinuity: 0,
            start,
            duration: dur.unwrap_or(0.0),
        }],
        whole: true,
    }
}

fn template_plan(
    t: &Layered<'_>,
    base: &Url,
    rep_id: &str,
    bandwidth: Option<u64>,
    p_start: f64,
    p_dur: Option<f64>,
    lim: &ParseLimits,
) -> Result<SegmentPlan, ParseError> {
    let timescale = t.u64("timescale")?.unwrap_or(1).max(1);
    let pto = t.u64("presentationTimeOffset")?.unwrap_or(0);
    let start_number = t.u64("startNumber")?.unwrap_or(1);
    let media = t
        .attr("media")
        .ok_or_else(|| ParseError::invalid("a SegmentTemplate has no media attribute"))?;
    let mut plan = SegmentPlan::default();
    if let Some(init) = t.attr("initialization") {
        let s = expand(init, rep_id, None, bandwidth, None)?;
        plan.inits.push(Object {
            url: resolve(base, &s)?,
            range: None,
        });
    }

    // (number, time in timescale ticks, duration in ticks)
    let mut entries: Vec<(u64, u64, u64)> = Vec::new();
    let push = |entries: &mut Vec<(u64, u64, u64)>, e: (u64, u64, u64)| -> Result<(), ParseError> {
        if entries.len() >= lim.max_segments_per_track {
            return Err(ParseError::TooMany {
                what: "segments",
                limit: lim.max_segments_per_track,
            });
        }
        entries.push(e);
        Ok(())
    };
    if let Some(tl) = t.timeline() {
        let mut number = start_number;
        let mut cursor: Option<u64> = None;
        let entries_s: Vec<&Node> = tl.children_named("S").collect();
        for (k, s) in entries_s.iter().enumerate() {
            let d = attr_u64(s, "d")?
                .filter(|d| *d > 0)
                .ok_or_else(|| ParseError::invalid("a timeline entry has no duration"))?;
            let mut time = match attr_u64(s, "t")? {
                Some(v) => v,
                None => cursor.unwrap_or(pto),
            };
            let r = match s.attr("r") {
                Some(r) => r
                    .trim()
                    .parse::<i64>()
                    .map_err(|_| ParseError::invalid("a timeline repeat count isn't a number"))?,
                None => 0,
            };
            let repeats = if r >= 0 {
                r as u64
            } else {
                // Repeats until the next entry's start, or the end of the period.
                let next = entries_s
                    .get(k + 1)
                    .map(|n| attr_u64(n, "t"))
                    .transpose()?
                    .flatten();
                let end = match (next, p_dur) {
                    (Some(t), _) => t as f64,
                    (None, Some(p)) => pto as f64 + p * timescale as f64,
                    (None, None) => {
                        return Err(ParseError::unsupported(
                            "a timeline that runs to the end of a period of unknown length",
                        ));
                    }
                };
                let span = (end - time as f64).max(0.0);
                ((span / d as f64).ceil() as u64).saturating_sub(1)
            };
            for _ in 0..=repeats {
                push(&mut entries, (number, time, d))?;
                number += 1;
                time = time.saturating_add(d);
            }
            cursor = Some(time);
        }
    } else {
        let d = t.u64("duration")?.filter(|d| *d > 0).ok_or_else(|| {
            ParseError::unsupported("a SegmentTemplate has neither a timeline nor a duration")
        })?;
        let p =
            p_dur.ok_or_else(|| ParseError::unsupported("the length of a period isn't known"))?;
        let count = ((p * timescale as f64) / d as f64).ceil() as u64;
        for i in 0..count {
            push(
                &mut entries,
                (
                    start_number.saturating_add(i),
                    pto.saturating_add(i.saturating_mul(d)),
                    d,
                ),
            )?;
        }
    }

    let has_init = !plan.inits.is_empty();
    for (i, (number, time, d)) in entries.into_iter().enumerate() {
        let s = expand(media, rep_id, Some(number), bandwidth, Some(time))?;
        plan.segments.push(Segment {
            seq: i as u64,
            object: Object {
                url: resolve(base, &s)?,
                range: None,
            },
            init: has_init.then_some(0),
            key: None,
            discontinuity: 0,
            start: p_start + time.saturating_sub(pto) as f64 / timescale as f64,
            duration: d as f64 / timescale as f64,
        });
    }
    if plan.segments.is_empty() {
        return Err(ParseError::invalid(
            "a SegmentTemplate describes no segments",
        ));
    }
    Ok(plan)
}

fn list_plan(
    l: &Node,
    base: &Url,
    p_start: f64,
    lim: &ParseLimits,
) -> Result<SegmentPlan, ParseError> {
    let timescale = attr_u64(l, "timescale")?.unwrap_or(1).max(1);
    let seg_dur = attr_u64(l, "duration")?.unwrap_or(0) as f64 / timescale as f64;
    let mut plan = SegmentPlan::default();
    if let Some(init) = l.child("Initialization") {
        let url = match init.attr("sourceURL") {
            Some(s) => resolve(base, s)?,
            None => base.clone(),
        };
        plan.inits.push(Object {
            url,
            range: init.attr("range").map(parse_range).transpose()?,
        });
    }
    let urls: Vec<&Node> = l.children_named("SegmentURL").collect();
    if urls.len() > lim.max_segments_per_track {
        return Err(ParseError::TooMany {
            what: "segments",
            limit: lim.max_segments_per_track,
        });
    }
    let has_init = !plan.inits.is_empty();
    for (i, su) in urls.iter().enumerate() {
        let url = match su.attr("media") {
            Some(m) => resolve(base, m)?,
            None => base.clone(),
        };
        plan.segments.push(Segment {
            seq: i as u64,
            object: Object {
                url,
                range: su.attr("mediaRange").map(parse_range).transpose()?,
            },
            init: has_init.then_some(0),
            key: None,
            discontinuity: 0,
            start: p_start + i as f64 * seg_dur,
            duration: seg_dur,
        });
    }
    if plan.segments.is_empty() {
        return Err(ParseError::invalid("a SegmentList describes no segments"));
    }
    Ok(plan)
}

/// Expands a DASH URL template: `$RepresentationID$`, `$Number$`, `$Bandwidth$`, `$Time$`
/// (with an optional `%0Nd` width) and `$$`.
fn expand(
    template: &str,
    rep_id: &str,
    number: Option<u64>,
    bandwidth: Option<u64>,
    time: Option<u64>,
) -> Result<String, ParseError> {
    let mut out = String::with_capacity(template.len() + 16);
    let mut rest = template;
    while let Some(i) = rest.find('$') {
        out.push_str(&rest[..i]);
        let after = &rest[i + 1..];
        let j = after
            .find('$')
            .ok_or_else(|| ParseError::invalid("a URL template has an unclosed $"))?;
        let token = &after[..j];
        rest = &after[j + 1..];
        if token.is_empty() {
            out.push('$');
            continue;
        }
        let (name, width) = match token.split_once('%') {
            Some((n, f)) => (n, parse_width(f)?),
            None => (token, 0),
        };
        let missing = || {
            ParseError::invalid(format!(
                "a URL template uses ${name}$ where it has no value"
            ))
        };
        let value = match name {
            "RepresentationID" => rep_id.to_string(),
            "Number" => format!("{:0width$}", number.ok_or_else(missing)?),
            "Bandwidth" => format!("{:0width$}", bandwidth.ok_or_else(missing)?),
            "Time" => format!("{:0width$}", time.ok_or_else(missing)?),
            _ => {
                return Err(ParseError::invalid(
                    "a URL template uses an unknown variable",
                ));
            }
        };
        out.push_str(&value);
    }
    out.push_str(rest);
    Ok(out)
}

/// `0Nd` or `Nd`: the zero-padded width of a template number.
fn parse_width(f: &str) -> Result<usize, ParseError> {
    let digits = f
        .strip_suffix('d')
        .ok_or_else(|| ParseError::invalid("a URL template number format isn't %0Nd"))?;
    if !digits.bytes().all(|b| b.is_ascii_digit()) {
        return Err(ParseError::invalid(
            "a URL template number format isn't %0Nd",
        ));
    }
    // "0" alone (no width) parses as zero.
    let n: usize = digits.trim_start_matches('0').parse().unwrap_or(0);
    if n > 10 {
        return Err(ParseError::invalid(
            "a URL template number is padded too wide",
        ));
    }
    Ok(n)
}

fn attr_u64(node: &Node, name: &str) -> Result<Option<u64>, ParseError> {
    match node.attr(name) {
        None => Ok(None),
        Some(v) => v
            .trim()
            .parse::<u64>()
            .map(Some)
            .map_err(|_| ParseError::invalid(format!("the attribute {name} isn't a number"))),
    }
}

/// `first-last` (inclusive) to offset and length.
fn parse_range(s: &str) -> Result<ByteRange, ParseError> {
    let bad = || ParseError::invalid("a byte range isn't first-last");
    let (a, b) = s.trim().split_once('-').ok_or_else(bad)?;
    let first: u64 = a.trim().parse().map_err(|_| bad())?;
    let last: u64 = b.trim().parse().map_err(|_| bad())?;
    if last < first {
        return Err(bad());
    }
    Ok(ByteRange {
        offset: first,
        length: last - first + 1,
    })
}

/// An ISO 8601 duration in seconds: `PnD`, `PTnH`, `PTnM`, `PTnS` and combinations. Years and
/// months have no fixed length and are refused.
pub(crate) fn iso_duration(s: &str) -> Result<f64, ParseError> {
    let bad = || ParseError::invalid("a duration isn't in ISO 8601 form");
    let body = s.trim().strip_prefix('P').ok_or_else(bad)?;
    let (date, time) = match body.split_once('T') {
        Some((d, t)) => (d, Some(t)),
        None => (body, None),
    };
    let mut secs = number_units(date, &[('D', 86_400.0)])?;
    if let Some(t) = time {
        secs += number_units(t, &[('H', 3_600.0), ('M', 60.0), ('S', 1.0)])?;
    }
    if !secs.is_finite() || secs < 0.0 {
        return Err(bad());
    }
    Ok(secs)
}

fn number_units(s: &str, units: &[(char, f64)]) -> Result<f64, ParseError> {
    let bad = || ParseError::invalid("a duration isn't in ISO 8601 form");
    let mut total = 0.0;
    let mut num = String::new();
    for c in s.chars() {
        if c.is_ascii_digit() || c == '.' {
            num.push(c);
        } else {
            let mult = units
                .iter()
                .find(|(u, _)| *u == c)
                .map(|(_, m)| *m)
                .ok_or_else(|| ParseError::unsupported("durations in years or months"))?;
            let v: f64 = num.parse().map_err(|_| bad())?;
            total += v * mult;
            num.clear();
        }
    }
    if !num.is_empty() {
        return Err(bad());
    }
    Ok(total)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> Url {
        Url::parse("https://cdn.example/vod/manifest.mpd").expect("URL")
    }

    fn lim() -> ParseLimits {
        ParseLimits::default()
    }

    const MPD: &str = r#"<?xml version="1.0"?>
<MPD xmlns="urn:mpeg:dash:schema:mpd:2011" type="static" mediaPresentationDuration="PT20S">
  <ProgramInformation><Title>Demo</Title></ProgramInformation>
  <Period id="1" start="PT0S" duration="PT10S">
    <AdaptationSet id="v" mimeType="video/mp4" codecs="avc1.4d401f" width="1280" height="720" lang="">
      <ContentProtection schemeIdUri="urn:mpeg:dash:mp4protection:2011" value="cenc"/>
      <SegmentTemplate timescale="1000" duration="2000" initialization="v/$RepresentationID$/init.mp4" media="v/$RepresentationID$/$Number%05d$.m4s" startNumber="1"/>
      <Representation id="hd" bandwidth="3000000"/>
    </AdaptationSet>
    <AdaptationSet id="a" mimeType="audio/mp4" codecs="mp4a.40.2" lang="en">
      <Role schemeIdUri="urn:mpeg:dash:role:2011" value="main"/>
      <AudioChannelConfiguration schemeIdUri="urn:mpeg:dash:23003-3:audio_channel_configuration:2011" value="2"/>
      <SegmentTemplate timescale="1000" presentationTimeOffset="0" initialization="a/init.mp4" media="a/$Time$.m4s">
        <SegmentTimeline><S t="0" d="3000" r="1"/><S d="2000"/></SegmentTimeline>
      </SegmentTemplate>
      <Representation id="aac" bandwidth="128000"/>
    </AdaptationSet>
  </Period>
  <Period id="2" duration="PT10S">
    <AdaptationSet id="v" mimeType="video/mp4" codecs="avc1.4d401f" width="1280" height="720">
      <SegmentTemplate timescale="1000" duration="2000" initialization="v/$RepresentationID$/init.mp4" media="v/$RepresentationID$/$Number%05d$.m4s" startNumber="6"/>
      <Representation id="hd" bandwidth="3000000"/>
    </AdaptationSet>
    <AdaptationSet id="s" mimeType="text/vtt" lang="de">
      <Representation id="de"><BaseURL>subs/</BaseURL><SegmentList duration="10000" timescale="1000"><SegmentURL media="de.vtt"/></SegmentList></Representation>
    </AdaptationSet>
  </Period>
</MPD>"#;

    #[test]
    fn durations() {
        assert_eq!(iso_duration("PT10S").expect("ok"), 10.0);
        assert_eq!(iso_duration("PT1H2M3.5S").expect("ok"), 3723.5);
        assert_eq!(iso_duration("P1DT1S").expect("ok"), 86_401.0);
        assert_eq!(iso_duration("PT0S").expect("ok"), 0.0);
        assert!(matches!(
            iso_duration("P1Y"),
            Err(ParseError::Unsupported(_))
        ));
        assert!(iso_duration("10S").is_err());
        assert!(iso_duration("PT5").is_err());
        assert!(iso_duration("PTxS").is_err());
    }

    #[test]
    fn templates_expand() {
        let s = expand(
            "$RepresentationID$/$Number%05d$-$Bandwidth$-$Time$-$$.m4s",
            "hd",
            Some(7),
            Some(1500),
            Some(42),
        )
        .expect("ok");
        assert_eq!(s, "hd/00007-1500-42-$.m4s");
        assert!(expand("$Number$", "x", None, None, None).is_err());
        assert!(expand("$Unknown$", "x", Some(1), None, None).is_err());
        assert!(expand("open $Number", "x", Some(1), None, None).is_err());
        assert!(expand("$Number%xd$", "x", Some(1), None, None).is_err());
        assert!(expand("$Number%0123456789012d$", "x", Some(1), None, None).is_err());
    }

    fn clear_mpd() -> String {
        MPD.replace(
            r#"<ContentProtection schemeIdUri="urn:mpeg:dash:mp4protection:2011" value="cenc"/>"#,
            "",
        )
    }

    #[test]
    fn full_mpd_with_periods_and_joins() {
        let p = parse(clear_mpd().as_bytes(), &base(), &lim()).expect("parses");
        assert_eq!(p.kind, ManifestKind::Dash);
        assert_eq!(p.liveness, Liveness::Vod);
        assert_eq!(p.duration, Some(20.0));
        assert_eq!(p.title.as_deref(), Some("Demo"));
        // One video track joined across both periods: 5 + 5 segments.
        assert_eq!(p.variants.len(), 1);
        let v = &p.variants[0].video;
        assert_eq!(v.resolution, Some((1280, 720)));
        assert_eq!(v.plan.segments.len(), 10);
        assert_eq!(v.plan.inits.len(), 1);
        assert_eq!(
            v.plan.segments[0].object.url.as_str(),
            "https://cdn.example/vod/v/hd/00001.m4s"
        );
        // Period 2 numbering continues from startNumber 6 and starts at 10 s.
        assert_eq!(
            v.plan.segments[5].object.url.as_str(),
            "https://cdn.example/vod/v/hd/00006.m4s"
        );
        assert_eq!(v.plan.segments[5].start, 10.0);
        assert_eq!(v.container, SegmentContainer::Fmp4);
        assert_eq!(v.protection, Protection::Clear);
    }

    #[test]
    fn cenc_marker_drops_the_video() {
        let p = parse(MPD.as_bytes(), &base(), &lim()).expect("parses");
        // The video track is one track across both periods; period 1 carries the CENC marker,
        // so the track is refused, not offered.
        assert!(p.variants.is_empty());
        assert_eq!(p.dropped_drm, 1);
    }

    #[test]
    fn protected_video_is_dropped_and_audio_kept() {
        let p = parse(MPD.as_bytes(), &base(), &lim()).expect("parses");
        // The adaptation set carries the CENC marker: the video track is DRM.
        assert_eq!(p.dropped_drm, 1);
        assert!(p.variants.is_empty());
        assert_eq!(p.audio.len(), 1);
        let a = &p.audio[0];
        assert_eq!(a.lang.as_deref(), Some("en"));
        assert!(a.is_default);
        assert_eq!(a.channels, Some(2));
        // Timeline: t=0 d=3000 r=1 gives two 3 s segments; then d=2000 continues at 6 s.
        let times: Vec<f64> = a.plan.segments.iter().map(|s| s.start).collect();
        assert_eq!(times, [0.0, 3.0, 6.0]);
        assert_eq!(a.plan.segments[2].duration, 2.0);
        assert_eq!(a.plan.segments[0].init, Some(0));
    }

    #[test]
    fn clear_mpd_without_drm() {
        let clear = MPD.replace(
            r#"<ContentProtection schemeIdUri="urn:mpeg:dash:mp4protection:2011" value="cenc"/>"#,
            "",
        );
        let p = parse(clear.as_bytes(), &base(), &lim()).expect("parses");
        assert_eq!(p.dropped_drm, 0);
        assert_eq!(p.variants.len(), 1);
        assert_eq!(p.variants[0].audio.len(), 1);
        assert_eq!(p.subtitles.len(), 1);
        let sub = &p.subtitles[0];
        assert_eq!(sub.container, SegmentContainer::Vtt);
        assert_eq!(
            sub.plan.segments[0].object.url.as_str(),
            "https://cdn.example/vod/subs/de.vtt"
        );
        assert_eq!(p.variants[0].subtitles, vec![sub.id.clone()]);
        assert!(!p.variants[0].muxed_audio);
    }

    #[test]
    fn segment_base_and_dynamic() {
        let single = r#"<MPD type="dynamic" mediaPresentationDuration="PT5S"><Period><AdaptationSet mimeType="video/mp4" codecs="avc1"><Representation id="1" bandwidth="1000" width="10" height="10"><BaseURL>movie.mp4</BaseURL><SegmentBase indexRange="0-100"><Initialization range="0-500"/></SegmentBase></Representation></AdaptationSet></Period></MPD>"#;
        let p = parse(single.as_bytes(), &base(), &lim()).expect("parses");
        assert_eq!(p.liveness, Liveness::Live);
        let v = &p.variants[0].video;
        assert!(v.plan.whole);
        assert_eq!(
            v.plan.segments[0].object.url.as_str(),
            "https://cdn.example/vod/movie.mp4"
        );
        assert_eq!(v.plan.segments[0].object.range, None);
    }

    #[test]
    fn refused_inputs_are_typed_errors() {
        let b = base();
        let l = lim();
        assert!(matches!(
            parse(b"<!DOCTYPE MPD><MPD/>", &b, &l),
            Err(ParseError::Doctype)
        ));
        assert!(parse(b"<html/>", &b, &l).is_err());
        assert!(parse(b"<MPD type=\"static\"/>", &b, &l).is_err());
        assert!(parse(b"<MPD><Period/></MPD>", &b, &l).is_err());
        let no_segments = r#"<MPD><Period><AdaptationSet mimeType="video/mp4"><Representation id="1" bandwidth="1"/></AdaptationSet></Period></MPD>"#;
        assert!(parse(no_segments.as_bytes(), &b, &l).is_err());
        let open_timeline = r#"<MPD><Period><AdaptationSet mimeType="video/mp4"><SegmentTemplate media="x$Number$" initialization="i"><SegmentTimeline><S t="0" d="10" r="-1"/></SegmentTimeline></SegmentTemplate><Representation id="1" bandwidth="1"/></AdaptationSet></Period></MPD>"#;
        assert!(matches!(
            parse(open_timeline.as_bytes(), &b, &l),
            Err(ParseError::Unsupported(_))
        ));
    }

    #[test]
    fn limits_are_enforced() {
        let b = base();
        let big = MPD.as_bytes();
        let tiny = ParseLimits {
            max_manifest_bytes: 100,
            ..lim()
        };
        assert!(matches!(
            parse(big, &b, &tiny),
            Err(ParseError::TooLarge { .. })
        ));
        let few_periods = ParseLimits {
            max_periods: 1,
            ..lim()
        };
        assert!(matches!(
            parse(big, &b, &few_periods),
            Err(ParseError::TooMany {
                what: "periods",
                ..
            })
        ));
        let few_segments = ParseLimits {
            max_segments_per_track: 3,
            ..lim()
        };
        let p = parse(MPD.as_bytes(), &b, &few_segments);
        assert!(
            matches!(
                p,
                Err(ParseError::TooMany {
                    what: "segments",
                    ..
                })
            ),
            "{p:?}"
        );
        let timeline = r#"<MPD><Period><AdaptationSet mimeType="audio/mp4" codecs="mp4a.40.2"><SegmentTemplate media="a$Number$" initialization="i"><SegmentTimeline><S t="0" d="1" r="999999"/></SegmentTimeline></SegmentTemplate><Representation id="1" bandwidth="1"/></AdaptationSet></Period></MPD>"#;
        assert!(matches!(
            parse(timeline.as_bytes(), &b, &few_segments),
            Err(ParseError::TooMany {
                what: "segments",
                ..
            })
        ));
        let few_tracks = ParseLimits {
            max_tracks: 1,
            ..lim()
        };
        assert!(matches!(
            parse(big, &b, &few_tracks),
            Err(ParseError::TooMany { what: "tracks", .. })
        ));
    }

    #[test]
    fn byte_ranges_in_lists() {
        assert_eq!(
            parse_range("0-499").expect("ok"),
            ByteRange {
                offset: 0,
                length: 500
            }
        );
        assert!(parse_range("5-4").is_err());
        assert!(parse_range("x").is_err());
        let list = r#"<MPD><Period><AdaptationSet mimeType="video/mp4" codecs="avc1"><SegmentList timescale="1" duration="2"><Initialization sourceURL="init.mp4" range="0-99"/><SegmentURL mediaRange="100-199" media="all.mp4"/><SegmentURL mediaRange="200-299" media="all.mp4"/></SegmentList><Representation id="1" bandwidth="9"/></AdaptationSet></Period></MPD>"#;
        let p = parse(list.as_bytes(), &base(), &lim()).expect("parses");
        let v = &p.variants[0].video;
        assert_eq!(
            v.plan.inits[0].range,
            Some(ByteRange {
                offset: 0,
                length: 100
            })
        );
        assert_eq!(
            v.plan.segments[1].object.range,
            Some(ByteRange {
                offset: 200,
                length: 100
            })
        );
        assert_eq!(v.plan.segments[1].start, 2.0);
    }
}

#[cfg(test)]
mod regressions {
    use super::*;

    fn parse_str(mpd: &str) -> Result<Presentation, ParseError> {
        parse(
            mpd.as_bytes(),
            &Url::parse("https://cdn.example/vod/manifest.mpd").expect("URL"),
            &ParseLimits::default(),
        )
    }

    #[test]
    fn template_attributes_are_inherited_level_by_level() {
        // The adaptation set has the template; the representation only overrides startNumber;
        // the period-level template supplies the timescale.
        let p = parse_str(
            r#"<MPD type="static" mediaPresentationDuration="PT8S"><Period>
                 <SegmentTemplate timescale="1000"/>
                 <AdaptationSet mimeType="video/mp4" codecs="avc1.4d401f">
                   <SegmentTemplate media="v/$RepresentationID$/$Number$.m4s" initialization="v/$RepresentationID$/init.mp4" duration="4000"/>
                   <Representation id="hd" bandwidth="1000" width="1280" height="720">
                     <SegmentTemplate startNumber="10"/>
                   </Representation>
                 </AdaptationSet>
               </Period></MPD>"#,
        )
        .expect("parses");
        let segs = &p.variants[0].video.plan.segments;
        assert_eq!(segs.len(), 2);
        assert!(segs[0].object.url.as_str().ends_with("/v/hd/10.m4s"));
        assert!(segs[1].object.url.as_str().ends_with("/v/hd/11.m4s"));
        assert!((segs[0].duration - 4.0).abs() < 1e-9);
        assert!(
            p.variants[0].video.plan.inits[0]
                .url
                .as_str()
                .ends_with("/v/hd/init.mp4")
        );
    }

    #[test]
    fn an_open_repeat_runs_up_to_the_next_entry() {
        let p = parse_str(
            r#"<MPD type="static" mediaPresentationDuration="PT100S"><Period><AdaptationSet mimeType="audio/mp4" codecs="mp4a.40.2"><SegmentTemplate timescale="1" media="a$Time$.m4s"><SegmentTimeline><S t="0" d="2" r="-1"/><S t="10" d="5"/></SegmentTimeline></SegmentTemplate><Representation id="a" bandwidth="1"/></AdaptationSet></Period></MPD>"#,
        )
        .expect("parses");
        let names: Vec<&str> = p.audio[0]
            .plan
            .segments
            .iter()
            .map(|s| s.object.url.path().rsplit('/').next().unwrap_or(""))
            .collect();
        assert_eq!(
            names,
            ["a0.m4s", "a2.m4s", "a4.m4s", "a6.m4s", "a8.m4s", "a10.m4s"]
        );
    }

    #[test]
    fn unaddressable_representations_are_skipped_and_webm_is_known() {
        let p = parse_str(
            r#"<MPD type="static" mediaPresentationDuration="PT4S"><Period>
                 <AdaptationSet mimeType="video/webm" codecs="vp9"><Representation id="v" bandwidth="9"><BaseURL>v.webm</BaseURL><SegmentBase indexRange="0-9"/></Representation></AdaptationSet>
                 <AdaptationSet mimeType="audio/mp4" codecs="mp4a.40.2"><Representation id="lost" bandwidth="9"/></AdaptationSet>
                 <AdaptationSet mimeType="application/mp4" codecs="stpp"><SegmentTemplate media="t$Number$.m4s" initialization="ti.mp4" duration="4"/><Representation id="t" bandwidth="1"/></AdaptationSet>
               </Period></MPD>"#,
        )
        .expect("parses");
        assert_eq!(p.variants.len(), 1);
        assert_eq!(p.variants[0].video.container, SegmentContainer::Webm);
        assert!(p.variants[0].video.plan.whole);
        assert!(
            p.audio.is_empty(),
            "the representation with no segments is skipped"
        );
        assert!(
            p.subtitles.is_empty(),
            "subtitles inside MP4 aren't offered"
        );
        assert_eq!(p.warnings.len(), 2, "{:?}", p.warnings);
    }

    #[test]
    fn undetermined_language_is_none() {
        let p = parse_str(
            r#"<MPD type="static" mediaPresentationDuration="PT4S"><Period><AdaptationSet mimeType="audio/mp4" codecs="mp4a.40.2" lang="und"><SegmentTemplate media="a$Number$.m4s" duration="4"/><Representation id="a" bandwidth="1"/></AdaptationSet></Period></MPD>"#,
        )
        .expect("parses");
        assert_eq!(p.audio[0].lang, None);
    }
}
