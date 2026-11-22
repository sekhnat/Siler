//! Adobe HTTP Dynamic Streaming: an F4M manifest, a bootstrap (`abst` box) per bitrate that
//! numbers the fragments, and F4F fragments whose `mdat` boxes hold FLV tags. Joined after
//! an FLV header, the tags are an ordinary FLV file FFmpeg copies into MP4 or Matroska.
//!
//! The box layouts follow Adobe's F4V/F4F specification (as read by yt-dlp's f4m
//! downloader, which this follows). DRM-protected media (`drmAdditionalHeader`) is refused.

use base64::Engine as _;
use std::collections::HashMap;
use url::Url;

use crate::error::ParseError;
use crate::limits::ParseLimits;
use crate::model::{
    Liveness, ManifestKind, Object, Presentation, Protection, Segment, SegmentContainer,
    SegmentPlan, Track, TrackId, TrackKind, Variant,
};

/// The header of an FLV file with audio and video, followed by the first (zero) previous
/// tag size.
pub const FLV_HEADER: [u8; 13] = [
    b'F', b'L', b'V', 0x01, 0x05, 0x00, 0x00, 0x00, 0x09, 0x00, 0x00, 0x00, 0x00,
];

/// Where a media's bootstrap comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Bootstrap {
    Inline(Vec<u8>),
    Url(Url),
}

#[derive(Debug, Clone)]
struct Media {
    url: String,
    bitrate: Option<u64>,
    width: Option<u32>,
    height: Option<u32>,
    bootstrap: Option<String>,
    drm: bool,
}

/// A parsed F4M manifest, before its bootstraps are fetched.
#[derive(Debug, Clone)]
pub struct Manifest {
    base: Url,
    query: Option<String>,
    live: bool,
    duration: Option<f64>,
    media: Vec<Media>,
    bootstraps: HashMap<String, Bootstrap>,
}

/// Reads an F4M manifest (version 1 or 2, without nested manifests).
pub fn parse_manifest(body: &[u8], url: &Url, lim: &ParseLimits) -> Result<Manifest, ParseError> {
    if body.len() > lim.max_manifest_bytes {
        return Err(ParseError::TooLarge {
            limit: lim.max_manifest_bytes,
        });
    }
    let root = crate::xml::parse(body, lim)?;
    if root.name != "manifest" {
        return Err(ParseError::invalid("it isn't an F4M manifest"));
    }
    let text = |n: &str| root.child(n).map(|c| c.text_trimmed().to_string());
    let base = text("baseURL")
        .and_then(|b| url.join(&b).ok())
        .unwrap_or_else(|| url.clone());
    let live = text("streamType").is_some_and(|t| t.eq_ignore_ascii_case("live"));
    let duration = text("duration").and_then(|d| d.parse::<f64>().ok());
    let drm_ids: Vec<String> = root
        .children_named("drmAdditionalHeader")
        .filter_map(|d| d.attr("id").map(String::from))
        .collect();
    let any_drm = root.child("drmAdditionalHeader").is_some();

    let mut bootstraps = HashMap::new();
    for (i, b) in root.children_named("bootstrapInfo").enumerate() {
        let id = b
            .attr("id")
            .map(String::from)
            .unwrap_or_else(|| format!("#{i}"));
        let src = match b.attr("url") {
            Some(u) => Bootstrap::Url(
                url.join(u)
                    .map_err(|_| ParseError::invalid("a bootstrap address can't be read"))?,
            ),
            None => Bootstrap::Inline(decode_b64(b.text_trimmed())?),
        };
        bootstraps.insert(id, src);
    }
    let only_boot = (bootstraps.len() == 1)
        .then(|| bootstraps.keys().next().cloned())
        .flatten();

    let mut media = Vec::new();
    for m in root.children_named("media") {
        if m.attr("href").is_some() && m.attr("url").is_none() {
            return Err(ParseError::unsupported(
                "F4M manifests that point to other manifests",
            ));
        }
        let Some(u) = m.attr("url") else { continue };
        let drm = m
            .attr("drmAdditionalHeaderId")
            .is_some_and(|id| drm_ids.iter().any(|d| d == id) || any_drm)
            || (any_drm && drm_ids.is_empty());
        media.push(Media {
            url: u.to_string(),
            bitrate: m.attr("bitrate").and_then(|b| b.trim().parse().ok()),
            width: m.attr("width").and_then(|w| w.trim().parse().ok()),
            height: m.attr("height").and_then(|h| h.trim().parse().ok()),
            bootstrap: m
                .attr("bootstrapInfoId")
                .map(String::from)
                .or_else(|| only_boot.clone()),
            drm,
        });
    }
    if media.is_empty() {
        return Err(ParseError::invalid("the manifest lists no media"));
    }
    Ok(Manifest {
        base,
        query: url.query().map(String::from),
        live,
        duration,
        media,
        bootstraps,
    })
}

