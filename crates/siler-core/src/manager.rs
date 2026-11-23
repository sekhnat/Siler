use reqwest::Url;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering::Relaxed;
use std::sync::{Arc, Mutex, MutexGuard, RwLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::runtime::Handle;
use tokio::sync::broadcast;

use crate::Traffic;
use crate::checkpoint::{Checkpoints, Snapshot};
use crate::checksum::{self, Algo, Checksum, ChecksumSource};
use crate::error::MediaError;
use crate::extras::{CommandResult, Extras, Mirror, MirrorOrigin, VerifyResult};
use crate::http::{Http, Limiters};
use crate::media::{
    self, FfmpegStatus, JobKind, MediaInfo, MediaProbe, MediaRecord, NewMediaDownload, SourceHint,
};
use crate::net::{Login, Net, host_key};
use crate::partfile;
use crate::probe::{self, ProbeResult};
use crate::proxy::ProxyRoute;
use crate::queue::{MAIN_QUEUE, Queue};
use crate::quota::{Quota, QuotaStatus};
use crate::ratelimit::RateLimiter;
use crate::scheduler::{self, Edge};
use crate::segment::{self, Segment, Table};
use crate::store::{Record, Store};
use crate::task::{
    self, Limits, Outcome, Running, STOP_PAUSE, STOP_QUOTA, STOP_SHUTDOWN, Source, Transfer,
};
use crate::{
    Category, DownloadInfo, EditDownload, Error, Event, MAX_BODY, NewDownload, OnConflict,
    ProgressItem, QueueMove, RequestMeta, Result, Settings, Status, filename,
};
use crate::{metalink, postrun, zip};
use siler_media::ParseLimits;
use siler_media::select::{self, ContainerChoice, Output, Selection, SubtitleMode};
use std::sync::atomic::AtomicBool;
use tokio_util::sync::CancellationToken;

/// The download engine. Cheap to clone; all clones share one queue.
#[derive(Clone)]
pub struct Engine {
    inner: Arc<Inner>,
}

pub(crate) struct Inner {
    store: Mutex<Store>,
    pub(crate) net: Arc<Net>,
    proxy_password: Mutex<Option<String>>,
    settings: RwLock<Settings>,
    pub(crate) global: Arc<RateLimiter>,
    state: Mutex<State>,
    events: broadcast::Sender<Event>,
    pub(crate) rt: Handle,
    /// The FFmpeg used to combine media, when it passed the capability check.
    ffmpeg: Mutex<Option<std::path::PathBuf>>,
    pub(crate) ffmpeg_status: Mutex<FfmpegStatus>,
    quota: Quota,
    /// When the quota's period was last saved.
    quota_saved: Mutex<Instant>,
    /// Commands after downloads run one at a time.
    command_turn: tokio::sync::Mutex<()>,
    /// Cancelled when Siler quits (stops a running command).
    pub(crate) quit: CancellationToken,
    /// Site grabs in progress, with the project each belongs to.
    pub(crate) grabs: Mutex<HashMap<u64, (Option<i64>, CancellationToken)>>,
    /// Who finds fresh links for expired ones (the app, through the browser).
    pub(crate) finder: RwLock<Option<Arc<dyn crate::LinkFinder>>>,
}

#[derive(Default)]
pub(crate) struct State {
    pub(crate) records: BTreeMap<i64, Record>,
    pub(crate) running: HashMap<i64, Arc<Running>>,
    /// "Start now": these start even when `max_concurrent` downloads are running.
    pub(crate) forced: HashSet<i64>,
    /// Downloads whose files are being moved; they don't start until that's done.
    pub(crate) moving: HashSet<i64>,
    /// Running downloads per server (`host_key`), oldest first, for the connection limit.
    pub(crate) hosts: HashMap<String, Vec<i64>>,
    pub(crate) queues: BTreeMap<i64, Queue>,
    /// Finished and failed downloads per active queue, for `QueueDone`.
    pub(crate) queue_results: HashMap<i64, (usize, usize)>,
    /// Running downloads stopped by their queue; they go back to Waiting, not Paused.
    pub(crate) hold: HashSet<i64>,
    /// Downloads being checked for a newer version.
    pub(crate) checking: HashSet<i64>,
    /// Queued downloads waiting for a fresh link from the browser; they don't start until then.
    pub(crate) refreshing: HashMap<i64, CancellationToken>,
    pub(crate) shutting_down: bool,
    pub(crate) idle_reported: bool,
}

impl State {
    /// Stops waiting for a fresh link from the browser.
    pub(crate) fn call_off_refresh(&mut self, id: i64) {
        if let Some(token) = self.refreshing.remove(&id) {
            token.cancel();
        }
    }
}

pub(crate) fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn differs(a: &Option<String>, b: &Option<String>) -> bool {
    matches!((a, b), (Some(a), Some(b)) if a != b)
}

fn preallocate(file: &File, len: u64) -> Result<()> {
    match rustix::fs::fallocate(file, rustix::fs::FallocateFlags::empty(), 0, len) {
        Ok(()) => Ok(()),
        Err(e) if e == rustix::io::Errno::NOSPC => Err(Error::DiskFull),
        // Filesystem without fallocate support: fall back to a sparse file.
        Err(_) => Ok(file.set_len(len)?),
    }
}

pub(crate) fn human_size(bytes: u64) -> String {
    let units = ["B", "KB", "MB", "GB", "TB"];
    let mut v = bytes as f64;
    let mut u = 0;
    while v >= 1000.0 && u < units.len() - 1 {
        v /= 1000.0;
        u += 1;
    }
    if u == 0 {
        format!("{bytes} B")
    } else {
        format!("{v:.1} {}", units[u])
    }
}

/// Forget everything learned about the file so the download starts from zero.
fn reset_progress(r: &mut Record) {
    r.extras.refresh = Default::default();
    r.extras.verify = None;
    r.extras.accept_unverified = false;
    r.extras.command = None;
    r.segments.clear();
    r.downloaded = 0;
    r.total_size = None;
    r.resumable = None;
    r.etag = None;
    r.last_modified = None;
    r.final_url = None;
    r.error = None;
    r.notice = None;
    r.completed_at = None;
    r.traffic = Traffic::default();
}

/// Renames, or copies and deletes when the target is on another filesystem.
fn move_file(from: &Path, to: &Path) -> std::io::Result<()> {
    if let Some(parent) = to.parent() {
        std::fs::create_dir_all(parent)?;
    }
    if from.is_dir() {
        // A media Work Directory: it only moves within one filesystem, like a part file.
        return std::fs::rename(from, to);
    }
    match std::fs::rename(from, to) {
        Err(e) if e.raw_os_error() == Some(18) => {
            // EXDEV
            std::fs::copy(from, to)?;
            std::fs::remove_file(from)
        }
        r => r,
    }
}

/// Splits `limit` connections among downloads wanting `wants` each (0 = no limit). Everyone
/// gets at least one, so many downloads on one server can go over the limit.
fn share_connections(limit: usize, wants: &[usize]) -> Vec<usize> {
    if limit == 0 {
        return vec![usize::MAX; wants.len()];
    }
    // Smallest wants first; among equals the oldest goes last and gets any remainder.
    let mut order: Vec<usize> = (0..wants.len()).collect();
    order.sort_by_key(|&i| (wants[i], std::cmp::Reverse(i)));
    let mut caps = vec![1; wants.len()];
    let mut left = limit;
    for (k, &i) in order.iter().enumerate() {
        let share = (left / (wants.len() - k)).max(1);
        caps[i] = wants[i].clamp(1, share);
        left = left.saturating_sub(caps[i]);
    }
    caps
}

fn visible_segments(segs: &[Segment]) -> Vec<Segment> {
    if segs.iter().any(|s| s.end == u64::MAX) {
        Vec::new()
    } else {
        segs.to_vec()
    }
}

/// An address typed for "Test with an address": http when it names no scheme.
fn checkable(url: &str) -> Result<url::Url> {
    let url = url.trim();
    if url.contains("://") {
        probe::parse_url(url)
    } else {
        probe::parse_url(&format!("http://{url}"))
    }
}

impl Engine {
    /// Opens (or creates) the engine database in `data_dir` and starts any queued
    /// downloads. Must be called from within a Tokio runtime.
    pub fn open(data_dir: impl AsRef<Path>) -> Result<Engine> {
        Self::open_with(data_dir, None, None)
    }

    /// The saved settings, without opening the engine (e.g. to fetch the proxy password
    /// from the keyring before [`Engine::open_with`]).
    pub fn peek_settings(data_dir: impl AsRef<Path>) -> Result<Settings> {
        let path = data_dir.as_ref().join("siler.db");
        if !path.exists() {
            return Ok(Settings::default());
        }
        Ok(Store::open(&path)?
            .load_settings()?
            .unwrap_or_default()
            .normalized())
    }

    /// Like [`Engine::open`], with the password of the manual proxy.
    ///
    /// `ffmpeg` is checked before any download is resumed: a media download picked up at start
    /// must already know it (see [`Engine::set_ffmpeg_path`], which can change it later).
    pub fn open_with(
        data_dir: impl AsRef<Path>,
        proxy_password: Option<String>,
        ffmpeg: Option<PathBuf>,
    ) -> Result<Engine> {
        use std::os::unix::fs::PermissionsExt;
        let dir = data_dir.as_ref();
        std::fs::create_dir_all(dir)?;
        let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
        let rt = Handle::try_current().map_err(|_| {
            Error::Protocol("Engine::open must be called inside a Tokio runtime".into())
        })?;

        let store = Store::open(&dir.join("siler.db"))?;
        let settings = store.load_settings()?.unwrap_or_default().normalized();
        let mut records = BTreeMap::new();
        for mut r in store.load_all()? {
            if r.status.is_running() {
                // Interrupted by a quit or crash.
                r.status = if settings.resume_on_startup {
                    Status::Queued
                } else {
                    Status::Paused
                };
                store.update(&r)?;
            }
            records.insert(r.id, r);
        }

        let mut queues: BTreeMap<i64, Queue> = store
            .load_queues()?
            .into_iter()
            .map(|q| (q.id, q))
            .collect();
        queues.entry(MAIN_QUEUE).or_insert_with(Queue::main);
        for r in records.values_mut() {
            if !queues.contains_key(&r.queue_id) {
                r.queue_id = MAIN_QUEUE;
            }
        }

        let net = Arc::new(Net::new(&settings.proxy, proxy_password.as_deref())?);
        *net.known_hosts.write().unwrap() = Some(dir.join("known_hosts"));
        let quota = Quota::new(store.load_value("quota")?.as_deref());

        let inner = Arc::new(Inner {
            store: Mutex::new(store),
            net,
            proxy_password: Mutex::new(proxy_password),
            global: Arc::new(RateLimiter::new(settings.global_speed_limit)),
            settings: RwLock::new(settings),
            state: Mutex::new(State {
                records,
                queues,
                ..Default::default()
            }),
            events: broadcast::channel(1024).0,
            rt,
            ffmpeg: Mutex::new(None),
            ffmpeg_status: Mutex::new(FfmpegStatus::default()),
            quota,
            quota_saved: Mutex::new(Instant::now()),
            command_turn: tokio::sync::Mutex::new(()),
            quit: CancellationToken::new(),
            grabs: Mutex::new(HashMap::new()),
            finder: RwLock::new(None),
        });

        let weak = Arc::downgrade(&inner);
        inner.rt.spawn(async move {
            let mut iv = tokio::time::interval(Duration::from_millis(250));
            // After a stall (a suspended laptop), one tick, not a burst of catch-up ticks.
            iv.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                iv.tick().await;
                match weak.upgrade() {
                    Some(inner) => inner.tick(),
                    None => break,
                }
            }
        });

        // Scheduled queues and keeping files up to date.
        let weak = Arc::downgrade(&inner);
        inner.rt.spawn(async move {
            let mut last = jiff::Timestamp::now();
            if let Some(inner) = weak.upgrade() {
                inner.catch_up_schedules(last);
            }
            let mut iv = tokio::time::interval(Duration::from_secs(15));
            iv.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            iv.tick().await;
            loop {
                iv.tick().await;
                let Some(inner) = weak.upgrade() else { break };
                let now = jiff::Timestamp::now();
                inner.run_schedules(last, now);
                inner.run_grab_schedules(last, now);
                inner.check_due_updates();
                last = now;
            }
        });

        let engine = Engine { inner };
        engine.set_ffmpeg_path(ffmpeg);
        engine.inner.schedule();
        Ok(engine)
    }

    pub(crate) fn inner(&self) -> &Arc<Inner> {
        &self.inner
    }

    pub(crate) fn from_inner(inner: Arc<Inner>) -> Engine {
        Engine { inner }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Event> {
        self.inner.events.subscribe()
    }

    pub fn settings(&self) -> Settings {
        self.inner.settings()
    }

    pub fn set_settings(&self, new: Settings) -> Result<Settings> {
        let new = new.normalized();
        self.inner.store.lock().unwrap().save_settings(&new)?;
        let old = self.inner.settings();
        if old.global_speed_limit != new.global_speed_limit {
            self.inner.global.set_rate(new.global_speed_limit);
        }
        *self.inner.settings.write().unwrap() = new.clone();
        if old.proxy != new.proxy {
            let pw = self.inner.proxy_password.lock().unwrap().clone();
            self.inner.net.configure(&new.proxy, pw.as_deref());
        }
        if old.max_connections_per_server != new.max_connections_per_server
            || old.server_limits != new.server_limits
        {
            let st = self.inner.state();
            let hosts: Vec<String> = st.hosts.keys().cloned().collect();
            for h in hosts {
                self.inner.rebalance(&st, &h);
            }
        }
        if old.connections != new.connections {
            // Unfinished downloads still on the old default follow the new one.
            let ids: Vec<i64> = self
                .inner
                .state()
                .records
                .values()
                .filter(|r| r.status != Status::Completed && r.connections == old.connections)
                .map(|r| r.id)
                .collect();
            for id in ids {
                let _ = self.set_connections(id, new.connections);
            }
        }
        self.inner.schedule();
        Ok(new)
    }

    pub fn list(&self) -> Vec<DownloadInfo> {
        let st = self.inner.state();
        st.records
            .values()
            .map(|r| {
                self.inner
                    .info(r, st.running.get(&r.id).map(|a| a.as_ref()))
            })
            .collect()
    }

    pub fn get(&self, id: i64) -> Option<DownloadInfo> {
        let st = self.inner.state();
        st.records
            .get(&id)
            .map(|r| self.inner.info(r, st.running.get(&id).map(|a| a.as_ref())))
    }

    /// Asks the server about `url` and suggests a filename and folder, without adding anything.
    pub async fn probe(&self, url: &str, meta: &RequestMeta) -> Result<ProbeResult> {
        let s = self.settings();
        let url = self.inner.strip_login(url)?;
        let mut p = probe::probe(&self.inner.net, &url, meta, &s.user_agent).await?;
        p.category = s.category_for(&p.filename);
        p.dir = s.category_dir(p.category);
        (p.exists, p.suggested) = self.name_status(&p.dir, &p.filename);
        Ok(p)
    }

    /// Whether `dir/name` is taken (on disk or by another download), and a free alternative.
    pub fn name_status(&self, dir: &Path, name: &str) -> (bool, String) {
        let st = self.inner.state();
        let taken = |n: &str| name_claimed(&st, dir, n, None);
        let exists =
            taken(name) || dir.join(name).exists() || filename::part_path(dir, name).exists();
        (exists, filename::unique_name(dir, name, taken))
    }

    pub fn add(&self, nd: NewDownload) -> Result<i64> {
        let url = probe::parse_url(&self.inner.strip_login(&nd.url)?)?;
        let s = self.settings();
        if nd.meta.body.as_ref().is_some_and(|b| b.len() > MAX_BODY) {
            return Err(Error::Protocol(
                "The form data is too large to send again".into(),
            ));
        }
        let filename = nd.filename.as_deref().and_then(filename::sanitize);
        if nd.zip_entry.is_some() && filename.is_none() {
            return Err(Error::Protocol(
                "A file from an archive needs a name".into(),
            ));
        }
        let mut mirrors: Vec<Mirror> = Vec::new();
        for m in &nd.mirrors {
            let u = probe::parse_url(&m.url)?;
            if u.as_str() != url.as_str() && !mirrors.iter().any(|x| x.url == u.as_str()) {
                mirrors.push(Mirror::new(u.to_string(), m.origin));
            }
        }
        let extras = Extras {
            checksum: nd.checksum.clone(),
            mirrors: if nd.zip_entry.is_some() {
                Vec::new()
            } else {
                mirrors
            },
            pieces: nd.pieces.clone().filter(|_| nd.zip_entry.is_none()),
            zip_entry: nd.zip_entry.clone(),
            link: nd.link.clone(),
            ..Default::default()
        };
        let category = nd
            .category
            .or_else(|| filename.as_deref().map(|n| s.category_for(n)))
            .unwrap_or_default();
        let on_conflict = match nd.on_conflict.unwrap_or(s.on_conflict) {
            OnConflict::Ask => OnConflict::Rename,
            p => p,
        };
        let mut st = self.inner.state();
        let queue_id = nd
            .queue_id
            .filter(|q| st.queues.contains_key(q))
            .unwrap_or(MAIN_QUEUE);
        let position = st.records.values().map(|r| r.position).max().unwrap_or(0) + 1;
        let mut rec = Record {
            id: 0,
            url: url.to_string(),
            final_url: None,
            filename,
            dir: nd.dir,
            category,
            total_size: None,
            downloaded: 0,
            status: if nd.start_paused {
                Status::Waiting
            } else {
                Status::Queued
            },
            resumable: None,
            error: None,
            notice: None,
            etag: None,
            last_modified: None,
            meta: nd.meta.bound_to(url.as_str()),
            connections: nd.connections.unwrap_or(s.connections).clamp(1, 32),
            speed_limit: 0,
            created_at: now_ms(),
            completed_at: None,
            segments: Vec::new(),
            position,
            on_conflict,
            queue_id,
            sync_every: 0,
            sync_checked_at: None,
            needs_login: None,
            needs_host_key: None,
            kind: JobKind::File,
            media: None,
            extras,
            traffic: Traffic::default(),
            auto_connections: false,
        };
        rec.id = self.inner.store.lock().unwrap().insert(&rec)?;
        let id = rec.id;
        let info = self.inner.info(&rec, None);
        st.records.insert(id, rec);
        drop(st);
        self.inner.emit(Event::Upsert {
            download: Box::new(info),
        });
        self.inner.schedule();
        Ok(id)
    }

    pub fn pause(&self, id: i64) -> Result<()> {
        let mut st = self.inner.state();
        if let Some(run) = st.running.get(&id) {
            run.stop(STOP_PAUSE);
            return Ok(());
        }
        st.call_off_refresh(id);
        st.forced.remove(&id);
        let rec = st.records.get_mut(&id).ok_or(Error::NotFound)?;
        if matches!(rec.status, Status::Queued | Status::Waiting) {
            rec.status = Status::Paused;
            self.inner.persist_and_emit(rec, None)?;
        }
        Ok(())
    }

    /// Ends a recording (a live stream, or an RTMP/MMS stream) and keeps what was recorded: a
    /// running one stops fetching and is rebuilt; a paused one is rebuilt from what it has.
    pub fn finish(&self, id: i64) -> Result<()> {
        {
            let mut st = self.inner.state();
            let rec = st.records.get_mut(&id).ok_or(Error::NotFound)?;
            let recording =
                rec.kind == JobKind::Stream || rec.media.as_ref().is_some_and(|m| m.live);
            if !recording {
                return Err(Error::Protocol("Only recordings can be finished".into()));
            }
            if let Some(m) = rec.media.as_mut() {
                m.finish = true;
            }
            self.inner.persist_and_emit(rec, None)?;
            if let Some(run) = st.running.get(&id) {
                run.finish.cancel();
                return Ok(());
            }
        }
        self.resume(id)
    }

    pub fn resume(&self, id: i64) -> Result<()> {
        {
            let mut st = self.inner.state();
            if st.running.contains_key(&id) {
                return Ok(());
            }
            let rec = st.records.get_mut(&id).ok_or(Error::NotFound)?;
            if matches!(
                rec.status,
                Status::Paused | Status::Failed | Status::Waiting
            ) {
                rec.status = Status::Queued;
                rec.error = None;
                self.inner.persist_and_emit(rec, None)?;
            }
        }
        self.inner.schedule();
        Ok(())
    }

    /// Starts the download right away, even if `max_concurrent` downloads are running.
    pub fn start_now(&self, id: i64) -> Result<()> {
        {
            let mut st = self.inner.state();
            if st.running.contains_key(&id) {
                return Ok(());
            }
            let State {
                records, forced, ..
            } = &mut *st;
            let rec = records.get_mut(&id).ok_or(Error::NotFound)?;
            if rec.status == Status::Completed {
                return Ok(());
            }
            rec.status = Status::Queued;
            rec.error = None;
            forced.insert(id);
            self.inner.persist_and_emit(rec, None)?;
        }
        self.inner.schedule();
        Ok(())
    }

    /// Starts a queue: its waiting downloads ("Download later") start as slots free up.
    pub fn start_queue(&self, queue_id: i64) -> Result<()> {
        self.inner.start_queue(queue_id)
    }

    /// Stops a queue: downloads that haven't started go back to waiting. With
    /// `pause_running`, its running downloads stop too and wait for the next start.
    pub fn stop_queue(&self, queue_id: i64, pause_running: bool) -> Result<()> {
        self.inner.stop_queue(queue_id, pause_running)
    }

    pub fn queues(&self) -> Vec<Queue> {
        self.inner.state().queues.values().cloned().collect()
    }

    /// Adds a queue and returns its id.
    pub fn create_queue(&self, q: Queue) -> Result<i64> {
        let mut q = Queue { id: 0, ..q }.normalized();
        q.active = false;
        let mut st = self.inner.state();
        q.id = self.inner.store.lock().unwrap().save_queue(&q)?;
        let id = q.id;
        st.queues.insert(id, q);
        self.inner.emit_queues(&st);
        Ok(id)
    }

    /// Changes a queue's name, limit, schedule or action when done.
    pub fn update_queue(&self, q: Queue) -> Result<()> {
        {
            let mut st = self.inner.state();
            let old = st.queues.get(&q.id).ok_or(Error::NotFound)?;
            let mut q = q.normalized();
            q.active = old.active;
            self.inner.store.lock().unwrap().save_queue(&q)?;
            st.queues.insert(q.id, q);
            self.inner.emit_queues(&st);
        }
        self.inner.schedule();
        Ok(())
    }

    /// Deletes a queue; its downloads move to the main queue.
    pub fn delete_queue(&self, id: i64) -> Result<()> {
        if id == MAIN_QUEUE {
            return Err(Error::Protocol("The main queue can't be deleted".into()));
        }
        {
            let mut st = self.inner.state();
            if !st.queues.contains_key(&id) {
                return Err(Error::NotFound);
            }
            self.inner.store.lock().unwrap().delete_queue(id)?;
            st.queues.remove(&id);
            st.queue_results.remove(&id);
            let State {
                records, running, ..
            } = &mut *st;
            for rec in records.values_mut().filter(|r| r.queue_id == id) {
                rec.queue_id = MAIN_QUEUE;
                self.inner.emit(Event::Upsert {
                    download: Box::new(
                        self.inner
                            .info(rec, running.get(&rec.id).map(|a| a.as_ref())),
                    ),
                });
            }
            self.inner.emit_queues(&st);
        }
        self.inner.schedule();
        Ok(())
    }

    /// Puts downloads into another queue.
    pub fn move_to_queue(&self, ids: &[i64], queue_id: i64) -> Result<()> {
        {
            let mut st = self.inner.state();
            if !st.queues.contains_key(&queue_id) {
                return Err(Error::NotFound);
            }
            let State {
                records, running, ..
            } = &mut *st;
            for id in ids {
                if let Some(rec) = records.get_mut(id)
                    && rec.queue_id != queue_id
                {
                    rec.queue_id = queue_id;
                    self.inner
                        .persist_and_emit(rec, running.get(id).map(|a| a.as_ref()))?;
                }
            }
        }
        self.inner.schedule();
        Ok(())
    }

    /// Checks a finished download for a newer version every `every_secs` seconds
    /// (0 = stop checking). A newer version replaces the file once it is downloaded.
    pub fn set_sync(&self, id: i64, every_secs: u64) -> Result<()> {
        let mut st = self.inner.state();
        let rec = st.records.get_mut(&id).ok_or(Error::NotFound)?;
        if rec.kind.is_media() && every_secs > 0 {
            return Err(Error::Protocol("Streams aren't kept up to date".into()));
        }
        if rec.extras.zip_entry.is_some() && every_secs > 0 {
            return Err(Error::Protocol(
                "Files from inside an archive aren't kept up to date".into(),
            ));
        }
        rec.sync_every = i64::try_from(every_secs).unwrap_or(i64::MAX);
        if rec.sync_checked_at.is_none() {
            rec.sync_checked_at = Some(now_ms());
        }
        self.inner.persist_and_emit(rec, None)
    }

    /// Checks a finished download for a newer version right away.
    pub fn check_for_update(&self, id: i64) -> Result<()> {
        {
            let mut st = self.inner.state();
            let rec = st.records.get(&id).ok_or(Error::NotFound)?;
            if rec.status != Status::Completed {
                return Err(Error::Protocol("This download hasn't finished yet".into()));
            }
            if rec.kind.is_media() {
                return Err(Error::Protocol("Streams aren't kept up to date".into()));
            }
            if !st.checking.insert(id) {
                return Ok(());
            }
        }
        let me = self.inner.clone();
        self.inner
            .rt
            .spawn(async move { me.check_update(id).await });
        Ok(())
    }

    /// Reorders unfinished downloads. Selected items keep their relative order.
    pub fn move_in_queue(&self, ids: &[i64], mv: QueueMove) -> Result<()> {
        let sel: HashSet<i64> = ids.iter().copied().collect();
        let mut st = self.inner.state();
        let mut order: Vec<(i64, i64)> = st
            .records
            .values()
            .filter(|r| r.status != Status::Completed)
            .map(|r| (r.position, r.id))
            .collect();
        order.sort();
        let mut slots: Vec<i64> = order.iter().map(|o| o.0).collect();
        for i in 1..slots.len() {
            slots[i] = slots[i].max(slots[i - 1] + 1);
        }
        let mut ids_ord: Vec<i64> = order.iter().map(|o| o.1).collect();
        match mv {
            QueueMove::Top | QueueMove::Bottom => {
                let (picked, rest): (Vec<i64>, Vec<i64>) =
                    ids_ord.iter().partition(|id| sel.contains(id));
                ids_ord = if mv == QueueMove::Top {
                    [picked, rest].concat()
                } else {
                    [rest, picked].concat()
                };
            }
            QueueMove::Up => {
                for i in 1..ids_ord.len() {
                    if sel.contains(&ids_ord[i]) && !sel.contains(&ids_ord[i - 1]) {
                        ids_ord.swap(i, i - 1);
                    }
                }
            }
            QueueMove::Down => {
                for i in (0..ids_ord.len().saturating_sub(1)).rev() {
                    if sel.contains(&ids_ord[i]) && !sel.contains(&ids_ord[i + 1]) {
                        ids_ord.swap(i, i + 1);
                    }
                }
            }
        }
        let changed: Vec<(i64, i64)> = ids_ord
            .iter()
            .zip(&slots)
            .filter(|(id, pos)| st.records.get(id).is_some_and(|r| r.position != **pos))
            .map(|(id, pos)| (*id, *pos))
            .collect();
        self.inner.store.lock().unwrap().set_positions(&changed)?;
        for (id, pos) in changed {
            if let Some(rec) = st.records.get_mut(&id) {
                rec.position = pos;
                let run = st.running.get(&id).cloned();
                let rec = &st.records[&id];
                self.inner.emit(Event::Upsert {
                    download: Box::new(self.inner.info(rec, run.as_deref())),
                });
            }
        }
        Ok(())
    }

    /// Stops a running download and waits until its state is saved. Returns whether it
    /// was running.
    async fn stop_and_wait(&self, id: i64) -> bool {
        let run = self.inner.state().running.get(&id).cloned();
        match run {
            Some(run) => {
                run.stop(STOP_PAUSE);
                run.finished.cancelled().await;
                true
            }
            None => false,
        }
    }

    /// Throws away the partial data and downloads the file again from the start.
    pub async fn restart(&self, id: i64) -> Result<()> {
        let completed = self.get(id).ok_or(Error::NotFound)?.status == Status::Completed;
        if completed {
            return self.redownload(id, false).await;
        }
        self.stop_and_wait(id).await;
        {
            let mut st = self.inner.state();
            st.call_off_refresh(id);
            let rec = st.records.get_mut(&id).ok_or(Error::NotFound)?;
            if let (Some(dir), Some(name)) = (&rec.dir, &rec.filename) {
                let _ = partial_remove(rec, dir, name);
            }
            if let Some(m) = rec.media.as_mut() {
                m.fingerprint = None;
            }
            reset_progress(rec);
            rec.status = Status::Queued;
            self.inner.persist_and_emit(rec, None)?;
        }
        self.inner.schedule();
        Ok(())
    }

    /// Downloads a finished file again. With `replace`, the new copy replaces the old file
    /// once it is complete; otherwise it is saved under a new name if the old one is there.
    pub async fn redownload(&self, id: i64, replace: bool) -> Result<()> {
        {
            let mut st = self.inner.state();
            let rec = st.records.get_mut(&id).ok_or(Error::NotFound)?;
            if rec.status != Status::Completed {
                return Err(Error::Protocol("This download hasn't finished yet".into()));
            }
            reset_progress(rec);
            rec.on_conflict = if replace {
                OnConflict::Overwrite
            } else {
                OnConflict::Rename
            };
            rec.status = Status::Queued;
            self.inner.persist_and_emit(rec, None)?;
        }
        self.inner.schedule();
        Ok(())
    }

    /// Points an unfinished download at a new link (e.g. after the old one expired),
    /// keeping the progress. Refused if the new link is evidently a different file.
    pub async fn refresh_url(&self, id: i64, url: &str, meta: Option<RequestMeta>) -> Result<()> {
        let found = crate::FoundLink {
            url: url.to_string(),
            meta,
            ..Default::default()
        };
        self.refresh_link(id, found).await
    }

    /// The same with all the browser knows about the new link.
    pub async fn refresh_link(&self, id: i64, found: crate::FoundLink) -> Result<()> {
        use crate::refresh::By;
        // Checked first, so a bad link doesn't interrupt a running download.
        let fresh = self.inner.check_link(id, found, By::User).await?;
        self.stop_and_wait(id).await;
        {
            let mut st = self.inner.state();
            st.call_off_refresh(id);
            let rec = st.records.get_mut(&id).ok_or(Error::NotFound)?;
            Inner::put_link(rec, fresh, By::User);
            rec.status = Status::Queued;
            self.inner.persist_and_emit(rec, None)?;
        }
        self.inner.schedule();
        Ok(())
    }

    /// Who is asked for a fresh link when one expires; none means the user is asked at once.
    pub fn set_link_finder(&self, finder: Option<Arc<dyn crate::LinkFinder>>) {
        *self.inner.finder.write().unwrap() = finder;
    }

    /// An unfinished download with progress that looks like the same file, so a fresh
    /// link from the browser can continue it instead of starting over.
    pub fn find_resumable(&self, name: &str, size: Option<u64>) -> Option<DownloadInfo> {
        let size = size.filter(|&s| s > 0)?;
        let name = filename::sanitize(name)?;
        let st = self.inner.state();
        st.records
            .values()
            .filter(|r| {
                !st.running.contains_key(&r.id)
                    && matches!(
                        r.status,
                        Status::Paused | Status::Failed | Status::Waiting | Status::Queued
                    )
                    && r.downloaded > 0
                    && r.total_size == Some(size)
                    && r.filename.as_deref() == Some(name.as_str())
            })
            .max_by_key(|r| r.created_at)
            .map(|r| self.inner.info(r, None))
    }

    /// Number of connections for one download; applies immediately if it is running.
    pub fn set_connections(&self, id: i64, connections: u8) -> Result<()> {
        let connections = connections.clamp(1, 32);
        let mut st = self.inner.state();
        let State {
            records, running, ..
        } = &mut *st;
        let rec = records.get_mut(&id).ok_or(Error::NotFound)?;
        rec.connections = connections;
        let run = running.get(&id).cloned();
        if let Some(run) = &run {
            run.set_target(connections);
        }
        self.inner.persist_and_emit(rec, run.as_deref())?;
        let hosts = run
            .map(|r| r.hosts.lock().unwrap().clone())
            .unwrap_or_default();
        for host in hosts {
            self.inner.rebalance(&st, &host);
        }
        Ok(())
    }

    /// Uses `login` for a server from now on (kept in memory only), and resumes the
    /// downloads that stopped because that server wanted a login.
    pub fn set_login(&self, host: &str, login: Login) -> Result<()> {
        self.inner.net.set_login(host, Some(login));
        let ids: Vec<i64> = self
            .inner
            .state()
            .records
            .values()
            .filter(|r| r.needs_login.as_ref().is_some_and(|l| l.host == host))
            .map(|r| r.id)
            .collect();
        for id in ids {
            self.resume(id)?;
        }
        Ok(())
    }

    /// Fetches a small document (at most `max` bytes) through Siler's own connection
    /// settings (the proxy), following redirects. For the app's own needs, such as the
    /// update check; not a download.
    pub async fn fetch_bytes(
        &self,
        url: &str,
        max: u64,
        headers: &[(&str, &str)],
    ) -> Result<Vec<u8>> {
        use futures_util::StreamExt;
        let resp = self.send_own(url, headers).await?;
        let mut body = Vec::new();
        let mut stream = resp.into_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(Error::Network)?;
            if body.len() as u64 + chunk.len() as u64 > max {
                return Err(Error::Protocol("The answer is larger than expected".into()));
            }
            body.extend_from_slice(&chunk);
        }
        Ok(body)
    }

    /// Fetches `url` into `path` (replacing it) through Siler's own connection settings,
    /// reporting bytes done and the total when known. For the app's own needs (an update);
    /// not a download in the list.
    pub async fn fetch_to_file(
        &self,
        url: &str,
        path: &Path,
        headers: &[(&str, &str)],
        progress: impl Fn(u64, Option<u64>),
    ) -> Result<()> {
        use futures_util::StreamExt;
        use tokio::io::AsyncWriteExt;
        let resp = self.send_own(url, headers).await?;
        let total = resp.content_length();
        let mut file = tokio::fs::File::create(path).await?;
        let mut done = 0u64;
        let mut stream = resp.into_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(Error::Network)?;
            file.write_all(&chunk).await?;
            done += chunk.len() as u64;
            progress(done, total);
        }
        file.flush().await?;
        file.sync_all().await?;
        if total.is_some_and(|t| t != done) {
            return Err(Error::Network("connection closed early".into()));
        }
        Ok(())
    }

    /// A request of the app's own: no context, no login, only these headers.
    async fn send_own(&self, url: &str, headers: &[(&str, &str)]) -> Result<crate::session::Reply> {
        let url = probe::parse_web_url(url)?;
        let meta = RequestMeta::default();
        let resp = self
            .inner
            .net
            .send(crate::session::Request {
                url: &url,
                meta: &meta,
                user_agent: &self.settings().user_agent,
                range: None,
                policy: crate::session::Policy::Own(headers),
            })
            .await?;
        if !resp.status().is_success() {
            return Err(Error::Http(resp.status().as_u16()));
        }
        Ok(resp)
    }

    /// Trusts an SFTP server's key (saved in Siler's own known_hosts next to the database),
    /// and resumes the downloads that stopped for it.
    pub fn trust_host_key(&self, request: &crate::HostKeyRequest) -> Result<()> {
        let list = self
            .inner
            .net
            .known_hosts
            .read()
            .unwrap()
            .clone()
            .ok_or_else(|| Error::Protocol("No place to save server keys".into()))?;
        crate::transport::sftp::trust(&list, request)?;
        let ids: Vec<i64> = self
            .inner
            .state()
            .records
            .values()
            .filter(|r| {
                r.needs_host_key
                    .as_ref()
                    .is_some_and(|k| k.host == request.host)
            })
            .map(|r| r.id)
            .collect();
        for id in ids {
            self.resume(id)?;
        }
        Ok(())
    }

    /// Stops sending a login to a server.
    pub fn forget_login(&self, host: &str) {
        self.inner.net.set_login(host, None);
    }

    /// Where connections go under the current proxy settings.
    pub fn proxy_route(&self) -> ProxyRoute {
        self.inner.net.route()
    }

    /// Where `siler-pac`, which runs proxy scripts, is when it isn't next to Siler.
    pub fn set_pac_helper(&self, path: Option<PathBuf>) {
        self.inner.net.router.set_helper(path);
    }

    /// Which way an address would go and why ("Test with an address"); nothing is sent to it.
    pub async fn check_proxy(&self, url: &str) -> Result<crate::proxy::ProxyCheck> {
        self.inner.net.router.check(&checkable(url)?).await
    }

    /// The same with settings that aren't saved yet.
    pub async fn check_proxy_with(
        &self,
        settings: &crate::proxy::ProxySettings,
        url: &str,
    ) -> Result<crate::proxy::ProxyCheck> {
        let url = checkable(url)?;
        let trial = self.inner.net.router.trying(settings);
        trial.check(&url).await
    }

    /// The password for the manual proxy (from the keyring); applies immediately.
    pub fn set_proxy_password(&self, password: Option<String>) {
        *self.inner.proxy_password.lock().unwrap() = password.clone();
        self.inner
            .net
            .configure(&self.settings().proxy, password.as_deref());
    }

    /// Renames a download or moves it to another folder, together with its file
    /// (the finished file, or the partial one).
    pub async fn edit(&self, id: i64, edit: EditDownload) -> Result<()> {
        let new_name = match edit.filename.as_deref() {
            Some(n) => Some(
                filename::sanitize(n)
                    .ok_or_else(|| Error::Protocol("That file name isn't allowed".into()))?,
            ),
            None => None,
        };
        let was_running = self.stop_and_wait(id).await;
        let planned: Result<_> = (|| {
            let mut st = self.inner.state();
            let rec = st.records.get(&id).ok_or(Error::NotFound)?;
            let name = new_name.clone().or_else(|| rec.filename.clone());
            let dir = edit.dir.clone().or_else(|| rec.dir.clone());
            let moved = name != rec.filename || dir != rec.dir;
            let mut planned = None;
            if moved
                && let (Some(od), Some(on), Some(nd), Some(nn)) =
                    (&rec.dir, &rec.filename, &dir, &name)
            {
                let completed = rec.status == Status::Completed;
                let (from, to) = if completed {
                    (od.join(on), nd.join(nn))
                } else if rec.kind.is_media() {
                    (media::workdir(od, on), media::workdir(nd, nn))
                } else {
                    (filename::part_path(od, on), filename::part_path(nd, nn))
                };
                if from.exists() {
                    if nd.join(nn).exists()
                        || filename::part_path(nd, nn).exists()
                        || name_claimed(&st, nd, nn, Some(id))
                    {
                        return Err(Error::Exists(nn.clone()));
                    }
                    planned = Some((from, to));
                }
            }
            if planned.is_some() {
                st.moving.insert(id);
            }
            Ok(planned
                .map(|p| (p, name.clone(), dir.clone()))
                .ok_or((name, dir)))
        })();
        let planned = match planned {
            Ok(p) => p,
            Err(e) => {
                if was_running {
                    let _ = self.resume(id);
                }
                return Err(e);
            }
        };

        let (name, dir) = match planned {
            Ok(((from, to), name, dir)) => {
                let res = tokio::task::spawn_blocking(move || move_file(&from, &to))
                    .await
                    .map_err(|e| Error::Protocol(e.to_string()));
                self.inner.state().moving.remove(&id);
                if let Err(e) = res.and_then(|r| r.map_err(Error::from)) {
                    if was_running {
                        let _ = self.resume(id);
                    }
                    return Err(e);
                }
                (name, dir)
            }
            Err((name, dir)) => (name, dir),
        };
        {
            let mut st = self.inner.state();
            let rec = st.records.get_mut(&id).ok_or(Error::NotFound)?;
            if rec.kind.is_media()
                && rec.status == Status::Completed
                && let (Some(nd), Some(nn)) = (&dir, &name)
            {
                move_sidecars(rec, nd, nn);
            }
            rec.filename = name;
            rec.dir = dir;
            if was_running {
                rec.status = Status::Queued;
            }
            self.inner.persist_and_emit(rec, None)?;
        }
        self.inner.schedule();
        Ok(())
    }

    pub fn pause_all(&self) -> Result<()> {
        let ids: Vec<i64> = self
            .inner
            .state()
            .records
            .values()
            .filter(|r| r.status == Status::Queued || r.status.is_running())
            .map(|r| r.id)
            .collect();
        ids.into_iter().try_for_each(|id| self.pause(id))
    }

    pub fn resume_all(&self) -> Result<()> {
        let ids: Vec<i64> = self
            .inner
            .state()
            .records
            .values()
            .filter(|r| matches!(r.status, Status::Paused | Status::Failed))
            .map(|r| r.id)
            .collect();
        ids.into_iter().try_for_each(|id| self.resume(id))
    }

    /// Removes a download from the list. Unfinished partial data is always deleted;
    /// a completed file is deleted only when `delete_file` is set.
    pub fn remove(&self, id: i64, delete_file: bool) -> Result<()> {
        if self.remove_many(&[id], delete_file)? == 0 {
            return Err(Error::NotFound);
        }
        Ok(())
    }

    /// Removes several downloads at once; returns how many existed.
    pub fn remove_many(&self, ids: &[i64], delete_files: bool) -> Result<usize> {
        let removed: Vec<(Record, bool)> = {
            let mut st = self.inner.state();
            for id in ids {
                st.call_off_refresh(*id);
            }
            ids.iter()
                .filter_map(|id| {
                    let rec = st.records.remove(id)?;
                    st.forced.remove(id);
                    // A running download's task owns its part file and deletes it on exit.
                    let running = match st.running.get(id) {
                        Some(run) => {
                            run.stop(task::STOP_REMOVE);
                            true
                        }
                        None => false,
                    };
                    Some((rec, running))
                })
                .collect()
        };
        let removed_ids: Vec<i64> = removed.iter().map(|(r, _)| r.id).collect();
        self.inner.store.lock().unwrap().delete_many(&removed_ids)?;
        for (rec, running) in &removed {
            if let (Some(dir), Some(name)) = (&rec.dir, &rec.filename) {
                if rec.status != Status::Completed {
                    if !running {
                        let _ = partial_remove(rec, dir, name);
                    }
                } else if delete_files {
                    let _ = std::fs::remove_file(dir.join(name));
                    for s in rec.media.iter().flat_map(|m| &m.sidecars) {
                        let _ = std::fs::remove_file(dir.join(s));
                    }
                }
            }
            self.inner.emit(Event::Removed { id: rec.id });
        }
        self.inner.schedule();
        Ok(removed.len())
    }

    /// Per-download speed limit in bytes/s (0 = unlimited).
    pub fn set_speed_limit(&self, id: i64, bytes_per_sec: u64) -> Result<()> {
        let mut st = self.inner.state();
        let State {
            records, running, ..
        } = &mut *st;
        let rec = records.get_mut(&id).ok_or(Error::NotFound)?;
        rec.speed_limit = bytes_per_sec;
        let run = running.get(&id);
        if let Some(run) = run {
            run.limiter.set_rate(bytes_per_sec);
        }
        self.inner.persist_and_emit(rec, run.map(|a| a.as_ref()))
    }

    /// Looks at a link before anything is downloaded: a plain file, a page, or a stream with
    /// the video, audio and subtitles it offers.
    pub async fn probe_media(
        &self,
        url: &str,
        meta: &RequestMeta,
        hint: Option<&SourceHint>,
    ) -> Result<MediaProbe> {
        if media::stream::is_stream_url(url) {
            media::stream::parse_stream_url(url)?;
            return Ok(media::stream_view(
                self.inner.ffmpeg().is_some() && self.ffmpeg_status().network,
            ));
        }
        let s = self.settings();
        let lim = ParseLimits::default();
        let hint = hint.cloned().map(|h| h.checked(&lim)).transpose()?;
        let url = probe::parse_web_url(&self.inner.strip_login(url)?)?;
        let run_limit = RateLimiter::new(0);
        let io = Limiters {
            global: &self.inner.global,
            run: &run_limit,
            count: None,
        };
        let http = Http::new(
            self.inner.net.clone(),
            &url,
            meta.clone(),
            &s.user_agent,
            s.allow_private_media_addresses,
        );
        let src = media::resolve(&http, &url, hint.as_ref(), &lim, &io).await?;
        Ok(media::probe_view(
            src,
            &media::preferences(&s),
            self.inner.ffmpeg().is_some(),
        ))
    }

    /// Adds a stream as a media download: the chosen (or the default) video, audio and
    /// subtitle tracks, in the chosen container. The stream is read again at every attempt.
    pub async fn add_media(&self, nd: NewMediaDownload) -> Result<i64> {
        if media::stream::is_stream_url(&nd.url) {
            return self.add_stream(nd).await;
        }
        let s = self.settings();
        let lim = ParseLimits::default();
        let hint = nd.hint.clone().map(|h| h.checked(&lim)).transpose()?;
        let url = probe::parse_web_url(&self.inner.strip_login(&nd.url)?)?;
        if nd.meta.body.as_ref().is_some_and(|b| b.len() > MAX_BODY) {
            return Err(Error::Protocol(
                "The form data is too large to send again".into(),
            ));
        }
        let run_limit = RateLimiter::new(0);
        let io = Limiters {
            global: &self.inner.global,
            run: &run_limit,
            count: None,
        };
        let http = Http::new(
            self.inner.net.clone(),
            &url,
            nd.meta.clone(),
            &s.user_agent,
            s.allow_private_media_addresses,
        );
        let src = media::resolve(&http, &url, hint.as_ref(), &lim, &io).await?;
        let final_url = src.final_url.clone();
        // A live stream is recorded: the picker told the user so.
        let pres = media::presentation_or_error(src, true)?;
        let selection = nd
            .selection
            .clone()
            .unwrap_or_else(|| select::default_selection(&pres, &media::preferences(&s)));
        let resolved = select::resolve(&pres, &selection)
            .map_err(|_| Error::Media(MediaError::SelectionUnavailable))?;
        let choice = nd.container.unwrap_or(s.default_media_container);
        let subtitles = nd.subtitles.unwrap_or(s.default_subtitle_mode);
        let (output, reason) = select::resolve_container(choice, &resolved, subtitles)
            .map_err(|e| Error::Protocol(e.to_string()))?;
        if output != Output::Original && self.inner.ffmpeg().is_none() {
            return Err(Error::Media(MediaError::FfmpegMissing));
        }
        let only = if output == Output::Original {
            resolved.tracks().first().copied()
        } else {
            None
        };
        let ext = media::extension(output, only);
        let title = pres.title.clone();
        let base = nd
            .filename
            .clone()
            .unwrap_or_else(|| media::default_name(title.as_deref(), &final_url));
        let stem = media::strip_media_extension(&base).to_string();
        let wanted = filename::sanitize(&format!("{stem}.{ext}"))
            .ok_or_else(|| Error::Protocol("That file name isn't allowed".into()))?;
        let dir = nd
            .dir
            .clone()
            .unwrap_or_else(|| s.category_dir(Category::Video));
        let on_conflict = match nd.on_conflict.unwrap_or(s.on_conflict) {
            OnConflict::Ask => OnConflict::Rename,
            p => p,
        };
        let mrec = MediaRecord {
            manifest: pres.kind,
            picked: media::same::picked(&pres, &selection),
            selection,
            container: choice,
            output,
            reason,
            subtitles,
            title,
            labels: resolved
                .tracks()
                .iter()
                .map(|t| media::describe(t))
                .collect(),
            fingerprint: None,
            sidecars: Vec::new(),
            hint,
            live: pres.is_live(),
            finish: false,
        };
        self.insert_media(MediaInsert {
            link: nd.link.clone(),
            url,
            final_url,
            dir,
            wanted,
            on_conflict,
            queue_id: nd.queue_id,
            start_paused: nd.start_paused,
            meta: nd.meta,
            kind: JobKind::Media,
            media: mrec,
        })
    }

    /// Adds an RTMP or MMS stream, recorded through FFmpeg until Finish.
    async fn add_stream(&self, nd: NewMediaDownload) -> Result<i64> {
        let s = self.settings();
        let url = media::stream::parse_stream_url(&nd.url)?;
        if self.inner.ffmpeg().is_none() {
            return Err(Error::Media(MediaError::FfmpegMissing));
        }
        let choice = nd.container.unwrap_or(s.default_media_container);
        // RTMP carries H.264 and AAC, which MP4 holds; MMS carries Windows Media, which only
        // Matroska does.
        let output = match choice {
            ContainerChoice::Mp4 => Output::Mp4,
            ContainerChoice::Auto if url.scheme().starts_with("rtmp") => Output::Mp4,
            _ => Output::Mkv,
        };
        let ext = media::extension(output, None);
        let base = nd
            .filename
            .clone()
            .unwrap_or_else(|| media::default_name(None, &url));
        let stem = media::strip_media_extension(&base).to_string();
        let wanted = filename::sanitize(&format!("{stem}.{ext}"))
            .ok_or_else(|| Error::Protocol("That file name isn't allowed".into()))?;
        let dir = nd
            .dir
            .clone()
            .unwrap_or_else(|| s.category_dir(Category::Video));
        let on_conflict = match nd.on_conflict.unwrap_or(s.on_conflict) {
            OnConflict::Ask => OnConflict::Rename,
            p => p,
        };
        let label = if url.scheme().starts_with("rtmp") {
            "RTMP stream"
        } else {
            "MMS stream"
        };
        let mrec = MediaRecord {
            manifest: siler_media::ManifestKind::Stream,
            selection: Selection::default(),
            container: choice,
            output,
            reason: "Recorded as it arrives, without re-encoding".into(),
            subtitles: SubtitleMode::None,
            title: None,
            labels: vec![label.into()],
            fingerprint: None,
            sidecars: Vec::new(),
            hint: None,
            picked: Vec::new(),
            live: true,
            finish: false,
        };
        self.insert_media(MediaInsert {
            link: nd.link.clone(),
            final_url: url.clone(),
            url,
            dir,
            wanted,
            on_conflict,
            queue_id: nd.queue_id,
            start_paused: nd.start_paused,
            meta: RequestMeta::default(),
            kind: JobKind::Stream,
            media: mrec,
        })
    }

    fn insert_media(&self, m: MediaInsert) -> Result<i64> {
        let MediaInsert {
            url,
            final_url,
            dir,
            wanted,
            on_conflict,
            queue_id,
            start_paused,
            meta,
            kind,
            media: mrec,
            link,
        } = m;
        let s = self.settings();
        let mut st = self.inner.state();
        // Two unfinished media downloads never share a name, so never a Work Directory. An
        // existing file of that name is dealt with when the output is published.
        let name = free_media_name(&st, &dir, &wanted);
        let queue_id = queue_id
            .filter(|q| st.queues.contains_key(q))
            .unwrap_or(MAIN_QUEUE);
        let position = st.records.values().map(|r| r.position).max().unwrap_or(0) + 1;
        let mut rec = Record {
            id: 0,
            url: url.to_string(),
            final_url: Some(final_url.to_string()),
            filename: Some(name),
            dir: Some(dir),
            category: Category::Video,
            total_size: None,
            downloaded: 0,
            status: if start_paused {
                Status::Waiting
            } else {
                Status::Queued
            },
            resumable: None,
            error: None,
            notice: None,
            etag: None,
            last_modified: None,
            meta: meta.bound_to(url.as_str()),
            connections: s.connections.clamp(1, 32),
            speed_limit: 0,
            created_at: now_ms(),
            completed_at: None,
            segments: Vec::new(),
            position,
            on_conflict,
            queue_id,
            sync_every: 0,
            sync_checked_at: None,
            needs_login: None,
            needs_host_key: None,
            kind,
            media: Some(mrec),
            extras: Extras {
                link,
                ..Default::default()
            },
            traffic: Traffic::default(),
            auto_connections: false,
        };
        rec.id = self.inner.store.lock().unwrap().insert(&rec)?;
        let id = rec.id;
        let info = self.inner.info(&rec, None);
        st.records.insert(id, rec);
        drop(st);
        self.inner.emit(Event::Upsert {
            download: Box::new(info),
        });
        self.inner.schedule();
        Ok(id)
    }

    /// Sets the FFmpeg that combines media (its path from the application's bundle, or
    /// `PATH` in development). Returns what was found, whether or not it is used.
    pub fn set_ffmpeg_path(&self, path: Option<PathBuf>) -> FfmpegStatus {
        let status = match &path {
            Some(p) => media::probe_ffmpeg(p),
            None => FfmpegStatus {
                missing: vec!["no FFmpeg was found".into()],
                ..Default::default()
            },
        };
        *self.inner.ffmpeg.lock().unwrap() = if status.ok { path } else { None };
        *self.inner.ffmpeg_status.lock().unwrap() = status.clone();
        status
    }

    /// What the FFmpeg check found.
    pub fn ffmpeg_status(&self) -> FfmpegStatus {
        self.inner.ffmpeg_status.lock().unwrap().clone()
    }

    /// The finished file, or the partial data while the download is unfinished (the part
    /// file, or a media download's Work Directory).
    pub fn file_path(&self, id: i64) -> Option<PathBuf> {
        let st = self.inner.state();
        let r = st.records.get(&id)?;
        let (dir, name) = (r.dir.as_ref()?, r.filename.as_ref()?);
        Some(if r.status == Status::Completed {
            dir.join(name)
        } else if r.kind.is_media() {
            media::workdir(dir, name)
        } else {
            filename::part_path(dir, name)
        })
    }

    /// Keeps a download whose checksum didn't match: the file is published as it is.
    pub fn accept_unverified(&self, id: i64) -> Result<()> {
        {
            let mut st = self.inner.state();
            let rec = st.records.get_mut(&id).ok_or(Error::NotFound)?;
            let mismatch = rec.status == Status::Failed
                && rec.extras.verify.as_ref().and_then(|v| v.matches()) == Some(false);
            if !mismatch {
                return Err(Error::Protocol(
                    "This download didn't fail a checksum check".into(),
                ));
            }
            rec.extras.accept_unverified = true;
            rec.status = Status::Queued;
            rec.error = None;
            self.inner.persist_and_emit(rec, None)?;
        }
        self.inner.schedule();
        Ok(())
    }

    /// Sets (or clears) the checksum a download is checked against when it finishes.
    pub fn set_checksum(&self, id: i64, checksum: Option<Checksum>) -> Result<()> {
        let mut st = self.inner.state();
        let State {
            records, running, ..
        } = &mut *st;
        let rec = records.get_mut(&id).ok_or(Error::NotFound)?;
        rec.extras.checksum = checksum;
        self.inner
            .persist_and_emit(rec, running.get(&id).map(|a| a.as_ref()))
    }

    /// Checks a finished file against `checksum` (or the download's own checksum). Without
    /// either, only its SHA-256 is worked out. The result is kept with the download.
    pub async fn verify_file(&self, id: i64, checksum: Option<Checksum>) -> Result<VerifyResult> {
        let (path, expected) = {
            let st = self.inner.state();
            let r = st.records.get(&id).ok_or(Error::NotFound)?;
            if r.status != Status::Completed {
                return Err(Error::Protocol("This download hasn't finished yet".into()));
            }
            let path = match (&r.dir, &r.filename) {
                (Some(d), Some(n)) => d.join(n),
                _ => return Err(Error::FileMissing),
            };
            (path, checksum.clone().or_else(|| r.extras.checksum.clone()))
        };
        if !path.exists() {
            return Err(Error::FileMissing);
        }
        let algo = expected.as_ref().map_or(Algo::Sha256, |c| c.algo);
        let stop = Arc::new(AtomicBool::new(false));
        let actual =
            tokio::task::spawn_blocking(move || checksum::hash_file(&path, algo, None, &stop))
                .await
                .map_err(|e| Error::Protocol(e.to_string()))??
                .unwrap_or_default();
        let result = VerifyResult {
            algo,
            expected: expected.as_ref().map(|c| c.hex.clone()),
            actual,
            at: now_ms(),
        };
        let mut st = self.inner.state();
        if let Some(r) = st.records.get_mut(&id) {
            if checksum.is_some() {
                r.extras.checksum = checksum;
            }
            r.extras.verify = Some(result.clone());
            self.inner.persist_and_emit(r, None)?;
        }
        Ok(result)
    }

    /// MD5, SHA-1 and SHA-256 of a finished file, read once.
    pub async fn digests(&self, id: i64) -> Result<Vec<(Algo, String)>> {
        let path = self
            .file_path(id)
            .filter(|_| self.get(id).is_some_and(|d| d.status == Status::Completed))
            .ok_or_else(|| Error::Protocol("This download hasn't finished yet".into()))?;
        let algos = [Algo::Md5, Algo::Sha1, Algo::Sha256];
        let stop = AtomicBool::new(false);
        let hashes = tokio::task::spawn_blocking(move || {
            checksum::hash_range(&path, &algos, 0, None, None, &stop)
        })
        .await
        .map_err(|e| Error::Protocol(e.to_string()))??
        .unwrap_or_default();
        Ok(algos.into_iter().zip(hashes).collect())
    }

    /// Replaces a download's mirrors (other addresses of the same file). A running download
    /// restarts its transfer to use them.
    pub async fn set_mirrors(&self, id: i64, urls: Vec<String>) -> Result<()> {
        let mut parsed = Vec::new();
        for u in &urls {
            let u = u.trim();
            if u.is_empty() {
                continue;
            }
            parsed.push(probe::parse_url(u)?.to_string());
        }
        {
            let st = self.inner.state();
            let r = st.records.get(&id).ok_or(Error::NotFound)?;
            if r.kind.is_media() || r.extras.zip_entry.is_some() {
                return Err(Error::Protocol(
                    "Mirrors can only be used for ordinary files".into(),
                ));
            }
        }
        let was_running = self.stop_and_wait(id).await;
        {
            let mut st = self.inner.state();
            let r = st.records.get_mut(&id).ok_or(Error::NotFound)?;
            let old = std::mem::take(&mut r.extras.mirrors);
            let own = r.url.clone();
            for u in parsed {
                if u == own || r.extras.mirrors.iter().any(|m| m.url == u) {
                    continue;
                }
                let kept = old.iter().find(|m| m.url == u).cloned();
                r.extras
                    .mirrors
                    .push(kept.unwrap_or_else(|| Mirror::new(u, MirrorOrigin::User)));
            }
            if was_running {
                r.status = Status::Queued;
            }
            self.inner.persist_and_emit(r, None)?;
        }
        self.inner.schedule();
        Ok(())
    }

    /// Where the data quota stands.
    pub fn quota_status(&self) -> QuotaStatus {
        self.inner.quota.status(&self.settings().quota)
    }

    /// Lifts the data quota until its period ends.
    pub fn ignore_quota(&self) -> QuotaStatus {
        let status = self.inner.quota.ignore(&self.settings().quota);
        self.inner.global.set_budget_end(u64::MAX);
        self.inner.emit(Event::Quota(status.clone()));
        self.inner.schedule();
        status
    }

    /// Lists the files inside a ZIP archive on a server, with range requests.
    pub async fn zip_list(&self, url: &str, meta: &RequestMeta) -> Result<zip::ZipListing> {
        let s = self.settings();
        let url = self.inner.strip_login(url)?;
        let p = probe::probe(&self.inner.net, &url, meta, &s.user_agent).await?;
        let Some(size) = p.total_size.filter(|_| p.resumable) else {
            return Err(Error::Protocol(
                "This server can't show what's inside the archive; download the whole archive instead."
                    .into(),
            ));
        };
        let get = |start: u64, len: u64| {
            probe::fetch_range(
                &self.inner.net,
                &p.final_url,
                &p.final_meta,
                &s.user_agent,
                start,
                len,
            )
        };
        let tail_len = size.min(zip::TAIL);
        let tail_start = size - tail_len;
        let tail = get(tail_start, tail_len).await?;
        let mut end = zip::find_end(&tail, tail_start).map_err(Error::Protocol)?;
        if let zip::End::Zip64At(at) = end {
            let rec = if at >= tail_start && at + 56 <= size {
                tail[(at - tail_start) as usize..(at - tail_start + 56) as usize].to_vec()
            } else {
                get(at, 56).await?
            };
            end = zip::read_end64(&rec).map_err(Error::Protocol)?;
        }
        let zip::End::Directory {
            offset,
            size: cd_size,
            count,
        } = end
        else {
            return Err(Error::Protocol(
                "The archive's end record is damaged.".into(),
            ));
        };
        if cd_size > zip::MAX_DIRECTORY {
            return Err(Error::Protocol(
                "The archive's list of files is too large to show.".into(),
            ));
        }
        if offset.saturating_add(cd_size) > size {
            return Err(Error::Protocol(
                "The archive's list of files is damaged.".into(),
            ));
        }
        let cd = if offset >= tail_start {
            tail[(offset - tail_start) as usize..(offset - tail_start + cd_size) as usize].to_vec()
        } else {
            get(offset, cd_size).await?
        };
        let entries = zip::parse_directory(&cd, count).map_err(Error::Protocol)?;
        Ok(zip::ZipListing {
            url: p.final_url,
            archive_size: size,
            entries,
        })
    }

    /// Runs the after-download command on a file now, to try it out.
    pub async fn test_command(&self, command: &str, path: &Path) -> CommandResult {
        let words = match postrun::split(command) {
            Ok(w) => w,
            Err(e) => {
                return CommandResult {
                    error: Some(e),
                    ..Default::default()
                };
            }
        };
        let size = std::fs::metadata(path).ok().map(|m| m.len());
        let words = postrun::fill(
            &words,
            &postrun::Vars {
                path,
                url: "",
                size,
            },
        );
        postrun::run(words, &self.inner.quit).await
    }

    /// Writes downloads as a list: plain addresses, or Siler JSON. Cookies go in only with
    /// `include_cookies`.
    pub fn export_list(
        &self,
        ids: &[i64],
        format: crate::lists::ListFormat,
        include_cookies: bool,
    ) -> Result<String> {
        let st = self.inner.state();
        let items: Vec<crate::lists::ExportItem> = st
            .records
            .values()
            .filter(|r| ids.is_empty() || ids.contains(&r.id))
            .map(|r| crate::lists::ExportItem {
                url: &r.url,
                filename: r.filename.as_deref(),
                dir: r.dir.as_deref(),
                queue: st.queues.get(&r.queue_id).map(|q| q.name.as_str()),
                size: r.total_size,
                checksum: r.extras.checksum.as_ref(),
                mirrors: r.extras.mirrors.iter().map(|m| m.url.as_str()).collect(),
                referrer: r.meta.referrer.as_deref(),
                // A list pairs cookies with the link, so only when they are that link's.
                cookies: r.meta.cookies.as_deref().filter(|_| {
                    include_cookies && reqwest::Url::parse(&r.url).is_ok_and(|u| r.meta.is_home(&u))
                }),
                user_agent: r.meta.user_agent.as_deref(),
            })
            .collect();
        crate::lists::export(&items, format).map_err(Error::Protocol)
    }

    /// Stops all transfers, checkpointing their progress so they resume on next start.
    pub async fn shutdown(&self) {
        let runs: Vec<Arc<Running>> = {
            let mut st = self.inner.state();
            st.shutting_down = true;
            st.running.values().cloned().collect()
        };
        for r in &runs {
            r.stop(STOP_SHUTDOWN);
        }
        self.inner.quit.cancel();
        for (_, token) in self.inner.grabs.lock().unwrap().values() {
            token.cancel();
        }
        if let Some(json) = self.inner.quota.take_dirty() {
            let _ = self.inner.store.lock().unwrap().save_value("quota", &json);
        }
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline && !self.inner.state().running.is_empty() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
}

