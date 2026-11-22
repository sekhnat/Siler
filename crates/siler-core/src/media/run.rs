//! One media download, from resolving its source to publishing the output (media-design
//! §11). Every attempt re-reads the manifest, binds the saved selection by track id, and
//! checks the fingerprint, so completed pieces are reused across pauses, restarts and expired
//! links. Pieces are fetched with bounded concurrency; a stale link triggers at most three
//! manifest refreshes per attempt.

use reqwest::Url;
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering::Relaxed;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::OnceCell;
use tokio::task::JoinSet;

use siler_media::classify;
use siler_media::crypto::{self, AesKey};
use siler_media::select::{self, Output, Selection, SubtitleMode};
use siler_media::{Key, ParseLimits, Presentation, SegmentContainer, Track, TrackKind, protection};

use super::probe;
use super::reconstruct::{self, MuxInput, Source};
use crate::error::MediaError;
use crate::http::{self, Http, Limiters, Overflow, Req};
use crate::manager::Inner;
use crate::net::host_key;
use crate::ratelimit::RateLimiter;
use crate::task::{Outcome, Running, retry_delay};
use crate::{Error, Event, OnConflict, Result, Status, filename};

use super::{MediaProgress, SourceHint, same, workdir};

/// Everything a piece download needs, shared by its tasks.
pub(super) struct Fetch {
    http: Http,
    global: Arc<RateLimiter>,
    pub(super) run: Arc<Running>,
    /// One cell per key URL: the first piece that needs a key fetches it, and the pieces
    /// that need it meanwhile wait for that fetch instead of starting their own.
    keys: Mutex<HashMap<Url, Arc<OnceCell<Arc<AesKey>>>>>,
    max_key: u64,
    /// What the browser said about the source (an inline manifest, ranged fetching).
    hint: Option<SourceHint>,
    /// What the chosen tracks are, to find them in a manifest read again.
    picked: Vec<same::Picked>,
    /// The size of every piece being fetched, as far as it is known.
    sizes: Mutex<Sizes>,
}

/// What the pieces of a download add up to: `bytes_total` while it downloads.
#[derive(Default)]
struct Sizes {
    /// Tracks joined in an earlier attempt (their pieces are gone).
    joined: u64,
    known: HashMap<PathBuf, u64>,
    unknown: HashSet<PathBuf>,
}

impl Sizes {
    fn total(&self) -> Option<u64> {
        self.unknown
            .is_empty()
            .then(|| self.joined + self.known.values().sum::<u64>())
    }
}

impl Fetch {
    /// Notes the size of the pieces about to be fetched: a finished piece by its file, a
    /// ranged one by its range; a whole file learns its own when its first answer comes.
    fn plan_sizes(&self, work: &Path, jobs: &VecDeque<Job>) {
        let mut s = Sizes {
            joined: joined_bytes(work),
            ..Sizes::default()
        };
        for j in jobs {
            let size = std::fs::metadata(&j.dest)
                .map(|m| m.len())
                .ok()
                .or(j.range.map(|(a, b)| b - a + 1));
            match size {
                Some(n) => {
                    s.known.insert(j.dest.clone(), n);
                }
                None => {
                    s.unknown.insert(j.dest.clone());
                }
            }
        }
        let total = s.total();
        *self.sizes.lock().expect("sizes lock") = s;
        set_media(&self.run, |m| m.bytes_total = total);
    }

    /// A whole file's size, from the server.
    fn sized(&self, dest: &Path, n: u64) {
        let total = {
            let mut s = self.sizes.lock().expect("sizes lock");
            s.unknown.remove(dest);
            s.known.insert(dest.to_path_buf(), n);
            s.total()
        };
        set_media(&self.run, |m| m.bytes_total = total);
    }
}

impl Fetch {
    /// The links come from a manifest the page gave as text, so they can't be refreshed.
    fn fixed_links(&self) -> bool {
        self.hint.as_ref().is_some_and(|h| h.manifest.is_some())
    }
}

impl Fetch {
    /// The limiters, and the download's byte count (for the pieces kept on disk).
    fn io(&self) -> Limiters<'_> {
        Limiters {
            global: &self.global,
            run: &self.run.limiter,
            count: Some(&self.run.downloaded),
        }
    }

    /// The limiters alone: for manifests and keys, which aren't part of the download.
    fn io_uncounted(&self) -> Limiters<'_> {
        Limiters {
            count: None,
            ..self.io()
        }
    }

    /// Bytes fetched again to compare, by way of `scratch`; they don't count.
    pub(super) async fn fetch_range_again(
        &self,
        url: &Url,
        range: (u64, u64),
        scratch: &Path,
    ) -> Result<Vec<u8>> {
        self.read_again(url, Some(range), scratch).await
    }

    /// A piece as it would be kept, fetched again to compare.
    pub(super) async fn fetch_again(&self, job: &Job, scratch: &Path) -> Result<Vec<u8>> {
        let raw = self.read_again(&job.url, job.range, scratch).await?;
        plain_piece(self, job, raw).await
    }

    async fn read_again(
        &self,
        url: &Url,
        range: Option<(u64, u64)>,
        scratch: &Path,
    ) -> Result<Vec<u8>> {
        let io = self.io_uncounted();
        http::fetch_object(&self.http, url, scratch, range, false, &io).await?;
        let bytes = tokio::fs::read(scratch).await?;
        tokio::fs::remove_file(scratch).await?;
        Ok(bytes)
    }
}