impl Manifest {
    /// The bootstraps that have to be fetched before [`Manifest::presentation`].
    pub fn bootstrap_urls(&self) -> Vec<Url> {
        let mut v: Vec<Url> = self
            .bootstraps
            .values()
            .filter_map(|b| match b {
                Bootstrap::Url(u) => Some(u.clone()),
                Bootstrap::Inline(_) => None,
            })
            .collect();
        v.sort();
        v.dedup();
        v
    }

    /// The presentation, given the bodies of the bootstraps named by URL.
    pub fn presentation(
        &self,
        fetched: &HashMap<Url, Vec<u8>>,
        lim: &ParseLimits,
    ) -> Result<Presentation, ParseError> {
        let mut variants = Vec::new();
        let mut dropped_drm = 0;
        let mut live = self.live;
        let mut duration = self.duration;
        for m in &self.media {
            if m.drm {
                dropped_drm += 1;
                continue;
            }
            let boot = m
                .bootstrap
                .as_ref()
                .and_then(|id| self.bootstraps.get(id))
                .ok_or_else(|| ParseError::invalid("a media has no bootstrap"))?;
            let bytes = match boot {
                Bootstrap::Inline(b) => b.as_slice(),
                Bootstrap::Url(u) => fetched
                    .get(u)
                    .map(Vec::as_slice)
                    .ok_or_else(|| ParseError::invalid("a bootstrap wasn't fetched"))?,
            };
            let abst = read_bootstrap(bytes)?;
            live |= abst.live;
            let frags = abst.fragments(lim)?;
            if frags.is_empty() {
                return Err(ParseError::invalid("the bootstrap lists no fragments"));
            }
            let media_base = self
                .base
                .join(&m.url)
                .map_err(|_| ParseError::invalid("a media address can't be read"))?;
            let mut segments = Vec::with_capacity(frags.len());
            let mut start = 0.0;
            for f in &frags {
                let mut u = format!("{media_base}Seg{}-Frag{}", f.segment, f.fragment);
                if let Some(q) = &self.query {
                    u.push('?');
                    u.push_str(q);
                }
                let url = Url::parse(&u).map_err(|_| ParseError::invalid("a fragment address"))?;
                segments.push(Segment {
                    seq: f.fragment as u64,
                    object: Object { url, range: None },
                    init: None,
                    key: None,
                    discontinuity: 0,
                    start,
                    duration: f.duration,
                });
                start += f.duration;
            }
            if duration.is_none() && start > 0.0 {
                duration = Some(start);
            }
            let bitrate = m.bitrate.unwrap_or(0).to_string();
            let video = Track {
                id: TrackId::of(&["hds", &m.url, &bitrate]),
                kind: TrackKind::Video,
                lang: None,
                name: None,
                codecs: None,
                container: SegmentContainer::F4f,
                bandwidth: m.bitrate.map(|b| b * 1000),
                resolution: m.width.zip(m.height),
                channels: None,
                is_default: false,
                protection: Protection::Clear,
                plan: SegmentPlan {
                    inits: Vec::new(),
                    segments,
                    whole: false,
                },
            };
            variants.push(Variant {
                video,
                audio: Vec::new(),
                subtitles: Vec::new(),
                muxed_audio: true,
            });
        }
        Ok(Presentation {
            kind: ManifestKind::Hds,
            liveness: if live { Liveness::Live } else { Liveness::Vod },
            duration,
            title: None,
            variants,
            audio: Vec::new(),
            subtitles: Vec::new(),
            warnings: Vec::new(),
            dropped_drm,
        })
    }
}

