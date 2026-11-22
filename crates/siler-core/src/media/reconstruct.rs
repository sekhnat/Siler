//! Rebuilding the output from the fetched pieces (media-design §13). FFmpeg muxes the tracks
//! with stream copy (no re-encoding). Each track's initialisation segment and segments are
//! streamed into FFmpeg through a named pipe, so the media isn't copied once more on disk;
//! when that isn't possible, or FFmpeg can't read a track from a pipe, the pieces are joined
//! into one file per track instead. Subtitles become one WebVTT file.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use tokio_util::sync::CancellationToken;

use siler_media::classify::sniff_segment;
use siler_media::select::Output;
use siler_media::{ParseLimits, SegmentContainer, Track, TrackKind, subtitle};

use crate::error::MediaError;
use crate::{Error, Result};

/// The directory a track's pieces live in, inside the Work Directory.
pub(crate) fn track_dir(work: &Path, track: &Track) -> PathBuf {
    work.join("t").join(track.id.as_str())
}

/// The file of one fetched segment (or initialisation segment) of a track.
pub(crate) fn piece_path(work: &Path, track: &Track, index: usize) -> PathBuf {
    track_dir(work, track).join(format!("{index:08}.seg"))
}

/// The file of one initialisation segment of a track.
pub(crate) fn init_path(work: &Path, track: &Track, index: usize) -> PathBuf {
    track_dir(work, track).join(format!("init-{index}.bin"))
}

/// The file of a track that is one whole file (DASH `SegmentBase`, a plain URL).
pub(crate) fn whole_path(work: &Path, track: &Track) -> PathBuf {
    track_dir(work, track).join("whole.bin")
}

/// The file a track's pieces are joined into.
pub(crate) fn joined_path(work: &Path, track: &Track) -> PathBuf {
    track_dir(work, track).join(format!("track.{}", track.container.extension()))
}

/// The files that make up a track, in order: its initialisation segment (when the container
/// needs one), then its segments. All of them must be there.
pub(crate) fn pieces_of(work: &Path, track: &Track) -> Result<Vec<PathBuf>> {
    let inits: Vec<usize> = {
        let mut v: Vec<usize> = track.plan.segments.iter().filter_map(|s| s.init).collect();
        v.sort_unstable();
        v.dedup();
        v
    };
    if inits.len() > 1 && track.container == SegmentContainer::Fmp4 {
        return Err(Error::Media(MediaError::DiscontinuityIncompatible(
            "the stream starts over with a new header partway through".into(),
        )));
    }
    let mut files = Vec::with_capacity(track.plan.segments.len() + 1);
    if let Some(&first) = inits.first() {
        files.push(init_path(work, track, first));
    }
    files.extend((0..track.plan.segments.len()).map(|i| piece_path(work, track, i)));
    if files.iter().any(|f| !f.is_file()) {
        return Err(Error::Media(MediaError::SegmentInvalid));
    }
    Ok(files)
}

/// A track that is already one file: a whole-file track, or one joined by an earlier attempt.
fn one_file(work: &Path, track: &Track) -> Result<Option<PathBuf>> {
    if track.plan.whole {
        let p = whole_path(work, track);
        if !p.exists() {
            return Err(Error::Media(MediaError::SegmentInvalid));
        }
        return Ok(Some(p));
    }
    let joined = joined_path(work, track);
    Ok(joined.exists().then_some(joined))
}

/// Joins a track's pieces into one file. The pieces are deleted once the joined file is
/// complete, so the track is on disk once, not twice; a later attempt reuses the joined
/// file. A whole-file track is already one file.
pub(crate) fn join_track(work: &Path, track: &Track) -> Result<PathBuf> {
    if let Some(p) = one_file(work, track)? {
        return Ok(p);
    }
    let out = joined_path(work, track);
    let part = PathBuf::from(format!("{}.part", out.display()));
    let files = pieces_of(work, track)?;
    {
        use std::io::Write;
        let mut file = std::io::BufWriter::new(std::fs::File::create(&part)?);
        let f4f = track.container == SegmentContainer::F4f;
        if f4f {
            // HDS: the FLV tags of every fragment, after an FLV header.
            file.write_all(&siler_media::hds::FLV_HEADER)?;
        }
        for p in &files {
            if f4f {
                let bytes =
                    std::fs::read(p).map_err(|_| Error::Media(MediaError::SegmentInvalid))?;
                let tags = siler_media::hds::flv_tags(&bytes)
                    .map_err(|_| Error::Media(MediaError::SegmentInvalid))?;
                file.write_all(&tags)?;
                continue;
            }
            let mut src =
                std::fs::File::open(p).map_err(|_| Error::Media(MediaError::SegmentInvalid))?;
            std::io::copy(&mut src, &mut file)?;
        }
        file.flush()?;
        file.get_ref().sync_all()?;
    }
    std::fs::rename(&part, &out)?;
    for i in 0..track.plan.segments.len() {
        let _ = std::fs::remove_file(piece_path(work, track, i));
    }
    for i in 0..track.plan.inits.len() {
        let _ = std::fs::remove_file(init_path(work, track, i));
    }
    Ok(out)
}

