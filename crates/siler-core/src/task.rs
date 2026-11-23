//! The transfer loop for one download: connection workers and their coordinator.

use futures_util::StreamExt;
use reqwest::Url;
use std::collections::HashMap;
use std::os::unix::fs::FileExt;
use std::path::PathBuf;
use std::sync::atomic::{
    AtomicBool, AtomicU8, AtomicU32, AtomicU64, AtomicUsize, Ordering::Relaxed,
};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::Notify;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

use crate::net::{Net, host_key};
use crate::ratelimit::RateLimiter;
use crate::segment::Table;
use crate::transport::{self, OpenError};
use crate::{Error, RequestMeta, Result};

pub(crate) const STOP_PAUSE: u8 = 1;
pub(crate) const STOP_REMOVE: u8 = 2;
pub(crate) const STOP_SHUTDOWN: u8 = 3;
/// The data quota for this period is used up: back to the queue until the next one.
pub(crate) const STOP_QUOTA: u8 = 4;

/// Live state of a running download, shared between its task, its workers and the
/// engine's progress ticker.
pub(crate) struct Running {
    pub cancel: CancellationToken,
    /// Cancelled once the download's task has fully stopped and its state is saved.
    pub finished: CancellationToken,
    /// A recording (live stream, RTMP/MMS) should end now and be kept: unlike `cancel`, the
    /// download goes on to build and publish what it has.
    pub finish: CancellationToken,
    pub stop_reason: AtomicU8,
    /// Connections the user wants; changes take effect while the transfer runs.
    pub target: AtomicUsize,
    /// The download's share of each of its servers' connection limits (absent = no limit).
    pub caps: Mutex<HashMap<String, usize>>,
    /// The servers it uses (`host_key` of each source's final URL), once known. The first is
    /// the download's own server.
    pub hosts: Mutex<Vec<String>>,
    /// The sources of the transfer in progress, for the UI.
    pub sources: Mutex<Vec<Arc<Source>>>,
    /// What a finished transfer is doing before the file is ready (`verifying`, `unpacking`),
    /// with bytes done and total.
    pub phase: Mutex<Option<&'static str>>,
    pub phase_done: AtomicU64,
    pub phase_total: AtomicU64,
    /// Wakes the transfer loop when `target` or a server's share changes.
    pub changed: Notify,
    /// The part file, once created. The task deletes it when the download is removed.
    pub part: Mutex<Option<PathBuf>>,
    pub table: Mutex<Table>,
    /// Phase and piece counts of a media download (None for a file).
    pub media: Mutex<Option<crate::media::MediaProgress>>,
    pub downloaded: AtomicU64,
    pub ranged: AtomicBool,
    pub active: AtomicU32,
    pub speed: AtomicU64,
    pub limiter: RateLimiter,
    pub sample: Mutex<(Instant, u64, f64)>,
}

impl Running {
    pub fn new(speed_limit: u64, connections: u8) -> Self {
        Running {
            cancel: CancellationToken::new(),
            finished: CancellationToken::new(),
            finish: CancellationToken::new(),
            stop_reason: AtomicU8::new(0),
            target: AtomicUsize::new(connections.max(1) as usize),
            caps: Mutex::new(HashMap::new()),
            hosts: Mutex::new(Vec::new()),
            sources: Mutex::new(Vec::new()),
            phase: Mutex::new(None),
            phase_done: AtomicU64::new(0),
            phase_total: AtomicU64::new(0),
            changed: Notify::new(),
            part: Mutex::new(None),
            table: Mutex::new(Table::default()),
            media: Mutex::new(None),
            downloaded: AtomicU64::new(0),
            ranged: AtomicBool::new(false),
            active: AtomicU32::new(0),
            speed: AtomicU64::new(0),
            limiter: RateLimiter::new(speed_limit),
            sample: Mutex::new((Instant::now(), 0, 0.0)),
        }
    }

    pub fn stop(&self, reason: u8) {
        self.stop_reason.store(reason, Relaxed);
        self.cancel.cancel();
    }

    pub fn set_target(&self, connections: u8) {
        self.target.store(connections.max(1) as usize, Relaxed);
        self.changed.notify_one();
    }

    pub fn set_host_cap(&self, host: &str, cap: usize) {
        let cap = cap.max(1);
        let old = self.caps.lock().unwrap().insert(host.to_string(), cap);
        if old != Some(cap) {
            self.changed.notify_one();
        }
    }

