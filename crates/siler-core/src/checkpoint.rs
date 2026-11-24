//! A running download's progress checkpoints: what the database says is downloaded, so a
//! resume can skip it.
//!
//! The rule: a checkpoint never claims more than the mode promises is on disk. By default
//! (Performance) that is what was written, which survives Siler crashing but not the computer
//! losing power. In Durable mode ("Extra-safe resume") the partial file is flushed to disk
//! first, and only the progress taken *before* the flush is saved; a flush that fails saves
//! nothing. Flushing and saving wait for the disk, so they run off the async runtime, one
//! checkpoint at a time.

use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::sync::{Arc, Mutex};
use tokio::task::JoinHandle;

use crate::Traffic;
use crate::segment::Segment;

/// Progress as it stood at one moment.
pub(crate) struct Snapshot {
    pub downloaded: u64,
    pub segments: Vec<Segment>,
    pub traffic: Traffic,
}

pub(crate) struct Checkpoints {
    durable: bool,
    /// `downloaded` of the last saved checkpoint, to skip saving the same again.
    saved: Arc<AtomicU64>,
    /// The Durable flush and save in progress.
    job: Mutex<Option<JoinHandle<()>>>,
}

impl Checkpoints {
    pub fn new(durable: bool) -> Checkpoints {
        Checkpoints {
            durable,
            saved: Arc::new(AtomicU64::new(u64::MAX)),
            job: Mutex::new(None),
        }
    }

    /// Saves `snap` with `persist`; in Durable mode only after `sync` has put the file on
    /// disk. `snap` must be taken before this is called. Skipped when nothing changed, or
    /// while the previous checkpoint is still being written.
    pub fn save(
        &self,
        snap: Snapshot,
        sync: impl FnOnce() -> std::io::Result<()> + Send + 'static,
        persist: impl FnOnce(&Snapshot) + Send + 'static,
    ) {
        if snap.downloaded == self.saved.load(Relaxed) {
            return;
        }
        let mut job = self.job.lock().unwrap();
        if job.as_ref().is_some_and(|j| !j.is_finished()) {
            return;
        }
        let saved = self.saved.clone();
        let durable = self.durable;
        // Even without the flush, the database write can wait on a busy disk: it too runs
        // off the async runtime, so the transfer and the windows don't wait with it.
        *job = Some(tokio::task::spawn_blocking(move || {
            if durable && let Err(e) = sync() {
                tracing::warn!(%e, "couldn't flush the partial file; progress not saved");
                return;
            }
            persist(&snap);
            saved.store(snap.downloaded, Relaxed);
        }));
    }

    /// Waits for a checkpoint still being written, so that it can't land after what
    /// the download saves next (a pause, a failure, or the finished download).
    pub async fn settle(&self) {
        let job = self.job.lock().unwrap().take();
        if let Some(job) = job {
            let _ = job.await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicBool;

    type Trace = Arc<Mutex<Vec<String>>>;

    fn snap(downloaded: u64) -> Snapshot {
        Snapshot {
            downloaded,
            segments: vec![Segment::new(0, 100)],
            traffic: Traffic::default(),
        }
    }

    fn save(c: &Checkpoints, trace: &Trace, downloaded: u64, sync_ok: bool) {
        let (t1, t2) = (trace.clone(), trace.clone());
        c.save(
            snap(downloaded),
            move || {
                t1.lock().unwrap().push("sync".into());
                if sync_ok {
                    Ok(())
                } else {
                    Err(std::io::Error::other("disk gone"))
                }
            },
            move |s| t2.lock().unwrap().push(format!("persist {}", s.downloaded)),
        );
    }

    #[tokio::test]
    async fn durable_flushes_before_saving() {
        let c = Checkpoints::new(true);
        let trace = Trace::default();
        save(&c, &trace, 10, true);
        c.settle().await;
        assert_eq!(*trace.lock().unwrap(), ["sync", "persist 10"]);
    }

    #[tokio::test]
    async fn a_failed_flush_saves_nothing() {
        let c = Checkpoints::new(true);
        let trace = Trace::default();
        save(&c, &trace, 10, false);
        c.settle().await;
        assert_eq!(*trace.lock().unwrap(), ["sync"]);
        // And the same progress is tried again next time.
        save(&c, &trace, 10, true);
        c.settle().await;
        assert_eq!(*trace.lock().unwrap(), ["sync", "sync", "persist 10"]);
    }

    #[tokio::test]
    async fn performance_saves_without_flushing() {
        let c = Checkpoints::new(false);
        let trace = Trace::default();
        save(&c, &trace, 10, true);
        c.settle().await;
        assert_eq!(*trace.lock().unwrap(), ["persist 10"]);
    }

    #[tokio::test]
    async fn unchanged_progress_is_not_saved_again() {
        for durable in [false, true] {
            let c = Checkpoints::new(durable);
            let trace = Trace::default();
            save(&c, &trace, 10, true);
            c.settle().await;
            let n = trace.lock().unwrap().len();
            save(&c, &trace, 10, true);
            c.settle().await;
            assert_eq!(trace.lock().unwrap().len(), n, "durable: {durable}");
        }
    }

    /// What is saved is what was downloaded when the snapshot was taken, not what the file
    /// held by the time the flush finished.
    #[tokio::test]
    async fn saves_the_progress_from_before_the_flush() {
        let c = Checkpoints::new(true);
        let downloaded = Arc::new(AtomicU64::new(10));
        let got = Arc::new(AtomicU64::new(0));
        let (d, g) = (downloaded.clone(), got.clone());
        c.save(
            snap(downloaded.load(Relaxed)),
            move || {
                // More arrives while the disk is busy.
                d.store(20, Relaxed);
                Ok(())
            },
            move |s| g.store(s.downloaded, Relaxed),
        );
        c.settle().await;
        assert_eq!(got.load(Relaxed), 10);
    }

    #[tokio::test]
    async fn one_flush_at_a_time() {
        let c = Checkpoints::new(true);
        let release = Arc::new(AtomicBool::new(false));
        let r = release.clone();
        c.save(
            snap(10),
            move || {
                while !r.load(Relaxed) {
                    std::thread::sleep(std::time::Duration::from_millis(1));
                }
                Ok(())
            },
            |_| {},
        );
        let trace = Trace::default();
        // Still flushing: this one is skipped, not queued.
        save(&c, &trace, 20, true);
        release.store(true, Relaxed);
        c.settle().await;
        assert!(trace.lock().unwrap().is_empty());
    }
}