/// Deletes the partial data of a download that isn't running: a media Work Directory, or a
/// part file.
fn partial_remove(rec: &Record, dir: &Path, name: &str) -> std::io::Result<()> {
    if rec.kind.is_media() {
        std::fs::remove_dir_all(media::workdir(dir, name))
    } else {
        std::fs::remove_file(filename::part_path(dir, name))
    }
}

/// `name`, or the first `name (n)` that no unfinished download claims and no Work Directory
/// uses in `dir`.
fn free_media_name(st: &State, dir: &Path, name: &str) -> String {
    let taken = |n: &str| name_claimed(st, dir, n, None) || media::workdir(dir, n).exists();
    if !taken(name) {
        return name.to_string();
    }
    let (stem, ext) = filename::split_ext(name);
    (1..)
        .map(|i| format!("{stem} ({i}){ext}"))
        .find(|n| !taken(n))
        .unwrap_or_else(|| name.to_string())
}

/// Moves (and renames, with the output) the subtitle files written beside a finished media
/// download. Missing files are left out; a file in the way is not replaced.
fn move_sidecars(rec: &mut Record, new_dir: &Path, new_name: &str) {
    let (Some(old_dir), Some(old_name)) = (rec.dir.clone(), rec.filename.clone()) else {
        return;
    };
    let Some(m) = rec.media.as_mut() else { return };
    let old_stem = filename::split_ext(&old_name).0.to_string();
    let new_stem = filename::split_ext(new_name).0;
    let mut kept = Vec::new();
    for s in &m.sidecars {
        let renamed = match s.strip_prefix(&old_stem) {
            Some(rest) if rest.starts_with('.') => format!("{new_stem}{rest}"),
            _ => s.clone(),
        };
        let (from, to) = (old_dir.join(s), new_dir.join(&renamed));
        if !from.exists() {
            continue;
        }
        if from == to || (!to.exists() && move_file(&from, &to).is_ok()) {
            kept.push(renamed);
        } else {
            kept.push(s.clone());
        }
    }
    m.sidecars = kept;
}

