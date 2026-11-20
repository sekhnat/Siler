//! Just enough ISO BMFF (MP4) reading to take timed text out of a track: subtitles carried
//! in MP4 as `wvtt` (WebVTT, ISO/IEC 14496-30) or `stpp` (TTML), and the first decode time
//! of a fragmented track. FFmpeg can't read `wvtt`, so K-Star reads the samples itself.
//!
//! Every length and offset is checked against the data before it is used; a box that runs
//! past its parent, or a sample outside the data, is an error, never a panic.

use crate::error::ParseError;
use crate::limits::ParseLimits;

/// How the text in a track's samples is written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextFormat {
    /// WebVTT cue boxes (`vttc`, `vtte`).
    Wvtt,
    /// A TTML document per sample.
    Stpp,
}

/// What an initialisation segment says about its text track.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextTrack {
    pub format: TextFormat,
    pub track_id: u32,
    /// Units per second of the track's times.
    pub timescale: u32,
    default_duration: u32,
    default_size: u32,
}

/// One sample: its decode time and duration in the track's timescale, and its bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sample {
    pub time: u64,
    pub duration: u64,
    pub data: Vec<u8>,
}

fn bad(what: &str) -> ParseError {
    ParseError::invalid(format!("an MP4 subtitle track {what}"))
}

/// One box: its type, its payload, and where the box and its payload start in the data it
/// was read from.
struct Bx<'a> {
    kind: [u8; 4],
    body: &'a [u8],
    at: usize,
    body_at: usize,
}

/// The boxes in `data`, one level deep.
fn boxes(data: &[u8]) -> Result<Vec<Bx<'_>>, ParseError> {
    let mut out = Vec::new();
    let mut o = 0usize;
    while o < data.len() {
        let head = data.get(o..o + 8).ok_or_else(|| bad("has a cut-off box"))?;
        let mut size = u32::from_be_bytes([head[0], head[1], head[2], head[3]]) as u64;
        let kind = [head[4], head[5], head[6], head[7]];
        let mut header = 8usize;
        if size == 1 {
            let large = data
                .get(o + 8..o + 16)
                .ok_or_else(|| bad("has a cut-off box"))?;
            size = u64::from_be_bytes(large.try_into().map_err(|_| bad("has a cut-off box"))?);
            header = 16;
        } else if size == 0 {
            size = (data.len() - o) as u64;
        }
        let end = usize::try_from(size)
            .ok()
            .and_then(|s| o.checked_add(s))
            .filter(|e| *e <= data.len() && size as usize >= header)
            .ok_or_else(|| bad("has a box that runs past its end"))?;
        out.push(Bx {
            kind,
            body: &data[o + header..end],
            at: o,
            body_at: o + header,
        });
        o = end;
    }
    Ok(out)
}

/// The complete boxes at the start of `data`, stopping at one that is cut off (for reading
/// the header of a file from its first bytes).
fn leading_boxes(data: &[u8]) -> Vec<Bx<'_>> {
    let mut out = Vec::new();
    let mut o = 0usize;
    while let Some(head) = data.get(o..o + 8) {
        let size = u32::from_be_bytes([head[0], head[1], head[2], head[3]]) as usize;
        let Some(end) = o
            .checked_add(size)
            .filter(|e| size >= 8 && *e <= data.len())
        else {
            break;
        };
        out.push(Bx {
            kind: [head[4], head[5], head[6], head[7]],
            body: &data[o + 8..end],
            at: o,
            body_at: o + 8,
        });
        o = end;
    }
    out
}

fn child<'a>(data: &'a [u8], kind: &[u8; 4]) -> Result<Option<&'a [u8]>, ParseError> {
    Ok(boxes(data)?
        .into_iter()
        .find(|b| &b.kind == kind)
        .map(|b| b.body))
}

/// Big-endian readers that fail instead of panicking.
struct Rd<'a> {
    d: &'a [u8],
    o: usize,
}