/// One object to fetch into the Work Directory.
#[derive(Clone)]
pub(super) struct Job {
    pub(super) dest: PathBuf,
    pub(super) url: Url,
    /// Inclusive byte range.
    pub(super) range: Option<(u64, u64)>,
    pub(super) key: Option<Key>,
    pub(super) seq: u64,
    /// A whole-file track: fetched resumably, not as a piece.
    pub(super) whole: bool,
    /// An initialisation segment (checked for encryption boxes).
    pub(super) init: bool,
    /// Wait this long before starting (a retry).
    pub(super) delay: Duration,
}

/// The pieces every track needs, for the files that are not already there.
fn jobs_of(pres: &Presentation, sel: &Selection, work: &Path) -> Result<Vec<Job>> {
    let resolved =
        select::resolve(pres, sel).map_err(|_| Error::Media(MediaError::SelectionUnavailable))?;
    let mut jobs = Vec::new();
    for t in resolved.tracks() {
        let dir = reconstruct::track_dir(work, t);
        std::fs::create_dir_all(&dir)?;
        if reconstruct::joined_path(work, t).exists() {
            // Joined in an earlier attempt (its pieces are gone): nothing to fetch.
            continue;
        }
        if t.plan.whole {
            if let Some(obj) = t.plan.segments.first() {
                jobs.push(Job {
                    dest: reconstruct::whole_path(work, t),
                    url: obj.object.url.clone(),
                    range: None,
                    key: None,
                    seq: 0,
                    whole: true,
                    init: false,
                    delay: Duration::ZERO,
                });
            }
            continue;
        }
        for i in 0..t.plan.inits.len() {
            jobs.push(Job::init(work, t, i));
        }
        for i in 0..t.plan.segments.len() {
            jobs.push(Job::piece(work, t, i));
        }
    }
    Ok(jobs)
}

impl Job {
    /// Initialisation segment `i` of a track.
    pub(super) fn init(work: &Path, t: &Track, i: usize) -> Job {
        let init = &t.plan.inits[i];
        Job {
            dest: reconstruct::init_path(work, t, i),
            url: init.object.url.clone(),
            range: range_of(init.object.range),
            key: init.key.clone(),
            seq: 0,
            whole: false,
            init: true,
            delay: Duration::ZERO,
        }
    }

    /// Segment `i` of a track.
    pub(super) fn piece(work: &Path, t: &Track, i: usize) -> Job {
        let seg = &t.plan.segments[i];
        Job {
            dest: reconstruct::piece_path(work, t, i),
            url: seg.object.url.clone(),
            range: range_of(seg.object.range),
            key: seg.key.clone(),
            seq: seg.seq,
            whole: false,
            init: false,
            delay: Duration::ZERO,
        }
    }
}

fn range_of(r: Option<siler_media::ByteRange>) -> Option<(u64, u64)> {
    r.filter(|r| r.length > 0)
        .map(|r| (r.offset, r.offset + r.length - 1))
}

/// Fetches one object. Pieces are checked (not HTML, decryptable, not DRM) before they are
/// kept; a whole-file track resumes from its `.part` file.
pub(super) async fn fetch_job(c: Arc<Fetch>, job: Job) -> Result<()> {
    if !job.delay.is_zero() {
        tokio::time::sleep(job.delay).await;
    }
    let io = c.io();
    if job.whole {
        let chunk = c.hint.as_ref().and_then(|h| h.chunk_bytes);
        let size = match chunk {
            Some(n) => {
                let sized = |size| c.sized(&job.dest, size);
                http::fetch_whole_chunked(&c.http, &job.url, &job.dest, n, &io, &sized).await?
            }
            None => http::fetch_object(&c.http, &job.url, &job.dest, None, true, &io).await?,
        };
        let head = read_head(&job.dest).await?;
        if classify::looks_like_html(&head) {
            let _ = tokio::fs::remove_file(&job.dest).await;
            uncount(&c.run, size);
            return Err(Error::Media(MediaError::SegmentInvalid));
        }
        return Ok(());
    }
    let raw = PathBuf::from(format!("{}.raw", job.dest.display()));
    http::fetch_object(&c.http, &job.url, &raw, job.range, false, &io).await?;
    let bytes = tokio::fs::read(&raw).await?;
    let _ = tokio::fs::remove_file(&raw).await;
    let fetched = bytes.len() as u64;
    let kept = keep_piece(&c, &job, bytes).await;
    if kept.is_err() {
        // The piece isn't kept, so its bytes don't count.
        uncount(&c.run, fetched);
    }
    kept
}