fn decode_b64(s: &str) -> Result<Vec<u8>, ParseError> {
    let clean: String = s.chars().filter(|c| !c.is_whitespace()).collect();
    base64::engine::general_purpose::STANDARD
        .decode(clean)
        .map_err(|_| ParseError::invalid("the bootstrap isn't valid base64"))
}

/// The FLV tags inside one F4F fragment (the payload of its `mdat` boxes).
pub fn flv_tags(fragment: &[u8]) -> Result<Vec<u8>, ParseError> {
    let mut r = Reader::new(fragment);
    let mut out = Vec::new();
    let mut found = false;
    while !r.done() {
        let (kind, body) = r.boxed()?;
        if kind == *b"mdat" {
            out.extend_from_slice(body);
            found = true;
        }
    }
    if !found {
        return Err(ParseError::invalid("a fragment has no media data"));
    }
    Ok(out)
}

// ---------------------------------------------------------------- bootstrap (abst)

struct Reader<'a> {
    b: &'a [u8],
    at: usize,
}

fn short() -> ParseError {
    ParseError::invalid("the bootstrap is cut short")
}

impl<'a> Reader<'a> {
    fn new(b: &'a [u8]) -> Self {
        Reader { b, at: 0 }
    }

    fn done(&self) -> bool {
        self.at >= self.b.len()
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8], ParseError> {
        let end = self.at.checked_add(n).ok_or_else(short)?;
        let s = self.b.get(self.at..end).ok_or_else(short)?;
        self.at = end;
        Ok(s)
    }

    fn u8(&mut self) -> Result<u8, ParseError> {
        Ok(self.take(1)?[0])
    }

    fn u32(&mut self) -> Result<u32, ParseError> {
        let s = self.take(4)?;
        Ok(u32::from_be_bytes([s[0], s[1], s[2], s[3]]))
    }

    fn u64(&mut self) -> Result<u64, ParseError> {
        let s = self.take(8)?;
        let mut a = [0u8; 8];
        a.copy_from_slice(s);
        Ok(u64::from_be_bytes(a))
    }

    /// A NUL-terminated string.
    fn string(&mut self) -> Result<&'a [u8], ParseError> {
        let rest = self.b.get(self.at..).ok_or_else(short)?;
        let n = rest.iter().position(|&c| c == 0).ok_or_else(short)?;
        let s = &rest[..n];
        self.at += n + 1;
        Ok(s)
    }

    /// One box: its type and body.
    fn boxed(&mut self) -> Result<([u8; 4], &'a [u8]), ParseError> {
        let size = self.u32()? as u64;
        let t = self.take(4)?;
        let kind = [t[0], t[1], t[2], t[3]];
        let (size, header) = match size {
            1 => (self.u64()?, 16),
            0 => ((self.b.len() - self.at + 8) as u64, 8),
            s => (s, 8),
        };
        let len = size.checked_sub(header).ok_or_else(short)?;
        let body = self.take(usize::try_from(len).map_err(|_| short())?)?;
        Ok((kind, body))
    }
}

struct Abst {
    live: bool,
    /// (first segment, fragments per segment)
    segment_runs: Vec<(u32, u32)>,
    /// (first fragment, duration in timescale units; 0 = discontinuity), and the timescale.
    fragment_runs: Vec<(u32, u32)>,
    timescale: u32,
}

struct Fragment {
    segment: u32,
    fragment: u32,
    duration: f64,
}