    /// The download's share of `host`'s connection limit (`usize::MAX` = no limit).
    pub fn host_cap(&self, host: &str) -> usize {
        self.caps
            .lock()
            .unwrap()
            .get(host)
            .copied()
            .unwrap_or(usize::MAX)
    }

    /// The share of the download's own server (the first one it joined).
    pub fn primary_cap(&self) -> usize {
        match self.hosts.lock().unwrap().first() {
            Some(h) => self.host_cap(h),
            None => usize::MAX,
        }
    }

    pub fn set_phase(&self, phase: Option<&'static str>, total: u64) {
        self.phase_done.store(0, Relaxed);
        self.phase_total.store(total, Relaxed);
        *self.phase.lock().unwrap() = phase;
    }

    /// Recomputes the smoothed speed; called by the ticker.
    pub fn sample_speed(&self) -> u64 {
        let now = Instant::now();
        let bytes = self.downloaded.load(Relaxed);
        let mut s = self.sample.lock().unwrap();
        let dt = now.duration_since(s.0).as_secs_f64();
        if dt < 0.05 {
            return self.speed.load(Relaxed);
        }
        let inst = bytes.saturating_sub(s.1) as f64 / dt;
        s.2 = if s.2 == 0.0 {
            inst
        } else {
            s.2 * 0.7 + inst * 0.3
        };
        *s = (now, bytes, s.2);
        let v = s.2.round() as u64;
        self.speed.store(v, Relaxed);
        v
    }
}

/// One address the file can be fetched from: the download's own URL or a mirror.
pub(crate) struct Source {
    pub url: Url,
    /// `host_key` of the URL, for the per-server connection limit.
    pub host: String,
    /// 0 = the download's own URL; `i` = mirror `i - 1`.
    pub index: usize,
    /// Connections this source tolerates, learned from 429/503 answers.
    pub learned_cap: AtomicUsize,
    /// Failures in a row.
    pub fails: AtomicU32,
    pub disabled: Mutex<Option<String>>,
    /// Bytes it delivered in this run.
    pub bytes: AtomicU64,
    /// Connections currently fetching from it.
    pub active: AtomicU32,
}

impl Source {
    pub fn new(url: Url, index: usize) -> Source {
        Source {
            host: host_key(&url),
            url,
            index,
            learned_cap: AtomicUsize::new(usize::MAX),
            fails: AtomicU32::new(0),
            disabled: Mutex::new(None),
            bytes: AtomicU64::new(0),
            active: AtomicU32::new(0),
        }
    }

    pub fn is_disabled(&self) -> bool {
        self.disabled.lock().unwrap().is_some()
    }

    fn disable(&self, why: String) {
        let mut d = self.disabled.lock().unwrap();
        if d.is_none() {
            tracing::info!(url = %self.url, %why, "mirror disabled");
            *d = Some(why);
        }
    }
}

/// Failures in a row after which a source is given up (when there are others).
const SOURCE_MAX_FAILS: u32 = 3;

pub(crate) struct Transfer {
    pub net: Arc<Net>,
    /// Where the file can be fetched from; the first is the download's own URL. Only the
    /// first is used unless ranges work.
    pub sources: Vec<Arc<Source>>,
    /// Where the file starts on the server (an entry inside an archive); 0 for a whole file.
    pub base: u64,
    pub meta: RequestMeta,
    pub user_agent: String,
    pub file: std::fs::File,
    pub run: Arc<Running>,
    pub global: Arc<RateLimiter>,
    /// Range requests are possible (server supports them and the size is known).
    pub ranged: bool,
}

#[derive(Clone)]
pub(crate) struct Limits {
    pub min_split: u64,
    pub max_retries: u32,
}

pub(crate) enum Outcome {
    Done,
    Stopped,
}

enum WorkerError {
    /// Worth retrying (dropped connection, 5xx, timeout).
    Transient(String),
    /// The server is limiting connections (429/503), possibly saying when to retry.
    Throttled(u16, Option<Duration>),
    Fatal(Error),
    /// Stopped because fewer connections are wanted; the segment is free for others.
    Released,
}

struct ActiveGuard<'a>(&'a AtomicU32, &'a AtomicU32);
impl Drop for ActiveGuard<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Relaxed);
        self.1.fetch_sub(1, Relaxed);
    }
}