/// True if another unfinished download already intends to write `dir/name`.
pub(crate) fn name_claimed(st: &State, dir: &Path, name: &str, except: Option<i64>) -> bool {
    st.records.values().any(|r| {
        Some(r.id) != except
            && r.status != Status::Completed
            && r.dir.as_deref() == Some(dir)
            && r.filename.as_deref() == Some(name)
    })
}

impl Inner {
    pub(crate) fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap()
    }

    pub(crate) fn settings(&self) -> Settings {
        self.settings.read().unwrap().clone()
    }

    /// The FFmpeg to combine media with, when it is usable.
    pub(crate) fn ffmpeg(&self) -> Option<std::path::PathBuf> {
        self.ffmpeg.lock().unwrap().clone()
    }

    pub(crate) fn emit(&self, ev: Event) {
        let _ = self.events.send(ev);
    }

    fn info(&self, r: &Record, run: Option<&Running>) -> DownloadInfo {
        let (downloaded, speed, active, segments) = match run {
            Some(run) => {
                let table = run.table.lock().unwrap();
                let segs = if table.segs.is_empty() {
                    r.segments.clone()
                } else {
                    table.segs.clone()
                };
                (
                    if table.segs.is_empty() && !r.kind.is_media() {
                        r.downloaded
                    } else {
                        run.downloaded.load(Relaxed)
                    },
                    run.speed.load(Relaxed),
                    run.active.load(Relaxed),
                    segs,
                )
            }
            None => (r.downloaded, 0, 0, r.segments.clone()),
        };
        let file_missing = r.status == Status::Completed
            && match (&r.dir, &r.filename) {
                (Some(d), Some(n)) => !d.join(n).exists(),
                _ => false,
            };
        DownloadInfo {
            id: r.id,
            url: r.url.clone(),
            final_url: r.final_url.clone(),
            referrer: r.meta.referrer.clone(),
            page_url: r.meta.page_url.clone(),
            method: r.meta.method.clone(),
            filename: r.filename.clone(),
            dir: r.dir.clone(),
            category: r.category,
            total_size: r.total_size,
            downloaded,
            status: r.status,
            resumable: r.resumable,
            error: r.error.clone(),
            notice: r.notice.clone(),
            connections: r.connections,
            speed_limit: r.speed_limit,
            created_at: r.created_at,
            completed_at: r.completed_at,
            position: r.position,
            on_conflict: r.on_conflict,
            queue_id: r.queue_id,
            sync_every: u64::try_from(r.sync_every).unwrap_or(0),
            sync_checked_at: r.sync_checked_at,
            file_missing,
            error_code: r.error.as_ref().and(r.extras.error_code.clone()),
            needs_login: r.needs_login.clone(),
            needs_host_key: r.needs_host_key.clone(),
            needs_link: r.status == Status::Failed && r.extras.refresh.expired,
            speed,
            active_connections: active,
            segments: visible_segments(&segments),
            media: r.media.as_ref().map(|m| MediaInfo {
                manifest: m.manifest,
                output: m.output,
                reason: m.reason.clone(),
                subtitles: m.subtitles,
                labels: m.labels.clone(),
                sidecars: m.sidecars.clone(),
                live: m.live,
                finished: m.finish,
            }),
            checksum: r.extras.checksum.clone(),
            verify: r.extras.verify.clone(),
            checksum_failed: r.status == Status::Failed
                && r.extras.verify.as_ref().and_then(|v| v.matches()) == Some(false),
            sources: source_views(r, run),
            zip_path: r.extras.zip_entry.as_ref().map(|z| z.path.clone()),
            command: r.extras.command.clone(),
            traffic: run.map_or(r.traffic, Running::traffic).measured(),
        }
    }

    pub(crate) fn persist_and_emit(&self, rec: &Record, run: Option<&Running>) -> Result<()> {
        self.store.lock().unwrap().update(rec)?;
        self.emit(Event::Upsert {
            download: Box::new(self.info(rec, run)),
        });
        Ok(())
    }

    fn tick(self: &Arc<Self>) {
        self.check_quota();
        let mut guard = self.state();
        let went_idle = guard.running.is_empty();
        if went_idle {
            if guard.idle_reported {
                return;
            }
            guard.idle_reported = true;
        } else {
            guard.idle_reported = false;
        }
        let st = &*guard;
        let mut total_speed = 0;
        let items = st
            .running
            .iter()
            .map(|(&id, run)| {
                let speed = run.sample_speed();
                total_speed += speed;
                ProgressItem {
                    id,
                    downloaded: run.downloaded.load(Relaxed),
                    total_size: st.records.get(&id).and_then(|r| r.total_size),
                    speed,
                    active_connections: run.active.load(Relaxed),
                    segments: visible_segments(&run.table.lock().unwrap().segs),
                    media: run.media.lock().unwrap().clone(),
                    phase: run.phase.lock().unwrap().map(|name| crate::PhaseProgress {
                        name,
                        done: run.phase_done.load(Relaxed),
                        total: run.phase_total.load(Relaxed),
                    }),
                    source_bytes: st
                        .records
                        .get(&id)
                        .map(|r| source_views(r, Some(run)).iter().map(|v| v.bytes).collect())
                        .unwrap_or_default(),
                    traffic: run.traffic().measured(),
                }
            })
            .collect();
        drop(guard);
        self.emit(Event::Progress { items, total_speed });
        if went_idle {
            self.store.lock().unwrap().idle_maintenance();
        }
    }

    /// Starts queued downloads while there are free slots.
    /// Moves `user:password@` from a URL into the login for its server.
    pub(crate) fn strip_login(&self, url: &str) -> Result<String> {
        let mut u = probe::parse_url(url)?;
        self.net.take_userinfo(&mut u);
        Ok(u.to_string())
    }

    pub(crate) fn join_host(&self, id: i64, run: &Running, host: String) {
        let mut st = self.state();
        if !st.running.contains_key(&id) {
            return;
        }
        {
            let mut hosts = run.hosts.lock().unwrap();
            if hosts.contains(&host) {
                return;
            }
            hosts.push(host.clone());
        }
        st.hosts.entry(host.clone()).or_default().push(id);
        self.rebalance(&st, &host);
    }

    /// Shares a server's connection limit among its running downloads: water-filling, so
    /// a download that wants few connections leaves the rest to the others.
    fn rebalance(&self, st: &State, host: &str) {
        let Some(ids) = st.hosts.get(host) else {
            return;
        };
        let runs: Vec<&Arc<Running>> = ids.iter().filter_map(|i| st.running.get(i)).collect();
        let name = host.rsplit_once(':').map_or(host, |(h, p)| {
            if p.chars().all(|c| c.is_ascii_digit()) {
                h
            } else {
                host
            }
        });
        let limit = self.settings.read().unwrap().connection_limit(name);
        for (run, cap) in runs.iter().zip(share_connections(
            limit,
            &runs
                .iter()
                .map(|r| r.target.load(Relaxed))
                .collect::<Vec<_>>(),
        )) {
            run.set_host_cap(host, cap);
        }
    }

    pub(crate) fn schedule(self: &Arc<Self>) {
        let max = self.settings.read().unwrap().max_concurrent;
        let mut st = self.state();
        if st.shutting_down || self.quota.reached() || self.global.over_budget() {
            return;
        }
        let State {
            records,
            running,
            forced,
            moving,
            refreshing,
            queues,
            queue_results,
            ..
        } = &mut *st;
        let mut per_queue: HashMap<i64, usize> = HashMap::new();
        for id in running.keys() {
            if let Some(r) = records.get(id) {
                *per_queue.entry(r.queue_id).or_default() += 1;
            }
        }
        let mut queues_changed = false;
        // "Start now" first, then by queue position.
        let mut queued: Vec<(bool, i64, i64)> = records
            .values()
            .filter(|r| {
                r.status == Status::Queued
                    && !running.contains_key(&r.id)
                    && !moving.contains(&r.id)
                    && !refreshing.contains_key(&r.id)
            })
            .map(|r| (!forced.contains(&r.id), r.position, r.id))
            .collect();
        queued.sort();
        for (unforced, _, id) in queued {
            if unforced && running.len() >= max {
                break;
            }
            let qid = records[&id].queue_id;
            let queue_max = queues.get(&qid).map_or(0, |q| q.max_concurrent as usize);
            let in_queue = per_queue.entry(qid).or_default();
            if unforced && queue_max > 0 && *in_queue >= queue_max {
                continue;
            }
            *in_queue += 1;
            if let Some(q) = queues.get_mut(&qid)
                && !q.active
            {
                q.active = true;
                queue_results.insert(qid, (0, 0));
                queues_changed = true;
            }
            forced.remove(&id);
            let rec = records.get_mut(&id).expect("queued id exists");
            rec.status = Status::Probing;
            rec.error = None;
            rec.needs_login = None;
            rec.needs_host_key = None;
            rec.extras.refresh.expired = false;
            let run = Arc::new(Running::new(rec.speed_limit, rec.connections));
            run.seed_traffic(rec.traffic);
            running.insert(id, run.clone());
            if let Err(e) = self.persist_and_emit(rec, Some(&run)) {
                tracing::warn!("failed to persist download {id}: {e}");
            }
            let me = self.clone();
            self.rt.spawn(async move { me.run(id, run).await });
        }
        if queues_changed {
            self.emit_queues(&st);
        }
    }

    fn emit_queues(&self, st: &State) {
        self.emit(Event::Queues {
            queues: st.queues.values().cloned().collect(),
        });
    }

    fn start_queue(self: &Arc<Self>, queue_id: i64) -> Result<()> {
        {
            let mut st = self.state();
            let State {
                records,
                running,
                queues,
                queue_results,
                ..
            } = &mut *st;
            let q = queues.get_mut(&queue_id).ok_or(Error::NotFound)?;
            for rec in records.values_mut() {
                if rec.queue_id == queue_id
                    && rec.status == Status::Waiting
                    && !running.contains_key(&rec.id)
                {
                    rec.status = Status::Queued;
                    self.persist_and_emit(rec, None)?;
                }
            }
            if !q.active {
                q.active = true;
                queue_results.insert(queue_id, (0, 0));
            }
            self.emit_queues(&st);
        }
        self.schedule();
        // Nothing to run: it is done straight away, without an event.
        let mut st = self.state();
        self.finish_queues(&mut st, false);
        Ok(())
    }

    fn stop_queue(&self, queue_id: i64, pause_running: bool) -> Result<()> {
        let mut st = self.state();
        let State {
            records,
            running,
            queues,
            hold,
            ..
        } = &mut *st;
        let q = queues.get_mut(&queue_id).ok_or(Error::NotFound)?;
        q.active = false;
        for rec in records.values_mut().filter(|r| r.queue_id == queue_id) {
            match running.get(&rec.id) {
                Some(run) if pause_running => {
                    hold.insert(rec.id);
                    run.stop(STOP_PAUSE);
                }
                Some(_) => {}
                None if rec.status == Status::Queued => {
                    rec.status = Status::Waiting;
                    self.persist_and_emit(rec, None)?;
                }
                None => {}
            }
        }
        self.emit_queues(&st);
        Ok(())
    }

    /// Ends active queues with nothing left to run. With `announce`, a queue that ran
    /// something reports `QueueDone`; otherwise (the user stopped things) it ends quietly.
    pub(crate) fn finish_queues(&self, st: &mut State, announce: bool) {
        let State {
            records,
            running,
            queues,
            queue_results,
            ..
        } = st;
        let mut changed = false;
        for q in queues.values_mut().filter(|q| q.active) {
            let busy = records.values().any(|r| {
                r.queue_id == q.id && (r.status == Status::Queued || running.contains_key(&r.id))
            });
            if busy {
                continue;
            }
            q.active = false;
            changed = true;
            let (completed, failed) = queue_results.remove(&q.id).unwrap_or_default();
            if announce && completed + failed > 0 {
                self.emit(Event::QueueDone {
                    queue_id: q.id,
                    name: q.name.clone(),
                    after_done: q.after_done,
                    completed,
                    failed,
                });
            }
        }
        if changed {
            self.emit_queues(st);
        }
    }

    /// Starts scheduled queues whose window is open (after Siler was started).
    fn catch_up_schedules(self: &Arc<Self>, now: jiff::Timestamp) {
        let tz = scheduler::local_tz();
        let open: Vec<i64> = self
            .state()
            .queues
            .values()
            .filter(|q| q.schedule.is_open(&tz, now))
            .map(|q| q.id)
            .collect();
        for id in open {
            tracing::info!("queue {id}: inside its scheduled time, starting");
            let _ = self.start_queue(id);
        }
    }

    fn run_schedules(self: &Arc<Self>, last: jiff::Timestamp, now: jiff::Timestamp) {
        let tz = scheduler::local_tz();
        let due: Vec<(i64, Edge, bool)> = self
            .state()
            .queues
            .values()
            .filter_map(|q| {
                let edge = q.schedule.edges(&tz, last, now).last()?.1;
                // A one-off schedule is used up when its window closes.
                let used_up =
                    q.schedule.once && (edge == Edge::Stop || q.schedule.stop_at.is_none());
                Some((q.id, edge, used_up))
            })
            .collect();
        for (id, edge, used_up) in due {
            tracing::info!("queue {id}: scheduled {edge:?}");
            let _ = match edge {
                Edge::Start => self.start_queue(id),
                Edge::Stop => self.stop_queue(id, true),
            };
            if used_up {
                let mut st = self.state();
                if let Some(q) = st.queues.get_mut(&id) {
                    q.schedule.enabled = false;
                    let q = q.clone();
                    let _ = self.store.lock().unwrap().save_queue(&q);
                    self.emit_queues(&st);
                }
            }
        }
    }

    /// Starts checks for downloads that are due to be checked for a newer version.
    fn check_due_updates(self: &Arc<Self>) {
        let now = now_ms();
        let due: Vec<i64> = {
            let mut st = self.state();
            let due: Vec<i64> = st
                .records
                .values()
                .filter(|r| {
                    // Media streams aren't kept up to date (the stream's own manifest would
                    // be checked as if it were a file).
                    r.kind == JobKind::File
                        && r.status == Status::Completed
                        && r.sync_every > 0
                        && !st.checking.contains(&r.id)
                        && now - r.sync_checked_at.or(r.completed_at).unwrap_or(0)
                            >= r.sync_every.saturating_mul(1000)
                })
                .map(|r| r.id)
                .collect();
            st.checking.extend(&due);
            due
        };
        for id in due {
            let me = self.clone();
            self.rt.spawn(async move { me.check_update(id).await });
        }
    }

    /// Asks the server whether a finished download changed, and downloads it again if so.
    async fn check_update(self: Arc<Self>, id: i64) {
        let Some(rec) = self.state().records.get(&id).cloned() else {
            self.state().checking.remove(&id);
            return;
        };
        let s = self.settings();
        let result = probe::probe(&self.net, &rec.url, &rec.meta, &s.user_agent).await;
        let mut changed = false;
        {
            let mut st = self.state();
            st.checking.remove(&id);
            let Some(r) = st.records.get_mut(&id) else {
                return;
            };
            if r.status != Status::Completed {
                return;
            }
            r.sync_checked_at = Some(now_ms());
            match result {
                Ok(p) => {
                    changed = matches!((r.total_size, p.total_size), (Some(a), Some(b)) if a != b)
                        || differs(&r.etag, &p.etag)
                        || differs(&r.last_modified, &p.last_modified);
                    if changed {
                        tracing::info!("download {id}: newer version on the server");
                        reset_progress(r);
                        r.on_conflict = OnConflict::Overwrite;
                        r.status = Status::Queued;
                        r.notice = Some("A newer version is on the server; downloading it.".into());
                    } else {
                        r.notice = None;
                    }
                }
                Err(e) => {
                    r.notice = Some(format!("Couldn't check for a newer version: {e}"));
                }
            }
            let _ = self.persist_and_emit(r, None);
        }
        if changed {
            self.schedule();
        }
    }

    async fn run(self: Arc<Self>, id: i64, run: Arc<Running>) {
        let (kind, waited) = match self.state().records.get(&id) {
            Some(r) => (Some(r.kind), crate::refresh::waited(r, now_ms())),
            None => (None, false),
        };
        let mut expired = false;
        // An attempt at a fresh link that is left to the browser.
        let mut attempt = None;
        let mut result = loop {
            let result = match kind {
                Some(JobKind::Media) => media::drive(&self, id, &run).await,
                Some(JobKind::Stream) => media::stream::drive(&self, id, &run).await,
                _ => self.drive(id, &run).await,
            };
            let Err(e) = &result else { break result };
            if run.stop_reason.load(Relaxed) != 0 {
                break result;
            }
            expired = self.state().records.get(&id).is_some_and(|rec| {
                let table = run.table.lock().unwrap().downloaded();
                let downloaded = rec.downloaded.max(table).max(run.downloaded.load(Relaxed));
                crate::refresh::expired(rec, downloaded, waited, e)
            });
            if !expired {
                break result;
            }
            attempt = self.begin_refresh(id, &run);
            // The second attempt goes straight to the browser.
            if attempt != Some(1) || !self.renew_silently(id, &run).await {
                break result;
            }
            expired = false;
            attempt = None;
        };
        let reason = run.stop_reason.load(Relaxed);
        if expired {
            result = result.map_err(crate::refresh::failure);
        }
        let mut failed = None;
        let mut seek = None;
        let removed;
        {
            let mut st = self.state();
            st.running.remove(&id);
            let held = st.hold.remove(&id);
            if reason == 0 && attempt.is_some() && !st.shutting_down {
                seek = st
                    .records
                    .get(&id)
                    .and_then(|rec| self.browser_to_ask(rec))
                    .map(|ask| (ask, CancellationToken::new()));
                if let Some((_, token)) = &seek {
                    st.refreshing.insert(id, token.clone());
                }
            }
            let qid = st.records.get(&id).map(|r| r.queue_id);
            if let Some(counts) = qid.and_then(|q| st.queue_results.get_mut(&q)) {
                match (&result, reason) {
                    (Ok(Outcome::Done), _) => counts.0 += 1,
                    // Not yet, while a fresh link is looked for.
                    (Err(_), 0) if seek.is_none() => counts.1 += 1,
                    _ => {}
                }
            }
            let hosts = std::mem::take(&mut *run.hosts.lock().unwrap());
            for host in hosts {
                if let Some(ids) = st.hosts.get_mut(&host) {
                    ids.retain(|&i| i != id);
                    if ids.is_empty() {
                        st.hosts.remove(&host);
                    }
                }
                self.rebalance(&st, &host);
            }
            removed = !st.records.contains_key(&id);
            if let Some(rec) = st.records.get_mut(&id)
                && !matches!(result, Ok(Outcome::Done))
            {
                {
                    let table = run.table.lock().unwrap();
                    if !table.segs.is_empty() {
                        rec.segments = table.segs.clone();
                        rec.downloaded = table.downloaded();
                    } else if rec.kind.is_media() {
                        rec.downloaded = run.downloaded.load(Relaxed);
                    }
                    drop(table);
                    rec.traffic = run.traffic();
                    match (&result, reason) {
                        (_, STOP_SHUTDOWN) => {
                            // Keep the "running" status on disk so it resumes on next start.
                            let _ = self.store.lock().unwrap().update_progress(
                                id,
                                rec.downloaded,
                                &rec.segments,
                                rec.traffic,
                            );
                        }
                        (Err(e), 0) if seek.is_some() => {
                            tracing::info!(
                                "download {id}: its link expired ({e}); asking the browser"
                            );
                            rec.status = Status::Queued;
                            rec.error = None;
                            rec.notice = Some(
                                "The link expired. Getting a fresh one from the browser…".into(),
                            );
                            let _ = self.persist_and_emit(rec, None);
                        }
                        (Err(e), 0) => {
                            tracing::info!("download {id} failed: {e}");
                            rec.extras.refresh.expired = expired;
                            rec.status = Status::Failed;
                            rec.error = Some(e.to_string());
                            rec.extras.error_code = Some(e.into());
                            rec.needs_login = e.login_request();
                            rec.needs_host_key = match e {
                                Error::HostKeyUnknown(r) => Some((**r).clone()),
                                _ => None,
                            };
                            failed = Some(match (e.login_request(), rec.needs_host_key.clone()) {
                                (Some(request), _) => Event::LoginRequired {
                                    id,
                                    filename: rec.filename.clone(),
                                    request,
                                },
                                (None, Some(request)) => Event::HostKeyUnknown {
                                    id,
                                    filename: rec.filename.clone(),
                                    request,
                                },
                                (None, None) => Event::Failed {
                                    id,
                                    filename: rec.filename.clone(),
                                    error: e.to_string(),
                                    code: Some(e.into()),
                                    needs_link: expired,
                                },
                            });
                            let _ = self.persist_and_emit(rec, None);
                        }
                        (_, STOP_QUOTA) => {
                            rec.status = Status::Queued;
                            rec.notice = Some(self.quota_notice());
                            let _ = self.persist_and_emit(rec, None);
                        }
                        _ => {
                            rec.status = if held {
                                Status::Waiting
                            } else {
                                Status::Paused
                            };
                            let _ = self.persist_and_emit(rec, None);
                        }
                    }
                }
            }
        }
        if removed && let Some(part) = run.part.lock().unwrap().take() {
            let _ = if part.is_dir() {
                std::fs::remove_dir_all(part)
            } else {
                std::fs::remove_file(part)
            };
        }
        run.finished.cancel();
        if let Some(ev) = failed {
            self.emit(ev);
        }
        let done = matches!(result, Ok(Outcome::Done));
        if let (Some(((finder, ask), token)), Err(failure)) = (seek, result) {
            self.rt
                .spawn(self.clone().refresh_in_browser(finder, ask, failure, token));
        }
        if done && reason == 0 && !removed {
            let me = self.clone();
            self.rt.spawn(async move { me.after_download(id).await });
        }
        if reason != STOP_SHUTDOWN {
            self.schedule();
            // Stopped by the user (or the queue): end the queue quietly if nothing is left.
            let announce = reason == 0;
            self.finish_queues(&mut self.state(), announce);
        }
    }

    /// Shows a transient message on a download (e.g. retry progress).
    fn note(&self, id: i64, run: &Running, notice: String) {
        let mut st = self.state();
        if let Some(rec) = st.records.get_mut(&id) {
            rec.notice = Some(notice);
            let _ = self.persist_and_emit(rec, Some(run));
        }
    }

    /// Asks the server about the download, retrying passing failures like the transfer does.
    async fn probe_with_retries(
        &self,
        id: i64,
        run: &Running,
        rec: &Record,
        s: &Settings,
    ) -> Result<Option<ProbeResult>> {
        let mut attempt = 0;
        loop {
            let r = tokio::select! {
                r = probe::probe(&self.net, &rec.url, &rec.meta, &s.user_agent) => r,
                _ = run.cancel.cancelled() => return Ok(None),
            };
            match r {
                Ok(p) => return Ok(Some(p)),
                Err(e) if e.is_transient() && attempt < s.max_retries => {
                    attempt += 1;
                    tracing::info!("download {id}: probe failed ({e}), retry {attempt}");
                    self.note(
                        id,
                        run,
                        format!(
                            "Couldn't reach the server — retry {attempt} of {}: {e}",
                            s.max_retries
                        ),
                    );
                    let delay = task::retry_delay(attempt, e.retry_after());
                    tokio::select! {
                        _ = tokio::time::sleep(delay) => {}
                        _ = run.cancel.cancelled() => return Ok(None),
                    }
                }
                Err(e) => return Err(e),
            }
        }
    }

    async fn drive(self: &Arc<Self>, id: i64, run: &Arc<Running>) -> Result<Outcome> {
        let rec = self
            .state()
            .records
            .get(&id)
            .cloned()
            .ok_or(Error::NotFound)?;
        let s = self.settings();

        // "Keep anyway" after a checksum mismatch: the data is all there, publish it as it is.
        if rec.extras.accept_unverified
            && let Some(done) = self.publish_kept(id, run, &rec).await?
        {
            return Ok(done);
        }

        let Some(probe) = self.probe_with_retries(id, run, &rec, &s).await? else {
            return Ok(Outcome::Stopped);
        };
        if let Ok(u) = Url::parse(&probe.final_url) {
            self.join_host(id, run, host_key(&u));
        }

        let zip = rec.extras.zip_entry.clone();
        if s.open_metalinks
            && zip.is_none()
            && rec.segments.is_empty()
            && metalink::is_metalink(
                probe.content_type.as_deref(),
                rec.filename.as_deref().unwrap_or(&probe.filename),
            )
        {
            return self.open_metalink(id, run, &rec, &probe, &s).await;
        }

        let mut extras = rec.extras.clone();
        extras.accept_unverified = false;
        // A file inside an archive: its compressed bytes, at their place in the archive.
        let (base, total) = match &zip {
            Some(z) => {
                if probe.total_size != Some(z.archive_size) {
                    return Err(Error::LinkMismatch(
                        "The archive changed on the server since its contents were listed. Preview it again to pick the file."
                            .into(),
                    ));
                }
                if !probe.resumable {
                    return Err(Error::Protocol(
                        "The server doesn't send parts of the archive any more, so a file can't be fetched out of it."
                            .into(),
                    ));
                }
                let data = match z.data_offset {
                    Some(d) => d,
                    None => {
                        let head = tokio::select! {
                            r = probe::fetch_range(&self.net, &probe.final_url, &probe.final_meta, &s.user_agent, z.header_offset, 30) => r?,
                            _ = run.cancel.cancelled() => return Ok(Outcome::Stopped),
                        };
                        zip::data_offset(&head, z.header_offset).map_err(Error::Protocol)?
                    }
                };
                if data.saturating_add(z.compressed) > z.archive_size {
                    return Err(Error::Protocol(
                        "The archive's list of files is damaged.".into(),
                    ));
                }
                if let Some(e) = extras.zip_entry.as_mut() {
                    e.data_offset = Some(data);
                }
                (data, Some(z.compressed))
            }
            None => (0, probe.total_size),
        };
        if zip.is_none() {
            // A checksum the server sends is used unless the user gave one.
            if matches!(
                &extras.checksum,
                None | Some(Checksum {
                    source: ChecksumSource::Server,
                    ..
                })
            ) && let Some(d) = probe.digest.clone()
            {
                extras.checksum = Some(d);
            }
            for u in &probe.duplicates {
                if extras.mirrors.len() >= 10 {
                    break;
                }
                if *u != rec.url && !extras.mirrors.iter().any(|m| m.url == *u) {
                    extras
                        .mirrors
                        .push(Mirror::new(u.clone(), MirrorOrigin::Server));
                }
            }
        }

        let mut segments = rec.segments.clone();
        let mut notice: Option<&str> = None;
        if !segments.is_empty() {
            let changed = rec.total_size != total
                || differs(&rec.etag, &probe.etag)
                || differs(&rec.last_modified, &probe.last_modified);
            if changed {
                segments.clear();
                notice = Some("The file changed on the server, so the download started over.");
            } else if !probe.resumable {
                segments.clear();
                notice = Some("This server can't resume downloads, so the download started over.");
            }
        }

        let name = rec
            .filename
            .clone()
            .unwrap_or_else(|| probe.filename.clone());
        let category = if rec.filename.is_none() && rec.category == Category::Other {
            s.category_for(&name)
        } else {
            rec.category
        };
        let dir = rec.dir.clone().unwrap_or_else(|| s.category_dir(category));
        std::fs::create_dir_all(&dir)?;

        let part_exists = filename::part_path(&dir, &name).exists();
        let own_part = !rec.segments.is_empty() && part_exists;
        if !segments.is_empty() && !part_exists {
            segments.clear();
            notice = Some("The partial file was missing, so the download started over.");
        }
        if segments.is_empty() && !rec.segments.is_empty() {
            // Starting over: what it had will be fetched again.
            run.wasted.fetch_add(rec.downloaded, Relaxed);
        }

        let ranged = probe.resumable && total.is_some();
        let primary = Url::parse(&probe.final_url).map_err(|e| Error::InvalidUrl(e.to_string()))?;
        let sources = self
            .probe_mirrors(run, &rec, &mut extras, primary, total, ranged, &s)
            .await;
        if run.cancel.is_cancelled() {
            return Ok(Outcome::Stopped);
        }
        for src in &sources[1..] {
            self.join_host(id, run, src.host.clone());
        }
        *run.sources.lock().unwrap() = sources.clone();

        let (name, part, file) = {
            // Claim the name and create the part file under the lock so two downloads
            // can never pick the same file.
            let mut st = self.state();
            // Removed or stopped while probing: don't create a file nobody will clean up.
            if run.cancel.is_cancelled() || !st.records.contains_key(&id) {
                return Ok(Outcome::Stopped);
            }
            let name = if own_part {
                name
            } else {
                let on_disk = dir.join(&name).exists();
                let claimed = name_claimed(&st, &dir, &name, Some(id))
                    || filename::part_path(&dir, &name).exists();
                match rec.on_conflict {
                    OnConflict::Skip if on_disk || claimed => return Err(Error::Exists(name)),
                    OnConflict::Overwrite if !claimed => name,
                    _ => {
                        filename::unique_name(&dir, &name, |n| name_claimed(&st, &dir, n, Some(id)))
                    }
                }
            };
            let part = filename::part_path(&dir, &name);
            // Never through a link or into a file with other names (see `partfile`). Our own
            // partial is continued, or replaced when the download starts over.
            let file = match (own_part, segments.is_empty()) {
                (true, false) => partfile::resume(&part)?,
                (true, true) => partfile::replace(&part)?,
                (false, _) => partfile::create(&part)?,
            };
            *run.part.lock().unwrap() = Some(part.clone());
            if segments.is_empty() {
                if let Some(total) = total.filter(|&t| t > 0) {
                    preallocate(&file, total)?;
                }
                segments = match total {
                    Some(total) if ranged => {
                        segment::initial(total, rec.connections as usize, s.min_split_size)
                    }
                    Some(total) => vec![Segment::new(0, total)],
                    None => vec![Segment::new(0, u64::MAX)],
                };
            }
            let downloaded: u64 = segments.iter().map(Segment::downloaded).sum();

            *run.table.lock().unwrap() = Table::new(segments.clone());
            run.downloaded.store(downloaded, Relaxed);
            run.ranged.store(ranged, Relaxed);
            *run.sample.lock().unwrap() = (Instant::now(), downloaded, 0.0);

            let r = st.records.get_mut(&id).ok_or(Error::NotFound)?;
            r.final_url = Some(probe.final_url.clone());
            r.filename = Some(name.clone());
            r.dir = Some(dir.clone());
            r.category = category;
            r.total_size = total;
            r.resumable = Some(probe.resumable);
            r.etag = probe.etag.clone();
            r.last_modified = probe.last_modified.clone();
            r.status = Status::Downloading;
            r.error = None;
            r.notice = notice.map(String::from);
            r.segments = segments;
            r.downloaded = downloaded;
            r.extras = extras.clone();
            self.persist_and_emit(r, Some(run))?;
            (name, part, file)
        };

        let t = Arc::new(Transfer {
            net: self.net.clone(),
            sources: sources.clone(),
            base,
            meta: probe.final_meta.clone(),
            user_agent: s.user_agent.clone(),
            file,
            run: run.clone(),
            global: self.global.clone(),
            ranged,
        });
        // Never claims more than the mode promises is on disk (see `checkpoint`).
        let checkpoints = Checkpoints::new(s.durable_checkpoints);
        let checkpoint = || {
            if !run.ranged.load(Relaxed) {
                return;
            }
            let snap = {
                // Taken together: a chunk is written and counted under this lock.
                let table = run.table.lock().unwrap();
                Snapshot {
                    downloaded: run.downloaded.load(Relaxed),
                    segments: table.segs.clone(),
                    traffic: run.traffic(),
                }
            };
            let (t, me) = (t.clone(), self.clone());
            checkpoints.save(
                snap,
                move || t.file.sync_data(),
                move |snap| {
                    let _ = me.store.lock().unwrap().update_progress(
                        id,
                        snap.downloaded,
                        &snap.segments,
                        snap.traffic,
                    );
                },
            );
        };
        let limits = Limits {
            min_split: s.min_split_size,
            max_retries: s.max_retries,
        };

        let mut repaired = false;
        loop {
            let outcome = task::transfer(t.clone(), limits.clone(), &checkpoint).await;
            checkpoints.settle().await;
            self.save_source_stats(id, &sources);
            let outcome = match outcome {
                Ok(outcome) => outcome,
                Err(e) => {
                    // `run` saves the segments next: what they cover must be on disk first,
                    // as on a pause.
                    let t = t.clone();
                    match tokio::task::spawn_blocking(move || t.file.sync_data()).await {
                        Ok(Err(sync)) => tracing::warn!(%sync, "couldn't flush a failed download"),
                        Err(join) => tracing::warn!(%join, "couldn't flush a failed download"),
                        Ok(Ok(())) => {}
                    }
                    return Err(e);
                }
            };
            match outcome {
                Outcome::Stopped => {
                    let t = t.clone();
                    let _ = tokio::task::spawn_blocking(move || t.file.sync_data()).await;
                    return Ok(Outcome::Stopped);
                }
                Outcome::Done => {}
            }
            let size = total.unwrap_or_else(|| run.downloaded.load(Relaxed));
            // Waiting for the disk can take seconds: not on the runtime's own threads. Most
            // of the data is already written (`task` starts writeback as it goes), and
            // `sync_data` still covers the new length, which reading the file back needs.
            let t2 = t.clone();
            tokio::task::spawn_blocking(move || {
                t2.file.set_len(size)?;
                t2.file.sync_data()
            })
            .await
            .map_err(|e| Error::Protocol(e.to_string()))??;
            let job = Finish {
                dir: dir.clone(),
                name: name.clone(),
                part: part.clone(),
                size,
                repair: !repaired,
            };
            match self.finish(id, run, job).await? {
                Finished::Published => return Ok(Outcome::Done),
                Finished::Stopped => return Ok(Outcome::Stopped),
                Finished::Repair(bad) => {
                    // Fetch only the damaged pieces again, then check once more.
                    repaired = true;
                    let segs = repair_segments(size, &bad);
                    let left: u64 = bad.iter().map(|(a, b)| b - a).sum();
                    run.wasted.fetch_add(left, Relaxed);
                    *run.table.lock().unwrap() = Table::new(segs);
                    run.downloaded.store(size - left, Relaxed);
                    self.note(
                        id,
                        run,
                        format!(
                            "{} damaged {} fetched again.",
                            bad.len(),
                            if bad.len() == 1 {
                                "piece is"
                            } else {
                                "pieces are"
                            }
                        ),
                    );
                }
            }
        }
    }

    /// Publishes a download whose checksum didn't match, when the user chose to keep it.
    /// None when the data isn't all there any more (it then downloads normally).
    async fn publish_kept(
        self: &Arc<Self>,
        id: i64,
        run: &Arc<Running>,
        rec: &Record,
    ) -> Result<Option<Outcome>> {
        let (Some(dir), Some(name), Some(size)) = (&rec.dir, &rec.filename, rec.total_size) else {
            return Ok(None);
        };
        let part = filename::part_path(dir, name);
        let complete = !rec.segments.is_empty() && rec.segments.iter().all(Segment::done);
        if !complete || !part.exists() {
            return Ok(None);
        }
        *run.part.lock().unwrap() = Some(part.clone());
        {
            let mut st = self.state();
            let r = st.records.get_mut(&id).ok_or(Error::NotFound)?;
            r.status = Status::Downloading;
            self.persist_and_emit(r, Some(run))?;
        }
        let job = Finish {
            dir: dir.clone(),
            name: name.clone(),
            part,
            size,
            repair: false,
        };
        Ok(Some(match self.finish(id, run, job).await? {
            Finished::Published => Outcome::Done,
            _ => Outcome::Stopped,
        }))
    }

    /// Probes the mirrors of a download. Returns the sources to fetch from: the download's
    /// own URL first, then every mirror that serves the same file with ranges.
    #[allow(clippy::too_many_arguments)]
    async fn probe_mirrors(
        &self,
        run: &Running,
        rec: &Record,
        extras: &mut Extras,
        primary: Url,
        total: Option<u64>,
        ranged: bool,
        s: &Settings,
    ) -> Vec<Arc<Source>> {
        let private_ok = siler_media::policy::is_private_source(&primary)
            || Url::parse(&rec.url).is_ok_and(|u| siler_media::policy::is_private_source(&u));
        let mut sources = vec![Arc::new(Source::new(primary, 0))];
        if extras.mirrors.is_empty() {
            return sources;
        }
        let Some(total) = total.filter(|_| ranged) else {
            for m in &mut extras.mirrors {
                m.disabled = Some(
                    "The main address can't send parts of the file, so mirrors can't share the work."
                        .into(),
                );
            }
            return sources;
        };
        // Cookies and the browser's headers belong to the download's own server.
        let meta = RequestMeta {
            user_agent: rec.meta.user_agent.clone(),
            ..Default::default()
        };
        let net = self.net.clone();
        let checks = extras.mirrors.iter().map(|m| {
            let (net, meta, url, ua) = (
                net.clone(),
                meta.clone(),
                m.url.clone(),
                s.user_agent.clone(),
            );
            // Addresses that came from a file or a server, not from the user, must be public.
            let guard = m.origin != MirrorOrigin::User && !private_ok;
            async move {
                let work = async {
                    if guard {
                        public_address(&url).await?;
                    }
                    let p = probe::probe(&net, &url, &meta, &ua)
                        .await
                        .map_err(|e| e.to_string())?;
                    if guard && p.final_url != url {
                        public_address(&p.final_url).await?;
                    }
                    Ok::<_, String>(p)
                };
                tokio::time::timeout(Duration::from_secs(30), work)
                    .await
                    .unwrap_or_else(|_| Err("It didn't answer in time".into()))
            }
        });
        let results = tokio::select! {
            r = futures_util::future::join_all(checks) => r,
            _ = run.cancel.cancelled() => return sources,
        };
        for (i, (m, r)) in extras.mirrors.iter_mut().zip(results).enumerate() {
            m.disabled = match r {
                Err(e) => Some(e),
                Ok(p) if p.total_size != Some(total) => Some(match p.total_size {
                    Some(n) => format!(
                        "It has a different file ({} instead of {})",
                        human_size(n),
                        human_size(total)
                    ),
                    None => "It doesn't say the file's size".into(),
                }),
                Ok(p) if !p.resumable => Some("It can't send parts of the file".into()),
                Ok(p) => match Url::parse(&p.final_url) {
                    Ok(u) => {
                        sources.push(Arc::new(Source::new(u, i + 1)));
                        None
                    }
                    Err(e) => Some(e.to_string()),
                },
            };
        }
        sources
    }

    /// Adds what each source delivered to the saved totals, and why any was given up.
    fn save_source_stats(&self, id: i64, sources: &[Arc<Source>]) {
        if sources.len() < 2 {
            return;
        }
        let mut st = self.state();
        let Some(r) = st.records.get_mut(&id) else {
            return;
        };
        for src in sources {
            let bytes = src.bytes.swap(0, Relaxed);
            let why = src.disabled.lock().unwrap().clone();
            if src.index == 0 {
                r.extras.own_bytes += bytes;
            } else if let Some(m) = r.extras.mirrors.get_mut(src.index - 1) {
                m.bytes += bytes;
                if why.is_some() {
                    m.disabled = why;
                }
            }
        }
        let _ = self.store.lock().unwrap().update(r);
    }

    /// Replaces a metalink download with the files it lists.
    async fn open_metalink(
        self: &Arc<Self>,
        id: i64,
        run: &Arc<Running>,
        rec: &Record,
        probe: &ProbeResult,
        s: &Settings,
    ) -> Result<Outcome> {
        let url = Url::parse(&probe.final_url).map_err(|e| Error::InvalidUrl(e.to_string()))?;
        let http = Http::new(
            self.net.clone(),
            &url,
            probe.final_meta.clone(),
            &s.user_agent,
            true,
        );
        let io = Limiters {
            global: &self.global,
            run: &run.limiter,
            count: None,
        };
        let fetched = tokio::select! {
            r = crate::http::fetch_bounded(&http, &url, crate::http::Req::Source, metalink::MAX_BYTES, crate::http::Overflow::Refuse, &io) => r?,
            _ = run.cancel.cancelled() => return Ok(Outcome::Stopped),
        };
        let files = metalink::parse(&fetched.body).map_err(Error::Protocol)?;
        let files: Vec<crate::NewDownload> = files
            .iter()
            .map(|f| {
                let base = rec
                    .dir
                    .clone()
                    .unwrap_or_else(|| s.category_dir(s.category_for(&f.name)));
                let mut nd = f.to_download(Some(&base));
                nd.queue_id = Some(rec.queue_id);
                nd.meta.user_agent = rec.meta.user_agent.clone();
                nd
            })
            .collect();
        tracing::info!("download {id} is a metalink with {} files", files.len());
        self.emit(Event::Metalink { id, files });
        let engine = Engine {
            inner: self.clone(),
        };
        engine.remove_many(&[id], false)?;
        Ok(Outcome::Stopped)
    }

    /// The end of a file download: unpack it (a file from an archive), check its checksum,
    /// and move it to its final name.
    async fn finish(
        self: &Arc<Self>,
        id: i64,
        run: &Arc<Running>,
        job: Finish,
    ) -> Result<Finished> {
        let (zip, checksum, pieces, accept) = {
            let st = self.state();
            let r = st.records.get(&id).ok_or(Error::NotFound)?;
            (
                r.extras.zip_entry.clone(),
                r.extras.checksum.clone(),
                r.extras.pieces.clone(),
                r.extras.accept_unverified,
            )
        };
        // Long work below can be stopped like the transfer.
        let stop = Arc::new(AtomicBool::new(false));
        let watcher = {
            let (stop, cancel) = (stop.clone(), run.cancel.clone());
            tokio::spawn(async move {
                cancel.cancelled().await;
                stop.store(true, Relaxed);
            })
        };
        let result = self
            .finish_inner(id, run, &job, zip, checksum, pieces, accept, &stop)
            .await;
        watcher.abort();
        run.set_phase(None, 0);
        result
    }

    #[allow(clippy::too_many_arguments)]
    async fn finish_inner(
        self: &Arc<Self>,
        id: i64,
        run: &Arc<Running>,
        job: &Finish,
        zip: Option<crate::extras::ZipEntry>,
        checksum: Option<Checksum>,
        pieces: Option<checksum::Pieces>,
        accept: bool,
        stop: &Arc<AtomicBool>,
    ) -> Result<Finished> {
        // The file that will be published: the part file, or what was unpacked from it.
        let mut out = job.part.clone();
        let mut size = job.size;
        if let Some(z) = &zip {
            run.set_phase(Some("unpacking"), z.size);
            let unpacked = job.dir.join(format!(".{}.siler-unpack", job.name));
            let (part, dest, entry, run2, stop2) = (
                job.part.clone(),
                unpacked.clone(),
                z.clone(),
                run.clone(),
                stop.clone(),
            );
            let r = tokio::task::spawn_blocking(move || {
                // The scratch file is in the user's folder: never through what is there.
                let out = match entry.method {
                    0 => None,
                    _ => Some(partfile::replace(&dest)?),
                };
                Ok::<_, Error>(zip::unpack(&part, out, &entry, &run2.phase_done, &stop2))
            })
            .await
            .map_err(|e| Error::Protocol(e.to_string()))??;
            match r {
                Ok(true) => {}
                Ok(false) => {
                    let _ = std::fs::remove_file(&unpacked);
                    return Ok(Finished::Stopped);
                }
                Err(e) => {
                    let _ = std::fs::remove_file(&unpacked);
                    return Err(Error::Protocol(e));
                }
            }
            if z.method != 0 {
                out = unpacked;
            }
            size = z.size;
        }
        let cleanup = |out: &Path| {
            if *out != job.part {
                let _ = std::fs::remove_file(out);
            }
        };

        let mut verify = None;
        if let Some(c) = checksum.filter(|_| !accept) {
            run.set_phase(Some("verifying"), size);
            let (path, run2, stop2, algo) = (out.clone(), run.clone(), stop.clone(), c.algo);
            let actual = tokio::task::spawn_blocking(move || {
                checksum::hash_file(&path, algo, Some(&run2.phase_done), &stop2)
            })
            .await
            .map_err(|e| Error::Protocol(e.to_string()))??;
            let Some(actual) = actual else {
                cleanup(&out);
                return Ok(Finished::Stopped);
            };
            let result = VerifyResult {
                algo: c.algo,
                expected: Some(c.hex.clone()),
                actual: actual.clone(),
                at: now_ms(),
            };
            if result.matches() != Some(true) {
                // With piece hashes, find the damaged pieces and fetch only those again.
                if job.repair
                    && zip.is_none()
                    && let Some(p) = pieces.filter(|p| p.fits(size))
                {
                    run.set_phase(Some("verifying"), size);
                    let (path, run2, stop2) = (out.clone(), run.clone(), stop.clone());
                    let bad = tokio::task::spawn_blocking(move || {
                        checksum::bad_pieces(&path, &p, size, Some(&run2.phase_done), &stop2).map(
                            |b| b.map(|b| b.iter().map(|&i| p.range(i, size)).collect::<Vec<_>>()),
                        )
                    })
                    .await
                    .map_err(|e| Error::Protocol(e.to_string()))??;
                    match bad {
                        None => return Ok(Finished::Stopped),
                        Some(bad) if !bad.is_empty() => return Ok(Finished::Repair(bad)),
                        Some(_) => {}
                    }
                }
                cleanup(&out);
                let mut st = self.state();
                if let Some(r) = st.records.get_mut(&id) {
                    r.extras.verify = Some(result);
                }
                return Err(Error::ChecksumMismatch {
                    algo: c.algo.label(),
                    expected: c.hex,
                    actual,
                });
            }
            verify = Some(result);
        }

        let mut st = self.state();
        let Some(on_conflict) = st.records.get(&id).map(|r| r.on_conflict) else {
            // Removed in the last moment; run() deletes the part file.
            cleanup(&out);
            return Ok(Finished::Stopped);
        };
        let (dir, name) = (&job.dir, &job.name);
        let final_name = if dir.join(name).exists() && on_conflict != OnConflict::Overwrite {
            filename::unique_name(dir, name, |n| name_claimed(&st, dir, n, Some(id)))
        } else {
            name.clone()
        };
        let path = dir.join(&final_name);
        partfile::publishable(&out)?;
        std::fs::rename(&out, &path)?;
        if out != job.part {
            let _ = std::fs::remove_file(&job.part);
        }
        run.part.lock().unwrap().take();
        let r = st.records.get_mut(&id).ok_or(Error::NotFound)?;
        r.filename = Some(final_name.clone());
        r.traffic = run.traffic();
        r.status = Status::Completed;
        r.completed_at = Some(now_ms());
        r.segments.clear();
        r.downloaded = size;
        r.total_size = Some(size);
        if verify.is_some() {
            r.extras.verify = verify;
            r.notice = None;
        } else if accept {
            r.notice = Some("Kept although its checksum didn't match.".into());
        }
        self.persist_and_emit(r, None)?;
        drop(st);
        self.emit(Event::Completed {
            id,
            filename: final_name,
            path,
        });
        Ok(Finished::Published)
    }

    /// Reads the quota a few times a second: stops downloads when it is used up, and lets
    /// them go on when a new period starts.
    fn check_quota(self: &Arc<Self>) {
        let q = self.settings.read().unwrap().quota;
        let total = self.global.total();
        let changed = self.quota.update(&q, total, now_ms());
        // File transfers stop themselves once the rest of the allowance has passed.
        self.global.set_budget_end(match self.quota.remaining(&q) {
            Some(left) => total.saturating_add(left),
            None => u64::MAX,
        });
        if let Some(status) = changed {
            if status.reached {
                tracing::info!("data quota reached");
                let st = self.state();
                for run in st.running.values() {
                    run.stop(STOP_QUOTA);
                }
            } else {
                self.schedule();
            }
            self.emit(Event::Quota(status));
        }
        let mut saved = self.quota_saved.lock().unwrap();
        if saved.elapsed() >= Duration::from_secs(10) {
            *saved = Instant::now();
            if let Some(json) = self.quota.take_dirty() {
                let _ = self.store.lock().unwrap().save_value("quota", &json);
            }
        }
    }

    fn quota_notice(&self) -> String {
        let q = self.settings.read().unwrap().quota;
        let at = self
            .quota
            .status(&q)
            .resumes_at
            .and_then(|ms| jiff::Timestamp::from_millisecond(ms).ok())
            .map(|t| {
                t.to_zoned_datetime(scheduler::local_tz())
                    .strftime("%H:%M")
                    .to_string()
            });
        match at {
            Some(at) => format!("Waiting for the data quota; continues at {at}."),
            None => "Waiting for the data quota.".into(),
        }
    }

    /// Runs the user's command for a finished download, one at a time.
    async fn after_download(self: Arc<Self>, id: i64) {
        let cfg = self.settings().after_download;
        if !cfg.enabled || cfg.command.is_empty() {
            return;
        }
        let Some((path, url, size, category, filename)) =
            self.state().records.get(&id).and_then(|r| {
                Some((
                    r.dir.as_ref()?.join(r.filename.as_ref()?),
                    r.url.clone(),
                    r.total_size,
                    r.category,
                    r.filename.clone(),
                ))
            })
        else {
            return;
        };
        if !cfg.categories.is_empty() && !cfg.categories.contains(&category) {
            return;
        }
        let words = postrun::split(&cfg.command).map(|w| {
            postrun::fill(
                &w,
                &postrun::Vars {
                    path: &path,
                    url: &url,
                    size,
                },
            )
        });
        let _turn = self.command_turn.lock().await;
        if self.quit.is_cancelled() {
            return;
        }
        let started = CommandResult {
            running: true,
            started_at: now_ms(),
            ..Default::default()
        };
        self.set_command(id, started, None);
        let result = match words {
            Ok(w) => postrun::run(w, &self.quit).await,
            Err(e) => CommandResult {
                error: Some(e),
                started_at: now_ms(),
                finished_at: Some(now_ms()),
                ..Default::default()
            },
        };
        let notice = (!result.ok()).then(|| match (&result.error, result.exit_code) {
            (Some(e), _) => format!("The command after the download didn't run: {e}"),
            (None, Some(code)) => format!("The command after the download exited with {code}."),
            (None, None) => "The command after the download failed.".into(),
        });
        self.set_command(id, result.clone(), notice);
        self.emit(Event::CommandDone {
            id,
            filename,
            result,
        });
    }

    fn set_command(&self, id: i64, result: CommandResult, notice: Option<String>) {
        let mut st = self.state();
        if let Some(r) = st.records.get_mut(&id) {
            r.extras.command = Some(result);
            if notice.is_some() {
                r.notice = notice;
            }
            let _ = self.persist_and_emit(r, None);
        }
    }

    /// Starts scheduled site grabs whose time came.
    fn run_grab_schedules(self: &Arc<Self>, last: jiff::Timestamp, now: jiff::Timestamp) {
        crate::grabber::run_schedules(self, last, now);
    }

    pub(crate) fn store(&self) -> MutexGuard<'_, Store> {
        self.store.lock().unwrap()
    }
}