fn read_bootstrap(bytes: &[u8]) -> Result<Abst, ParseError> {
    let mut outer = Reader::new(bytes);
    let (kind, body) = outer.boxed()?;
    if kind != *b"abst" {
        return Err(ParseError::invalid("the bootstrap isn't an abst box"));
    }
    let mut r = Reader::new(body);
    r.take(4)?; // version and flags
    r.u32()?; // bootstrap info version
    let flags = r.u8()?;
    let live = flags & 0x20 != 0;
    r.u32()?; // timescale of the whole presentation
    r.u64()?; // current media time
    r.u64()?; // SMPTE time code offset
    r.string()?; // movie identifier
    for _ in 0..r.u8()? {
        r.string()?; // server entries
    }
    for _ in 0..r.u8()? {
        r.string()?; // quality entries
    }
    r.string()?; // DRM data
    r.string()?; // metadata
    let mut segment_runs = Vec::new();
    for _ in 0..r.u8()? {
        let (kind, body) = r.boxed()?;
        if kind != *b"asrt" {
            return Err(ParseError::invalid("a segment run table is missing"));
        }
        let mut s = Reader::new(body);
        s.take(4)?;
        for _ in 0..s.u8()? {
            s.string()?;
        }
        for _ in 0..s.u32()?.min(1_000_000) {
            segment_runs.push((s.u32()?, s.u32()?));
        }
    }
    let mut fragment_runs = Vec::new();
    let mut timescale = 1000;
    for _ in 0..r.u8()? {
        let (kind, body) = r.boxed()?;
        if kind != *b"afrt" {
            return Err(ParseError::invalid("a fragment run table is missing"));
        }
        let mut f = Reader::new(body);
        f.take(4)?;
        timescale = f.u32()?.max(1);
        for _ in 0..f.u8()? {
            f.string()?;
        }
        for _ in 0..f.u32()?.min(1_000_000) {
            let first = f.u32()?;
            f.u64()?; // first timestamp
            let duration = f.u32()?;
            if duration == 0 {
                f.u8()?; // discontinuity indicator
            }
            fragment_runs.push((first, duration));
        }
    }
    if segment_runs.is_empty() || fragment_runs.is_empty() {
        return Err(ParseError::invalid("the bootstrap has no run tables"));
    }
    Ok(Abst {
        live,
        segment_runs,
        fragment_runs,
        timescale,
    })
}

impl Abst {
    fn fragments(&self, lim: &ParseLimits) -> Result<Vec<Fragment>, ParseError> {
        let mut next = self.fragment_runs[0].0;
        let mut out = Vec::new();
        for &(segment, count) in &self.segment_runs {
            // Some live streams say "4294967295 fragments"; only VOD is downloaded anyway.
            let count = if count == u32::MAX && self.live {
                2
            } else {
                count
            };
            for _ in 0..count {
                if out.len() >= lim.max_segments_per_track {
                    return Err(ParseError::TooMany {
                        what: "fragments",
                        limit: lim.max_segments_per_track,
                    });
                }
                out.push(Fragment {
                    segment,
                    fragment: next,
                    duration: self.duration_of(next),
                });
                next = next.checked_add(1).ok_or_else(short)?;
            }
        }
        Ok(out)
    }

