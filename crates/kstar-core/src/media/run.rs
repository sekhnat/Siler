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
use tokio::task::JoinSet;

use kstar_media::classify::{self, Kind};
use kstar_media::crypto::{self, AesKey};
use kstar_media::select::{self, Output, Selection, SubtitleMode};
use kstar_media::{Key, ParseLimits, Presentation, SegmentContainer, TrackKind, protection};

use super::probe;
use super::reconstruct::{self, MuxInput};
use crate::error::MediaError;
use crate::http::{self, Limiters, Overflow};
use crate::manager::Inner;
use crate::net::{Net, host_key};
use crate::ratelimit::RateLimiter;
use crate::task::{Outcome, Running, retry_delay};
use crate::{Error, Event, RequestMeta, Result, Status, filename};

use super::{MediaProgress, workdir};

/// Everything a piece download needs, shared by its tasks.
struct Fetch {
    /// Hosts whose addresses were checked against the URL policy for this download.
    checked: Mutex<HashSet<String>>,
    net: Arc<Net>,
    global: Arc<RateLimiter>,
    run: Arc<Running>,
    ua: String,
    meta: RequestMeta,
    allow_private: bool,
    keys: Mutex<HashMap<Url, Arc<AesKey>>>,
    max_key: u64,
}

impl Fetch {
    fn io(&self) -> Limiters<'_> {
        Limiters {
            global: &self.global,
            run: &self.run.limiter,
        }
    }
}

/// One object to fetch into the Work Directory.
#[derive(Clone)]
struct Job {
    dest: PathBuf,
    url: Url,
    /// Inclusive byte range.
    range: Option<(u64, u64)>,
    key: Option<Key>,
    seq: u64,
    /// A whole-file track: fetched resumably, not as a piece.
    whole: bool,
    /// An initialisation segment (checked for encryption boxes).
    init: bool,
    /// Wait this long before starting (a retry).
    delay: Duration,
}

/// The pieces every track needs, for the files that are not already there.
fn jobs_of(pres: &Presentation, sel: &Selection, work: &Path) -> Result<Vec<Job>> {
    let resolved =
        select::resolve(pres, sel).map_err(|_| Error::Media(MediaError::SelectionUnavailable))?;
    let mut jobs = Vec::new();
    for t in resolved.tracks() {
        let dir = reconstruct::track_dir(work, t);
        std::fs::create_dir_all(&dir)?;
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
        for (i, init) in t.plan.inits.iter().enumerate() {
            jobs.push(Job {
                dest: reconstruct::init_path(work, t, i),
                url: init.url.clone(),
                range: range_of(init.range),
                key: None,
                seq: 0,
                whole: false,
                init: true,
                delay: Duration::ZERO,
            });
        }
        for (i, seg) in t.plan.segments.iter().enumerate() {
            jobs.push(Job {
                dest: reconstruct::piece_path(work, t, i),
                url: seg.object.url.clone(),
                range: range_of(seg.object.range),
                key: seg.key.clone(),
                seq: seg.seq,
                whole: false,
                init: false,
                delay: Duration::ZERO,
            });
        }
    }
    Ok(jobs)
}

fn range_of(r: Option<kstar_media::ByteRange>) -> Option<(u64, u64)> {
    r.filter(|r| r.length > 0)
        .map(|r| (r.offset, r.offset + r.length - 1))
}

/// Fetches one object. Pieces are checked (not HTML, decryptable, not DRM) before they are
/// kept; whole files resume from their `.part` file.
impl Fetch {
    /// Checks a URL against the policy, once per host for this download.
    async fn guard(&self, url: &Url) -> Result<()> {
        let host = host_key(url);
        if self.checked.lock().expect("checked lock").contains(&host) {
            return Ok(());
        }
        probe::guard(&self.net, url, self.allow_private).await?;
        self.checked.lock().expect("checked lock").insert(host);
        Ok(())
    }
}

