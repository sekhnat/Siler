//! Subtitles: WebVTT segments merged into one track on the playlist timeline, and TTML
//! (DFXP, IMSC text profile) converted to WebVTT. FFmpeg reads WebVTT but not TTML, and
//! segmented WebVTT needs each segment's cues moved to where the segment plays.

use crate::error::ParseError;
use crate::limits::ParseLimits;
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

/// Merges WebVTT segments into one WebVTT document.
///
/// Segmented WebVTT (HLS) states cue times on the stream's own timeline, tied to the video by
/// `X-TIMESTAMP-MAP=MPEGTS:…,LOCAL:…`. The video is rebuilt to start at zero, so the cues
/// are placed relative to the first segment's mapping: a segment whose mapping says the same
/// as the first one keeps its times, and one mapped later moves by the difference. Without a
/// mapping, times are kept as they are. Cues repeated across segment boundaries appear once.
pub fn merge_webvtt(parts: &[VttPart<'_>]) -> Result<String, ParseError> {
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
    Ok(out)
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
    let root = xml::parse(body, lim)?;
    if root.name != "tt" {
        return Err(ParseError::invalid("the subtitle file isn't TTML"));
    }
    let rates = Rates::of(&root)?;
    let mut cues: Vec<(f64, f64, String)> = Vec::new();
    collect(&root, 0.0, &rates, &mut cues, lim)?;
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
        let out = merge_webvtt(&[part(a), part(b)]).expect("merges");
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
        let out = merge_webvtt(&[part(a), part(b), part(c)]).expect("merges");
        assert!(out.contains("00:00:00.500 --> 00:00:01.000\nfirst"));
        assert!(out.contains("00:00:04.500 --> 00:00:05.000\nsecond"));
        assert!(out.contains("00:00:00.000 --> 00:00:01.000\nlocal"));
        // A wrapped MPEG-TS clock still counts forward.
        let late = b"WEBVTT\nX-TIMESTAMP-MAP=MPEGTS:8589844592,LOCAL:00:00:00.000\n\n00:00:00.000 --> 00:00:01.000\nx\n";
        let wrapped = b"WEBVTT\nX-TIMESTAMP-MAP=MPEGTS:90000,LOCAL:00:00:00.000\n\n00:00:00.000 --> 00:00:01.000\ny\n";
        let out = merge_webvtt(&[part(late), part(wrapped)]).expect("merges");
        assert!(out.contains("00:00:02.000 --> 00:00:03.000\ny"), "{out}");
    }

    #[test]
    fn repeated_cues_appear_once_and_bad_input_is_refused() {
        let a = b"WEBVTT\n\n00:00:01.000 --> 00:00:03.000\nacross\n";
        let out = merge_webvtt(&[part(a), part(a)]).expect("merges");
        assert_eq!(out.matches("across").count(), 1);
        assert!(merge_webvtt(&[part(b"<html>")]).is_err());
        assert!(merge_webvtt(&[part(b"WEBVTT\n\n00:99 --> nope\nx\n")]).is_err());
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
}
