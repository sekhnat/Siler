//! Subtitles: WebVTT segments merged into one track on the playlist timeline, and TTML
//! (DFXP, IMSC text profile) converted to WebVTT. FFmpeg reads WebVTT but not TTML, and
//! segmented WebVTT needs each segment's cues moved to where the segment plays.

use crate::error::ParseError;
use crate::limits::ParseLimits;
use crate::xml::{self, Node};

/// One WebVTT segment and where it plays on the presentation timeline.
#[derive(Debug, Clone, Copy)]
pub struct VttPart<'a> {
    /// Presentation time of the segment's start, in seconds.
    pub start: f64,
    pub body: &'a [u8],
}

/// Merges WebVTT segments into one WebVTT document. Each segment's cue times are relative to
/// its `X-TIMESTAMP-MAP` `LOCAL` time (zero when absent) and are moved to `start`.
pub fn merge_webvtt(parts: &[VttPart<'_>]) -> Result<String, ParseError> {
    let mut out = String::from("WEBVTT\n\n");
    for part in parts {
        let text = String::from_utf8_lossy(part.body);
        let text = text
            .trim_start_matches('\u{feff}')
            .replace("\r\n", "\n")
            .replace('\r', "\n");
        if !text.starts_with("WEBVTT") {
            return Err(ParseError::invalid("a subtitle segment isn't WebVTT"));
        }
        let local = text
            .lines()
            .find_map(|l| l.strip_prefix("X-TIMESTAMP-MAP="))
            .and_then(|m| m.split(',').find_map(|kv| kv.trim().strip_prefix("LOCAL:")))
            .and_then(parse_time)
            .unwrap_or(0.0);
        let shift = part.start - local;
        for block in text.split("\n\n") {
            let lines: Vec<&str> = block.lines().collect();
            let first = lines.first().copied().unwrap_or("");
            if first.starts_with("NOTE")
                || first.starts_with("STYLE")
                || first.starts_with("REGION")
            {
                continue;
            }
            let Some(idx) = lines.iter().position(|l| l.contains("-->")) else {
                continue;
            };
            let timing = lines[idx];
            let (a, b) = timing.split_once("-->").unwrap_or(("", ""));
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
            for id in &lines[..idx] {
                out.push_str(id);
                out.push('\n');
            }
            out.push_str(&format_time(s));
            out.push_str(" --> ");
            out.push_str(&format_time(e));
            if !settings.is_empty() {
                out.push(' ');
                out.push_str(settings);
            }
            out.push('\n');
            for l in &lines[idx + 1..] {
                out.push_str(l);
                out.push('\n');
            }
            out.push('\n');
        }
    }
    Ok(out)
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

/// Converts a TTML document (`<tt>` with `<p>` cues) to WebVTT. Timing in frames or ticks
/// and SMPTE time codes are refused: converting them needs the frame rate and tick rate,
/// which these files don't reliably say.
pub fn ttml_to_webvtt(body: &[u8], lim: &ParseLimits) -> Result<String, ParseError> {
    let root = xml::parse(body, lim)?;
    if root.name != "tt" {
        return Err(ParseError::invalid("the subtitle file isn't TTML"));
    }
    let mut cues: Vec<(f64, f64, String)> = Vec::new();
    collect(&root, &mut cues, lim)?;
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

fn collect(
    node: &Node,
    out: &mut Vec<(f64, f64, String)>,
    lim: &ParseLimits,
) -> Result<(), ParseError> {
    for child in &node.children {
        if child.name == "p" {
            let Some(begin) = child.attr("begin") else {
                continue;
            };
            let start = clock(begin)?;
            let end = match (child.attr("end"), child.attr("dur")) {
                (Some(e), _) => clock(e)?,
                (None, Some(d)) => start + clock(d)?,
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
            collect(child, out, lim)?;
        }
    }
    Ok(())
}

/// A TTML clock time: `hh:mm:ss(.fraction)`, `mm:ss`, or `n` with `h`, `m`, `s`, `ms`.
fn clock(s: &str) -> Result<f64, ParseError> {
    let s = s.trim();
    let bad = || ParseError::invalid("a subtitle has a time that isn't understood");
    for (unit, mult) in [("ms", 0.001), ("h", 3600.0), ("m", 60.0), ("s", 1.0)] {
        if let Some(n) = s
            .strip_suffix(unit)
            .filter(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit() || c == '.'))
        {
            return Ok(n.parse::<f64>().map_err(|_| bad())? * mult);
        }
    }
    if s.ends_with('f') || s.ends_with('t') {
        return Err(ParseError::unsupported(
            "subtitle timing in frames or ticks",
        ));
    }
    let parts: Vec<&str> = s.split(':').collect();
    match parts.len() {
        2 | 3 => {
            let secs = parse_time(s).ok_or_else(bad)?;
            Ok(secs)
        }
        _ => Err(ParseError::unsupported(
            "subtitle timing with SMPTE time codes",
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vtt_segments_land_on_the_timeline() {
        let a = b"WEBVTT\nX-TIMESTAMP-MAP=MPEGTS:900000,LOCAL:00:00:00.000\n\n00:00:01.000 --> 00:00:02.500 align:start\nhello\n";
        let b = b"WEBVTT\n\nNOTE skip me --> nothing\n\n00:00:00.000 --> 00:00:01.000\nworld\n";
        // Its LOCAL time is 5 s, so this segment's cue ends before the timeline starts.
        let c = b"WEBVTT\nX-TIMESTAMP-MAP=LOCAL:00:00:05.000,MPEGTS:0\n\n00:00:00.000 --> 00:00:01.000\nearly\n";
        let out = merge_webvtt(&[
            VttPart {
                start: 0.0,
                body: a,
            },
            VttPart {
                start: 4.0,
                body: b,
            },
            VttPart {
                start: 0.0,
                body: c,
            },
        ])
        .expect("merges");
        assert!(out.starts_with("WEBVTT\n\n"));
        assert!(out.contains("00:00:01.000 --> 00:00:02.500 align:start\nhello\n"));
        assert!(out.contains("00:00:04.000 --> 00:00:05.000\nworld\n"));
        // A cue that ends before the timeline starts is dropped.
        assert!(!out.contains("early"));
        assert!(!out.contains("NOTE"));
    }

    #[test]
    fn local_time_moves_the_segment() {
        let body = b"WEBVTT\nX-TIMESTAMP-MAP=LOCAL:00:00:10.000,MPEGTS:0\n\n00:00:10.000 --> 00:00:11.000\nlater\n";
        let out = merge_webvtt(&[VttPart { start: 3.0, body }]).expect("merges");
        assert!(out.contains("00:00:03.000 --> 00:00:04.000\nlater"));
        assert!(
            merge_webvtt(&[VttPart {
                start: 0.0,
                body: b"<html>"
            }])
            .is_err()
        );
        assert!(
            merge_webvtt(&[VttPart {
                start: 0.0,
                body: b"WEBVTT\n\n00:99 --> nope\nx\n"
            }])
            .is_err()
        );
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
    fn ttml_refusals() {
        let frames =
            br#"<tt><body><div><p begin="00:00:01:12" end="00:00:02:00">x</p></div></body></tt>"#;
        assert!(matches!(
            ttml_to_webvtt(frames, &ParseLimits::default()),
            Err(ParseError::Unsupported(_))
        ));
        let ticks = br#"<tt><body><div><p begin="10t" end="20t">x</p></div></body></tt>"#;
        assert!(matches!(
            ttml_to_webvtt(ticks, &ParseLimits::default()),
            Err(ParseError::Unsupported(_))
        ));
        assert!(ttml_to_webvtt(b"<html/>", &ParseLimits::default()).is_err());
        let bad = br#"<tt><body><div><p begin="soon" end="later">x</p></div></body></tt>"#;
        assert!(ttml_to_webvtt(bad, &ParseLimits::default()).is_err());
    }
}