/// A media download about to be added.
struct MediaInsert {
    link: crate::LinkSource,
    url: Url,
    final_url: Url,
    dir: PathBuf,
    wanted: String,
    on_conflict: OnConflict,
    queue_id: Option<i64>,
    start_paused: bool,
    meta: RequestMeta,
    kind: JobKind,
    media: MediaRecord,
}

/// What `finish` needs to know about the transfer.
struct Finish {
    dir: PathBuf,
    name: String,
    part: PathBuf,
    size: u64,
    /// Damaged pieces may be fetched again (only once).
    repair: bool,
}

enum Finished {
    Published,
    Stopped,
    /// These byte ranges are damaged: fetch them again.
    Repair(Vec<(u64, u64)>),
}

/// Segments for a complete file of `size` bytes in which `bad` ranges must be fetched again.
fn repair_segments(size: u64, bad: &[(u64, u64)]) -> Vec<Segment> {
    let mut bad = bad.to_vec();
    bad.sort();
    let mut out = Vec::new();
    let mut at = 0;
    for (a, b) in bad {
        if a > at {
            out.push(Segment {
                start: at,
                end: a,
                pos: a,
            });
        }
        out.push(Segment::new(a, b));
        at = b;
    }
    if at < size {
        out.push(Segment {
            start: at,
            end: size,
            pos: size,
        });
    }
    out
}