/// Takes `n` bytes back from the download's count.
pub(super) fn uncount(run: &Running, n: u64) {
    let _ = run
        .downloaded
        .try_update(Relaxed, Relaxed, |v| Some(v.saturating_sub(n)));
}

/// A fetched piece as it is kept: not a web page, and decrypted.
async fn plain_piece(c: &Fetch, job: &Job, bytes: Vec<u8>) -> Result<Vec<u8>> {
    if classify::looks_like_html(&bytes) {
        return Err(Error::Media(MediaError::SegmentInvalid));
    }
    let Some(k) = &job.key else {
        return Ok(bytes);
    };
    let key = key_for(c, &k.url, &c.io_uncounted()).await?;
    let iv = k.iv.unwrap_or_else(|| crypto::iv_from_sequence(job.seq));
    let plain = crypto::decrypt_aes128_cbc(&bytes, &key, &iv)
        .map_err(|_| Error::Media(MediaError::Decrypt))?;
    // An init's IV may have been guessed (no IV on the key line): the result must
    // look like the start of an initialisation segment.
    if job.init && !classify::looks_like_init(&plain) {
        return Err(Error::Media(MediaError::Decrypt));
    }
    Ok(plain)
}

/// Checks a fetched piece and keeps it: not a web page, decrypted, not DRM-protected.
async fn keep_piece(c: &Fetch, job: &Job, bytes: Vec<u8>) -> Result<()> {
    let data = plain_piece(c, job, bytes).await?;
    if job.init && protection::init_protection(&data).is_some() {
        return Err(Error::Media(MediaError::Drm));
    }
    let part = http::part_of(&job.dest);
    tokio::fs::write(&part, &data).await?;
    tokio::fs::rename(&part, &job.dest).await?;
    Ok(())
}

/// The first bytes of a file, to recognise what it is.
async fn read_head(path: &Path) -> Result<Vec<u8>> {
    use tokio::io::AsyncReadExt;
    let mut buf = Vec::with_capacity(512);
    tokio::fs::File::open(path)
        .await?
        .take(512)
        .read_to_end(&mut buf)
        .await?;
    Ok(buf)
}

/// An AES-128 key, fetched once per URL and kept in memory only. A failed fetch leaves the
/// cell empty, so the piece's retry fetches it again.
async fn key_for(c: &Fetch, url: &Url, io: &Limiters<'_>) -> Result<Arc<AesKey>> {
    let cell = c
        .keys
        .lock()
        .expect("keys lock")
        .entry(url.clone())
        .or_default()
        .clone();
    cell.get_or_try_init(|| async {
        let f = http::fetch_bounded(&c.http, url, Req::Derived, c.max_key, Overflow::Refuse, io)
            .await?;
        Ok(Arc::new(AesKey::from_bytes(&f.body).map_err(|e| {
            Error::Media(MediaError::KeyFetch(e.to_string()))
        })?))
    })
    .await
    .cloned()
}

pub(super) fn set_media(run: &Running, f: impl FnOnce(&mut MediaProgress)) {
    if let Some(m) = run.media.lock().expect("media lock").as_mut() {
        f(m);
    }
}

/// The fingerprint of the selected tracks of a presentation.
fn fingerprint_of(pres: &Presentation, sel: &Selection) -> Result<String> {
    let r =
        select::resolve(pres, sel).map_err(|_| Error::Media(MediaError::SelectionUnavailable))?;
    Ok(select::fingerprint(&r))
}

/// Fails when the stream the downloaded pieces belong to has changed.
fn check_fingerprint(old: Option<&str>, new: &str, work: &Path) -> Result<()> {
    match old {
        Some(old) if old != new && work.exists() => Err(Error::Media(MediaError::ManifestChanged)),
        _ => Ok(()),
    }
}

