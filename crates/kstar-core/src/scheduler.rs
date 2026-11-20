//! When scheduled queues start and stop.
//!
//! A schedule has a start time and an optional stop time on chosen days of the week. A stop
//! time at or before the start time means the window runs past midnight into the next day.
//! The engine acts on *edges* (a window opening or closing), so stopping a queue by hand in
//! the middle of a window sticks until the next start.

use jiff::Timestamp;
use jiff::civil::{Date, DateTime};
use jiff::tz::TimeZone;
use serde::{Deserialize, Serialize};

/// Minutes after midnight.
pub type Minute = u16;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Schedule {
    pub enabled: bool,
    /// Start time (minutes after midnight).
    pub start_at: Option<Minute>,
    /// Stop time; none means the queue runs until it is done.
    pub stop_at: Option<Minute>,
    /// Monday first. Ignored for `once`.
    pub days: [bool; 7],
    /// Runs on the next start time only, then turns itself off.
    pub once: bool,
}

impl Default for Schedule {
    fn default() -> Self {
        Schedule {
            enabled: false,
            start_at: Some(2 * 60),
            stop_at: Some(7 * 60),
            days: [true; 7],
            once: false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Edge {
    Start,
    Stop,
}

impl Schedule {
    pub(crate) fn normalized(mut self) -> Self {
        self.start_at = self.start_at.map(|m| m.min(24 * 60 - 1));
        self.stop_at = self.stop_at.map(|m| m.min(24 * 60 - 1));
        if self.start_at.is_none() {
            self.enabled = false;
        }
        self
    }

    fn runs_on(&self, d: Date) -> bool {
        self.once || self.days[d.weekday().to_monday_zero_offset() as usize]
    }

    /// The window that starts on day `d`, if the schedule runs that day. Without a stop
    /// time the window lasts until midnight (for catching up after a late start).
    fn window(&self, d: Date, tz: &TimeZone) -> Option<(Timestamp, Option<Timestamp>, Timestamp)> {
        let start = self.start_at?;
        if !self.enabled || !self.runs_on(d) {
            return None;
        }
        let at = |day: Date, m: Minute| -> Option<Timestamp> {
            let dt: DateTime = day.at((m / 60) as i8, (m % 60) as i8, 0, 0);
            tz.to_timestamp(dt).ok()
        };
        let next = d.tomorrow().ok()?;
        let s = at(d, start)?;
        let stop = match self.stop_at {
            Some(e) if e > start => Some(at(d, e)?),
            Some(e) => Some(at(next, e)?),
            None => None,
        };
        let end = stop.or_else(|| at(next, 0))?;
        Some((s, stop, end))
    }

    /// Edges in `(after, until]`, oldest first.
    pub fn edges(
        &self,
        tz: &TimeZone,
        after: Timestamp,
        until: Timestamp,
    ) -> Vec<(Timestamp, Edge)> {
        let mut out = Vec::new();
        if until <= after {
            return out;
        }
        let first = tz.to_datetime(after).date().yesterday();
        let last = tz.to_datetime(until).date();
        let Ok(mut d) = first else { return out };
        while d <= last {
            if let Some((s, stop, _)) = self.window(d, tz) {
                if s > after && s <= until {
                    out.push((s, Edge::Start));
                }
                if let Some(e) = stop
                    && e > after
                    && e <= until
                {
                    out.push((e, Edge::Stop));
                }
            }
            match d.tomorrow() {
                Ok(n) => d = n,
                Err(_) => break,
            }
        }
        out.sort_by_key(|e| e.0);
        out
    }

    /// Whether a window is open at `now` (used once at startup to catch up).
    pub fn is_open(&self, tz: &TimeZone, now: Timestamp) -> bool {
        let today = tz.to_datetime(now).date();
        [today.yesterday().ok(), Some(today)]
            .into_iter()
            .flatten()
            .filter_map(|d| self.window(d, tz))
            .any(|(s, _, end)| s <= now && now < end)
    }

    /// A short description such as "22:00–06:00, weekdays".
    pub fn describe(&self) -> String {
        let hm = |m: Minute| format!("{:02}:{:02}", m / 60, m % 60);
        let Some(s) = self.start_at.filter(|_| self.enabled) else {
            return String::new();
        };
        let time = match self.stop_at {
            Some(e) => format!("{}–{}", hm(s), hm(e)),
            None => format!("from {}", hm(s)),
        };
        const NAMES: [&str; 7] = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];
        let days = if self.once {
            "once".to_string()
        } else if self.days == [true; 7] {
            "every day".to_string()
        } else if self.days == [true, true, true, true, true, false, false] {
            "weekdays".to_string()
        } else if self.days == [false, false, false, false, false, true, true] {
            "weekends".to_string()
        } else {
            NAMES
                .iter()
                .zip(self.days)
                .filter(|(_, on)| *on)
                .map(|(n, _)| *n)
                .collect::<Vec<_>>()
                .join(" ")
        };
        format!("{time}, {days}")
    }
}

/// The system time zone (falls back to UTC).
pub(crate) fn local_tz() -> TimeZone {
    TimeZone::system()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tz() -> TimeZone {
        TimeZone::get("Europe/Berlin").unwrap()
    }

    fn ts(s: &str) -> Timestamp {
        let dt: DateTime = s.parse().unwrap();
        tz().to_timestamp(dt).unwrap()
    }

    fn sched(start: Minute, stop: Option<Minute>) -> Schedule {
        Schedule {
            enabled: true,
            start_at: Some(start),
            stop_at: stop,
            days: [true; 7],
            once: false,
        }
    }

    #[test]
    fn edges_in_a_day() {
        let s = sched(22 * 60, Some(23 * 60 + 30));
        let e = s.edges(&tz(), ts("2026-11-20T21:59"), ts("2026-11-20T22:00"));
        assert_eq!(e, vec![(ts("2026-11-20T22:00"), Edge::Start)]);
        let e = s.edges(&tz(), ts("2026-11-20T22:00"), ts("2026-11-20T23:00"));
        assert!(e.is_empty());
        let e = s.edges(&tz(), ts("2026-11-20T21:00"), ts("2026-11-21T00:00"));
        assert_eq!(
            e,
            vec![
                (ts("2026-11-20T22:00"), Edge::Start),
                (ts("2026-11-20T23:30"), Edge::Stop)
            ]
        );
    }

    #[test]
    fn window_past_midnight() {
        let s = sched(23 * 60, Some(6 * 60));
        let e = s.edges(&tz(), ts("2026-11-21T05:00"), ts("2026-11-21T06:30"));
        assert_eq!(e, vec![(ts("2026-11-21T06:00"), Edge::Stop)]);
        assert!(s.is_open(&tz(), ts("2026-11-21T02:00")));
        assert!(!s.is_open(&tz(), ts("2026-11-21T07:00")));
        assert!(s.is_open(&tz(), ts("2026-11-21T23:10")));
    }

    #[test]
    fn days_of_the_week() {
        // 2026-11-20 is a Friday; weekdays only.
        let mut s = sched(9 * 60, Some(10 * 60));
        s.days = [true, true, true, true, true, false, false];
        let e = s.edges(&tz(), ts("2026-11-20T10:30"), ts("2026-11-23T09:30"));
        assert_eq!(e, vec![(ts("2026-11-23T09:00"), Edge::Start)]);
        assert!(!s.is_open(&tz(), ts("2026-11-21T09:30")));
        // The overnight part of Friday's window still belongs to Friday.
        let mut night = sched(23 * 60, Some(2 * 60));
        night.days = [false, false, false, false, true, false, false];
        assert!(night.is_open(&tz(), ts("2026-11-21T01:00")));
        assert!(!night.is_open(&tz(), ts("2026-11-22T01:00")));
    }

    #[test]
    fn without_a_stop_time() {
        let s = sched(20 * 60, None);
        let e = s.edges(&tz(), ts("2026-11-20T19:00"), ts("2026-11-21T19:00"));
        assert_eq!(e, vec![(ts("2026-11-20T20:00"), Edge::Start)]);
        assert!(s.is_open(&tz(), ts("2026-11-20T21:00")));
        assert!(!s.is_open(&tz(), ts("2026-11-21T10:00")));
    }

    #[test]
    fn daylight_saving_changes() {
        // 2026-03-29 02:00 doesn't exist in Berlin (clocks jump to 03:00).
        let s = sched(2 * 60 + 30, Some(4 * 60));
        let e = s.edges(&tz(), ts("2026-03-29T01:00"), ts("2026-03-29T05:00"));
        assert_eq!(e.len(), 2);
        assert_eq!(e[0].1, Edge::Start);
        assert!(e[0].0 < e[1].0);
        // 2026-10-25 02:30 happens twice; it still starts only once.
        let e = s.edges(&tz(), ts("2026-10-25T00:00"), ts("2026-10-25T05:00"));
        assert_eq!(e.iter().filter(|x| x.1 == Edge::Start).count(), 1);
    }

    #[test]
    fn disabled_and_described() {
        let mut s = sched(22 * 60, Some(6 * 60));
        s.days = [true, true, true, true, true, false, false];
        assert_eq!(s.describe(), "22:00–06:00, weekdays");
        s.once = true;
        assert_eq!(s.describe(), "22:00–06:00, once");
        s.enabled = false;
        assert_eq!(s.describe(), "");
        assert!(
            s.edges(&tz(), ts("2026-11-20T00:00"), ts("2026-11-22T00:00"))
                .is_empty()
        );
    }
}
