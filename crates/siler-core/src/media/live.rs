//! Recording a live HLS or DASH stream: the playlist is read again every few seconds and
//! every new segment is kept, until the user presses Finish or the stream ends. Pieces are
//! numbered in the order they were recorded, so the usual rebuild joins them.
//!
//! What has been recorded is noted in `live.json` in the Work Directory, so a recording
//! paused (or interrupted by quitting) continues from the live edge when resumed; the
//! segments missed meanwhile are counted as a gap.

use futures_util::StreamExt;
use reqwest::Url;
use serde::{Deserialize, Serialize};
use siler_media::select::{self, Selection};
use siler_media::{
    ParseLimits, Presentation, Segment, SegmentPlan, Track, TrackId, TrackKind, Variant,
};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::Ordering::Relaxed;
use std::time::Duration;

use super::reconstruct;
use super::run::{Fetch, Job, fetch_job, set_media, uncount};
use crate::error::MediaError;
use crate::{Error, Result};

/// Segments taken from a stream already running when the recording starts (as players do:
/// a few target durations behind the live edge).
const START_SEGMENTS: usize = 3;
/// Reloads in a row that may fail before the recording gives up.
const MAX_RELOAD_FAILURES: u32 = 10;
/// Segments of one track fetched at once when a recording has fallen behind the live edge.
const MAX_PARALLEL: usize = 4;

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
struct TrackState {
    /// The last segment recorded (HLS media sequence, DASH `$Number$`).
    last_seq: Option<u64>,
    /// The durations of the pieces recorded, in order.
    durations: Vec<f64>,
    /// The initialisation segment was fetched.
    init: bool,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub(crate) struct LiveState {
    tracks: BTreeMap<String, TrackState>,
    gaps: u32,
}

impl LiveState {
    fn path(work: &Path) -> PathBuf {
        work.join("live.json")
    }

    pub(crate) fn load(work: &Path) -> LiveState {
        std::fs::read(Self::path(work))
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }

    fn save(&self, work: &Path) -> Result<()> {
        std::fs::create_dir_all(work)?;
        let tmp = work.join("live.json.part");
        std::fs::write(&tmp, serde_json::to_vec(self).unwrap_or_default())?;
        std::fs::rename(tmp, Self::path(work))?;
        Ok(())
    }

    fn seconds(&self) -> f64 {
        self.tracks
            .values()
            .map(|t| t.durations.iter().sum::<f64>())
            .fold(0.0, f64::max)
    }

