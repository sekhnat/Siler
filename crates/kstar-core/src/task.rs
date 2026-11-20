//! The transfer loop for one download: connection workers and their coordinator.

use futures_util::StreamExt;
use reqwest::header::{CONTENT_RANGE, RANGE};
use reqwest::{StatusCode, Url};
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

use crate::error::describe;
use crate::net::{Net, host_key, parse_challenge};
use crate::probe::{build_request, busy, parse_content_range};
use crate::ratelimit::RateLimiter;
use crate::segment::Table;
use crate::{Error, RequestMeta, Result};

pub(crate) const STOP_PAUSE: u8 = 1;
pub(crate) const STOP_REMOVE: u8 = 2;
pub(crate) const STOP_SHUTDOWN: u8 = 3;

/// Live state of a running download, shared between its task, its workers and the
/// engine's progress ticker.
pub(crate) struct Running {
    pub cancel: CancellationToken,
    /// Cancelled once the download's task has fully stopped and its state is saved.
    pub finished: CancellationToken,
    pub stop_reason: AtomicU8,
    /// Connections the user wants; changes take effect while the transfer runs.
    pub target: AtomicUsize,
    /// The download's share of its server's connection limit (`usize::MAX` = no limit).
    pub host_cap: AtomicUsize,
    /// The server (`host_key` of the final URL), once known.
    pub host: Mutex<Option<String>>,
    /// Wakes the transfer loop when `target` or `host_cap` changes.
    pub changed: Notify,
    /// The part file, once created. The task deletes it when the download is removed.
    pub part: Mutex<Option<PathBuf>>,
    pub table: Mutex<Table>,
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
            stop_reason: AtomicU8::new(0),
            target: AtomicUsize::new(connections.max(1) as usize),
            host_cap: AtomicUsize::new(usize::MAX),
            host: Mutex::new(None),
            changed: Notify::new(),
            part: Mutex::new(None),
            table: Mutex::new(Table::default()),
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

    pub fn set_host_cap(&self, cap: usize) {
        if self.host_cap.swap(cap.max(1), Relaxed) != cap.max(1) {
            self.changed.notify_one();
        }
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

pub(crate) struct Transfer {
    pub net: Arc<Net>,
    pub url: Url,
    pub meta: RequestMeta,
    pub user_agent: String,
    pub file: std::fs::File,
    pub run: Arc<Running>,
    pub global: Arc<RateLimiter>,
    /// Range requests are possible (server supports them and the size is known).
    pub ranged: bool,
}

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

struct ActiveGuard<'a>(&'a AtomicU32);
impl Drop for ActiveGuard<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Relaxed);
    }
}

async fn worker(
    t: Arc<Transfer>,
    idx: usize,
    delay: Duration,
    release: CancellationToken,
) -> (usize, Result<(), WorkerError>) {
    // Cancelling between awaits is safe: data is written and accounted under the table
    // lock without awaiting, so a released worker never leaves a half-recorded chunk.
    let run = async {
        if !delay.is_zero() {
            tokio::time::sleep(delay).await;
        }
        t.run.active.fetch_add(1, Relaxed);
        let _guard = ActiveGuard(&t.run.active);
        fetch(&t, idx).await
    };
    let res = tokio::select! {
        r = run => r,
        _ = release.cancelled() => Err(WorkerError::Released),
    };
    (idx, res)
}

async fn fetch(t: &Transfer, idx: usize) -> Result<(), WorkerError> {
    let (pos, end) = {
        let table = t.run.table.lock().unwrap();
        (table.segs[idx].pos, table.segs[idx].end)
    };
    if pos >= end {
        return Ok(());
    }

    let auth = t.net.auth_for(&t.url);
    let sent = auth.as_ref().is_some_and(|a| a.ready());
    let mut rb = build_request(
        &t.net.client(),
        t.url.clone(),
        &t.meta,
        &t.user_agent,
        auth.as_deref(),
    );
    if t.ranged {
        rb = rb.header(RANGE, format!("bytes={}-{}", pos, end - 1));
    }
    let resp = rb
        .send()
        .await
        .map_err(|e| WorkerError::Transient(describe(&e)))?;
    let status = resp.status();
    if status == StatusCode::UNAUTHORIZED {
        let challenge = parse_challenge(resp.headers());
        let realm = challenge.as_ref().and_then(|c| c.realm.clone());
        let host = host_key(resp.url());
        if let Some(c) = challenge
            && t.net.entry(&host).learn(c, sent)
        {
            // A stale Digest nonce: the same login works with the new one.
            return Err(WorkerError::Transient("login renewed".into()));
        }
        return Err(WorkerError::Fatal(Error::AuthRequired {
            host,
            realm,
            rejected: sent,
        }));
    }
    if status == StatusCode::PROXY_AUTHENTICATION_REQUIRED {
        return Err(WorkerError::Fatal(Error::ProxyAuth));
    }
    if status == StatusCode::TOO_MANY_REQUESTS || status == StatusCode::SERVICE_UNAVAILABLE {
        return Err(WorkerError::Throttled(
            status.as_u16(),
            busy(&resp).retry_after(),
        ));
    }
    if !status.is_success() {
        return Err(
            if status.is_server_error() || status == StatusCode::REQUEST_TIMEOUT {
                WorkerError::Transient(format!("HTTP {status}"))
            } else {
                WorkerError::Fatal(Error::Http(status.as_u16()))
            },
        );
    }
    if t.ranged {
        if status == StatusCode::PARTIAL_CONTENT {
            let start = resp
                .headers()
                .get(CONTENT_RANGE)
                .and_then(|v| v.to_str().ok())
                .and_then(parse_content_range)
                .map(|r| r.0);
            if start != Some(pos) {
                return Err(WorkerError::Fatal(Error::Protocol(
                    "The server returned an unexpected byte range".into(),
                )));
            }
        } else if pos != 0 {
            return Err(WorkerError::Fatal(Error::Protocol(
                "The server stopped accepting range requests".into(),
            )));
        }
    }

    let mut stream = resp.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| WorkerError::Transient(describe(&e)))?;
        if chunk.is_empty() {
            continue;
        }
        t.global.acquire(chunk.len() as u64).await;
        t.run.limiter.acquire(chunk.len() as u64).await;
        let finished = {
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
            }
            seg.done()
        };
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