/// Fetches every missing piece with bounded concurrency. Returns `false` when stopped.
#[allow(clippy::too_many_arguments)]
async fn fetch_all(
    c: &Arc<Fetch>,
    pres: &mut Presentation,
    sel: &Selection,
    url: &Url,
    lim: &ParseLimits,
    fingerprint: &str,
    work: &Path,
    mut pending: VecDeque<Job>,
    max_retries: u32,
    concurrency: usize,
) -> Result<bool> {
    let mut set: JoinSet<(Job, Result<()>)> = JoinSet::new();
    let mut inflight: HashSet<PathBuf> = HashSet::new();
    let mut retries: HashMap<PathBuf, u32> = HashMap::new();
    let mut refreshes = 0u32;
    let total = pending.iter().filter(|j| !j.init).count() as u64;
    c.plan_sizes(work, &pending);
    let mut done = 0u64;
    set_media(&c.run, |m| {
        m.phase = "downloading";
        m.segments_total = total;
        m.segments_done = 0;
    });
    loop {
        let cap = concurrency
            .min(c.run.target.load(Relaxed))
            .min(c.run.primary_cap())
            .max(1);
        while set.len() < cap {
            let Some(job) = pending.pop_front() else {
                break;
            };
            if job.dest.exists() {
                if !job.init {
                    done += 1;
                }
                continue;
            }
            if inflight.contains(&job.dest) {
                continue;
            }
            inflight.insert(job.dest.clone());
            let cc = c.clone();
            set.spawn(async move {
                let r = fetch_job(cc, job.clone()).await;
                (job, r)
            });
        }
        set_media(&c.run, |m| {
            m.segments_done = done;
        });
        if set.is_empty() {
            break;
        }
        tokio::select! {
            _ = c.run.cancel.cancelled() => {
                set.shutdown().await;
                return Ok(false);
            }
            _ = c.run.changed.notified() => {}
            joined = set.join_next() => {
                let Some(joined) = joined else { continue };
                let (job, res) = joined.map_err(|e| Error::Protocol(format!("A download task stopped unexpectedly: {e}")))?;
                inflight.remove(&job.dest);
                match res {
                    Ok(()) => {
                        if !job.init {
                            done += 1;
                        }
                    }
                    Err(_) if c.run.cancel.is_cancelled() => {}
                    Err(e) => {
                        let stale = matches!(e, Error::Http(401 | 403 | 404 | 410))
                            || matches!(e, Error::Media(MediaError::SegmentInvalid));
                        if c.fixed_links() && matches!(e, Error::Http(401 | 403 | 404 | 410)) {
                            set.shutdown().await;
                            return Err(Error::Media(MediaError::LinksExpired));
                        }
                        if stale && refreshes < 3 && !c.fixed_links() {
                            refreshes += 1;
                            tracing::info!("media {url}: refreshing the playlist ({refreshes} of 3)");
                            let mut fresh = refresh(c, url, lim).await?;
                            same::adopt(&mut fresh, &c.picked);
                            let now = fingerprint_of(&fresh, sel)?;
                            if check_fingerprint(Some(fingerprint), &now, work).is_err() {
                                same::verify(c, &fresh, sel, work).await?;
                            }
                            *pres = fresh;
                            pending = jobs_of(pres, sel, work)?
                                .into_iter()
                                .filter(|j| !j.dest.exists() && !inflight.contains(&j.dest))
                                .collect();
                            continue;
                        }
                        let transient = e.is_transient() || matches!(e, Error::Media(MediaError::SegmentInvalid));
                        let attempt = retries.entry(job.dest.clone()).or_insert(0);
                        if transient && *attempt < max_retries {
                            *attempt += 1;
                            let mut again = job.clone();
                            again.delay = retry_delay(*attempt, e.retry_after());
                            pending.push_front(again);
                        } else {
                            set.shutdown().await;
                            if stale {
                                return Err(Error::Media(MediaError::RefreshExhausted));
                            }
                            return Err(e);
                        }
                    }
                }
            }
        }
    }
    Ok(true)
}

/// Re-reads the manifest for fresh segment links.
async fn refresh(c: &Fetch, url: &Url, lim: &ParseLimits) -> Result<Presentation> {
    let io = c.io_uncounted();
    let src = probe::resolve(&c.http, url, c.hint.as_ref(), lim, &io).await?;
    super::presentation_or_error(src, false)
}

/// Re-reads a live stream's playlist for its newest segments. A manifest the page gave as
/// text can't be read again, so a live stream is always read from its address.
pub(super) async fn refresh_live(c: &Fetch, url: &Url, lim: &ParseLimits) -> Result<Presentation> {
    let io = c.io_uncounted();
    let src = probe::resolve(&c.http, url, None, lim, &io).await?;
    super::presentation_or_error(src, true)
}

/// One subtitle track, joined into a WebVTT file.
pub(super) struct SubFile {
    /// For the sidecar's name: its language, or its number (only letters, digits and `-`).
    label: String,
    lang: Option<String>,
    path: PathBuf,
}

/// The output and the subtitle files to write beside it.
type Built = (PathBuf, Vec<SubFile>);