/// Where the rebuilt output's zero is on a track's media timeline, in seconds: FFmpeg moves
/// each input so it starts at zero, and subtitles carried in MP4 (timed on the media
/// timeline) must move with it. FFmpeg's own prober says where the track starts (after edit
/// lists and composition offsets), from its header and first segment. Without `ffprobe`, a
/// fragmented MP4 track's first decode time is used. `None` when neither works.
pub(crate) async fn start_time(work: &Path, track: &Track, ffprobe: Option<&Path>) -> Option<f64> {
    use std::io::Read;
    // The header and the first segment are in the first few megabytes.
    let head = |p: &Path| -> Option<Vec<u8>> {
        let mut buf = Vec::new();
        std::fs::File::open(p)
            .ok()?
            .take(4 << 20)
            .read_to_end(&mut buf)
            .ok()?;
        Some(buf)
    };
    let bytes = match one_file(work, track).ok().flatten() {
        Some(p) => head(&p)?,
        None => {
            let files = pieces_of(work, track).ok()?;
            let take = if track.plan.inits.is_empty() { 1 } else { 2 };
            let mut b = Vec::new();
            for f in files.iter().take(take) {
                b.extend(head(f)?);
            }
            b
        }
    };
    if let Some(probe) = ffprobe {
        let sample = track_dir(work, track).join(format!("start.{}", track.container.extension()));
        if std::fs::write(&sample, &bytes).is_ok() {
            let out = tokio::process::Command::new(probe)
                .args(["-v", "error", "-protocol_whitelist", "file"])
                .args(["-show_entries", "format=start_time", "-of", "csv=p=0"])
                .arg(format!("file:{}", sample.display()))
                .stdin(std::process::Stdio::null())
                .kill_on_drop(true)
                .output()
                .await;
            let _ = std::fs::remove_file(&sample);
            if let Some(t) = out
                .ok()
                .filter(|o| o.status.success())
                .and_then(|o| {
                    String::from_utf8_lossy(&o.stdout)
                        .trim()
                        .parse::<f64>()
                        .ok()
                })
                .filter(|t| t.is_finite())
            {
                return Some(t);
            }
        }
    }
    if track.container != SegmentContainer::Fmp4 {
        return None;
    }
    let ts = siler_media::mp4::timescale(&bytes)?;
    let t = siler_media::mp4::first_decode_time(&bytes)?;
    Some(t as f64 / ts as f64)
}

/// Joins the cues of a subtitle track into one WebVTT file (converting TTML, and reading
/// subtitles carried in MP4). Returns the file's path.
///
/// `zero` is where the video starts on the media timeline (subtitles in MP4 are timed on
/// it, and segmented WebVTT maps onto it with `X-TIMESTAMP-MAP`); `until` is the end of the
/// video and audio: cues are cut
/// there, so the subtitles never make the file longer than its picture and sound.
pub(crate) fn join_subtitles(
    work: &Path,
    track: &Track,
    lim: &ParseLimits,
    zero: Option<f64>,
    until: Option<f64>,
) -> Result<PathBuf> {
    let out = track_dir(work, track).join("subs.vtt");
    if track.container == SegmentContainer::Fmp4 {
        let read =
            |p: &Path| std::fs::read(p).map_err(|_| Error::Media(MediaError::SegmentInvalid));
        let (init, pieces) = if track.plan.whole {
            (None, vec![read(&whole_path(work, track))?])
        } else {
            let files = pieces_of(work, track)?;
            let mut bodies = files.iter().map(|f| read(f)).collect::<Result<Vec<_>>>()?;
            let init =
                (!track.plan.inits.is_empty() && !bodies.is_empty()).then(|| bodies.remove(0));
            (init, bodies)
        };
        let refs: Vec<&[u8]> = pieces.iter().map(Vec::as_slice).collect();
        let vtt = subtitle::mp4_to_webvtt(init.as_deref(), &refs, zero, lim)
            .map_err(super::probe::map_parse)?;
        let merged = subtitle::merge_webvtt(
            &[subtitle::VttPart {
                body: vtt.as_bytes(),
            }],
            None,
            until,
        )
        .map_err(super::probe::map_parse)?;
        std::fs::write(&out, merged)?;
        return Ok(out);
    }
    let mut bodies: Vec<Vec<u8>> = Vec::new();
    if track.plan.whole {
        bodies.push(std::fs::read(whole_path(work, track))?);
    } else {
        for i in 0..track.plan.segments.len() {
            bodies.push(
                std::fs::read(piece_path(work, track, i))
                    .map_err(|_| Error::Media(MediaError::SegmentInvalid))?,
            );
        }
    }
    // Each piece is WebVTT on the stream's timeline, or TTML with absolute times.
    let mut converted: Vec<Vec<u8>> = Vec::with_capacity(bodies.len());
    for body in bodies {
        match sniff_segment(&body) {
            SegmentContainer::Ttml => {
                let vtt = subtitle::ttml_to_webvtt(&body, lim).map_err(super::probe::map_parse)?;
                converted.push(vtt.into_bytes());
            }
            SegmentContainer::Vtt => converted.push(body),
            _ => {
                return Err(Error::Media(MediaError::Unsupported(
                    "this subtitle format isn't supported yet".into(),
                )));
            }
        }
    }
    let parts: Vec<subtitle::VttPart<'_>> = converted
        .iter()
        .map(|body| subtitle::VttPart { body })
        .collect();
    let merged = subtitle::merge_webvtt(&parts, zero, until).map_err(super::probe::map_parse)?;
    std::fs::write(&out, merged)?;
    Ok(out)
}