async fn worker(
    t: Arc<Transfer>,
    idx: usize,
    src: usize,
    delay: Duration,
    release: CancellationToken,
) -> (usize, usize, Result<(), WorkerError>) {
    // Cancelling between awaits is safe: data is written and accounted under the table
    // lock without awaiting, so a released worker never leaves a half-recorded chunk.
    let run = async {
        if !delay.is_zero() {
            tokio::time::sleep(delay).await;
        }
        let source = &t.sources[src];
        t.run.active.fetch_add(1, Relaxed);
        source.active.fetch_add(1, Relaxed);
        let _guard = ActiveGuard(&t.run.active, &source.active);
        fetch(&t, idx, source).await
    };
    let res = tokio::select! {
        r = run => r,
        _ = release.cancelled() => Err(WorkerError::Released),
    };
    (idx, src, res)
}

async fn fetch(t: &Transfer, idx: usize, source: &Source) -> Result<(), WorkerError> {
    let (pos, end) = {
        let table = t.run.table.lock().unwrap();
        (table.segs[idx].pos, table.segs[idx].end)
    };
    if pos >= end {
        return Ok(());
    }

    let range = t.ranged.then(|| (t.base + pos, t.base + end));
    let opened = match transport::kind(&source.url) {
        transport::Kind::Http => {
            // Cookies, referrer and the browser's headers belong to the download's own server.
            let other_meta;
            let meta = if source.index == 0 {
                &t.meta
            } else {
                other_meta = RequestMeta {
                    user_agent: t.meta.user_agent.clone(),
                    ..Default::default()
                };
                &other_meta
            };
            transport::http::open_range(&t.net, &source.url, meta, &t.user_agent, range).await
        }
        _ => {
            let (start, stop) = match range {
                Some((a, b)) => (a, Some(b)),
                None => (t.base, None),
            };
            transport::open_range(&t.net, &source.url, start, stop).await
        }
    };
    let mut stream = opened.map_err(|e| match e {
        OpenError::Transient(m) => WorkerError::Transient(m),
        OpenError::Throttled(code, after) => WorkerError::Throttled(code, after),
        OpenError::Fatal(e) => WorkerError::Fatal(e),
    })?;
    // Where this connection's bytes not yet handed to the disk start.
    let mut unflushed = pos;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(WorkerError::Transient)?;
        if chunk.is_empty() {
            continue;
        }
        t.global.acquire(chunk.len() as u64).await;
        t.run.limiter.acquire(chunk.len() as u64).await;
        if t.global.over_budget() {
            // The data quota ran out: stop now rather than at the engine's next check.
            t.run.stop(STOP_QUOTA);
        }
        if t.run.cancel.is_cancelled() {
            // A fast server never leaves the stream waiting, so the cancellation would only
            // be seen at the end of the segment: stop here, before this chunk is written.
            return Err(WorkerError::Released);
        }
        let (finished, written_to) = {
            let mut table = t.run.table.lock().unwrap();
            let seg = &mut table.segs[idx];
            // The segment may have been shortened by a split while we were reading.
            let n = (chunk.len() as u64).min(seg.end.saturating_sub(seg.pos)) as usize;
            if n > 0 {
                t.file
                    .write_all_at(&chunk[..n], seg.pos)
                    .map_err(|e| WorkerError::Fatal(e.into()))?;
                seg.pos += n as u64;
                t.run.downloaded.fetch_add(n as u64, Relaxed);
                source.bytes.fetch_add(n as u64, Relaxed);
                source.fails.store(0, Relaxed);
            }
            (seg.done(), seg.pos)
        };
        if finished || written_to - unflushed >= WRITEBACK_STEP {
            start_writeback(&t.file, unflushed, written_to - unflushed);
            unflushed = written_to;
        }
        if finished {
            return Ok(());
        }
    }

    let mut table = t.run.table.lock().unwrap();
    let seg = &mut table.segs[idx];
    if seg.end == u64::MAX {
        // Unknown size: end of stream is the end of the file.
        seg.end = seg.pos;
    }
    if seg.done() {
        Ok(())
    } else {
        Err(WorkerError::Transient("connection closed early".into()))
    }
}

/// How much a connection writes before the kernel is asked to start putting it on disk.
/// Left alone, the kernel may keep most of a fast download in memory until the final flush,
/// which then holds up the finish for as long as the disk needs to write it all.
const WRITEBACK_STEP: u64 = 32 << 20;

/// Starts writing `len` bytes of `file` from `from` to disk, without waiting for them.
fn start_writeback(file: &std::fs::File, from: u64, len: u64) {
    #[cfg(target_os = "linux")]
    if len > 0 {
        use std::os::fd::AsRawFd;
        let (Ok(from), Ok(len)) = (i64::try_from(from), i64::try_from(len)) else {
            return;
        };
        // SAFETY: the descriptor stays open for the call (`file` is borrowed), and the call
        // only reads its plain integer arguments. A failure only means no early writeback.
        unsafe {
            libc::sync_file_range(file.as_raw_fd(), from, len, libc::SYNC_FILE_RANGE_WRITE);
        }
    }
    #[cfg(not(target_os = "linux"))]
    let _ = (file, from, len);
}