/// Joins the fetched pieces and muxes them with FFmpeg. `None` when stopped.
#[allow(clippy::too_many_arguments)]
async fn build(
    run: &Arc<Running>,
    pres: &Presentation,
    sel: &Selection,
    output: Output,
    subtitles: SubtitleMode,
    work: &Path,
    ffmpeg: Option<&Path>,
    lim: &ParseLimits,
) -> Result<Option<Built>> {
    let resolved =
        select::resolve(pres, sel).map_err(|_| Error::Media(MediaError::SelectionUnavailable))?;
    set_media(run, |m| {
        m.phase = "rebuilding";
        m.permille = 0;
    });
    // The subtitles follow the picture: zero where the video (or audio) starts, and no cue
    // past the end of the longest video or audio track.
    let main = resolved.video.or_else(|| resolved.audio.first().copied());
    // Beside FFmpeg; a bare `ffmpeg` (found on PATH) has a bare `ffprobe` beside it.
    let ffprobe = ffmpeg
        .map(|f| f.with_file_name("ffprobe"))
        .filter(|p| p.is_file() || p.components().count() == 1);
    let zero = match main {
        Some(t) if !resolved.subtitles.is_empty() => {
            reconstruct::start_time(work, t, ffprobe.as_deref()).await
        }
        _ => None,
    };
    let until = resolved
        .video
        .into_iter()
        .chain(resolved.audio.iter().copied())
        .map(|t| t.plan.duration())
        .fold(None, |m: Option<f64>, d| Some(m.map_or(d, |m| m.max(d))))
        .filter(|d| *d > 0.0);
    let mut sub_files: Vec<SubFile> = Vec::new();
    for (k, t) in resolved.subtitles.iter().enumerate() {
        sub_files.push(SubFile {
            label: file_label(t.lang.as_deref(), k),
            lang: t.lang.clone(),
            path: reconstruct::join_subtitles(work, t, lim, zero, until)?,
        });
    }
    if output == Output::Original {
        // One track in its own format; a subtitle track is its joined WebVTT.
        let path = match (
            resolved.video.or_else(|| resolved.audio.first().copied()),
            sub_files.first(),
        ) {
            (Some(track), _) => reconstruct::join_track(work, track)?,
            (None, Some(sub)) => sub.path.clone(),
            (None, None) => return Err(Error::Media(MediaError::SelectionUnavailable)),
        };
        return Ok(Some((path, Vec::new())));
    }
    let ff = ffmpeg.ok_or(Error::Media(MediaError::FfmpegMissing))?;
    let mut total_us = 0f64;
    for t in resolved.tracks() {
        total_us = total_us.max(t.plan.duration() * 1e6);
    }
    let mut tracks = Vec::new();
    if let Some(v) = resolved.video {
        tracks.push(Source {
            track: v,
            kind: TrackKind::Video,
            lang: None,
            adts: false,
        });
    }
    for a in &resolved.audio {
        tracks.push(Source {
            track: a,
            kind: TrackKind::Audio,
            lang: a.lang.clone(),
            adts: matches!(a.container, SegmentContainer::Ts | SegmentContainer::Aac),
        });
    }
    let mut subs = Vec::new();
    let mut sidecars = Vec::new();
    match subtitles {
        SubtitleMode::Embed => {
            for sub in &sub_files {
                subs.push(MuxInput {
                    path: sub.path.clone(),
                    kind: TrackKind::Subtitle,
                    lang: sub.lang.clone(),
                    adts: false,
                });
            }
        }
        SubtitleMode::Sidecar => sidecars = sub_files,
        SubtitleMode::None => {}
    }
    let ext = super::extension(output, None);
    let out = work.join("out").join(format!("result.{ext}"));
    std::fs::create_dir_all(work.join("out"))?;
    // A rebuild that was stopped part way leaves its output behind.
    let _ = std::fs::remove_file(&out);
    let run2 = run.clone();
    let finished = reconstruct::mux(
        ff,
        work,
        &tracks,
        subs,
        output,
        &out,
        &run.cancel,
        total_us as u64,
        Arc::new(move |p| set_media(&run2, |m| m.permille = p)),
        true,
    )
    .await?;
    if !finished {
        return Ok(None);
    }
    Ok(Some((out, sidecars)))
}

/// A subtitle's label in a file name: its language when it has a usable one, else its
/// number. The language comes from the manifest, so only letters, digits and `-` are kept.
fn file_label(lang: Option<&str>, index: usize) -> String {
    let clean: String = lang
        .unwrap_or("")
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
        .take(16)
        .collect();
    let clean = clean.trim_matches('-').to_string();
    if clean.is_empty() {
        (index + 1).to_string()
    } else {
        clean
    }
}