impl<'a> Rd<'a> {
    fn new(d: &'a [u8]) -> Self {
        Rd { d, o: 0 }
    }
    fn take(&mut self, n: usize) -> Result<&'a [u8], ParseError> {
        let s = self
            .d
            .get(self.o..self.o + n)
            .ok_or_else(|| bad("has a box that is too short"))?;
        self.o += n;
        Ok(s)
    }
    fn u32(&mut self) -> Result<u32, ParseError> {
        let b = self.take(4)?;
        Ok(u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
    }
    fn u64(&mut self) -> Result<u64, ParseError> {
        let b = self.take(8)?;
        Ok(u64::from_be_bytes(
            b.try_into().map_err(|_| bad("is cut off"))?,
        ))
    }
    fn skip(&mut self, n: usize) -> Result<(), ParseError> {
        self.take(n).map(|_| ())
    }
    /// Version and flags of a full box.
    fn full(&mut self) -> Result<(u8, u32), ParseError> {
        let v = self.u32()?;
        Ok(((v >> 24) as u8, v & 0x00ff_ffff))
    }
}

/// The text track an initialisation segment (or a whole file) describes: the first track
/// whose sample description is `wvtt` or `stpp`. `None` when it has none.
pub fn text_track(init: &[u8]) -> Result<Option<TextTrack>, ParseError> {
    let Some(moov) = child(init, b"moov")? else {
        return Ok(None);
    };
    let mut defaults = Vec::new();
    if let Some(mvex) = child(moov, b"mvex")? {
        for b in boxes(mvex)? {
            if &b.kind == b"trex" {
                let mut r = Rd::new(b.body);
                r.full()?;
                let id = r.u32()?;
                r.skip(4)?;
                defaults.push((id, r.u32()?, r.u32()?));
            }
        }
    }
    for trak in boxes(moov)?.into_iter().filter(|b| &b.kind == b"trak") {
        let Some(mdia) = child(trak.body, b"mdia")? else {
            continue;
        };
        let Some(stbl) = child(mdia, b"minf")?
            .map(|m| child(m, b"stbl"))
            .transpose()?
            .flatten()
        else {
            continue;
        };
        let Some(stsd) = child(stbl, b"stsd")? else {
            continue;
        };
        let entries = stsd
            .get(8..)
            .ok_or_else(|| bad("has a short sample description"))?;
        let format = match boxes(entries)?.first().map(|b| b.kind) {
            Some(k) if &k == b"wvtt" => TextFormat::Wvtt,
            Some(k) if &k == b"stpp" => TextFormat::Stpp,
            _ => continue,
        };
        let mdhd = child(mdia, b"mdhd")?.ok_or_else(|| bad("has no media header"))?;
        let mut r = Rd::new(mdhd);
        let (v, _) = r.full()?;
        r.skip(if v == 1 { 16 } else { 8 })?;
        let timescale = r.u32()?;
        if timescale == 0 {
            return Err(bad("has a timescale of zero"));
        }
        let tkhd = child(trak.body, b"tkhd")?.ok_or_else(|| bad("has no track header"))?;
        let mut r = Rd::new(tkhd);
        let (v, _) = r.full()?;
        r.skip(if v == 1 { 16 } else { 8 })?;
        let track_id = r.u32()?;
        let (default_duration, default_size) = defaults
            .iter()
            .find(|d| d.0 == track_id)
            .map_or((0, 0), |d| (d.1, d.2));
        return Ok(Some(TextTrack {
            format,
            track_id,
            timescale,
            default_duration,
            default_size,
        }));
    }
    Ok(None)
}