pub(crate) fn backoff(attempt: u32) -> Duration {
    let base = 500u64.saturating_mul(1 << attempt.saturating_sub(1).min(6));
    Duration::from_millis(base.min(30_000) + fastrand::u64(0..250))
}

/// The longest wait a server's `Retry-After` can impose on one attempt.
const MAX_RETRY_AFTER: Duration = Duration::from_secs(300);

/// Exponential backoff, but never sooner than the server asked for.
pub(crate) fn retry_delay(attempt: u32, retry_after: Option<Duration>) -> Duration {
    let b = backoff(attempt);
    match retry_after {
        Some(ra) => b.max(ra.min(MAX_RETRY_AFTER)),
        None => b,
    }
}

type Workers = JoinSet<(usize, usize, Result<(), WorkerError>)>;

/// The coordinator's view of its workers.
struct Crew {
    set: Workers,
    /// Live workers in start order: segment, source, and the token that releases it.
    live: Vec<(usize, usize, CancellationToken)>,
}

impl Crew {
    fn spawn(&mut self, t: &Arc<Transfer>, idx: usize, src: usize, delay: Duration) {
        let token = t.run.cancel.child_token();
        self.live.push((idx, src, token.clone()));
        self.set.spawn(worker(t.clone(), idx, src, delay, token));
    }

    fn forget(&mut self, idx: usize) {
        self.live.retain(|(i, _, _)| *i != idx);
    }

    fn on_source(&self, src: usize) -> usize {
        self.live.iter().filter(|l| l.1 == src).count()
    }

    /// How many connections a source may have: its server's share, and what it tolerates.
    fn cap(t: &Transfer, src: usize) -> usize {
        let s = &t.sources[src];
        t.run
            .host_cap(&s.host)
            .min(s.learned_cap.load(Relaxed))
            .max(1)
    }

    /// The usable source with the fewest connections for its share (the download's own URL
    /// wins ties), or None when every usable source is at its limit.
    fn pick(&self, t: &Transfer) -> Option<usize> {
        let usable = if t.ranged { t.sources.len() } else { 1 };
        (0..usable)
            .filter(|&i| !t.sources[i].is_disabled())
            .filter(|&i| self.on_source(i) < Self::cap(t, i))
            .min_by_key(|&i| (self.on_source(i), i))
    }

    /// Sources still in use.
    fn usable(t: &Transfer) -> usize {
        let n = if t.ranged { t.sources.len() } else { 1 };
        t.sources[..n].iter().filter(|s| !s.is_disabled()).count()
    }

    /// Releases the most recently started workers until no source is over its limit and at
    /// most `target` remain.
    fn shrink(&mut self, t: &Transfer, target: usize) {
        for src in 0..t.sources.len() {
            while self.on_source(src) > Self::cap(t, src) {
                let Some(pos) = self.live.iter().rposition(|l| l.1 == src) else {
                    break;
                };
                self.live.remove(pos).2.cancel();
            }
        }
        while self.live.len() > target {
            if let Some((_, _, token)) = self.live.pop() {
                token.cancel();
            }
        }
    }

    /// Keeps up to `target` workers busy, first on unclaimed segments, then by splitting.
    fn fill(&mut self, t: &Arc<Transfer>, target: usize, min_split: u64) {
        let target = if t.ranged { target.max(1) } else { 1 };
        while self.live.len() < target {
            let Some(src) = self.pick(t) else { break };
            let idx = {
                let mut table = t.run.table.lock().unwrap();
                match table.take_free() {
                    Some(i) => i,
                    None if t.ranged => match table.split_largest(min_split) {
                        Some(i) => i,
                        None => break,
                    },
                    None => break,
                }
            };
            self.spawn(t, idx, src, Duration::ZERO);
        }
    }
}

