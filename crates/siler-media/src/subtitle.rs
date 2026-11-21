//! Subtitles: WebVTT segments merged into one track on the playlist timeline, and TTML
//! (DFXP, IMSC text profile) and subtitles carried in MP4 (`wvtt`, `stpp`) converted to
//! WebVTT. FFmpeg reads WebVTT but not TTML or `wvtt`, and segmented WebVTT needs each
//! segment's cues moved to where the segment plays.

use crate::error::ParseError;
use crate::limits::ParseLimits;
use crate::mp4::{self, TextFormat};
use crate::xml::{self, Node};

/// One WebVTT segment of a subtitle track, in playlist order.
#[derive(Debug, Clone, Copy)]
pub struct VttPart<'a> {
    pub body: &'a [u8],
}

/// The MPEG-TS clock rate and wrap-around of `X-TIMESTAMP-MAP`.
const MPEGTS_HZ: f64 = 90_000.0;
const MPEGTS_WRAP: u64 = 1 << 33;

/// A cue on the merged timeline.
struct Cue {
    start: f64,
    end: f64,
    /// The identifier lines, the settings after the end time, and the text lines.
    ids: Vec<String>,
    settings: String,
    text: Vec<String>,
}

/// Merges WebVTT segments into one WebVTT document. With `until`, cues are cut at that time
/// (the end of the video) and cues that start after it are left out.
///
/// Segmented WebVTT (HLS) states cue times on its own clock, tied to the video's MPEG-TS
/// clock by `X-TIMESTAMP-MAP=MPEGTS:…,LOCAL:…`. The video is rebuilt to start at zero.
/// `zero` is where it started on the MPEG-TS clock, in seconds: a cue is placed at its
/// MPEG-TS time minus that. Without `zero`, the first segment's mapping is taken as the
/// video's start (what most packagers write): a segment mapped the same keeps its times,
/// and one mapped later moves by the difference. Without a mapping, times are kept as they
/// are. Cues repeated across segment boundaries appear once.
pub fn merge_webvtt(
    parts: &[VttPart<'_>],
    zero: Option<f64>,
    until: Option<f64>,
) -> Result<String, ParseError> {
    let mut reference: Option<(u64, f64)> = None;
    let mut cues: Vec<Cue> = Vec::new();
    for part in parts {
        let text = String::from_utf8_lossy(part.body);
        let text = text
            .trim_start_matches('\u{feff}')
            .replace("\r\n", "\n")
            .replace('\r', "\n");
        if !text.starts_with("WEBVTT") {
            return Err(ParseError::invalid("a subtitle segment isn't WebVTT"));
        }
        let shift = match timestamp_map(&text) {
            Some((mpegts, local)) if zero.is_some() => {
                // The distance from the video's start on the 33-bit clock, either way round.
                let start = (zero.unwrap_or(0.0) * MPEGTS_HZ).round() as i64;
                let wrap = MPEGTS_WRAP as i64;
                let mut d = (mpegts as i64 - start).rem_euclid(wrap);
                if d > wrap / 2 {
                    d -= wrap;
                }
                d as f64 / MPEGTS_HZ - local
            }
            Some((mpegts, local)) => {
                let (ref_ts, ref_local) = *reference.get_or_insert((mpegts, local));
                // A later segment's clock may have wrapped past 2^33.
                let ts = if mpegts + MPEGTS_WRAP / 2 < ref_ts {
                    mpegts + MPEGTS_WRAP
                } else {
                    mpegts
                };
                (ts as f64 - ref_ts as f64) / MPEGTS_HZ - (local - ref_local)
            }
            None => 0.0,
        };
        for block in text.split("\n\n") {
            let lines: Vec<&str> = block.lines().collect();
            let first = lines.first().copied().unwrap_or("");
            if first.starts_with("WEBVTT")
                || first.starts_with("NOTE")
                || first.starts_with("STYLE")
                || first.starts_with("REGION")
            {
                continue;
            }
            let Some(idx) = lines.iter().position(|l| l.contains("-->")) else {
                continue;
            };
            let (a, b) = lines[idx].split_once("-->").unwrap_or(("", ""));
            let mut rest = b.trim().splitn(2, char::is_whitespace);
            let end_text = rest.next().unwrap_or("");
            let settings = rest.next().unwrap_or("").trim();
            let (Some(start), Some(end)) = (parse_time(a.trim()), parse_time(end_text)) else {
                return Err(ParseError::invalid("a subtitle cue has a bad time"));
            };
            let s = (start + shift).max(0.0);
            let e = (end + shift).max(0.0);
            if e <= s {
                continue;
            }
            let cue = Cue {
                start: s,
                end: e,
                ids: lines[..idx].iter().map(|l| l.to_string()).collect(),
                settings: settings.to_string(),
                text: lines[idx + 1..].iter().map(|l| l.to_string()).collect(),
            };
            let repeated = cues.iter().rev().take(16).any(|c| {
                (c.start - cue.start).abs() < 0.001
                    && (c.end - cue.end).abs() < 0.001
                    && c.text == cue.text
            });
            if !repeated {
                cues.push(cue);
            }
        }
    }
    if let Some(limit) = until {
        cues.retain(|c| c.start < limit);
        for c in &mut cues {
            c.end = c.end.min(limit);
        }
    }
    Ok(write_cues(cues))
}

/// A WebVTT document of `cues`, in start order.
fn write_cues(mut cues: Vec<Cue>) -> String {
    cues.sort_by(|a, b| a.start.total_cmp(&b.start));
    let mut out = String::from("WEBVTT\n\n");
    for c in cues {
        for id in &c.ids {
            out.push_str(id);
            out.push('\n');
        }
        out.push_str(&format_time(c.start));
        out.push_str(" --> ");
        out.push_str(&format_time(c.end));
        if !c.settings.is_empty() {
            out.push(' ');
            out.push_str(&c.settings);
        }
        out.push('\n');
        for l in &c.text {
            out.push_str(l);
            out.push('\n');
        }
        out.push('\n');
    }
    out
}

/// Converts a subtitle track carried in MP4 to WebVTT: `wvtt` cue boxes, or a TTML document
/// per sample (`stpp`). `init` is the initialisation segment (`None` when the first piece is a
/// whole file with its own header); `pieces` are the media segments in order.
///
/// Times come from the fragments' decode times, so they are on the track's media timeline.
/// `zero` is the time there (in seconds) that becomes zero in the output: the start of the
/// video the subtitles go with. Without it, the track's own first sample is zero.
///
/// A `wvtt` cue that spans several samples (it is repeated in each) comes out once. TTML
/// times are on the media timeline; a sample whose times only fit when read from the
/// sample's own start is taken that way.
pub fn mp4_to_webvtt(
    init: Option<&[u8]>,
    pieces: &[&[u8]],
    zero: Option<f64>,
    lim: &ParseLimits,
) -> Result<String, ParseError> {
    let header = init.or_else(|| pieces.first().copied()).unwrap_or(&[]);
    let track = mp4::text_track(header)?
        .ok_or_else(|| ParseError::unsupported("this kind of subtitles inside MP4"))?;
    let mut samples = Vec::new();
    let mut next = 0u64;
    for p in pieces {
        mp4::text_samples(&track, p, &mut next, &mut samples, lim)?;
    }
    let ts = track.timescale as f64;
    let zero = zero.unwrap_or_else(|| samples.first().map_or(0.0, |s| s.time as f64 / ts));
    let mut cues: Vec<Cue> = Vec::new();
    for s in &samples {
        let start = s.time as f64 / ts;
        let end = start + s.duration as f64 / ts;
        match track.format {
            TextFormat::Wvtt => {
                for (id, settings, text) in mp4::wvtt_cues(&s.data)? {
                    let text: Vec<String> = text
                        .replace("\r\n", "\n")
                        .lines()
                        .filter(|l| !l.trim().is_empty())
                        .map(str::to_string)
                        .collect();
                    if text.is_empty() {
                        continue;
                    }
                    let ids: Vec<String> = (!id.is_empty()).then_some(id).into_iter().collect();
                    add_cue(&mut cues, start - zero, end - zero, ids, settings, text);
                }
            }
            TextFormat::Stpp => {
                // A sample may carry images after the document: only the document is read.
                let doc = match s.data.windows(5).rposition(|w| w == b"</tt>") {
                    Some(i) => &s.data[..i + 5],
                    None => &s.data[..],
                };
                if doc.iter().all(|b| b.is_ascii_whitespace()) {
                    continue;
                }
                let found = ttml_cues(doc, lim)?;
                let within = |shift: f64| {
                    found
                        .iter()
                        .all(|c| c.0 + shift >= start - 0.5 && c.1 + shift <= end + 0.5)
                };
                let shift = if !within(0.0) && within(start) {
                    start
                } else {
                    0.0
                };
                for (a, b, text) in found {
                    let lines = text
                        .lines()
                        .map(str::trim)
                        .filter(|l| !l.is_empty())
                        .map(str::to_string)
                        .collect();
                    add_cue(
                        &mut cues,
                        a + shift - zero,
                        b + shift - zero,
                        Vec::new(),
                        String::new(),
                        lines,
                    );
                }
            }
        }
        if cues.len() > lim.max_segments_per_track {
            return Err(ParseError::TooMany {
                what: "subtitle cues",
                limit: lim.max_segments_per_track,
            });
        }
    }
    Ok(write_cues(cues))
}

/// Adds a cue, or lengthens a recent one with the same text that ends where this one starts
/// (a cue continued in the next sample), or skips it when it is already there.
fn add_cue(
    cues: &mut Vec<Cue>,
    start: f64,
    end: f64,
    ids: Vec<String>,
    settings: String,
    text: Vec<String>,
) {
    let (start, end) = (start.max(0.0), end.max(0.0));
    if end <= start {
        return;
    }
    let same = |c: &Cue| c.text == text && c.settings == settings && c.ids == ids;
    let continues = |c: &Cue| (c.end - start).abs() < 0.002;
    let inside = |c: &Cue| start >= c.start - 0.002 && end <= c.end + 0.002;
    if let Some(c) = cues
        .iter_mut()
        .rev()
        .take(16)
        .find(|c| same(c) && (continues(c) || inside(c)))
    {
        c.end = c.end.max(end);
        return;
    }
    cues.push(Cue {
        start,
        end,
        ids,
        settings,
        text,
    });
}

/// The `X-TIMESTAMP-MAP` of a WebVTT header: (MPEG-TS time, local time in seconds).
fn timestamp_map(text: &str) -> Option<(u64, f64)> {
    let header = text.split("\n\n").next()?;
    let map = header
        .lines()
        .find_map(|l| l.trim().strip_prefix("X-TIMESTAMP-MAP="))?;
    let mut mpegts = None;
    let mut local = None;
    for kv in map.split(',') {
        let kv = kv.trim();
        if let Some(v) = kv.strip_prefix("MPEGTS:") {
            mpegts = v.trim().parse::<u64>().ok();
        } else if let Some(v) = kv.strip_prefix("LOCAL:") {
            local = parse_time(v);
        }
    }
    Some((mpegts.unwrap_or(0), local.unwrap_or(0.0)))
}

/// `[HH:]MM:SS.mmm` (or with a comma) to seconds.
pub(crate) fn parse_time(s: &str) -> Option<f64> {
    let s = s.trim();
    let mut parts: Vec<&str> = s.split(':').collect();
    if parts.len() < 2 || parts.len() > 3 {
        return None;
    }
    let last = parts.pop()?.replace(',', ".");
    let secs: f64 = last.parse().ok()?;
    if !secs.is_finite() || secs < 0.0 {
        return None;
    }
    let mut total = secs;
    let mut mult = 60.0;
    for p in parts.iter().rev() {
        let v: f64 = p.parse().ok()?;
        if !v.is_finite() || v < 0.0 {
            return None;
        }
        total += v * mult;
        mult *= 60.0;
    }
    Some(total)
}

/// Seconds to `HH:MM:SS.mmm`.
pub(crate) fn format_time(secs: f64) -> String {
    let ms = (secs * 1000.0).round().max(0.0) as u64;
    let h = ms / 3_600_000;
    let m = (ms / 60_000) % 60;
    let s = (ms / 1000) % 60;
    format!("{h:02}:{m:02}:{s:02}.{:03}", ms % 1000)
}

/// Converts a TTML document (`<tt>` with `<p>` cues) to WebVTT. Times may be clock times,
/// offsets in hours, minutes, seconds, milliseconds, frames or ticks, or `hh:mm:ss:ff` with
/// frames (at the document's `frameRate`, 30 when it says none); a `begin` on an enclosing
/// `body` or `div` moves the cues inside it.
pub fn ttml_to_webvtt(body: &[u8], lim: &ParseLimits) -> Result<String, ParseError> {
    let mut cues = ttml_cues(body, lim)?;
    cues.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut out = String::from("WEBVTT\n\n");
    for (start, end, text) in cues {
        out.push_str(&format_time(start));
        out.push_str(" --> ");
        out.push_str(&format_time(end));
        out.push('\n');
        // Blank lines would end the cue early.
        for line in text.lines().map(str::trim).filter(|l| !l.is_empty()) {
            out.push_str(line);
            out.push('\n');
        }
        out.push('\n');
    }
    Ok(out)
}

/// The cues of a TTML document: (start, end, text), times in seconds as the document states
/// them.
fn ttml_cues(body: &[u8], lim: &ParseLimits) -> Result<Vec<(f64, f64, String)>, ParseError> {
    let root = xml::parse(body, lim)?;
    if root.name != "tt" {
        return Err(ParseError::invalid("the subtitle file isn't TTML"));
    }
    let rates = Rates::of(&root)?;
    let mut cues: Vec<(f64, f64, String)> = Vec::new();
    collect(&root, 0.0, &rates, &mut cues, lim)?;
    Ok(cues)
}

/// The frame and tick rates a TTML document's times are counted in.
struct Rates {
    frames: f64,
    ticks: f64,
}

impl Rates {
    fn of(root: &Node) -> Result<Rates, ParseError> {
        let bad = || ParseError::invalid("the subtitle file's frame or tick rate isn't a number");
        let positive = |v: f64| (v.is_finite() && v > 0.0).then_some(v);
        let base = match root.attr("frameRate") {
            Some(f) => f
                .trim()
                .parse::<f64>()
                .ok()
                .and_then(positive)
                .ok_or_else(bad)?,
            None => 30.0,
        };
        let multiplier = match root.attr("frameRateMultiplier") {
            Some(m) => {
                let mut it = m.split_whitespace().map(|n| n.parse::<f64>().ok());
                match (it.next().flatten(), it.next().flatten()) {
                    (Some(n), Some(d)) if n > 0.0 && d > 0.0 => n / d,
                    _ => return Err(bad()),
                }
            }
            None => 1.0,
        };
        let frames = base * multiplier;
        let sub = match root.attr("subFrameRate") {
            Some(s) => s
                .trim()
                .parse::<f64>()
                .ok()
                .and_then(positive)
                .ok_or_else(bad)?,
            None => 1.0,
        };
        let ticks = match root.attr("tickRate") {
            Some(t) => t
                .trim()
                .parse::<f64>()
                .ok()
                .and_then(positive)
                .ok_or_else(bad)?,
            None if root.attr("frameRate").is_some() => frames * sub,
            None => 1.0,
        };
        Ok(Rates { frames, ticks })
    }
}

fn collect(
    node: &Node,
    offset: f64,
    rates: &Rates,
    out: &mut Vec<(f64, f64, String)>,
    lim: &ParseLimits,
) -> Result<(), ParseError> {
    for child in &node.children {
        if child.name == "p" {
            let Some(begin) = child.attr("begin") else {
                continue;
            };
            let start = offset + clock(begin, rates)?;
            let end = match (child.attr("end"), child.attr("dur")) {
                (Some(e), _) => offset + clock(e, rates)?,
                (None, Some(d)) => start + clock(d, rates)?,
                (None, None) => continue,
            };
            if end > start {
                out.push((start, end, child.text.clone()));
                if out.len() > lim.max_segments_per_track {
                    return Err(ParseError::TooMany {
                        what: "subtitle cues",
                        limit: lim.max_segments_per_track,
                    });
                }
            }
        } else {
            let inner = match child.attr("begin") {
                Some(b) if matches!(child.name.as_str(), "body" | "div") => {
                    offset + clock(b, rates)?
                }
                _ => offset,
            };
            collect(child, inner, rates, out, lim)?;
        }
    }
    Ok(())
}

/// A TTML time: `hh:mm:ss(.fraction)`, `hh:mm:ss:frames`, `mm:ss`, or a number with `h`, `m`,
/// `s`, `ms`, `f` (frames) or `t` (ticks).
fn clock(s: &str, rates: &Rates) -> Result<f64, ParseError> {
    let s = s.trim();
    let bad = || ParseError::invalid("a subtitle has a time that isn't understood");
    let number = |n: &str| -> Option<f64> {
        (!n.is_empty() && n.chars().all(|c| c.is_ascii_digit() || c == '.'))
            .then(|| n.parse::<f64>().ok())
            .flatten()
            .filter(|v| v.is_finite())
    };
    for (unit, per_second) in [
        ("ms", 1000.0),
        ("h", 1.0 / 3600.0),
        ("m", 1.0 / 60.0),
        ("s", 1.0),
        ("f", rates.frames),
        ("t", rates.ticks),
    ] {
        if let Some(n) = s.strip_suffix(unit).and_then(number) {
            return Ok(n / per_second);
        }
    }
    let parts: Vec<&str> = s.split(':').collect();
    match parts.len() {
        2 | 3 => parse_time(s).ok_or_else(bad),
        4 => {
            // hh:mm:ss:frames (a sub-frame part after a dot is ignored).
            let hms = parse_time(&parts[..3].join(":")).ok_or_else(bad)?;
            let frames = parts[3]
                .split('.')
                .next()
                .and_then(number)
                .ok_or_else(bad)?;
            Ok(hms + frames / rates.frames)
        }
        _ => Err(bad()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn part(body: &[u8]) -> VttPart<'_> {
        VttPart { body }
    }

    #[test]
    fn segment_cues_keep_their_stream_times() {
        // Every segment maps the same clocks (what packagers write): times are kept.
        let a = b"WEBVTT\nX-TIMESTAMP-MAP=MPEGTS:900000,LOCAL:00:00:00.000\n\n00:00:01.000 --> 00:00:02.500 align:start\nhello\n";
        let b = b"WEBVTT\nX-TIMESTAMP-MAP=MPEGTS:900000,LOCAL:00:00:00.000\n\nNOTE skip me --> nothing\n\n00:00:04.000 --> 00:00:05.000\nworld\n";
        let out = merge_webvtt(&[part(a), part(b)], None, None).expect("merges");
        assert!(out.starts_with("WEBVTT\n\n"));
        assert!(out.contains("00:00:01.000 --> 00:00:02.500 align:start\nhello\n"));
        assert!(out.contains("00:00:04.000 --> 00:00:05.000\nworld\n"));
        assert!(!out.contains("NOTE"));
        assert!(!out.contains("X-TIMESTAMP-MAP"));
    }

    #[test]
    fn a_later_mapping_moves_its_segment() {
        // The second segment's local zero is 4 s later on the MPEG-TS clock.
        let a = b"WEBVTT\nX-TIMESTAMP-MAP=MPEGTS:126000,LOCAL:00:00:00.000\n\n00:00:00.500 --> 00:00:01.000\nfirst\n";
        let b = b"WEBVTT\nX-TIMESTAMP-MAP=MPEGTS:486000,LOCAL:00:00:00.000\n\n00:00:00.500 --> 00:00:01.000\nsecond\n";
        let c = b"WEBVTT\nX-TIMESTAMP-MAP=LOCAL:00:00:10.000,MPEGTS:126000\n\n00:00:10.000 --> 00:00:11.000\nlocal\n";
        let out = merge_webvtt(&[part(a), part(b), part(c)], None, None).expect("merges");
        assert!(out.contains("00:00:00.500 --> 00:00:01.000\nfirst"));
        assert!(out.contains("00:00:04.500 --> 00:00:05.000\nsecond"));
        assert!(out.contains("00:00:00.000 --> 00:00:01.000\nlocal"));
        // A wrapped MPEG-TS clock still counts forward.
        let late = b"WEBVTT\nX-TIMESTAMP-MAP=MPEGTS:8589844592,LOCAL:00:00:00.000\n\n00:00:00.000 --> 00:00:01.000\nx\n";
        let wrapped = b"WEBVTT\nX-TIMESTAMP-MAP=MPEGTS:90000,LOCAL:00:00:00.000\n\n00:00:00.000 --> 00:00:01.000\ny\n";
        let out = merge_webvtt(&[part(late), part(wrapped)], None, None).expect("merges");
        assert!(out.contains("00:00:02.000 --> 00:00:03.000\ny"), "{out}");
    }

    #[test]
    fn repeated_cues_appear_once_and_bad_input_is_refused() {
        let a = b"WEBVTT\n\n00:00:01.000 --> 00:00:03.000\nacross\n";
        let out = merge_webvtt(&[part(a), part(a)], None, None).expect("merges");
        assert_eq!(out.matches("across").count(), 1);
        assert!(merge_webvtt(&[part(b"<html>")], None, None).is_err());
        assert!(merge_webvtt(&[part(b"WEBVTT\n\n00:99 --> nope\nx\n")], None, None).is_err());
    }

    #[test]
    fn times() {
        assert_eq!(parse_time("01:02:03.500"), Some(3723.5));
        assert_eq!(parse_time("02:03,250"), Some(123.25));
        assert_eq!(parse_time("1:2:3:4"), None);
        assert_eq!(parse_time("-1:00"), None);
        assert_eq!(format_time(3723.5), "01:02:03.500");
        assert_eq!(format_time(-5.0), "00:00:00.000");
    }

    #[test]
    fn ttml_becomes_webvtt() {
        let doc = br#"<?xml version="1.0"?>
<tt xmlns="http://www.w3.org/ns/ttml" xml:lang="en">
  <head><styling><style xml:id="s"/></styling></head>
  <body><div>
    <p begin="00:00:02.000" end="00:00:03.000">Hello &amp; welcome<br/>second line</p>
    <p begin="1s" dur="500ms">Short</p>
    <p begin="00:00:05.000">no end: skipped</p>
  </div></body>
</tt>"#;
        let out = ttml_to_webvtt(doc, &ParseLimits::default()).expect("converts");
        assert!(out.contains("00:00:01.000 --> 00:00:01.500\nShort\n"));
        assert!(out.contains("00:00:02.000 --> 00:00:03.000\nHello & welcome\nsecond line\n"));
        assert!(!out.contains("skipped"));
        assert!(out.find("Short").expect("present") < out.find("Hello").expect("present"));
    }

    #[test]
    fn ttml_frames_ticks_and_offsets() {
        let ticks = br#"<tt ttp:tickRate="10000000" xmlns:ttp="http://www.w3.org/ns/ttml#parameter"><body><div><p begin="10000000t" end="25000000t">ticks</p></div></body></tt>"#;
        let out = ttml_to_webvtt(ticks, &ParseLimits::default()).expect("converts");
        assert!(
            out.contains("00:00:01.000 --> 00:00:02.500\nticks"),
            "{out}"
        );
        let frames = br#"<tt ttp:frameRate="25"><body><div begin="10s"><p begin="00:00:01:12" end="50f">frames</p></div></body></tt>"#;
        let out = ttml_to_webvtt(frames, &ParseLimits::default()).expect("converts");
        assert!(
            out.contains("00:00:11.480 --> 00:00:12.000\nframes"),
            "{out}"
        );
    }

    #[test]
    fn ttml_refusals() {
        assert!(ttml_to_webvtt(b"<html/>", &ParseLimits::default()).is_err());
        let bad = br#"<tt><body><div><p begin="soon" end="later">x</p></div></body></tt>"#;
        assert!(ttml_to_webvtt(bad, &ParseLimits::default()).is_err());
        let rate = br#"<tt ttp:tickRate="0"><body/></tt>"#;
        assert!(ttml_to_webvtt(rate, &ParseLimits::default()).is_err());
    }

    // ------------------------------------------------------------ subtitles in MP4

    use crate::mp4::build::{cue, empty, fragment, init};

    #[test]
    fn wvtt_cues_continued_across_fragments_come_out_once() {
        let init = init(b"wvtt", 1, 1000);
        // "hello" runs 1.5–5 s, split by the fragment boundary at 4 s; "bye" is its own cue.
        let a = fragment(
            1,
            10_000,
            &[(1500, empty()), (2500, cue("1", "align:start", "hello"))],
        );
        let b = fragment(
            1,
            14_000,
            &[
                (1000, cue("1", "align:start", "hello")),
                (500, empty()),
                (1000, cue("", "", "bye\nnow")),
            ],
        );
        // Zero is the video's start, 10 s on this timeline.
        let out = mp4_to_webvtt(Some(&init), &[&a, &b], Some(10.0), &ParseLimits::default())
            .expect("converts");
        assert_eq!(
            out,
            "WEBVTT\n\n1\n00:00:01.500 --> 00:00:05.000 align:start\nhello\n\n00:00:05.500 --> 00:00:06.500\nbye\nnow\n\n"
        );
        // Without a reference, the track's first sample is zero.
        let out =
            mp4_to_webvtt(Some(&init), &[&a, &b], None, &ParseLimits::default()).expect("converts");
        assert!(out.contains("00:00:01.500 --> 00:00:05.000"), "{out}");
    }

    #[test]
    fn a_whole_file_carries_its_own_header() {
        let file = [
            init(b"wvtt", 1, 1000),
            fragment(1, 0, &[(2000, cue("", "", "one"))]),
        ]
        .concat();
        let out = mp4_to_webvtt(None, &[&file], None, &ParseLimits::default()).expect("converts");
        assert!(out.contains("00:00:00.000 --> 00:00:02.000\none"), "{out}");
    }

    #[test]
    fn stpp_times_on_the_timeline_or_from_the_sample_start() {
        let init = init(b"stpp", 1, 1000);
        let doc = |b: &str, e: &str, t: &str| {
            format!(r#"<?xml version="1.0"?><tt xmlns="http://www.w3.org/ns/ttml"><body><div><p begin="{b}" end="{e}">{t}</p></div></body></tt>"#).into_bytes()
        };
        // Sample 1 (0–4 s) states media times; sample 2 (4–8 s) counts from its own start.
        let a = fragment(
            1,
            0,
            &[(4000, doc("00:00:01.000", "00:00:02.000", "first"))],
        );
        let b = fragment(
            1,
            4000,
            &[(4000, doc("00:00:00.500", "00:00:01.500", "second"))],
        );
        let out = mp4_to_webvtt(Some(&init), &[&a, &b], Some(0.0), &ParseLimits::default())
            .expect("converts");
        assert!(
            out.contains("00:00:01.000 --> 00:00:02.000\nfirst"),
            "{out}"
        );
        assert!(
            out.contains("00:00:04.500 --> 00:00:05.500\nsecond"),
            "{out}"
        );
    }

    #[test]
    fn mp4_without_a_text_track_is_unsupported() {
        let err = mp4_to_webvtt(
            Some(&init(b"avc1", 1, 1000)),
            &[],
            None,
            &ParseLimits::default(),
        );
        assert!(matches!(err, Err(ParseError::Unsupported(_))), "{err:?}");
    }

    #[test]
    fn cues_stop_at_the_end_of_the_video() {
        let a = b"WEBVTT\n\n00:00:01.000 --> 00:00:02.000\nin\n\n00:00:58.500 --> 00:01:01.353\nacross the end\n\n00:01:00.500 --> 00:01:02.000\nafter\n";
        let out = merge_webvtt(&[part(a)], None, Some(60.0)).expect("merges");
        assert!(out.contains("00:00:01.000 --> 00:00:02.000\nin"));
        assert!(
            out.contains("00:00:58.500 --> 00:01:00.000\nacross the end"),
            "{out}"
        );
        assert!(!out.contains("after"));
    }

    #[test]
    fn mapped_cues_are_placed_against_the_video_start() {
        // Unified Streaming: local zero is 10 s on the MPEG-TS clock, the cue says 61 s, and
        // the video starts at 70 s: the cue plays 1 s into the file.
        let a = b"WEBVTT\nX-TIMESTAMP-MAP=MPEGTS:900000,LOCAL:00:00:00.000\n\n00:01:01.000 --> 00:01:03.000\nThis is pretty freaky.\n";
        let out = merge_webvtt(&[part(a)], Some(70.0), None).expect("merges");
        assert!(
            out.contains("00:00:01.000 --> 00:00:03.000\nThis is pretty freaky."),
            "{out}"
        );
        // The usual case (the map names the video's first timestamp) is unchanged.
        let b = b"WEBVTT\nX-TIMESTAMP-MAP=MPEGTS:144000,LOCAL:00:00:00.000\n\n00:00:00.500 --> 00:00:01.500\nx\n";
        let out = merge_webvtt(&[part(b)], Some(1.6), None).expect("merges");
        assert!(out.contains("00:00:00.500 --> 00:00:01.500"), "{out}");
        // A map just past the 33-bit wrap, for a video that started just before it.
        let c = b"WEBVTT\nX-TIMESTAMP-MAP=MPEGTS:90000,LOCAL:00:00:00.000\n\n00:00:00.000 --> 00:00:01.000\nwrapped\n";
        let before_wrap = (MPEGTS_WRAP - 90_000) as f64 / MPEGTS_HZ;
        let out = merge_webvtt(&[part(c)], Some(before_wrap), None).expect("merges");
        assert!(
            out.contains("00:00:02.000 --> 00:00:03.000\nwrapped"),
            "{out}"
        );
    }
}