    fn pieces(&self) -> u64 {
        self.tracks.values().map(|t| t.durations.len() as u64).sum()
    }
}

/// The tracks a recording keeps: the chosen video and audio (subtitles of a live stream
/// aren't recorded).
fn recorded(sel: &Selection) -> Selection {
    Selection {
        video: sel.video.clone(),
        audio: sel.audio.clone(),
        subtitles: Vec::new(),
    }
}

/// How long to wait before reading the playlist again: about one segment.
fn interval(pres: &Presentation, sel: &Selection) -> Duration {
    let secs = select::resolve(pres, sel)
        .ok()
        .and_then(|r| {
            r.tracks()
                .iter()
                .flat_map(|t| t.plan.segments.iter().map(|s| s.duration))
                .reduce(f64::max)
        })
        .unwrap_or(4.0);
    Duration::from_secs_f64(secs.clamp(1.0, 10.0))
}

fn progress(c: &Fetch, state: &LiveState) {
    let secs = state.seconds();
    let pieces = state.pieces();
    let gaps = state.gaps;
    set_media(&c.run, |m| {
        m.phase = "recording";
        m.segments_done = pieces;
        m.segments_total = pieces;
        m.recorded_secs = Some(secs);
        m.gaps = gaps;
    });
}

/// One segment, tried a few times.
async fn fetch_segment(c: Arc<Fetch>, job: Job, max_retries: u32) -> Result<()> {
    let mut attempt = 0;
    loop {
        match fetch_job(c.clone(), job.clone()).await {
            Ok(()) => return Ok(()),
            Err(e) if c.run.cancel.is_cancelled() => return Err(e),
            Err(e) if attempt < max_retries.min(3) => {
                attempt += 1;
                tracing::debug!("live segment {} failed ({e}), retry {attempt}", job.seq);
                tokio::time::sleep(crate::task::backoff(attempt)).await;
            }
            Err(e) => return Err(e),
        }
    }
}

/// Removes the pieces `from..to` of a track (fetched ahead of one that failed, or of a
/// Finish), so they don't count.
fn discard_pieces(c: &Fetch, work: &Path, track: &Track, from: usize, to: usize) {
    for i in from..to {
        let p = reconstruct::piece_path(work, track, i);
        if let Ok(m) = std::fs::metadata(&p) {
            uncount(&c.run, m.len());
        }
        let _ = std::fs::remove_file(&p);
        for ext in ["raw", "raw.part"] {
            let _ = std::fs::remove_file(format!("{}.{ext}", p.display()));
        }
    }
}

/// What one track adds in one round: its new segments, kept in order. A recording that has
/// fallen behind the live edge fetches a few at once. Finish stops the round, keeping the
/// segments fetched so far; each one shows in the progress as it arrives.
async fn round(
    c: &Arc<Fetch>,
    track: &Track,
    mut ts: TrackState,
    first_round: bool,
    work: &Path,
    max_retries: u32,
    shown: &std::sync::Mutex<LiveState>,
) -> (TrackState, bool, Result<()>) {
    if let Err(e) = std::fs::create_dir_all(reconstruct::track_dir(work, track)) {
        return (ts, false, Err(e.into()));
    }
    let segs = &track.plan.segments;
    let new: Vec<&Segment> = match ts.last_seq {
        Some(last) => segs.iter().filter(|s| s.seq > last).collect(),
        None if first_round => segs
            .iter()
            .skip(segs.len().saturating_sub(START_SEGMENTS))
            .collect(),
        None => segs.iter().collect(),
    };
    let gap = match (ts.last_seq, new.first()) {
        (Some(last), Some(first)) => first.seq > last + 1,
        _ => false,
    };
    if !ts.init
        && let Some(init) = new
            .first()
            .and_then(|s| s.init)
            .and_then(|i| track.plan.inits.get(i))
    {
        let job = Job {
            dest: reconstruct::init_path(work, track, 0),
            url: init.object.url.clone(),
            range: init
                .object
                .range
                .filter(|r| r.length > 0)
                .map(|r| (r.offset, r.offset + r.length - 1)),
            key: init.key.clone(),
            seq: 0,
            whole: false,
            init: true,
            delay: Duration::ZERO,
        };
        if let Err(e) = fetch_job(c.clone(), job).await {
            return (ts, gap, Err(e));
        }
        ts.init = true;
    }
    let base = ts.durations.len();
    let end = base + new.len();
    let jobs = new.iter().enumerate().map(|(i, s)| Job {
        dest: reconstruct::piece_path(work, track, base + i),
        url: s.object.url.clone(),
        range: s
            .object
            .range
            .filter(|r| r.length > 0)
            .map(|r| (r.offset, r.offset + r.length - 1)),
        key: s.key.clone(),
        seq: s.seq,
        whole: false,
        init: false,
        delay: Duration::ZERO,
    });
    let width = c
        .run
        .target
        .load(Relaxed)
        .min(c.run.primary_cap())
        .clamp(1, MAX_PARALLEL);
    // Owned futures (not a lazy map over borrowed segments), so the recording stays `Send`.
    let pending: Vec<_> = jobs
        .map(|job| fetch_segment(c.clone(), job, max_retries))
        .collect();
    let mut fetched = futures_util::stream::iter(pending).buffered(width);
    let finish = c.run.finish.clone();
    let mut next = new.iter();
    loop {
        let res = tokio::select! {
            biased;
            _ = finish.cancelled() => break,
            r = fetched.next() => r,
        };
        let (Some(res), Some(s)) = (res, next.next()) else {
            break;
        };
        match res {
            Ok(()) => {
                ts.durations.push(s.duration);
                ts.last_seq = Some(s.seq);
                let mut shown = shown.lock().expect("live progress lock");
                shown
                    .tracks
                    .insert(track.id.as_str().to_string(), ts.clone());
                progress(c, &shown);
            }
            // A segment that can't be had is skipped: the recording goes on without it.
            Err(e)
                if !c.run.cancel.is_cancelled() && !matches!(e, Error::Media(MediaError::Drm)) =>
            {
                tracing::info!("live segment {} skipped: {e}", s.seq);
                drop(fetched);
                discard_pieces(c, work, track, ts.durations.len(), end);
                ts.last_seq = Some(s.seq);
                return (ts, true, Ok(()));
            }
            Err(e) => return (ts, gap, Err(e)),
        }
    }
    drop(fetched);
    discard_pieces(c, work, track, ts.durations.len(), end);
    (ts, gap, Ok(()))
}

/// Records until Finish (or the end of the stream), then returns the presentation of what
/// was recorded, ready for the usual rebuild. `None` when paused or removed.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn record(
    c: &Arc<Fetch>,
    url: &Url,
    mut pres: Presentation,
    sel: &Selection,
    work: &Path,
    lim: &ParseLimits,
    max_retries: u32,
    finish_now: bool,
) -> Result<Option<(Presentation, Selection)>> {
    let sel = recorded(sel);
    let mut state = LiveState::load(work);
    let mut first_round = state.tracks.is_empty();
    let mut failures = 0u32;
    progress(c, &state);
    let finish = c.run.finish.clone();
    while !finish_now && !finish.is_cancelled() {
        let resolved = select::resolve(&pres, &sel)
            .map_err(|_| Error::Media(MediaError::SelectionUnavailable))?;
        let tracks: Vec<Track> = resolved
            .tracks()
            .into_iter()
            .filter(|t| t.kind != TrackKind::Subtitle)
            .cloned()
            .collect();
        let shown = std::sync::Mutex::new(state.clone());
        let rounds = tracks.iter().map(|t| {
            let ts = state.tracks.get(t.id.as_str()).cloned().unwrap_or_default();
            round(c, t, ts, first_round, work, max_retries, &shown)
        });
        let results = tokio::select! {
            r = futures_util::future::join_all(rounds) => r,
            _ = c.run.cancel.cancelled() => return Ok(None),
        };
        let mut gap = false;
        let mut grew = false;
        for (t, (ts, g, res)) in tracks.iter().zip(results) {
            res?;
            let before = state
                .tracks
                .get(t.id.as_str())
                .map_or(0, |s| s.durations.len());
            grew |= ts.durations.len() > before;
            gap |= g;
            state.tracks.insert(t.id.as_str().to_string(), ts);
        }
        if gap {
            state.gaps += 1;
        }
        first_round = false;
        state.save(work)?;
        progress(c, &state);
        if !pres.is_live() {
            // The stream ended (#EXT-X-ENDLIST, or the MPD became static).
            break;
        }
        let wait = if grew {
            interval(&pres, &sel)
        } else {
            interval(&pres, &sel) / 2
        };
        tokio::select! {
            _ = tokio::time::sleep(wait) => {}
            _ = finish.cancelled() => break,
            _ = c.run.cancel.cancelled() => return Ok(None),
        }
        match super::run::refresh_live(c, url, lim).await {
            Ok(p) => {
                pres = p;
                failures = 0;
            }
            Err(e) if c.run.cancel.is_cancelled() => return Err(e),
            Err(e) => {
                failures += 1;
                tracing::info!("live playlist reload failed ({e}), {failures} in a row");
                if failures >= MAX_RELOAD_FAILURES {
                    if state.pieces() == 0 {
                        return Err(e);
                    }
                    // The stream went away: keep what was recorded.
                    break;
                }
            }
        }
    }
    if state.pieces() == 0 {
        return Err(Error::Media(MediaError::Unsupported(
            "nothing was recorded before the stream was finished".into(),
        )));
    }
    Ok(Some(recorded_presentation(&pres, &sel, &state)?))
}