/// The bytes already downloaded into a Work Directory: finished pieces, and the part of a
/// whole-file track (which is resumed).
fn bytes_on_disk(work: &Path) -> u64 {
    let Ok(tracks) = std::fs::read_dir(work.join("t")) else {
        return 0;
    };
    let mut total = 0;
    for t in tracks.flatten() {
        let Ok(files) = std::fs::read_dir(t.path()) else {
            continue;
        };
        for f in files.flatten() {
            let name = f.file_name();
            let name = name.to_string_lossy();
            let counts = name.ends_with(".seg")
                || (name.starts_with("init-") && name.ends_with(".bin"))
                || name == "whole.bin"
                || name == "whole.bin.part"
                || (name.starts_with("track.") && !name.ends_with(".part"));
            if counts {
                total += f.metadata().map(|m| m.len()).unwrap_or(0);
            }
        }
    }
    total
}

/// The bytes of tracks joined in an earlier attempt, which `bytes_on_disk` counts.
fn joined_bytes(work: &Path) -> u64 {
    let Ok(tracks) = std::fs::read_dir(work.join("t")) else {
        return 0;
    };
    tracks
        .flatten()
        .flat_map(|t| std::fs::read_dir(t.path()).into_iter().flatten().flatten())
        .filter(|f| {
            let name = f.file_name();
            let name = name.to_string_lossy();
            name.starts_with("track.") && !name.ends_with(".part")
        })
        .map(|f| f.metadata().map(|m| m.len()).unwrap_or(0))
        .sum()
}

/// Free space on the filesystem holding `dir`, against what the download still needs: the
/// pieces not yet fetched, and the output written while the pieces are still there.
fn check_space(dir: &Path, need: Option<u64>, have: u64) -> Result<()> {
    let Some(need) = need else { return Ok(()) };
    if let Ok(s) = rustix::fs::statvfs(dir) {
        let avail = s.f_bavail.saturating_mul(s.f_frsize);
        let wanted = need.saturating_sub(have) as f64 + need as f64 * 1.1;
        if (avail as f64) < wanted {
            return Err(Error::Media(MediaError::DiskSpace));
        }
    }
    Ok(())
}

