use reqwest::{Client, Url};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering::Relaxed;
use std::sync::{Arc, Mutex, MutexGuard, RwLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::runtime::Handle;
use tokio::sync::broadcast;

use crate::probe::{self, ProbeResult};
use crate::ratelimit::RateLimiter;
use crate::segment::{self, Segment, Table};
use crate::store::{Record, Store};
use crate::task::{self, Limits, Outcome, Running, STOP_PAUSE, STOP_SHUTDOWN, Transfer};
use crate::{
    Category, DownloadInfo, EditDownload, Error, Event, MAX_BODY, NewDownload, OnConflict,
    ProgressItem, QueueMove, RequestMeta, Result, Settings, Status, filename,
};

/// The download engine. Cheap to clone; all clones share one queue.
#[derive(Clone)]
pub struct Engine {
    inner: Arc<Inner>,
}

struct Inner {
    store: Mutex<Store>,
    client: Client,
    settings: RwLock<Settings>,
    global: Arc<RateLimiter>,
    state: Mutex<State>,
    events: broadcast::Sender<Event>,
    rt: Handle,
}

#[derive(Default)]
struct State {
    records: BTreeMap<i64, Record>,
    running: HashMap<i64, Arc<Running>>,
    /// "Start now": these start even when `max_concurrent` downloads are running.
    forced: HashSet<i64>,
    /// Downloads whose files are being moved; they don't start until that's done.
    moving: HashSet<i64>,
    shutting_down: bool,
    idle_reported: bool,
}

fn now_ms() -> i64 {
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

fn human_size(bytes: u64) -> String {
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
}

/// Renames, or copies and deletes when the target is on another filesystem.
fn move_file(from: &Path, to: &Path) -> std::io::Result<()> {
    if let Some(parent) = to.parent() {
        std::fs::create_dir_all(parent)?;
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

fn visible_segments(segs: &[Segment]) -> Vec<Segment> {
    if segs.iter().any(|s| s.end == u64::MAX) {
        Vec::new()
    } else {
        segs.to_vec()
    }
}

impl Engine {
    /// Opens (or creates) the engine database in `data_dir` and starts any queued
    /// downloads. Must be called from within a Tokio runtime.
    pub fn open(data_dir: impl AsRef<Path>) -> Result<Engine> {
        use std::os::unix::fs::PermissionsExt;
        let dir = data_dir.as_ref();
        std::fs::create_dir_all(dir)?;
        let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
        let rt = Handle::try_current().map_err(|_| {
            Error::Protocol("Engine::open must be called inside a Tokio runtime".into())
        })?;

        let store = Store::open(&dir.join("kstar.db"))?;
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

        let client = Client::builder()
            .connect_timeout(Duration::from_secs(20))
            .read_timeout(Duration::from_secs(60))
            .pool_idle_timeout(Duration::from_secs(30))
            .build()
            .map_err(|e| Error::Network(e.to_string()))?;

        let inner = Arc::new(Inner {
            store: Mutex::new(store),
            client,
            global: Arc::new(RateLimiter::new(settings.global_speed_limit)),
            settings: RwLock::new(settings),
            state: Mutex::new(State {
                records,
                ..Default::default()
            }),
            events: broadcast::channel(1024).0,
            rt,
        });

        let weak = Arc::downgrade(&inner);
        inner.rt.spawn(async move {
            let mut iv = tokio::time::interval(Duration::from_millis(250));
            loop {
                iv.tick().await;
                match weak.upgrade() {
                    Some(inner) => inner.tick(),
                    None => break,
                }
            }
        });

        inner.schedule();
        Ok(Engine { inner })
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
        let mut p = probe::probe(&self.inner.client, url, meta, &s.user_agent).await?;
        p.category = s.category_for(&p.filename);
        p.dir = s.category_dir(p.category);
        (p.exists, p.suggested) = self.name_status(&p.dir, &p.filename);
        Ok(p)
    }

    /// Whether `dir/name` is taken (on disk or by another download), and a free alternative.
    pub fn name_status(&self, dir: &Path, name: &str) -> (bool, String) {
        let st = self.inner.state();
        let taken = |n: &str| name_claimed(&st, dir, n, None);
        let exists = taken(name)
            || dir.join(name).exists()
            || filename::part_path(dir, name).exists();
        (exists, filename::unique_name(dir, name, taken))
    }

    pub fn add(&self, nd: NewDownload) -> Result<i64> {
        let url = probe::parse_url(&nd.url)?;
        let s = self.settings();
        if nd.meta.body.as_ref().is_some_and(|b| b.len() > MAX_BODY) {
            return Err(Error::Protocol(
                "The form data is too large to send again".into(),
            ));
        }
        let filename = nd.filename.as_deref().and_then(filename::sanitize);
        let category = nd
            .category
            .or_else(|| filename.as_deref().map(|n| s.category_for(n)))
            .unwrap_or_default();
        let on_conflict = match nd.on_conflict.unwrap_or(s.on_conflict) {
            OnConflict::Ask => OnConflict::Rename,
            p => p,
        };
        let mut st = self.inner.state();
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
            meta: nd.meta,
            connections: nd.connections.unwrap_or(s.connections).clamp(1, 32),
            speed_limit: 0,
            created_at: now_ms(),
            completed_at: None,
            segments: Vec::new(),
            position,
            on_conflict,
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
        st.forced.remove(&id);
        let rec = st.records.get_mut(&id).ok_or(Error::NotFound)?;
        if matches!(rec.status, Status::Queued | Status::Waiting) {
            rec.status = Status::Paused;
            self.inner.persist_and_emit(rec, None)?;
        }
        Ok(())
    }

    pub fn resume(&self, id: i64) -> Result<()> {
        {
            let mut st = self.inner.state();
            if st.running.contains_key(&id) {
                return Ok(());
            }
            let rec = st.records.get_mut(&id).ok_or(Error::NotFound)?;
            if matches!(rec.status, Status::Paused | Status::Failed | Status::Waiting) {
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

    /// Releases every "Download later" item to the queue.
    pub fn start_queue(&self) -> Result<()> {
        self.set_status_where(|s| s == Status::Waiting, Status::Queued)?;
        self.inner.schedule();
        Ok(())
    }

    /// Holds back queued downloads that haven't started yet.
    pub fn stop_queue(&self) -> Result<()> {
        self.set_status_where(|s| s == Status::Queued, Status::Waiting)
    }

    fn set_status_where(&self, pred: impl Fn(Status) -> bool, to: Status) -> Result<()> {
        let mut st = self.inner.state();
        let State {
            records, running, ..
        } = &mut *st;
        for rec in records.values_mut() {
            if pred(rec.status) && !running.contains_key(&rec.id) {
                rec.status = to;
                self.inner.persist_and_emit(rec, None)?;
            }
        }
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
            let rec = st.records.get_mut(&id).ok_or(Error::NotFound)?;
            if let (Some(dir), Some(name)) = (&rec.dir, &rec.filename) {
                let _ = std::fs::remove_file(filename::part_path(dir, name));
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
        let url = probe::parse_url(url)?;
        let (old_size, old_meta) = {
            let st = self.inner.state();
            let rec = st.records.get(&id).ok_or(Error::NotFound)?;
            if rec.status == Status::Completed {
                return Err(Error::Protocol("This download has already finished".into()));
            }
            (rec.total_size, rec.meta.clone())
        };
        let meta = meta.unwrap_or(old_meta);
        let s = self.settings();
        // Probe first, so a bad link doesn't interrupt a running download.
        let p = probe::probe(&self.inner.client, url.as_str(), &meta, &s.user_agent).await?;
        match (old_size, p.total_size) {
            (Some(a), Some(b)) if a != b => {
                return Err(Error::LinkMismatch(format!(
                    "The new link points to a different file ({} instead of {}).",
                    human_size(b),
                    human_size(a)
                )));
            }
            (Some(_), None) => {
                return Err(Error::LinkMismatch(
                    "The new link doesn't report the file size, so K-Star can't tell whether it is the same file."
                        .into(),
                ));
            }
            _ => {}
        }
        self.stop_and_wait(id).await;
        {
            let mut st = self.inner.state();
            let rec = st.records.get_mut(&id).ok_or(Error::NotFound)?;
            rec.url = url.to_string();
            rec.final_url = Some(p.final_url);
            rec.meta = meta;
            rec.etag = p.etag;
            rec.last_modified = p.last_modified;
            rec.error = None;
            rec.notice = Some("The download link was updated.".into());
            rec.status = Status::Queued;
            self.inner.persist_and_emit(rec, None)?;
        }
        self.inner.schedule();
        Ok(())
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
        let run = running.get(&id);
        if let Some(run) = run {
            run.set_target(connections);
        }
        self.inner.persist_and_emit(rec, run.map(|a| a.as_ref()))
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
                && let (Some(od), Some(on), Some(nd), Some(nn)) = (&rec.dir, &rec.filename, &dir, &name)
            {
                let completed = rec.status == Status::Completed;
                let (from, to) = if completed {
                    (od.join(on), nd.join(nn))
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
                        let _ = std::fs::remove_file(filename::part_path(dir, name));
                    }
                } else if delete_files {
                    let _ = std::fs::remove_file(dir.join(name));
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

    /// The finished file, or the partial file while the download is unfinished.
    pub fn file_path(&self, id: i64) -> Option<PathBuf> {
        let st = self.inner.state();
        let r = st.records.get(&id)?;
        let (dir, name) = (r.dir.as_ref()?, r.filename.as_ref()?);
        Some(if r.status == Status::Completed {
            dir.join(name)
        } else {
            filename::part_path(dir, name)
        })
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
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline && !self.inner.state().running.is_empty() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
}

/// True if another unfinished download already intends to write `dir/name`.
fn name_claimed(st: &State, dir: &Path, name: &str, except: Option<i64>) -> bool {
    st.records.values().any(|r| {
        Some(r.id) != except
            && r.status != Status::Completed
            && r.dir.as_deref() == Some(dir)
            && r.filename.as_deref() == Some(name)
    })
}

impl Inner {
    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap()
    }

    fn settings(&self) -> Settings {
        self.settings.read().unwrap().clone()
    }

    fn emit(&self, ev: Event) {
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
                    if table.segs.is_empty() {
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
            file_missing,
            speed,
            active_connections: active,
            segments: visible_segments(&segments),
        }
    }

    fn persist_and_emit(&self, rec: &Record, run: Option<&Running>) -> Result<()> {
        self.store.lock().unwrap().update(rec)?;
        self.emit(Event::Upsert {
            download: Box::new(self.info(rec, run)),
        });
        Ok(())
    }

    fn tick(&self) {
        let mut guard = self.state();
        if guard.running.is_empty() {
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
                }
            })
            .collect();
        drop(guard);
        self.emit(Event::Progress { items, total_speed });
    }

    /// Starts queued downloads while there are free slots.
    fn schedule(self: &Arc<Self>) {
        let max = self.settings.read().unwrap().max_concurrent;
        let mut st = self.state();
        if st.shutting_down {
            return;
        }
        let State {
            records,
            running,
            forced,
            moving,
            ..
        } = &mut *st;
        // "Start now" first, then by queue position.
        let mut queued: Vec<(bool, i64, i64)> = records
            .values()
            .filter(|r| {
                r.status == Status::Queued && !running.contains_key(&r.id) && !moving.contains(&r.id)
            })
            .map(|r| (!forced.contains(&r.id), r.position, r.id))
            .collect();
        queued.sort();
        for (unforced, _, id) in queued {
            if unforced && running.len() >= max {
                break;
            }
            forced.remove(&id);
            let rec = records.get_mut(&id).expect("queued id exists");
            rec.status = Status::Probing;
            rec.error = None;
            let run = Arc::new(Running::new(rec.speed_limit, rec.connections));
            running.insert(id, run.clone());
            if let Err(e) = self.persist_and_emit(rec, Some(&run)) {
                tracing::warn!("failed to persist download {id}: {e}");
            }
            let me = self.clone();
            self.rt.spawn(async move { me.run(id, run).await });
        }
    }

    async fn run(self: Arc<Self>, id: i64, run: Arc<Running>) {
        let result = self.drive(id, &run).await;
        let reason = run.stop_reason.load(Relaxed);
        let mut failed = None;
        let removed;
        {
            let mut st = self.state();
            st.running.remove(&id);
            removed = !st.records.contains_key(&id);
            if let Some(rec) = st.records.get_mut(&id)
                && !matches!(result, Ok(Outcome::Done))
            {
                {
                    let table = run.table.lock().unwrap();
                    if !table.segs.is_empty() {
                        rec.segments = table.segs.clone();
                        rec.downloaded = table.downloaded();
                    }
                    drop(table);
                    match (&result, reason) {
                        (_, STOP_SHUTDOWN) => {
                            // Keep the "running" status on disk so it resumes on next start.
                            let _ = self.store.lock().unwrap().update_progress(
                                id,
                                rec.downloaded,
                                &rec.segments,
                            );
                        }
                        (Err(e), 0) => {
                            tracing::info!("download {id} failed: {e}");
                            rec.status = Status::Failed;
                            rec.error = Some(e.to_string());
                            failed = Some(Event::Failed {
                                id,
                                filename: rec.filename.clone(),
                                error: e.to_string(),
                            });
                            let _ = self.persist_and_emit(rec, None);
                        }
                        _ => {
                            rec.status = Status::Paused;
                            let _ = self.persist_and_emit(rec, None);
                        }
                    }
                }
            }
        }
        if removed && let Some(part) = run.part.lock().unwrap().take() {
            let _ = std::fs::remove_file(part);
        }
        run.finished.cancel();
        if let Some(ev) = failed {
            self.emit(ev);
        }
        if reason != STOP_SHUTDOWN {
            self.schedule();
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
                r = probe::probe(&self.client, &rec.url, &rec.meta, &s.user_agent) => r,
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

        let Some(probe) = self.probe_with_retries(id, run, &rec, &s).await? else {
            return Ok(Outcome::Stopped);
        };

        let mut segments = rec.segments.clone();
        let mut notice: Option<&str> = None;
        if !segments.is_empty() {
            let changed = rec.total_size != probe.total_size
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

        let ranged = probe.resumable && probe.total_size.is_some();
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
                    _ => filename::unique_name(&dir, &name, |n| {
                        name_claimed(&st, &dir, n, Some(id))
                    }),
                }
            };
            let part = filename::part_path(&dir, &name);
            let file = OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .open(&part)?;
            *run.part.lock().unwrap() = Some(part.clone());
            if segments.is_empty() {
                file.set_len(0)?;
                if let Some(total) = probe.total_size.filter(|&t| t > 0) {
                    preallocate(&file, total)?;
                }
                segments = match probe.total_size {
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
            r.total_size = probe.total_size;
            r.resumable = Some(probe.resumable);
            r.etag = probe.etag.clone();
            r.last_modified = probe.last_modified.clone();
            r.status = Status::Downloading;
            r.error = None;
            r.notice = notice.map(String::from);
            r.segments = segments;
            r.downloaded = downloaded;
            self.persist_and_emit(r, Some(run))?;
            (name, part, file)
        };

        let url = Url::parse(&probe.final_url).map_err(|e| Error::InvalidUrl(e.to_string()))?;
        let t = Arc::new(Transfer {
            client: self.client.clone(),
            url,
            meta: rec.meta.clone(),
            user_agent: s.user_agent.clone(),
            file,
            run: run.clone(),
            global: self.global.clone(),
            ranged,
        });
        let checkpoint = || {
            if run.ranged.load(Relaxed) {
                let segs = run.table.lock().unwrap().segs.clone();
                let _ = self.store.lock().unwrap().update_progress(
                    id,
                    run.downloaded.load(Relaxed),
                    &segs,
                );
            }
        };
        let limits = Limits {
            min_split: s.min_split_size,
            max_retries: s.max_retries,
        };

        match task::transfer(t.clone(), limits, &checkpoint).await? {
            Outcome::Stopped => {
                let _ = t.file.sync_data();
                Ok(Outcome::Stopped)
            }
            Outcome::Done => {
                let size = probe
                    .total_size
                    .unwrap_or_else(|| run.downloaded.load(Relaxed));
                t.file.set_len(size)?;
                t.file.sync_all()?;
                drop(t);

                let mut st = self.state();
                let Some(on_conflict) = st.records.get(&id).map(|r| r.on_conflict) else {
                    // Removed in the last moment; run() deletes the part file.
                    return Ok(Outcome::Stopped);
                };
                let final_name =
                    if dir.join(&name).exists() && on_conflict != OnConflict::Overwrite {
                        filename::unique_name(&dir, &name, |n| {
                            name_claimed(&st, &dir, n, Some(id))
                        })
                    } else {
                        name
                    };
                let path = dir.join(&final_name);
                std::fs::rename(&part, &path)?;
                run.part.lock().unwrap().take();
                let r = st.records.get_mut(&id).ok_or(Error::NotFound)?;
                r.filename = Some(final_name.clone());
                r.status = Status::Completed;
                r.completed_at = Some(now_ms());
                r.segments.clear();
                r.downloaded = size;
                r.total_size = Some(size);
                self.persist_and_emit(r, None)?;
                drop(st);
                self.emit(Event::Completed {
                    id,
                    filename: final_name,
                    path,
                });
                Ok(Outcome::Done)
            }
        }
    }
}