/// The presentation of what was recorded: the chosen tracks, each with one segment per
/// recorded piece (and the initialisation segment it was recorded with).
fn recorded_presentation(
    pres: &Presentation,
    sel: &Selection,
    state: &LiveState,
) -> Result<(Presentation, Selection)> {
    let resolved =
        select::resolve(pres, sel).map_err(|_| Error::Media(MediaError::SelectionUnavailable))?;
    let plan_of = |t: &Track| -> Track {
        let ts = state.tracks.get(t.id.as_str()).cloned().unwrap_or_default();
        let url = t
            .plan
            .segments
            .first()
            .map(|s| s.object.url.clone())
            .unwrap_or_else(|| Url::parse("http://recorded.invalid/").expect("valid URL"));
        let inits = if ts.init {
            t.plan.inits.first().cloned().into_iter().collect()
        } else {
            Vec::new()
        };
        let mut start = 0.0;
        let segments = ts
            .durations
            .iter()
            .enumerate()
            .map(|(i, d)| {
                let s = Segment {
                    seq: i as u64,
                    object: siler_media::Object {
                        url: url.clone(),
                        range: None,
                    },
                    init: ts.init.then_some(0),
                    key: None,
                    discontinuity: 0,
                    start,
                    duration: *d,
                };
                start += d;
                s
            })
            .collect();
        Track {
            plan: SegmentPlan {
                inits,
                segments,
                whole: false,
            },
            ..t.clone()
        }
    };
    let audio: Vec<Track> = resolved
        .audio
        .iter()
        .filter(|t| {
            state
                .tracks
                .get(t.id.as_str())
                .is_some_and(|s| !s.durations.is_empty())
        })
        .map(|t| plan_of(t))
        .collect();
    let variants: Vec<Variant> = match (resolved.variant, resolved.video) {
        (Some(v), Some(t))
            if state
                .tracks
                .get(t.id.as_str())
                .is_some_and(|s| !s.durations.is_empty()) =>
        {
            vec![Variant {
                video: plan_of(t),
                audio: audio.iter().map(|a| a.id.clone()).collect(),
                subtitles: Vec::new(),
                muxed_audio: v.muxed_audio,
            }]
        }
        _ => Vec::new(),
    };
    let sel = Selection {
        video: variants.first().map(|v| v.video.id.clone()),
        audio: audio.iter().map(|a| a.id.clone()).collect::<Vec<TrackId>>(),
        subtitles: Vec::new(),
    };
    let duration = state.seconds();
    Ok((
        Presentation {
            kind: pres.kind,
            liveness: siler_media::Liveness::Vod,
            duration: Some(duration),
            title: pres.title.clone(),
            variants,
            audio,
            subtitles: Vec::new(),
            warnings: Vec::new(),
            dropped_drm: 0,
        },
        sel,
    ))
}