/// Runs a media download attempt. Mirrors `Inner::drive` for files: `Outcome::Stopped` when
/// paused or removed, `Outcome::Done` after publishing, an error otherwise.
pub(crate) async fn drive(inner: &Arc<Inner>, id: i64, run: &Arc<Running>) -> Result<Outcome> {
    let rec = inner
        .state()
        .records
        .get(&id)
        .cloned()
        .ok_or(Error::NotFound)?;
    let mr = rec
        .media
        .clone()
        .ok_or_else(|| Error::Protocol("This download has no media details.".into()))?;
    let s = inner.settings();
    let lim = ParseLimits::default();
    let url = Url::parse(&rec.url).map_err(|e| Error::InvalidUrl(e.to_string()))?;
    let dir = rec
        .dir
        .clone()
        .ok_or_else(|| Error::Protocol("This download has no folder.".into()))?;
    let name = rec
        .filename
        .clone()
        .ok_or_else(|| Error::Protocol("This download has no name.".into()))?;
    if rec.on_conflict == OnConflict::Skip && dir.join(&name).exists() {
        return Err(Error::Exists(name));
    }
    let work = workdir(&dir, &name);
    set_media_start(run);
    *run.part.lock().expect("part lock") = Some(work.clone());
    let have = bytes_on_disk(&work);
    run.downloaded.store(have, Relaxed);
    *run.sample.lock().expect("sample lock") = (std::time::Instant::now(), have, 0.0);

    let c = Arc::new(Fetch {
        http: Http::new(
            inner.net.clone(),
            &url,
            rec.meta.clone(),
            &s.user_agent,
            s.allow_private_media_addresses,
        ),
        global: inner.global.clone(),
        run: run.clone(),
        keys: Mutex::new(HashMap::new()),
        max_key: lim.max_key_bytes as u64,
        hint: mr.hint.clone(),
        picked: mr.picked.clone(),
        sizes: Mutex::new(Sizes::default()),
    });

    // Some sites only serve the video after a wait (YouTube, while its ad would play).
    if let Some(at) = mr.hint.as_ref().and_then(|h| h.not_before_ms) {
        let wait = at.saturating_sub(crate::manager::now_ms().max(0) as u64);
        if wait > 0 {
            set_media(run, |m| m.phase = "waiting");
            tokio::select! {
                _ = tokio::time::sleep(Duration::from_millis(wait)) => {}
                _ = run.cancel.cancelled() => return Ok(Outcome::Stopped),
            }
            set_media(run, |m| m.phase = "resolving");
        }
    }

    // Resolve the source: its manifest and playlists.
    let src = {
        let io = c.io_uncounted();
        tokio::select! {
            r = probe::resolve(&c.http, &url, mr.hint.as_ref(), &lim, &io) => r?,
            _ = run.cancel.cancelled() => return Ok(Outcome::Stopped),
        }
    };
    let final_url = src.final_url.clone();
    let mut pres = super::presentation_or_error(src, mr.live)?;
    inner.join_host(id, run, host_key(&final_url));
    if mr.live {
        return record_live(
            inner, id, run, &c, &url, pres, &mr, &dir, &name, &work, &s, &lim,
        )
        .await;
    }
    same::adopt(&mut pres, &mr.picked);
    let resolved = select::resolve(&pres, &mr.selection)
        .map_err(|_| Error::Media(MediaError::SelectionUnavailable))?;
    let fingerprint = select::fingerprint(&resolved);
    let need = select::estimate_bytes(&resolved);
    if check_fingerprint(mr.fingerprint.as_deref(), &fingerprint, &work).is_err() {
        // Fresh links change the fingerprint; the pieces say whether it is the same stream.
        tokio::select! {
            r = same::verify(&c, &pres, &mr.selection, &work) => r?,
            _ = run.cancel.cancelled() => return Ok(Outcome::Stopped),
        }
    }
    let picked = match mr.picked.is_empty() {
        true => same::picked(&pres, &mr.selection),
        false => mr.picked.clone(),
    };
    if mr.fingerprint.as_deref() != Some(fingerprint.as_str()) || picked != mr.picked {
        let mut st = inner.state();
        if let Some(r) = st.records.get_mut(&id) {
            if let Some(m) = r.media.as_mut() {
                m.fingerprint = Some(fingerprint.clone());
                m.picked = picked;
            }
            inner.persist_and_emit(r, Some(run))?;
        }
    }
    let ffmpeg = inner.ffmpeg();
    if mr.output != Output::Original && ffmpeg.is_none() {
        return Err(Error::Media(MediaError::FfmpegMissing));
    }
    std::fs::create_dir_all(&dir)?;
    check_space(&dir, need, have)?;

    // Fetch the pieces that are missing.
    {
        let mut st = inner.state();
        if let Some(r) = st.records.get_mut(&id) {
            r.status = Status::Downloading;
            r.error = None;
            inner.persist_and_emit(r, Some(run))?;
        }
    }
    let pending: VecDeque<Job> = jobs_of(&pres, &mr.selection, &work)?.into();
    let concurrency = s.media_segment_concurrency as usize;
    if !fetch_all(
        &c,
        &mut pres,
        &mr.selection,
        &url,
        &lim,
        &fingerprint,
        &work,
        pending,
        s.max_retries,
        concurrency,
    )
    .await?
    {
        return Ok(Outcome::Stopped);
    }

    // Rebuild the output.
    let Some((out, sidecar_sources)) = build(
        run,
        &pres,
        &mr.selection,
        mr.output,
        mr.subtitles,
        &work,
        ffmpeg.as_deref(),
        &lim,
    )
    .await?
    else {
        return Ok(Outcome::Stopped);
    };

    publish(inner, id, run, out, sidecar_sources, &dir, &name, &work)
}

/// Moves the rebuilt output (and subtitle files) into place under the download's name,
/// applying its conflict policy, and marks the download complete.
#[allow(clippy::too_many_arguments)]
pub(super) fn publish(
    inner: &Arc<Inner>,
    id: i64,
    run: &Arc<Running>,
    out: PathBuf,
    sidecar_sources: Vec<SubFile>,
    dir: &Path,
    name: &str,
    work: &Path,
) -> Result<Outcome> {
    set_media(run, |m| m.phase = "publishing");
    // A whole-file track's name says nothing of its format: the download's does.
    let named = if out
        .file_name()
        .is_some_and(|f| f == reconstruct::WHOLE_FILE)
    {
        Path::new(name)
    } else {
        out.as_path()
    };
    let ext = named
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("bin")
        .to_string();
    let stem = name.rsplit_once('.').map_or(name, |(s, _)| s).to_string();
    let target_name = format!("{stem}.{ext}");
    let mut st = inner.state();
    let Some(on_conflict) = st.records.get(&id).map(|r| r.on_conflict) else {
        // Removed in the last moment; run() deletes the Work Directory.
        return Ok(Outcome::Stopped);
    };
    let final_name = if dir.join(&target_name).exists() && on_conflict != OnConflict::Overwrite {
        filename::unique_name(dir, &target_name, |n| {
            crate::manager::name_claimed(&st, dir, n, Some(id))
        })
    } else {
        target_name
    };
    let path = dir.join(&final_name);
    std::fs::rename(&out, &path)?;
    let final_stem = final_name
        .rsplit_once('.')
        .map_or(final_name.as_str(), |(s, _)| s)
        .to_string();
    let mut sidecars = Vec::new();
    for sub in &sidecar_sources {
        let wanted = format!("{final_stem}.{}.vtt", sub.label);
        let wanted = if dir.join(&wanted).exists() && on_conflict != OnConflict::Overwrite {
            filename::unique_name(dir, &wanted, |_| false)
        } else {
            wanted
        };
        std::fs::copy(&sub.path, dir.join(&wanted))?;
        sidecars.push(wanted);
    }
    let _ = std::fs::remove_dir_all(work);
    *run.part.lock().expect("part lock") = None;

    let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
    let r = st.records.get_mut(&id).ok_or(Error::NotFound)?;
    r.filename = Some(final_name.clone());
    r.status = Status::Completed;
    r.completed_at = Some(crate::manager::now_ms());
    r.segments.clear();
    r.downloaded = size;
    r.total_size = Some(size);
    if let Some(m) = r.media.as_mut() {
        m.sidecars = sidecars;
    }
    inner.persist_and_emit(r, None)?;
    drop(st);
    inner.emit(Event::Completed {
        id,
        filename: final_name,
        path,
    });
    Ok(Outcome::Done)
}