    /// The duration of fragment `n`, from the run it falls in.
    fn duration_of(&self, n: u32) -> f64 {
        let run = self
            .fragment_runs
            .iter()
            .filter(|(first, d)| *first <= n && *d > 0)
            .max_by_key(|(first, _)| *first);
        run.map(|(_, d)| *d as f64 / self.timescale as f64)
            .unwrap_or(0.0)
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    fn boxed(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
        let mut v = ((body.len() + 8) as u32).to_be_bytes().to_vec();
        v.extend_from_slice(kind);
        v.extend_from_slice(body);
        v
    }

    /// A bootstrap with one segment of `frags` fragments of `ms` milliseconds.
    pub(crate) fn bootstrap(frags: u32, ms: u32) -> Vec<u8> {
        let mut asrt = vec![0, 0, 0, 0, 0];
        asrt.extend_from_slice(&1u32.to_be_bytes());
        asrt.extend_from_slice(&1u32.to_be_bytes());
        asrt.extend_from_slice(&frags.to_be_bytes());
        let mut afrt = vec![0, 0, 0, 0];
        afrt.extend_from_slice(&1000u32.to_be_bytes());
        afrt.push(0);
        afrt.extend_from_slice(&1u32.to_be_bytes());
        afrt.extend_from_slice(&1u32.to_be_bytes());
        afrt.extend_from_slice(&0u64.to_be_bytes());
        afrt.extend_from_slice(&ms.to_be_bytes());
        let mut abst = vec![0, 0, 0, 0];
        abst.extend_from_slice(&1u32.to_be_bytes());
        abst.push(0); // profile/live/update
        abst.extend_from_slice(&1000u32.to_be_bytes());
        abst.extend_from_slice(&0u64.to_be_bytes());
        abst.extend_from_slice(&0u64.to_be_bytes());
        abst.push(0); // movie id ""
        abst.push(0); // servers
        abst.push(0); // qualities
        abst.push(0); // drm ""
        abst.push(0); // metadata ""
        abst.push(1);
        abst.extend(boxed(b"asrt", &asrt));
        abst.push(1);
        abst.extend(boxed(b"afrt", &afrt));
        boxed(b"abst", &abst)
    }

    #[test]
    fn manifest_with_inline_bootstrap() {
        let boot = base64::engine::general_purpose::STANDARD.encode(bootstrap(3, 4000));
        let f4m = format!(
            r#"<manifest xmlns="http://ns.adobe.com/f4m/1.0"><streamType>recorded</streamType>
              <bootstrapInfo profile="named" id="b1">{boot}</bootstrapInfo>
              <media url="v_500" bitrate="500" width="640" height="360" bootstrapInfoId="b1"/>
              <media url="v_1500" bitrate="1500" width="1280" height="720" bootstrapInfoId="b1"/>
            </manifest>"#
        );
        let url = Url::parse("http://h.example/vod/show.f4m?token=1").unwrap();
        let lim = ParseLimits::default();
        let m = parse_manifest(f4m.as_bytes(), &url, &lim).unwrap();
        assert!(m.bootstrap_urls().is_empty());
        let p = m.presentation(&HashMap::new(), &lim).unwrap();
        assert_eq!(p.kind, ManifestKind::Hds);
        assert!(!p.is_live());
        assert_eq!(p.variants.len(), 2);
        let v = &p.variants[1].video;
        assert_eq!(v.resolution, Some((1280, 720)));
        assert_eq!(v.bandwidth, Some(1_500_000));
        let urls: Vec<_> = v
            .plan
            .segments
            .iter()
            .map(|s| s.object.url.as_str())
            .collect();
        assert_eq!(
            urls,
            [
                "http://h.example/vod/v_1500Seg1-Frag1?token=1",
                "http://h.example/vod/v_1500Seg1-Frag2?token=1",
                "http://h.example/vod/v_1500Seg1-Frag3?token=1"
            ]
        );
        assert_eq!(p.duration, Some(12.0));
    }

    #[test]
    fn external_bootstrap_and_drm() {
        let f4m = br#"<manifest xmlns="http://ns.adobe.com/f4m/2.0">
              <bootstrapInfo id="b" url="boot.abst"/>
              <drmAdditionalHeader id="d">AAAA</drmAdditionalHeader>
              <media url="locked" bitrate="900" bootstrapInfoId="b" drmAdditionalHeaderId="d"/>
              <media url="open" bitrate="400" bootstrapInfoId="b"/>
            </manifest>"#;
        let url = Url::parse("http://h.example/a/m.f4m").unwrap();
        let lim = ParseLimits::default();
        let m = parse_manifest(f4m, &url, &lim).unwrap();
        let boot = Url::parse("http://h.example/a/boot.abst").unwrap();
        assert_eq!(m.bootstrap_urls(), vec![boot.clone()]);
        let mut fetched = HashMap::new();
        fetched.insert(boot, bootstrap(2, 2000));
        let p = m.presentation(&fetched, &lim).unwrap();
        assert_eq!(p.dropped_drm, 1);
        assert_eq!(p.variants.len(), 1);
        assert!(
            p.variants[0].video.plan.segments[0]
                .object
                .url
                .as_str()
                .ends_with("/a/openSeg1-Frag1")
        );
    }

    #[test]
    fn fragments_hold_flv_tags() {
        let mut frag = boxed(b"afra", &[0; 8]);
        frag.extend(boxed(b"moof", &[1, 2, 3]));
        frag.extend(boxed(b"mdat", b"TAGS"));
        assert_eq!(flv_tags(&frag).unwrap(), b"TAGS");
        assert!(flv_tags(&boxed(b"moof", &[])).is_err());
        assert!(read_bootstrap(&[0, 0, 0, 99, b'a', b'b', b's', b't']).is_err());
    }
}