async fn fetch_job(c: Arc<Fetch>, job: Job) -> Result<u64> {
    if !job.delay.is_zero() {
        tokio::time::sleep(job.delay).await;
    }
    c.guard(&job.url).await?;
    let io = c.io();
    if job.whole {
        let have = tokio::fs::metadata(http::part_of(&job.dest))
            .await
            .ok()
            .map(|m| m.len())
            .filter(|n| *n > 0);
        return http::fetch_object(&c.net, &job.url, &c.meta, &c.ua, &job.dest, None, have, &io)
            .await;
    }
    let raw = PathBuf::from(format!("{}.raw", job.dest.display()));
    let n =
        http::fetch_object(&c.net, &job.url, &c.meta, &c.ua, &raw, job.range, None, &io).await?;
    let bytes = tokio::fs::read(&raw).await?;
    let _ = tokio::fs::remove_file(&raw).await;
    if classify::looks_like_html(&bytes) {
        return Err(Error::Media(MediaError::SegmentInvalid));
    }
    let data = match &job.key {
        Some(k) => {
            let key = key_for(&c, &k.url, &io).await?;
            let iv = k.iv.unwrap_or_else(|| crypto::iv_from_sequence(job.seq));
            crypto::decrypt_aes128_cbc(&bytes, &key, &iv)
                .map_err(|_| Error::Media(MediaError::Decrypt))?
        }
        None => bytes,
    };
    if job.init && protection::init_protection(&data).is_some() {
        return Err(Error::Media(MediaError::Drm));
    }
    let part = http::part_of(&job.dest);
    tokio::fs::write(&part, &data).await?;
    tokio::fs::rename(&part, &job.dest).await?;
    Ok(n)
}

/// An AES-128 key, fetched once per URL and kept in memory only.
async fn key_for(c: &Fetch, url: &Url, io: &Limiters<'_>) -> Result<Arc<AesKey>> {
    if let Some(k) = c.keys.lock().expect("keys lock").get(url) {
        return Ok(k.clone());
    }
    c.guard(url).await?;
    let f =
        http::fetch_bounded(&c.net, url, &c.meta, &c.ua, c.max_key, Overflow::Refuse, io).await?;
    let key = Arc::new(
        AesKey::from_bytes(&f.body)
            .map_err(|e| Error::Media(MediaError::KeyFetch(e.to_string())))?,
    );
    c.keys
        .lock()
        .expect("keys lock")
        .insert(url.clone(), key.clone());
    Ok(key)
}