/// A live stream: recorded until Finish, then rebuilt and published like any download.
#[allow(clippy::too_many_arguments)]
async fn record_live(
    inner: &Arc<Inner>,
    id: i64,
    run: &Arc<Running>,
    c: &Arc<Fetch>,
    url: &Url,
    pres: Presentation,
    mr: &super::MediaRecord,
    dir: &Path,
    name: &str,
    work: &Path,
    s: &crate::Settings,
    lim: &ParseLimits,
) -> Result<Outcome> {
    let ffmpeg = inner.ffmpeg();
    if mr.output != Output::Original && ffmpeg.is_none() {
        return Err(Error::Media(MediaError::FfmpegMissing));
    }
    std::fs::create_dir_all(dir)?;
    {
        let mut st = inner.state();
        if let Some(r) = st.records.get_mut(&id) {
            r.status = Status::Downloading;
            r.error = None;
            inner.persist_and_emit(r, Some(run))?;
        }
    }
    let Some((recorded, sel)) = super::live::record(
        c,
        url,
        pres,
        &mr.selection,
        work,
        lim,
        s.max_retries,
        mr.finish,
    )
    .await?
    else {
        return Ok(Outcome::Stopped);
    };
    {
        // Finished: from now on the download is rebuilt, never recorded again.
        let mut st = inner.state();
        if let Some(r) = st.records.get_mut(&id)
            && let Some(m) = r.media.as_mut()
            && !m.finish
        {
            m.finish = true;
            inner.persist_and_emit(r, Some(run))?;
        }
    }
    let Some((out, sidecars)) = build(
        run,
        &recorded,
        &sel,
        mr.output,
        SubtitleMode::None,
        work,
        ffmpeg.as_deref(),
        lim,
    )
    .await?
    else {
        return Ok(Outcome::Stopped);
    };
    publish(inner, id, run, out, sidecars, dir, name, work)
}

fn set_media_start(run: &Running) {
    *run.media.lock().expect("media lock") = Some(MediaProgress::default());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranges_are_inclusive() {
        assert_eq!(
            range_of(Some(siler_media::ByteRange {
                offset: 10,
                length: 5
            })),
            Some((10, 14))
        );
        assert_eq!(
            range_of(Some(siler_media::ByteRange {
                offset: 10,
                length: 0
            })),
            None
        );
        assert_eq!(range_of(None), None);
    }

    #[test]
    fn subtitle_labels_are_safe_in_file_names() {
        assert_eq!(file_label(Some("en-US"), 0), "en-US");
        assert_eq!(file_label(Some("../../etc/x"), 1), "etcx");
        assert_eq!(file_label(Some("/"), 2), "3");
        assert_eq!(file_label(None, 0), "1");
        assert_eq!(file_label(Some("a".repeat(40).as_str()), 0).len(), 16);
    }

    #[test]
    fn only_a_changed_stream_is_refused() {
        let dir = std::env::temp_dir();
        assert!(check_fingerprint(None, "x", &dir).is_ok());
        assert!(check_fingerprint(Some("x"), "x", &dir).is_ok());
        assert!(matches!(
            check_fingerprint(Some("x"), "y", &dir),
            Err(Error::Media(MediaError::ManifestChanged))
        ));
        let missing = dir.join("siler-no-such-workdir-8c1f");
        assert!(check_fingerprint(Some("x"), "y", &missing).is_ok());
    }
}