/// Refuses an address on this computer or the local network (for addresses that came from a
/// file or a server rather than from the user).
async fn public_address(url: &str) -> std::result::Result<(), String> {
    let u = Url::parse(url).map_err(|e| e.to_string())?;
    match u.scheme() {
        // The media policy only knows the web; FTP and SFTP mirrors get the same address check.
        "ftp" | "ftps" | "sftp" if u.host_str().is_some() => {}
        _ => siler_media::policy::check(&u, false).map_err(|e| e.to_string())?,
    }
    let host = u
        .host_str()
        .unwrap_or("")
        .trim_matches(['[', ']'])
        .to_string();
    if let Ok(ip) = host.parse::<std::net::IpAddr>() {
        return if siler_media::policy::is_private(ip) {
            Err("It points to this computer or the local network".into())
        } else {
            Ok(())
        };
    }
    let port = u.port_or_known_default().unwrap_or(match u.scheme() {
        "sftp" => 22,
        "ftps" => 21,
        _ => 80,
    });
    let addrs = tokio::net::lookup_host((host.as_str(), port))
        .await
        .map_err(|e| format!("Its name can't be found: {e}"))?;
    if addrs
        .into_iter()
        .any(|a| siler_media::policy::is_private(a.ip()))
    {
        return Err("It points to this computer or the local network".into());
    }
    Ok(())
}