/// Runs the transfer until every segment is complete, it is cancelled, or it fails.
/// `persist` is called about once a second to checkpoint progress.
pub(crate) async fn transfer(
    t: Arc<Transfer>,
    limits: Limits,
    persist: &(dyn Fn() + Sync),
) -> Result<Outcome> {
    let mut crew = Crew {
        set: JoinSet::new(),
        live: Vec::new(),
    };
    let mut wanted = t.run.target.load(Relaxed);
    let mut retries: HashMap<usize, (u32, u64)> = HashMap::new();
    let mut tick = tokio::time::interval(Duration::from_secs(1));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    crew.fill(&t, wanted, limits.min_split);
    loop {
        if crew.set.is_empty() {
            if t.run.table.lock().unwrap().all_done() {
                return Ok(Outcome::Done);
            }
            crew.fill(&t, wanted, limits.min_split);
            if crew.set.is_empty() {
                return Err(Error::Protocol("No work left to schedule".into()));
            }
        }

        tokio::select! {
            _ = t.run.cancel.cancelled() => {
                crew.set.shutdown().await;
                return Ok(Outcome::Stopped);
            }
            _ = tick.tick() => persist(),
            _ = t.run.changed.notified() => {
                let now = t.run.target.load(Relaxed);
                if now != wanted {
                    // The user asked for a different count: try it, even past earlier caps.
                    wanted = now;
                    for s in &t.sources {
                        s.learned_cap.store(usize::MAX, Relaxed);
                    }
                }
                crew.shrink(&t, wanted);
                crew.fill(&t, wanted, limits.min_split);
            }
            Some(joined) = crew.set.join_next() => {
                let (idx, src, res) = match joined {
                    Ok(v) => v,
                    Err(e) if e.is_cancelled() => continue,
                    Err(e) => {
                        crew.set.shutdown().await;
                        return Err(Error::Protocol(format!("Connection worker crashed: {e}")));
                    }
                };
                t.run.table.lock().unwrap().busy[idx] = false;
                crew.forget(idx);
                let source = &t.sources[src];
                let others = Crew::usable(&t) > 1;

                let failure = match res {
                    Ok(()) => {
                        retries.remove(&idx);
                        None
                    }
                    Err(WorkerError::Released) => None,
                    Err(WorkerError::Fatal(e)) if others && !source.is_disabled() => {
                        // A mirror (or the download's own URL) that can't serve the file:
                        // carry on with the others.
                        source.disable(e.to_string());
                        retries.remove(&idx);
                        None
                    }
                    Err(WorkerError::Fatal(e)) if others => {
                        tracing::debug!(%e, "already disabled source failed");
                        None
                    }
                    Err(WorkerError::Fatal(e)) => {
                        crew.set.shutdown().await;
                        return Err(e);
                    }
                    Err(WorkerError::Throttled(code, _)) if crew.on_source(src) > 0 => {
                        // The server caps connections: keep the ones we have, free this segment.
                        let n = crew.on_source(src);
                        source.learned_cap.store(n, Relaxed);
                        tracing::debug!(code, cap = n, "server limits connections");
                        None
                    }
                    Err(WorkerError::Throttled(code, after)) => Some((format!("HTTP {code}"), after)),
                    Err(WorkerError::Transient(m)) => Some((m, None)),
                };

                if let Some((msg, after)) = failure {
                    let fails = source.fails.fetch_add(1, Relaxed) + 1;
                    if others && fails >= SOURCE_MAX_FAILS {
                        // Give up this source; the segment moves to another one at once.
                        source.disable(msg);
                        retries.remove(&idx);
                        crew.fill(&t, wanted, limits.min_split);
                        continue;
                    }
                    let attempt = {
                        let mut table = t.run.table.lock().unwrap();
                        let pos = table.segs[idx].pos;
                        let entry = retries.entry(idx).or_insert((0, pos));
                        if pos > entry.1 {
                            *entry = (0, pos); // made progress since the last failure
                        }
                        entry.0 += 1;
                        let attempt = entry.0;
                        if attempt <= limits.max_retries {
                            if !t.ranged {
                                // Can't resume mid-stream without ranges: start over.
                                let seg = &mut table.segs[idx];
                                t.run.downloaded.fetch_sub(seg.downloaded(), Relaxed);
                                seg.pos = seg.start;
                            }
                            table.busy[idx] = true;
                        }
                        attempt
                    };
                    if attempt > limits.max_retries {
                        crew.set.shutdown().await;
                        return Err(Error::Network(msg));
                    }
                    tracing::debug!(idx, attempt, %msg, "retrying segment");
                    let retry_src = if source.is_disabled() {
                        crew.pick(&t)
                    } else {
                        Some(src)
                    };
                    match retry_src {
                        Some(s) => crew.spawn(&t, idx, s, retry_delay(attempt, after)),
                        None => t.run.table.lock().unwrap().busy[idx] = false,
                    }
                }
                crew.fill(&t, wanted, limits.min_split);
            }
        }
    }
}