fn set_media(run: &Running, f: impl FnOnce(&mut MediaProgress)) {
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
    let mut set: JoinSet<(Job, Result<u64>)> = JoinSet::new();
    let mut inflight: HashSet<PathBuf> = HashSet::new();
    let mut retries: HashMap<PathBuf, u32> = HashMap::new();
    let mut refreshes = 0u32;
    let total = pending.iter().filter(|j| !j.init).count() as u64;
    let mut done = 0u64;
    set_media(&c.run, |m| {
        m.phase = "downloading";
        m.segments_total = total;
        m.segments_done = 0;
    });
    loop {
        let cap = concurrency
            .min(c.run.target.load(Relaxed))
            .min(c.run.host_cap.load(Relaxed))
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
                    Ok(n) => {
                        c.run.downloaded.fetch_add(n, Relaxed);
                        if !job.init {
                            done += 1;
                        }
                    }
                    Err(_) if c.run.cancel.is_cancelled() => {}
                    Err(e) => {
                        let stale = matches!(e, Error::Http(401 | 403 | 404 | 410))
                            || matches!(e, Error::Media(MediaError::SegmentInvalid));
                        if stale && refreshes < 3 {
                            refreshes += 1;
                            tracing::info!("media {url}: refreshing the playlist ({refreshes} of 3)");
                            let fresh = refresh(c, url, lim).await?;
                            check_fingerprint(Some(fingerprint), &fingerprint_of(&fresh, sel)?, work)?;
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
    let io = c.io();
    let src = probe::source(&c.net, url, &c.meta, &c.ua, c.allow_private, lim, &io).await?;
    src.presentation.ok_or(Error::Media(MediaError::NotMedia))
}

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
) -> Result<Option<(PathBuf, Vec<(String, PathBuf)>)>> {
    let resolved =
        select::resolve(pres, sel).map_err(|_| Error::Media(MediaError::SelectionUnavailable))?;
    set_media(run, |m| {
        m.phase = "rebuilding";
        m.permille = 0;
    });
    let mut sidecars: Vec<(String, PathBuf)> = Vec::new();
    // Each subtitle track: a label (its language, or its number) and one WebVTT file.
    let mut sub_files: Vec<(String, PathBuf)> = Vec::new();
    for (k, t) in resolved.subtitles.iter().enumerate() {
        let f = reconstruct::join_subtitles(work, t, lim)?;
        sub_files.push((t.lang.clone().unwrap_or_else(|| (k + 1).to_string()), f));
    }
    if output == Output::Original {
        let track = resolved
            .video
            .or_else(|| resolved.audio.first().copied())
            .or_else(|| resolved.subtitles.first().copied())
            .ok_or(Error::Media(MediaError::SelectionUnavailable))?;
        let path = reconstruct::join_track(work, track)?;
        if subtitles == SubtitleMode::Sidecar {
            sidecars.extend(sub_files.iter().cloned());
        }
        return Ok(Some((path, sidecars)));
    }
    let ff = ffmpeg.ok_or(Error::Media(MediaError::FfmpegMissing))?;
    let mut inputs = Vec::new();
    let mut total_us = 0f64;
    for t in resolved.tracks() {
        total_us = total_us.max(t.plan.duration() * 1e6);
    }
    if let Some(v) = resolved.video {
        inputs.push(MuxInput {
            path: reconstruct::join_track(work, v)?,
            kind: TrackKind::Video,
            lang: None,
            adts: false,
        });
    }
    for a in &resolved.audio {
        inputs.push(MuxInput {
            path: reconstruct::join_track(work, a)?,
            kind: TrackKind::Audio,
            lang: a.lang.clone(),
            adts: matches!(a.container, SegmentContainer::Ts | SegmentContainer::Aac),
        });
    }
    if subtitles == SubtitleMode::Embed {
        for (label, path) in &sub_files {
            inputs.push(MuxInput {
                path: path.clone(),
                kind: TrackKind::Subtitle,
                lang: resolved_lang(label),
                adts: false,
            });
        }
    } else {
        sidecars.extend(sub_files.iter().cloned());
    }
    let ext = super::extension(output, None);
    let out = work.join("out").join(format!("result.{ext}"));
    std::fs::create_dir_all(work.join("out"))?;
    let args = reconstruct::mux_args(&inputs, output, &out);
    let run2 = run.clone();
    let finished = super::ffmpeg::run(ff, &args, &run.cancel, total_us as u64, move |p| {
        set_media(&run2, |m| m.permille = p);
    })
    .await?;
    if !finished {
        return Ok(None);
    }
    Ok(Some((out, sidecars)))
}

/// Free space on the filesystem holding `dir`, against what the download needs (the pieces
/// and the output are both on disk at once).
fn check_space(dir: &Path, need: Option<u64>) -> Result<()> {
    let Some(need) = need else { return Ok(()) };
    if let Ok(s) = rustix::fs::statvfs(dir) {
        let avail = s.f_bavail.saturating_mul(s.f_frsize);
        if (avail as f64) < need as f64 * 2.1 {
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
    let work = workdir(&dir, &name);
    set_media_start(run);
    *run.part.lock().expect("part lock") = Some(work.clone());

    let c = Arc::new(Fetch {
        checked: Mutex::new(HashSet::new()),
        net: inner.net.clone(),
        global: inner.global.clone(),
        run: run.clone(),
        ua: s.user_agent.clone(),
        meta: rec.meta.clone(),
        // As for the probe: a source on a private address may point at other private ones.
        allow_private: s.allow_private_media_addresses
            || kstar_media::policy::is_private_source(&url),
        keys: Mutex::new(HashMap::new()),
        max_key: lim.max_key_bytes as u64,
    });

    // Resolve the source: its manifest and playlists.
    let src = {
        let io = c.io();
        tokio::select! {
            r = probe::source(&inner.net, &url, &rec.meta, &s.user_agent, c.allow_private, &lim, &io) => r?,
            _ = run.cancel.cancelled() => return Ok(Outcome::Stopped),
        }
    };
    let Some(mut pres) = src.presentation else {
        return Err(Error::Media(match src.kind {
            Kind::Html | Kind::File | Kind::Direct | Kind::WebVtt => MediaError::NotMedia,
            Kind::Hds | Kind::Playlist => {
                MediaError::Unsupported("this playlist format isn't supported yet".into())
            }
            Kind::Hls | Kind::Dash => MediaError::NotMedia,
        }));
    };
    if pres.is_live() {
        return Err(Error::Media(MediaError::LiveUnsupported));
    }
    inner.join_host(id, run, host_key(&src.final_url));
    let resolved = select::resolve(&pres, &mr.selection)
        .map_err(|_| Error::Media(MediaError::SelectionUnavailable))?;
    let fingerprint = select::fingerprint(&resolved);
    let need = select::estimate_bytes(&resolved);
    check_fingerprint(mr.fingerprint.as_deref(), &fingerprint, &work)?;
    if mr.fingerprint.as_deref() != Some(fingerprint.as_str()) {
        let mut st = inner.state();
        if let Some(r) = st.records.get_mut(&id) {
            if let Some(m) = r.media.as_mut() {
                m.fingerprint = Some(fingerprint.clone());
            }
            inner.persist_and_emit(r, Some(run))?;
        }
    }
    let ffmpeg = inner.ffmpeg();
    if mr.output != Output::Original && ffmpeg.is_none() {
        return Err(Error::Media(MediaError::FfmpegMissing));
    }
    check_space(&dir, need)?;

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

    // Publish under the download's name, applying its conflict policy.
    set_media(run, |m| m.phase = "publishing");
    let ext = out
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("bin")
        .to_string();
    let stem = name
        .rsplit_once('.')
        .map_or(name.as_str(), |(s, _)| s)
        .to_string();
    let target_name = format!("{stem}.{ext}");
    let final_name =
        if dir.join(&target_name).exists() && rec.on_conflict != crate::OnConflict::Overwrite {
            filename::unique_name(&dir, &target_name, |n| dir.join(n).exists())
        } else {
            target_name
        };
    let path = dir.join(&final_name);
    std::fs::rename(&out, &path)?;
    let mut sidecars = Vec::new();
    for (label, src_path) in &sidecar_sources {
        let wanted = filename::unique_name(&dir, &format!("{stem}.{label}.vtt"), |n| {
            dir.join(n).exists()
        });
        std::fs::copy(src_path, dir.join(&wanted))?;
        sidecars.push(wanted);
    }
    let _ = std::fs::remove_dir_all(&work);
    *run.part.lock().expect("part lock") = None;

    let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
    {
        let mut st = inner.state();
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
    }
    inner.emit(Event::Completed {
        id,
        filename: final_name,
        path,
    });
    Ok(Outcome::Done)
}

fn set_media_start(run: &Running) {
    *run.media.lock().expect("media lock") = Some(MediaProgress::default());
}

/// A subtitle label is its language when it has one; a plain number otherwise. Only a
/// language becomes the MP4/MKV `language` tag.
fn resolved_lang(label: &str) -> Option<String> {
    (!label.is_empty() && !label.chars().all(|c| c.is_ascii_digit())).then(|| label.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranges_are_inclusive() {
        assert_eq!(
            range_of(Some(kstar_media::ByteRange {
                offset: 10,
                length: 5
            })),
            Some((10, 14))
        );
        assert_eq!(
            range_of(Some(kstar_media::ByteRange {
                offset: 10,
                length: 0
            })),
            None
        );
        assert_eq!(range_of(None), None);
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
        let missing = dir.join("kstar-no-such-workdir-8c1f");
        assert!(check_fingerprint(Some("x"), "y", &missing).is_ok());
    }
}