/// The sources of a download with mirrors, with what each delivered (saved totals plus the
/// run in progress). Empty for a download without mirrors.
fn source_views(r: &Record, run: Option<&Running>) -> Vec<crate::SourceView> {
    if r.extras.mirrors.is_empty() {
        return Vec::new();
    }
    let live: Vec<Arc<Source>> = run
        .map(|r| r.sources.lock().unwrap().clone())
        .unwrap_or_default();
    let find = |i: usize| live.iter().find(|s| s.index == i);
    let mut out = vec![crate::SourceView {
        url: r.url.clone(),
        origin: MirrorOrigin::User,
        disabled: find(0).and_then(|s| s.disabled.lock().unwrap().clone()),
        bytes: r.extras.own_bytes + find(0).map_or(0, |s| s.bytes.load(Relaxed)),
        active: find(0).map_or(0, |s| s.active.load(Relaxed)),
    }];
    for (i, m) in r.extras.mirrors.iter().enumerate() {
        let s = find(i + 1);
        out.push(crate::SourceView {
            url: m.url.clone(),
            origin: m.origin,
            disabled: s
                .and_then(|s| s.disabled.lock().unwrap().clone())
                .or_else(|| m.disabled.clone()),
            bytes: m.bytes + s.map_or(0, |s| s.bytes.load(Relaxed)),
            active: s.map_or(0, |s| s.active.load(Relaxed)),
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::share_connections;

    #[test]
    fn sharing_connections() {
        assert_eq!(share_connections(6, &[8, 8]), vec![3, 3]);
        assert_eq!(share_connections(7, &[8, 8]).iter().sum::<usize>(), 7);
        assert_eq!(share_connections(10, &[8, 2]), vec![8, 2]);
        assert_eq!(share_connections(9, &[8, 2, 8]), vec![4, 2, 3]);
        assert_eq!(share_connections(2, &[4, 4, 4]), vec![1, 1, 1]);
        assert_eq!(share_connections(0, &[4]), vec![usize::MAX]);
        assert_eq!(share_connections(16, &[]), Vec::<usize>::new());
    }

    #[test]
    fn repairing_only_damaged_pieces() {
        use super::{Segment, repair_segments};
        let segs = repair_segments(100, &[(40, 60), (0, 10)]);
        assert_eq!(
            segs,
            vec![
                Segment::new(0, 10),
                Segment {
                    start: 10,
                    end: 40,
                    pos: 40
                },
                Segment::new(40, 60),
                Segment {
                    start: 60,
                    end: 100,
                    pos: 100
                },
            ]
        );
    }
}
