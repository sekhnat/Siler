//! The data quota: at most so many megabytes in each period of so many hours (IDM's "download
//! quota"). Every byte passes through the global rate limiter, which counts them; the engine
//! reads that count a few times a second, and when the period's allowance is used up it stops
//! the running downloads (they go back to the queue) until the next period starts.

use serde::{Deserialize, Serialize};
use std::sync::Mutex;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct QuotaSettings {
    pub enabled: bool,
    /// Megabytes (MiB, as sizes are shown everywhere in K-Star) per period.
    pub megabytes: u64,
    /// Length of a period.
    pub hours: u32,
    /// Tests only: a period of this many seconds instead of `hours`.
    #[serde(skip)]
    pub period_secs: Option<u64>,
}

impl Default for QuotaSettings {
    fn default() -> Self {
        QuotaSettings {
            enabled: false,
            megabytes: 1000,
            hours: 1,
            period_secs: None,
        }
    }
}

impl QuotaSettings {
    pub fn limit(&self) -> u64 {
        self.megabytes.saturating_mul(1024 * 1024)
    }

    pub fn period_ms(&self) -> i64 {
        let secs = self
            .period_secs
            .unwrap_or(u64::from(self.hours.max(1)) * 3600);
        i64::try_from(secs.saturating_mul(1000)).unwrap_or(i64::MAX)
    }

    pub fn normalized(mut self) -> Self {
        self.megabytes = self.megabytes.max(1);
        self.hours = self.hours.clamp(1, 24 * 31);
        self
    }
}

/// Where the quota stands, for the UI.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct QuotaStatus {
    pub enabled: bool,
    /// Downloads are held until `resumes_at`.
    pub reached: bool,
    /// The user lifted the limit for this period.
    pub ignored: bool,
    pub used: u64,
    pub limit: u64,
    /// When the current period ends (ms since the epoch).
    pub resumes_at: Option<i64>,
}

/// The current period, saved so a restart doesn't hand out a fresh allowance.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct Period {
    pub start: i64,
    pub used: u64,
    pub ignored: bool,
}

pub(crate) struct Quota {
    inner: Mutex<State>,
}

struct State {
    period: Period,
    /// The limiter's byte count at the last update.
    last_total: Option<u64>,
    reached: bool,
    /// Changed since it was last saved.
    dirty: bool,
}

impl Quota {
    pub fn new(saved: Option<&str>) -> Quota {
        let period = saved
            .and_then(|s| serde_json::from_str(s).ok())
            .unwrap_or_default();
        Quota {
            inner: Mutex::new(State {
                period,
                last_total: None,
                reached: false,
                dirty: false,
            }),
        }
    }

    pub fn reached(&self) -> bool {
        self.inner.lock().unwrap().reached
    }

    fn status_of(st: &State, s: &QuotaSettings) -> QuotaStatus {
        QuotaStatus {
            enabled: s.enabled,
            reached: st.reached,
            ignored: st.period.ignored,
            used: st.period.used,
            limit: s.limit(),
            resumes_at: (st.period.start > 0)
                .then(|| st.period.start.saturating_add(s.period_ms())),
        }
    }

    /// Bytes left in this period, or None when nothing limits them (off, or lifted).
    pub fn remaining(&self, s: &QuotaSettings) -> Option<u64> {
        let st = self.inner.lock().unwrap();
        (s.enabled && !st.period.ignored).then(|| s.limit().saturating_sub(st.period.used))
    }

    pub fn status(&self, s: &QuotaSettings) -> QuotaStatus {
        Self::status_of(&self.inner.lock().unwrap(), s)
    }

    /// Counts the bytes the limiter saw since the last call (`total` is its running count).
    /// Returns the new status when downloads must stop or may go on.
    pub fn update(&self, s: &QuotaSettings, total: u64, now: i64) -> Option<QuotaStatus> {
        let mut st = self.inner.lock().unwrap();
        let delta = st.last_total.map_or(0, |l| total.saturating_sub(l));
        st.last_total = Some(total);
        if !s.enabled {
            if st.reached {
                st.reached = false;
                return Some(Self::status_of(&st, s));
            }
            return None;
        }
        let mut changed = false;
        if st.period.start == 0 || now >= st.period.start.saturating_add(s.period_ms()) {
            // A new period starts with the first bytes after the last one ended.
            if delta > 0 || st.reached || st.period.start != 0 {
                st.period = Period {
                    start: if delta > 0 || st.reached { now } else { 0 },
                    used: 0,
                    ignored: false,
                };
                st.dirty = true;
                if st.reached {
                    st.reached = false;
                    changed = true;
                }
            }
        }
        if delta > 0 {
            if st.period.start == 0 {
                st.period.start = now;
            }
            st.period.used = st.period.used.saturating_add(delta);
            st.dirty = true;
        }
        let over = !st.period.ignored && st.period.used >= s.limit();
        if over != st.reached {
            st.reached = over;
            changed = true;
        }
        changed.then(|| Self::status_of(&st, s))
    }

    /// Lifts the limit until the period ends.
    pub fn ignore(&self, s: &QuotaSettings) -> QuotaStatus {
        let mut st = self.inner.lock().unwrap();
        st.period.ignored = true;
        st.reached = false;
        st.dirty = true;
        Self::status_of(&st, s)
    }

    /// The period to save, when it changed since the last call.
    pub fn take_dirty(&self) -> Option<String> {
        let mut st = self.inner.lock().unwrap();
        if !st.dirty {
            return None;
        }
        st.dirty = false;
        serde_json::to_string(&st.period).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings() -> QuotaSettings {
        QuotaSettings {
            enabled: true,
            megabytes: 1,
            hours: 1,
            period_secs: Some(60),
        }
    }

    #[test]
    fn stops_at_the_limit_and_resumes_next_period() {
        let s = settings();
        let q = Quota::new(None);
        assert_eq!(q.update(&s, 0, 1_000), None);
        assert_eq!(q.update(&s, 600_000, 2_000), None);
        let st = q.update(&s, 1_100_000, 3_000).unwrap();
        assert!(st.reached);
        assert_eq!(st.used, 1_100_000);
        assert_eq!(st.resumes_at, Some(2_000 + 60_000));
        assert!(q.reached());
        // Still inside the period: nothing changes.
        assert_eq!(q.update(&s, 1_100_000, 30_000), None);
        // The period is over.
        let st = q.update(&s, 1_100_000, 62_001).unwrap();
        assert!(!st.reached);
        assert_eq!(st.used, 0);
        assert!(!q.reached());
    }

    #[test]
    fn ignoring_lifts_it_for_the_period() {
        let s = settings();
        let q = Quota::new(None);
        q.update(&s, 0, 1);
        assert!(q.update(&s, 2_000_000, 2).unwrap().reached);
        assert!(!q.ignore(&s).reached);
        assert_eq!(q.update(&s, 3_000_000, 3), None);
        assert!(!q.reached());
    }

    #[test]
    fn a_saved_period_is_continued() {
        let s = settings();
        let saved = serde_json::to_string(&Period {
            start: 1_000,
            used: 1_045_000,
            ignored: false,
        })
        .unwrap();
        let q = Quota::new(Some(&saved));
        q.update(&s, 50, 2_000);
        assert!(q.update(&s, 5_050, 2_100).unwrap().reached);
        // Turning it off releases the hold.
        let off = QuotaSettings {
            enabled: false,
            ..s
        };
        assert!(!q.update(&off, 5_050, 2_200).unwrap().reached);
    }
}