/// The samples of `track` in `data`: a media segment (one or more `moof` + `mdat`), or a whole
/// file (which may also list samples in its `moov`). `next` is the decode time a fragment
/// without a `tfdt` starts at; it is moved past the samples read.
pub fn text_samples(
    track: &TextTrack,
    data: &[u8],
    next: &mut u64,
    out: &mut Vec<Sample>,
    lim: &ParseLimits,
) -> Result<(), ParseError> {
    let max = lim.max_segments_per_track.saturating_mul(16);
    let top = boxes(data)?;
    if let Some(moov) = top.iter().find(|b| &b.kind == b"moov") {
        table_samples(track, moov.body, data, next, out, max)?;
    }
    for (i, moof) in top.iter().enumerate().filter(|(_, b)| &b.kind == b"moof") {
        // The `mdat` after this `moof`: where samples start when a run states no offset.
        let mdat = top[i + 1..]
            .iter()
            .take_while(|b| &b.kind != b"moof")
            .find(|b| &b.kind == b"mdat")
            .map(|b| b.body_at);
        for traf in boxes(moof.body)?.into_iter().filter(|b| &b.kind == b"traf") {
            read_fragment(track, moof.at, mdat, traf.body, data, next, out, max)?;
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn read_fragment(
    track: &TextTrack,
    moof_at: usize,
    mdat_payload: Option<usize>,
    traf: &[u8],
    data: &[u8],
    next: &mut u64,
    out: &mut Vec<Sample>,
    max: usize,
) -> Result<(), ParseError> {
    let parts = boxes(traf)?;
    let tfhd = parts
        .iter()
        .find(|b| &b.kind == b"tfhd")
        .ok_or_else(|| bad("has a fragment with no header"))?;
    let mut r = Rd::new(tfhd.body);
    let (_, flags) = r.full()?;
    if r.u32()? != track.track_id {
        return Ok(());
    }
    let base = if flags & 0x1 != 0 {
        usize::try_from(r.u64()?).map_err(|_| bad("has an offset that is too large"))?
    } else {
        moof_at
    };
    if flags & 0x2 != 0 {
        r.skip(4)?;
    }
    let default_duration = if flags & 0x8 != 0 {
        r.u32()?
    } else {
        track.default_duration
    };
    let default_size = if flags & 0x10 != 0 {
        r.u32()?
    } else {
        track.default_size
    };
    if let Some(tfdt) = parts.iter().find(|b| &b.kind == b"tfdt") {
        let mut r = Rd::new(tfdt.body);
        let (v, _) = r.full()?;
        *next = if v == 1 { r.u64()? } else { r.u32()? as u64 };
    }
    let mut cursor: Option<usize> = mdat_payload;
    for trun in parts.iter().filter(|b| &b.kind == b"trun") {
        let mut r = Rd::new(trun.body);
        let (_, f) = r.full()?;
        let count = r.u32()? as usize;
        if out.len().saturating_add(count) > max {
            return Err(ParseError::TooMany {
                what: "subtitle samples",
                limit: max,
            });
        }
        if f & 0x1 != 0 {
            let off = r.u32()? as i32 as i64;
            let at = base as i64 + off;
            cursor = Some(usize::try_from(at).map_err(|_| bad("has a negative data offset"))?);
        }
        if f & 0x4 != 0 {
            r.skip(4)?;
        }
        let mut at = cursor.ok_or_else(|| bad("has samples with no data"))?;
        for _ in 0..count {
            let duration = if f & 0x100 != 0 {
                r.u32()?
            } else {
                default_duration
            };
            let size = if f & 0x200 != 0 {
                r.u32()?
            } else {
                default_size
            } as usize;
            if f & 0x400 != 0 {
                r.skip(4)?;
            }
            if f & 0x800 != 0 {
                r.skip(4)?;
            }
            let bytes = at
                .checked_add(size)
                .and_then(|end| data.get(at..end))
                .ok_or_else(|| bad("has a sample outside its data"))?;
            out.push(Sample {
                time: *next,
                duration: duration as u64,
                data: bytes.to_vec(),
            });
            *next = next.saturating_add(duration as u64);
            at += size;
        }
        cursor = Some(at);
    }
    Ok(())
}

/// Samples listed in a non-fragmented file's sample tables (`stts`, `stsz`, `stsc`,
/// `stco`/`co64`). A fragmented file's tables are empty.
fn table_samples(
    track: &TextTrack,
    moov: &[u8],
    data: &[u8],
    next: &mut u64,
    out: &mut Vec<Sample>,
    max: usize,
) -> Result<(), ParseError> {
    let mut stbl = None;
    for trak in boxes(moov)?.into_iter().filter(|b| &b.kind == b"trak") {
        let id = match child(trak.body, b"tkhd")? {
            Some(t) => {
                let mut r = Rd::new(t);
                let (v, _) = r.full()?;
                r.skip(if v == 1 { 16 } else { 8 })?;
                r.u32()?
            }
            None => continue,
        };
        if id == track.track_id {
            stbl = child(trak.body, b"mdia")?
                .map(|m| child(m, b"minf"))
                .transpose()?
                .flatten()
                .map(|m| child(m, b"stbl"))
                .transpose()?
                .flatten();
            break;
        }
    }
    let Some(stbl) = stbl else { return Ok(()) };
    // Sizes.
    let Some(stsz) = child(stbl, b"stsz")? else {
        return Ok(());
    };
    let mut r = Rd::new(stsz);
    r.full()?;
    let fixed = r.u32()?;
    let count = r.u32()? as usize;
    if count == 0 {
        return Ok(());
    }
    if out.len().saturating_add(count) > max {
        return Err(ParseError::TooMany {
            what: "subtitle samples",
            limit: max,
        });
    }
    let mut sizes = Vec::with_capacity(count);
    for _ in 0..count {
        sizes.push(if fixed != 0 { fixed } else { r.u32()? } as usize);
    }
    // Durations.
    let mut durations = Vec::with_capacity(count);
    if let Some(stts) = child(stbl, b"stts")? {
        let mut r = Rd::new(stts);
        r.full()?;
        for _ in 0..r.u32()? {
            let (n, d) = (r.u32()?, r.u32()?);
            for _ in 0..n {
                if durations.len() == count {
                    break;
                }
                durations.push(d as u64);
            }
        }
    }
    durations.resize(count, 0);
    // Chunk offsets, and how many samples each chunk holds.
    let mut chunks: Vec<u64> = Vec::new();
    if let Some(stco) = child(stbl, b"stco")? {
        let mut r = Rd::new(stco);
        r.full()?;
        for _ in 0..r.u32()?.min(count as u32) {
            chunks.push(r.u32()? as u64);
        }
    } else if let Some(co64) = child(stbl, b"co64")? {
        let mut r = Rd::new(co64);
        r.full()?;
        for _ in 0..r.u32()?.min(count as u32) {
            chunks.push(r.u64()?);
        }
    }
    let mut per_chunk: Vec<(u32, u32)> = Vec::new();
    if let Some(stsc) = child(stbl, b"stsc")? {
        let mut r = Rd::new(stsc);
        r.full()?;
        for _ in 0..r.u32()?.min(count as u32) {
            let first = r.u32()?;
            let n = r.u32()?;
            r.skip(4)?;
            per_chunk.push((first, n));
        }
    }
    let mut sample = 0usize;
    for (ci, &offset) in chunks.iter().enumerate() {
        let chunk_no = ci as u32 + 1;
        let n = per_chunk
            .iter()
            .rev()
            .find(|(first, _)| *first <= chunk_no)
            .map_or(1, |p| p.1) as usize;
        let mut at = usize::try_from(offset).map_err(|_| bad("has an offset that is too large"))?;
        for _ in 0..n {
            if sample >= count {
                break;
            }
            let size = sizes[sample];
            let bytes = at
                .checked_add(size)
                .and_then(|end| data.get(at..end))
                .ok_or_else(|| bad("has a sample outside its data"))?;
            out.push(Sample {
                time: *next,
                duration: durations[sample],
                data: bytes.to_vec(),
            });
            *next = next.saturating_add(durations[sample]);
            at += size;
            sample += 1;
        }
    }
    Ok(())
}

/// The timescale of the first track in an initialisation segment (or the first bytes of a
/// file that starts with one).
pub fn timescale(init: &[u8]) -> Option<u32> {
    let moov = leading_boxes(init)
        .into_iter()
        .find(|b| &b.kind == b"moov")?
        .body;
    let trak = boxes(moov).ok()?.into_iter().find(|b| &b.kind == b"trak")?;
    let mdhd = child(child(trak.body, b"mdia").ok()??, b"mdhd").ok()??;
    let mut r = Rd::new(mdhd);
    let (v, _) = r.full().ok()?;
    r.skip(if v == 1 { 16 } else { 8 }).ok()?;
    r.u32().ok().filter(|t| *t > 0)
}

/// The decode time of the first fragment in `data` (its `tfdt`), in its track's timescale.
/// `data` may be the first bytes of a longer file.
pub fn first_decode_time(data: &[u8]) -> Option<u64> {
    let top = leading_boxes(data);
    let moof = top.iter().find(|b| &b.kind == b"moof")?;
    let traf = boxes(moof.body)
        .ok()?
        .into_iter()
        .find(|b| &b.kind == b"traf")?;
    let tfdt = child(traf.body, b"tfdt").ok()??;
    let mut r = Rd::new(tfdt);
    let (v, _) = r.full().ok()?;
    if v == 1 {
        r.u64().ok()
    } else {
        r.u32().ok().map(u64::from)
    }
}

/// The boxes inside one `wvtt` sample, as cues: `(identifier, settings, text)`. An empty
/// sample (`vtte`) has none.
pub fn wvtt_cues(sample: &[u8]) -> Result<Vec<(String, String, String)>, ParseError> {
    let mut cues = Vec::new();
    for b in boxes(sample)? {
        if &b.kind != b"vttc" {
            continue;
        }
        let (mut id, mut settings, mut text) = (String::new(), String::new(), String::new());
        for c in boxes(b.body)? {
            let s = String::from_utf8_lossy(c.body).into_owned();
            match &c.kind {
                b"iden" => id = s,
                b"sttg" => settings = s,
                b"payl" => text = s,
                _ => {}
            }
        }
        cues.push((id, settings, text));
    }
    Ok(cues)
}

#[cfg(test)]
pub(crate) mod build {
    //! Writing the boxes the tests read.

    pub fn bx(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
        let mut v = ((body.len() + 8) as u32).to_be_bytes().to_vec();
        v.extend_from_slice(kind);
        v.extend_from_slice(body);
        v
    }

    pub fn full(kind: &[u8; 4], version: u8, flags: u32, body: &[u8]) -> Vec<u8> {
        let mut v = ((version as u32) << 24 | flags).to_be_bytes().to_vec();
        v.extend_from_slice(body);
        bx(kind, &v)
    }

    fn be(n: u32) -> [u8; 4] {
        n.to_be_bytes()
    }

    /// An initialisation segment with one text track (`wvtt` or `stpp`).
    pub fn init(entry: &[u8; 4], track_id: u32, timescale: u32) -> Vec<u8> {
        let mut tkhd = vec![0u8; 8];
        tkhd.extend_from_slice(&be(track_id));
        tkhd.extend_from_slice(&[0u8; 60]);
        let mut mdhd = vec![0u8; 8];
        mdhd.extend_from_slice(&be(timescale));
        mdhd.extend_from_slice(&[0u8; 8]);
        let mut sample_entry = vec![0u8; 8];
        if entry == b"wvtt" {
            sample_entry.extend(bx(b"vttC", b"WEBVTT"));
        }
        let mut stsd = be(1).to_vec();
        stsd.extend(bx(entry, &sample_entry));
        let empty = be(0);
        let stbl = [
            full(b"stsd", 0, 0, &stsd),
            full(b"stts", 0, 0, &empty),
            full(b"stsc", 0, 0, &empty),
            full(b"stsz", 0, 0, &[0, 0, 0, 0, 0, 0, 0, 0]),
            full(b"stco", 0, 0, &empty),
        ]
        .concat();
        let minf = bx(b"minf", &bx(b"stbl", &stbl));
        let mdia = bx(b"mdia", &[full(b"mdhd", 0, 0, &mdhd), minf].concat());
        let trak = bx(b"trak", &[full(b"tkhd", 0, 3, &tkhd), mdia].concat());
        let mut trex = be(track_id).to_vec();
        trex.extend_from_slice(&[0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        let mvex = bx(b"mvex", &full(b"trex", 0, 0, &trex));
        [
            bx(b"ftyp", b"iso6\0\0\0\0iso6"),
            bx(b"moov", &[trak, mvex].concat()),
        ]
        .concat()
    }

    /// One fragment: samples `(duration, bytes)` from decode time `time`, data offsets from
    /// the start of the `moof` (as CMAF writes them).
    pub fn fragment(track_id: u32, time: u64, samples: &[(u32, Vec<u8>)]) -> Vec<u8> {
        // default-base-is-moof, no other fields.
        let tfhd = full(b"tfhd", 0, 0x02_0000, &be(track_id));
        let tfdt = full(b"tfdt", 1, 0, &time.to_be_bytes());
        let trun_len = 8 + 4 + 4 + 4 + samples.len() * 8;
        let moof_len = 8 + (8 + 8) + 8 + tfhd.len() + tfdt.len() + trun_len;
        let mut trun = be(samples.len() as u32).to_vec();
        trun.extend_from_slice(&be((moof_len + 8) as u32));
        for (d, s) in samples {
            trun.extend_from_slice(&be(*d));
            trun.extend_from_slice(&be(s.len() as u32));
        }
        let trun = full(b"trun", 0, 0x301, &trun);
        let traf = bx(b"traf", &[tfhd, tfdt, trun].concat());
        let moof = bx(b"moof", &[full(b"mfhd", 0, 0, &be(1)), traf].concat());
        assert_eq!(moof.len(), moof_len);
        let mdat: Vec<u8> = samples.iter().flat_map(|(_, s)| s.clone()).collect();
        [moof, bx(b"mdat", &mdat)].concat()
    }

    /// A `wvtt` sample with one cue, or an empty one.
    pub fn cue(id: &str, settings: &str, text: &str) -> Vec<u8> {
        let mut body = Vec::new();
        if !id.is_empty() {
            body.extend(bx(b"iden", id.as_bytes()));
        }
        if !settings.is_empty() {
            body.extend(bx(b"sttg", settings.as_bytes()));
        }
        body.extend(bx(b"payl", text.as_bytes()));
        bx(b"vttc", &body)
    }

    pub fn empty() -> Vec<u8> {
        bx(b"vtte", b"")
    }
}

#[cfg(test)]
mod tests {
    use super::build::*;
    use super::*;

    fn lim() -> ParseLimits {
        ParseLimits::default()
    }

    #[test]
    fn a_text_track_is_found_in_its_init() {
        let t = text_track(&init(b"wvtt", 3, 1000))
            .expect("parses")
            .expect("a text track");
        assert_eq!(
            (t.format, t.track_id, t.timescale),
            (TextFormat::Wvtt, 3, 1000)
        );
        let s = text_track(&init(b"stpp", 1, 90000)).expect("parses");
        assert_eq!(s.map(|t| t.format), Some(TextFormat::Stpp));
        assert_eq!(timescale(&init(b"wvtt", 1, 1000)), Some(1000));
    }

    #[test]
    fn samples_come_from_every_fragment_with_their_times() {
        let t = text_track(&init(b"wvtt", 1, 1000))
            .expect("parses")
            .expect("track");
        let seg = [
            fragment(1, 0, &[(1500, empty()), (2500, cue("1", "", "hello"))]),
            fragment(1, 4000, &[(1000, cue("1", "", "hello"))]),
        ]
        .concat();
        let mut next = 0;
        let mut out = Vec::new();
        text_samples(&t, &seg, &mut next, &mut out, &lim()).expect("reads");
        let times: Vec<(u64, u64)> = out.iter().map(|s| (s.time, s.duration)).collect();
        assert_eq!(times, [(0, 1500), (1500, 2500), (4000, 1000)]);
        assert_eq!(next, 5000);
        assert_eq!(wvtt_cues(&out[1].data).expect("cues")[0].2, "hello");
        assert!(wvtt_cues(&out[0].data).expect("cues").is_empty());
        assert_eq!(first_decode_time(&seg), Some(0));
    }

    #[test]
    fn a_fragment_for_another_track_is_skipped() {
        let t = text_track(&init(b"wvtt", 2, 1000))
            .expect("parses")
            .expect("track");
        let seg = fragment(1, 0, &[(1000, cue("", "", "other"))]);
        let mut out = Vec::new();
        text_samples(&t, &seg, &mut 0, &mut out, &lim()).expect("reads");
        assert!(out.is_empty());
    }

    #[test]
    fn broken_boxes_are_errors() {
        let t = text_track(&init(b"wvtt", 1, 1000))
            .expect("parses")
            .expect("track");
        let mut seg = fragment(1, 0, &[(1000, cue("", "", "x"))]);
        // A sample size past the data.
        let len = seg.len();
        seg.truncate(len - 3);
        let fixed = bx(b"moof", b"");
        assert!(text_samples(&t, &seg, &mut 0, &mut Vec::new(), &lim()).is_err());
        assert!(text_samples(&t, &fixed[..6], &mut 0, &mut Vec::new(), &lim()).is_err());
        assert!(
            text_samples(
                &t,
                &bx(b"moof", &[0, 0, 0, 99, b't']),
                &mut 0,
                &mut Vec::new(),
                &lim()
            )
            .is_err()
        );
        assert_eq!(text_track(b"\0\0\0\x08free").expect("parses"), None);
    }
}