/// A track to mux, before it is decided whether it is streamed or joined.
pub(crate) struct Source<'a> {
    pub track: &'a Track,
    pub kind: TrackKind,
    pub lang: Option<String>,
    pub adts: bool,
}

/// Where the named pipes for one rebuild go: under `$XDG_RUNTIME_DIR` (memory, private to the
/// user), else in the Work Directory.
fn pipe_dir(work: &Path) -> PathBuf {
    use std::hash::{Hash, Hasher};
    match std::env::var_os("XDG_RUNTIME_DIR").filter(|d| !d.is_empty()) {
        Some(run) => {
            let mut h = std::collections::hash_map::DefaultHasher::new();
            work.hash(&mut h);
            PathBuf::from(run)
                .join("siler")
                .join("media")
                .join(format!("{:016x}", h.finish()))
        }
        None => work.join("out").join("pipes"),
    }
}

/// Makes a named pipe only this user can open.
fn make_pipe(path: &Path) -> std::io::Result<()> {
    use rustix::fs::{CWD, FileType, Mode, mknodat};
    let _ = std::fs::remove_file(path);
    mknodat(CWD, path, FileType::Fifo, Mode::from_raw_mode(0o600), 0)?;
    Ok(())
}

/// Writes `files` in order into the pipe at `pipe`, once FFmpeg has opened it for reading.
/// Opening first would be wrong both ways: a writer that finishes before FFmpeg opens the
/// pipe takes the unread data with it, and FFmpeg then waits for a writer forever. The
/// caller aborts this when FFmpeg exits.
async fn feed(pipe: PathBuf, files: Vec<PathBuf>) -> std::io::Result<()> {
    let mut tx = loop {
        match tokio::net::unix::pipe::OpenOptions::new().open_sender(&pipe) {
            Ok(tx) => break tx,
            // No reader yet.
            Err(e) if e.raw_os_error() == Some(rustix::io::Errno::NXIO.raw_os_error()) => {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            Err(e) => return Err(e),
        }
    };
    for f in files {
        let mut src = tokio::fs::File::open(&f).await?;
        tokio::io::copy(&mut src, &mut tx).await?;
    }
    Ok(())
}

type Progress = Arc<dyn Fn(u32) + Send + Sync>;

/// Muxes `tracks` (and the subtitle files in `subs`) into `out`. Tracks are streamed through
/// named pipes when `stream` is set; if that can't be set up, or FFmpeg fails reading from
/// the pipes, the tracks are joined into files and FFmpeg runs again. Returns `Ok(false)`
/// when stopped through `cancel`.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn mux(
    ff: &Path,
    work: &Path,
    tracks: &[Source<'_>],
    subs: Vec<MuxInput>,
    output: Output,
    out: &Path,
    cancel: &CancellationToken,
    total_us: u64,
    progress: Progress,
    stream: bool,
) -> Result<bool> {
    if stream {
        let dir = pipe_dir(work);
        match streamed(
            ff, work, &dir, tracks, &subs, output, out, cancel, total_us, &progress,
        )
        .await
        {
            Ok(Some(done)) => {
                let _ = std::fs::remove_dir_all(&dir);
                return Ok(done);
            }
            Ok(None) => {}
            Err(e) if cancel.is_cancelled() => {
                let _ = std::fs::remove_dir_all(&dir);
                return Err(e);
            }
            Err(e) => {
                tracing::warn!("rebuilding through pipes failed ({e}); joining the tracks first");
            }
        }
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_file(out);
    }
    let mut inputs = Vec::with_capacity(tracks.len() + subs.len());
    for s in tracks {
        inputs.push(MuxInput {
            path: join_track(work, s.track)?,
            kind: s.kind,
            lang: s.lang.clone(),
            adts: s.adts,
        });
    }
    inputs.extend(subs);
    let args = mux_args(&inputs, output, out);
    super::ffmpeg::run(ff, &args, cancel, total_us, move |p| progress(p)).await
}

/// The streamed attempt. `Ok(None)` when no track needs streaming (each is one file already).
#[allow(clippy::too_many_arguments)]
async fn streamed(
    ff: &Path,
    work: &Path,
    dir: &Path,
    tracks: &[Source<'_>],
    subs: &[MuxInput],
    output: Output,
    out: &Path,
    cancel: &CancellationToken,
    total_us: u64,
    progress: &Progress,
) -> Result<Option<bool>> {
    let mut inputs = Vec::with_capacity(tracks.len() + subs.len());
    let mut feeds = Vec::new();
    for (i, s) in tracks.iter().enumerate() {
        let path = match one_file(work, s.track)? {
            Some(p) => p,
            // HDS fragments are rewritten while they are joined: not through a pipe.
            None if s.track.container == SegmentContainer::F4f => join_track(work, s.track)?,
            None => {
                let files = pieces_of(work, s.track)?;
                if feeds.is_empty() {
                    // Pipes left by an attempt that was killed (same download, same folder).
                    let _ = std::fs::remove_dir_all(dir);
                    std::fs::create_dir_all(dir)?;
                    use std::os::unix::fs::PermissionsExt;
                    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
                }
                let pipe = dir.join(format!("in{i}.{}", s.track.container.extension()));
                make_pipe(&pipe)?;
                feeds.push((pipe.clone(), files));
                pipe
            }
        };
        inputs.push(MuxInput {
            path,
            kind: s.kind,
            lang: s.lang.clone(),
            adts: s.adts,
        });
    }
    if feeds.is_empty() {
        return Ok(None);
    }
    inputs.extend(subs.iter().map(|m| MuxInput {
        path: m.path.clone(),
        kind: m.kind,
        lang: m.lang.clone(),
        adts: m.adts,
    }));
    // From a pipe, FFmpeg can't seek back: a track that needs it (an MP4 with its index at
    // the end) fails to demux, and FFmpeg would still exit cleanly with an empty or partial
    // output. `-xerror` makes that an error, so the file attempt takes over.
    let mut args = mux_args(&inputs, output, out);
    args.insert(1, "-xerror".into());
    let mut writers: Vec<_> = feeds
        .into_iter()
        .map(|(pipe, files)| tokio::spawn(feed(pipe, files)))
        .collect();
    let p = progress.clone();
    let res = super::ffmpeg::run(ff, &args, cancel, total_us, move |x| p(x)).await;
    // FFmpeg finished reading every pipe, so each writer is done: a writer that failed means
    // FFmpeg saw a track cut short, and the output can't be trusted.
    let mut fed = true;
    for w in &mut writers {
        if matches!(res, Ok(true)) {
            match tokio::time::timeout(Duration::from_secs(10), &mut *w).await {
                Ok(Ok(Ok(()))) => {}
                Ok(Ok(Err(e))) => {
                    tracing::warn!("writing a track into FFmpeg failed: {e}");
                    fed = false;
                }
                _ => fed = false,
            }
        }
        w.abort();
    }
    match res {
        Ok(true) if !fed => Err(Error::Media(MediaError::FfmpegFailed(
            "a track couldn't be passed to FFmpeg".into(),
        ))),
        r => r.map(Some),
    }
}

/// One input to the mux.
pub(crate) struct MuxInput {
    pub path: PathBuf,
    pub kind: TrackKind,
    pub lang: Option<String>,
    /// AAC in ADTS framing (MPEG-TS or raw AAC): needs `aac_adtstoasc` for MP4.
    pub adts: bool,
}

/// The three-letter ISO 639-2 code MP4 and Matroska store for a language tag (`en` → `eng`).
/// Unknown languages are left untagged rather than tagged wrongly.
pub(crate) fn iso639_2(lang: &str) -> Option<&'static str> {
    let base = lang.split(['-', '_']).next()?.to_ascii_lowercase();
    Some(match base.as_str() {
        "en" => "eng",
        "de" => "deu",
        "fr" => "fra",
        "es" => "spa",
        "it" => "ita",
        "pt" => "por",
        "ru" => "rus",
        "ja" => "jpn",
        "zh" => "zho",
        "ko" => "kor",
        "nl" => "nld",
        "pl" => "pol",
        "sv" => "swe",
        "da" => "dan",
        "no" => "nor",
        "fi" => "fin",
        "tr" => "tur",
        "ar" => "ara",
        "hi" => "hin",
        "cs" => "ces",
        "el" => "ell",
        "he" => "heb",
        "hu" => "hun",
        "uk" => "ukr",
        _ => return None,
    })
}

/// The FFmpeg arguments that stream-copy the inputs into one output file (media-design §13.4).
/// Paths are passed with the `file:` prefix, and the protocol whitelist admits only files and
/// pipes. Nothing from a manifest is ever an argument: only Siler's own paths.
pub(crate) fn mux_args(inputs: &[MuxInput], output: Output, out: &Path) -> Vec<String> {
    let mut a: Vec<String> = [
        "-nostdin",
        "-hide_banner",
        "-nostats",
        "-loglevel",
        "error",
        "-progress",
        "pipe:1",
        "-protocol_whitelist",
        "file,pipe",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    for i in inputs {
        a.push("-i".into());
        a.push(format!("file:{}", i.path.display()));
    }
    // Streams are mapped by type, so a segment's timed metadata (ID3 in MPEG-TS) never
    // reaches a container that can't hold it. The video input keeps its own audio only when
    // no audio track was chosen separately (and an audio-only stream works the same way).
    let separate_audio = inputs.iter().any(|i| i.kind == TrackKind::Audio);
    for (idx, i) in inputs.iter().enumerate() {
        let maps: &[&str] = match i.kind {
            TrackKind::Video if separate_audio => &["v?"],
            TrackKind::Video => &["v?", "a?"],
            TrackKind::Audio => &["a"],
            TrackKind::Subtitle => &["s"],
        };
        for m in maps {
            a.push("-map".into());
            a.push(format!("{idx}:{m}"));
        }
    }
    a.push("-c".into());
    a.push("copy".into());
    let mp4 = output == Output::Mp4;
    if mp4 {
        if inputs.iter().any(|i| i.kind == TrackKind::Subtitle) {
            // WebVTT can't be stream-copied into MP4; mov_text is its MP4 form.
            a.push("-c:s".into());
            a.push("mov_text".into());
        }
        let audio: Vec<&MuxInput> = inputs
            .iter()
            .filter(|i| i.kind == TrackKind::Audio)
            .collect();
        if !audio.is_empty() && audio.iter().all(|i| i.adts) {
            a.push("-bsf:a".into());
            a.push("aac_adtstoasc".into());
        }
        a.push("-movflags".into());
        a.push("+faststart".into());
        a.push("-f".into());
        a.push("mp4".into());
    } else {
        a.push("-f".into());
        a.push("matroska".into());
    }
    let (mut ai, mut si) = (0usize, 0usize);
    for i in inputs {
        match i.kind {
            TrackKind::Audio => {
                if let Some(l) = i.lang.as_deref().and_then(iso639_2) {
                    a.push(format!("-metadata:s:a:{ai}"));
                    a.push(format!("language={l}"));
                }
                ai += 1;
            }
            TrackKind::Subtitle => {
                if let Some(l) = i.lang.as_deref().and_then(iso639_2) {
                    a.push(format!("-metadata:s:s:{si}"));
                    a.push(format!("language={l}"));
                }
                si += 1;
            }
            TrackKind::Video => {}
        }
    }
    a.push("-y".into());
    a.push(format!("file:{}", out.display()));
    a
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(kind: TrackKind, lang: Option<&str>, adts: bool) -> MuxInput {
        MuxInput {
            path: PathBuf::from(format!("/w/{kind:?}.bin")),
            kind,
            lang: lang.map(str::to_string),
            adts,
        }
    }

    #[test]
    fn mp4_mux_table() {
        let inputs = [
            input(TrackKind::Video, None, false),
            input(TrackKind::Audio, Some("en"), true),
            input(TrackKind::Subtitle, Some("de"), false),
        ];
        let a = mux_args(&inputs, Output::Mp4, Path::new("/w/out.mp4"));
        let joined = a.join(" ");
        assert!(
            joined.contains("-i file:/w/Video.bin -i file:/w/Audio.bin -i file:/w/Subtitle.bin")
        );
        assert!(joined.contains("-map 0:v? -map 1:a -map 2:s -c copy"));
        assert!(joined.contains("-c:s mov_text"));
        assert!(joined.contains("-bsf:a aac_adtstoasc"));
        assert!(
            joined.contains("-f mp4 -movflags +faststart")
                || joined.contains("-movflags +faststart -f mp4")
        );
        assert!(joined.contains("-metadata:s:a:0 language=en"));
        assert!(joined.contains("-metadata:s:s:0 language=de"));
        assert!(joined.contains("-protocol_whitelist file,pipe"));
        assert!(joined.ends_with("-y file:/w/out.mp4"));
    }

    #[test]
    fn mkv_keeps_subtitles_and_skips_the_mp4_fixes() {
        let inputs = [
            input(TrackKind::Video, None, false),
            input(TrackKind::Audio, None, false),
            input(TrackKind::Subtitle, None, false),
        ];
        let joined = mux_args(&inputs, Output::Mkv, Path::new("/w/o.mkv")).join(" ");
        assert!(joined.contains("-f matroska"));
        assert!(!joined.contains("mov_text"));
        assert!(!joined.contains("aac_adtstoasc"));
        assert!(!joined.contains("-movflags"));
    }

    #[test]
    fn a_video_track_keeps_its_own_audio_only_without_a_separate_one() {
        let alone = [input(TrackKind::Video, None, false)];
        let joined = mux_args(&alone, Output::Mkv, Path::new("/w/o.mkv")).join(" ");
        assert!(joined.contains("-map 0:v? -map 0:a? -c copy"), "{joined}");
        let with_audio = [
            input(TrackKind::Video, None, false),
            input(TrackKind::Audio, Some("en"), false),
        ];
        let joined = mux_args(&with_audio, Output::Mkv, Path::new("/w/o.mkv")).join(" ");
        assert!(joined.contains("-map 0:v? -map 1:a -c copy"), "{joined}");
        assert!(!joined.contains("0:a"), "{joined}");
    }

    #[test]
    fn the_bsf_needs_every_audio_input_to_be_adts() {
        let inputs = [
            input(TrackKind::Video, None, false),
            input(TrackKind::Audio, None, true),
            input(TrackKind::Audio, None, false),
        ];
        assert!(
            !mux_args(&inputs, Output::Mp4, Path::new("/w/o.mp4"))
                .join(" ")
                .contains("aac_adtstoasc")
        );
    }

    #[test]
    fn a_join_writes_init_then_pieces() {
        use siler_media::{Object, Protection, Segment, SegmentPlan, TrackId};
        let dir = std::env::temp_dir().join(format!("siler-join-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let track = Track {
            id: TrackId::of(&["j"]),
            kind: TrackKind::Video,
            lang: None,
            name: None,
            codecs: None,
            container: SegmentContainer::Fmp4,
            bandwidth: None,
            resolution: None,
            channels: None,
            is_default: false,
            protection: Protection::Clear,
            plan: SegmentPlan {
                inits: vec![siler_media::model::Init::plain(Object {
                    url: url::Url::parse("https://x/i").expect("URL"),
                    range: None,
                })],
                segments: (0..3)
                    .map(|i| Segment {
                        seq: i,
                        object: Object {
                            url: url::Url::parse("https://x/s").expect("URL"),
                            range: None,
                        },
                        init: Some(0),
                        key: None,
                        discontinuity: 0,
                        start: 0.0,
                        duration: 1.0,
                    })
                    .collect(),
                whole: false,
            },
        };
        std::fs::create_dir_all(track_dir(&dir, &track)).expect("dirs");
        std::fs::write(init_path(&dir, &track, 0), b"INIT").expect("init");
        for i in 0..3 {
            std::fs::write(piece_path(&dir, &track, i), format!("p{i}")).expect("piece");
        }
        let out = join_track(&dir, &track).expect("joins");
        assert_eq!(std::fs::read(&out).expect("read"), b"INITp0p1p2");
        // The pieces are gone, and joining again reuses the joined file.
        assert!(!piece_path(&dir, &track, 0).exists());
        assert!(!init_path(&dir, &track, 0).exists());
        assert_eq!(join_track(&dir, &track).expect("again"), out);
        std::fs::remove_file(&out).expect("remove");
        assert!(matches!(
            join_track(&dir, &track),
            Err(Error::Media(MediaError::SegmentInvalid))
        ));
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ---------------------------------------------------------- rebuilds with FFmpeg

    fn ffmpeg() -> Option<PathBuf> {
        let found = std::env::var_os("SILER_FFMPEG")
            .map(PathBuf::from)
            .or_else(|| {
                std::env::var_os("PATH").and_then(|p| {
                    std::env::split_paths(&p)
                        .map(|d| d.join("ffmpeg"))
                        .find(|f| f.is_file())
                })
            });
        if found.is_none() && std::env::var_os("SILER_REQUIRE_FFMPEG").is_some() {
            panic!("SILER_REQUIRE_FFMPEG is set but there is no FFmpeg");
        }
        found
    }

    fn fixture(rel: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/media")
            .join(rel)
    }

    /// A track of `n` segments (with an init when `init`), its pieces copied from `files`.
    fn staged(
        work: &Path,
        id: &str,
        container: SegmentContainer,
        files: &[PathBuf],
        init: bool,
    ) -> Track {
        use siler_media::{Object, Protection, Segment, SegmentPlan, TrackId};
        let obj = || Object {
            url: url::Url::parse("https://x/s").expect("URL"),
            range: None,
        };
        let n = files.len() - usize::from(init);
        let track = Track {
            id: TrackId::of(&[id]),
            kind: TrackKind::Video,
            lang: None,
            name: None,
            codecs: None,
            container,
            bandwidth: None,
            resolution: None,
            channels: None,
            is_default: false,
            protection: Protection::Clear,
            plan: SegmentPlan {
                inits: if init {
                    vec![siler_media::model::Init::plain(obj())]
                } else {
                    vec![]
                },
                segments: (0..n)
                    .map(|i| Segment {
                        seq: i as u64,
                        object: obj(),
                        init: init.then_some(0),
                        key: None,
                        discontinuity: 0,
                        start: 2.0 * i as f64,
                        duration: 2.0,
                    })
                    .collect(),
                whole: false,
            },
        };
        std::fs::create_dir_all(track_dir(work, &track)).expect("dirs");
        let mut it = files.iter();
        if init {
            std::fs::copy(it.next().expect("init"), init_path(work, &track, 0)).expect("copy");
        }
        for (i, f) in it.enumerate() {
            std::fs::copy(f, piece_path(work, &track, i)).expect("copy");
        }
        track
    }

    fn fmp4_files() -> Vec<PathBuf> {
        let mut v = vec![fixture("fmp4/init.mp4")];
        v.extend((0..3).map(|i| fixture(&format!("fmp4/seg{i}.m4s"))));
        v
    }

    fn ts_files() -> Vec<PathBuf> {
        (0..3)
            .map(|i| fixture(&format!("muxed/seg{i}.ts")))
            .collect()
    }

    fn no_progress() -> Progress {
        Arc::new(|_| {})
    }

    /// What FFmpeg's own prober says the output holds: stream kinds and the duration.
    fn probe_out(ff: &Path, file: &Path) -> (Vec<String>, f64) {
        let probe = ff.with_file_name("ffprobe");
        let out = std::process::Command::new(probe)
            .args([
                "-v",
                "error",
                "-show_entries",
                "stream=codec_type:format=duration",
                "-of",
                "csv=p=0",
            ])
            .arg(file)
            .output()
            .expect("ffprobe runs");
        let text = String::from_utf8_lossy(&out.stdout).to_string();
        let mut kinds = Vec::new();
        let mut dur = 0.0;
        for l in text.lines().map(str::trim).filter(|l| !l.is_empty()) {
            match l.parse::<f64>() {
                Ok(d) => dur = d,
                Err(_) => kinds.push(l.trim_end_matches(',').to_string()),
            }
        }
        (kinds, dur)
    }

    fn sources<'a>(v: &'a Track, a: &'a Track) -> Vec<Source<'a>> {
        vec![
            Source {
                track: v,
                kind: TrackKind::Video,
                lang: None,
                adts: false,
            },
            Source {
                track: a,
                kind: TrackKind::Audio,
                lang: Some("en".into()),
                adts: true,
            },
        ]
    }

    #[tokio::test]
    async fn tracks_stream_through_pipes_without_a_joined_copy() {
        let Some(ff) = ffmpeg() else { return };
        let work = tempfile::tempdir().expect("tmp");
        let v = staged(
            work.path(),
            "v",
            SegmentContainer::Fmp4,
            &fmp4_files(),
            true,
        );
        let a = staged(work.path(), "a", SegmentContainer::Ts, &ts_files(), false);
        let out = work.path().join("out.mp4");
        let done = mux(
            &ff,
            work.path(),
            &sources(&v, &a),
            vec![],
            Output::Mp4,
            &out,
            &CancellationToken::new(),
            6_000_000,
            no_progress(),
            true,
        )
        .await
        .expect("muxes");
        assert!(done);
        let (kinds, dur) = probe_out(&ff, &out);
        assert_eq!(kinds, ["video", "audio"]);
        assert!((dur - 6.0).abs() < 0.6, "{dur}");
        // Streamed: no joined file was written, and the pieces are still there.
        assert!(!joined_path(work.path(), &v).exists());
        assert!(!joined_path(work.path(), &a).exists());
        assert!(piece_path(work.path(), &v, 2).exists());
        assert!(!pipe_dir(work.path()).exists(), "the pipes are removed");
    }

    #[tokio::test]
    async fn without_streaming_the_tracks_are_joined_first() {
        let Some(ff) = ffmpeg() else { return };
        let work = tempfile::tempdir().expect("tmp");
        let v = staged(
            work.path(),
            "v",
            SegmentContainer::Fmp4,
            &fmp4_files(),
            true,
        );
        let a = staged(work.path(), "a", SegmentContainer::Ts, &ts_files(), false);
        let out = work.path().join("out.mkv");
        let done = mux(
            &ff,
            work.path(),
            &sources(&v, &a),
            vec![],
            Output::Mkv,
            &out,
            &CancellationToken::new(),
            6_000_000,
            no_progress(),
            false,
        )
        .await
        .expect("muxes");
        assert!(done);
        assert_eq!(probe_out(&ff, &out).0, ["video", "audio"]);
        assert!(joined_path(work.path(), &v).exists());
        assert!(!piece_path(work.path(), &v, 0).exists());
    }

    /// A regular MP4 with its index at the end can't be read from a pipe: the rebuild joins
    /// the track into a file and runs FFmpeg again.
    #[tokio::test]
    async fn a_track_ffmpeg_cant_read_from_a_pipe_is_joined_instead() {
        let Some(ff) = ffmpeg() else { return };
        let work = tempfile::tempdir().expect("tmp");
        let plain = work.path().join("plain.mp4");
        // Big enough that FFmpeg can't keep all of it in its read buffer.
        let st = std::process::Command::new(&ff)
            .args(["-v", "error", "-y", "-stream_loop", "40", "-i"])
            .arg(fixture("muxed/seg0.ts"))
            .args(["-c", "copy", "-bsf:a", "aac_adtstoasc", "-f", "mp4"])
            .arg(&plain)
            .status()
            .expect("ffmpeg runs");
        assert!(st.success());
        let t = staged(
            work.path(),
            "m",
            SegmentContainer::Fmp4,
            std::slice::from_ref(&plain),
            false,
        );
        let src = [Source {
            track: &t,
            kind: TrackKind::Video,
            lang: None,
            adts: false,
        }];
        let out = work.path().join("out.mkv");
        let done = mux(
            &ff,
            work.path(),
            &src,
            vec![],
            Output::Mkv,
            &out,
            &CancellationToken::new(),
            2_000_000,
            no_progress(),
            true,
        )
        .await
        .expect("muxes");
        assert!(done);
        let (kinds, dur) = probe_out(&ff, &out);
        assert_eq!(kinds, ["video", "audio"]);
        // The whole file, not the empty output FFmpeg leaves when the pipe can't be read.
        assert!(dur > 70.0, "{dur}");
        assert!(
            joined_path(work.path(), &t).exists(),
            "fell back to a joined file"
        );
    }

    #[tokio::test]
    async fn a_rebuild_stopped_partway_keeps_the_pieces_for_the_next_attempt() {
        let Some(ff) = ffmpeg() else { return };
        let work = tempfile::tempdir().expect("tmp");
        let v = staged(
            work.path(),
            "v",
            SegmentContainer::Fmp4,
            &fmp4_files(),
            true,
        );
        let a = staged(work.path(), "a", SegmentContainer::Ts, &ts_files(), false);
        let out = work.path().join("out.mkv");
        let cancel = CancellationToken::new();
        cancel.cancel();
        let done = mux(
            &ff,
            work.path(),
            &sources(&v, &a),
            vec![],
            Output::Mkv,
            &out,
            &cancel,
            6_000_000,
            no_progress(),
            true,
        )
        .await
        .expect("stops");
        assert!(!done);
        assert!(piece_path(work.path(), &v, 0).exists());
        assert!(!joined_path(work.path(), &v).exists());
        assert!(!pipe_dir(work.path()).exists());
        let done = mux(
            &ff,
            work.path(),
            &sources(&v, &a),
            vec![],
            Output::Mkv,
            &out,
            &CancellationToken::new(),
            6_000_000,
            no_progress(),
            true,
        )
        .await
        .expect("muxes");
        assert!(done);
        assert_eq!(probe_out(&ff, &out).0, ["video", "audio"]);
    }
}