type Workers = JoinSet<(usize, Result<(), WorkerError>)>;

/// The coordinator's view of its workers.
struct Crew {
    set: Workers,
    /// Live workers in start order, with the token that releases each one.
    live: Vec<(usize, CancellationToken)>,
}

impl Crew {
    fn spawn(&mut self, t: &Arc<Transfer>, idx: usize, delay: Duration) {
        let token = t.run.cancel.child_token();
        self.live.push((idx, token.clone()));
        self.set.spawn(worker(t.clone(), idx, delay, token));
    }

    fn forget(&mut self, idx: usize) {
        self.live.retain(|(i, _)| *i != idx);
    }

    /// Releases the most recently started workers until at most `target` remain.
    fn shrink(&mut self, target: usize) {
        while self.live.len() > target {
            if let Some((_, token)) = self.live.pop() {
                token.cancel();
            }
        }
    }

    /// Keeps up to `target` workers busy, first on unclaimed segments, then by splitting.
    fn fill(&mut self, t: &Arc<Transfer>, target: usize, min_split: u64) {
        let mut table = t.run.table.lock().unwrap();
        while self.live.len() < target {
            let idx = match table.take_free() {
                Some(i) => i,
                None if t.ranged => match table.split_largest(min_split) {
                    Some(i) => i,
                    None => break,
                },
                None => break,
            };
            drop(table);
            self.spawn(t, idx, Duration::ZERO);
            table = t.run.table.lock().unwrap();
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
    // The connection count the server tolerates, learned from 429/503 answers.
    let mut cap: Option<usize> = None;
    let mut wanted = t.run.target.load(Relaxed);
    let target = |cap: Option<usize>, wanted: usize| {
        if t.ranged {
            wanted
                .min(cap.unwrap_or(usize::MAX))
                .min(t.run.host_cap.load(Relaxed))
                .max(1)
        } else {
            1
        }
    };
    let mut retries: HashMap<usize, (u32, u64)> = HashMap::new();
    let mut tick = tokio::time::interval(Duration::from_secs(1));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    crew.fill(&t, target(cap, wanted), limits.min_split);
    loop {
        if crew.set.is_empty() {
            if t.run.table.lock().unwrap().all_done() {
                return Ok(Outcome::Done);
            }
            crew.fill(&t, target(cap, wanted), limits.min_split);
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
                    // The user asked for a different count: try it, even past an earlier cap.
                    wanted = now;
                    cap = None;
                }
                crew.shrink(target(cap, wanted));
                crew.fill(&t, target(cap, wanted), limits.min_split);
            }
            Some(joined) = crew.set.join_next() => {
                let (idx, res) = match joined {
                    Ok(v) => v,
                    Err(e) if e.is_cancelled() => continue,
                    Err(e) => {
                        crew.set.shutdown().await;
                        return Err(Error::Protocol(format!("Connection worker crashed: {e}")));
                    }
                };
                t.run.table.lock().unwrap().busy[idx] = false;
                crew.forget(idx);

                let failure = match res {
                    Ok(()) => {
                        retries.remove(&idx);
                        None
                    }
                    Err(WorkerError::Released) => None,
                    Err(WorkerError::Fatal(e)) => {
                        crew.set.shutdown().await;
                        return Err(e);
                    }
                    Err(WorkerError::Throttled(code, _)) if !crew.live.is_empty() => {
                        // The server caps connections: keep the ones we have, free this segment.
                        cap = Some(crew.live.len());
                        tracing::debug!(code, cap = crew.live.len(), "server limits connections");
                        None
                    }
                    Err(WorkerError::Throttled(code, after)) => Some((format!("HTTP {code}"), after)),
                    Err(WorkerError::Transient(m)) => Some((m, None)),
                };

                if let Some((msg, after)) = failure {
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
                    crew.spawn(&t, idx, retry_delay(attempt, after));
                }
                crew.fill(&t, target(cap, wanted), limits.min_split);
            }
        }
    }
}
