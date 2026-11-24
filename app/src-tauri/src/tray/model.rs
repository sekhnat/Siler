//! What the tray shows: totals, and a line for each running download. No D-Bus and no GTK,
//! so it is tested with plain values.

use siler_core::{DownloadInfo, ProgressItem, Status};
use std::collections::HashMap;

use crate::i18n::{tr, tr_args};

/// The part of a download the tray needs between progress events.
#[derive(Debug, Clone)]
struct Kept {
    name: String,
    status: Status,
    position: i64,
    total: Option<u64>,
    downloaded: u64,
    media: bool,
    recording: bool,
    needs_you: bool,
}

impl Kept {
    fn of(d: &DownloadInfo) -> Kept {
        let name = d
            .filename
            .clone()
            .or_else(|| siler_core::filename::name_from_url(&d.url))
            .unwrap_or_else(|| tr("notify-a-download"));
        Kept {
            name,
            status: d.status,
            position: d.position,
            total: d.total_size,
            downloaded: d.downloaded,
            media: d.media.is_some(),
            recording: d.media.as_ref().is_some_and(|m| m.live && !m.finished),
            needs_you: d.needs_login.is_some() || d.needs_host_key.is_some() || d.needs_link,
        }
    }
}

/// What a running download is doing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Doing {
    /// Percent, when the size is known, and seconds left.
    Transfer(Option<u8>, Option<u64>),
    /// Seconds recorded.
    Recording(u64),
    Checking(Option<u8>),
    Unpacking(Option<u8>),
    Building(Option<u8>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Line {
    pub id: i64,
    pub name: String,
    pub speed: u64,
    pub doing: Doing,
    /// A recording that can still be finished.
    pub can_finish: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Model {
    pub lines: Vec<Line>,
    pub speed: u64,
    /// Of everything running whose size is known.
    pub percent: Option<u8>,
    pub waiting: usize,
    pub needs_you: usize,
}

impl Model {
    pub fn recording(&self) -> bool {
        self.lines
            .iter()
            .any(|l| matches!(l.doing, Doing::Recording(_)))
    }

    pub fn ids(&self) -> Vec<i64> {
        self.lines.iter().map(|l| l.id).collect()
    }
}

/// Follows the engine's events and builds a [`Model`] from them.
#[derive(Default)]
pub struct Tracker {
    downloads: HashMap<i64, Kept>,
    progress: HashMap<i64, ProgressItem>,
    speed: u64,
}

impl Tracker {
    pub fn reset(&mut self, list: &[DownloadInfo]) {
        self.downloads = list.iter().map(|d| (d.id, Kept::of(d))).collect();
        self.progress.clear();
        self.speed = 0;
    }

    /// Returns whether anything the tray shows may have changed.
    pub fn apply(&mut self, event: &siler_core::Event) -> bool {
        use siler_core::Event;
        match event {
            Event::Upsert { download } => {
                self.downloads.insert(download.id, Kept::of(download));
            }
            Event::Removed { id } => {
                self.downloads.remove(id);
                self.progress.remove(id);
            }
            Event::Progress { items, total_speed } => {
                self.progress = items.iter().map(|i| (i.id, i.clone())).collect();
                self.speed = *total_speed;
            }
            _ => return false,
        }
        true
    }

    pub fn model(&self) -> Model {
        let mut running: Vec<(&i64, &Kept)> = self
            .downloads
            .iter()
            .filter(|(_, k)| k.status.is_running())
            .collect();
        running.sort_by_key(|(id, k)| (k.position, **id));

        let (mut done_sum, mut total_sum) = (0u64, 0u64);
        let lines: Vec<Line> = running
            .iter()
            .map(|(id, k)| {
                let p = self.progress.get(*id);
                let speed = p.map_or(0, |p| p.speed);
                let done = p.map_or(k.downloaded, |p| p.downloaded);
                let media = p.and_then(|p| p.media.as_ref());
                // A stream's size is what its pieces add up to.
                let total = if k.media {
                    media.and_then(|m| m.bytes_total)
                } else {
                    p.and_then(|p| p.total_size).or(k.total)
                }
                .filter(|t| *t > 0);
                let doing = match (p.and_then(|p| p.phase.as_ref()), media) {
                    (Some(phase), _) if phase.name == "verifying" => {
                        Doing::Checking(percent(phase.done, phase.total))
                    }
                    (Some(phase), _) => Doing::Unpacking(percent(phase.done, phase.total)),
                    (None, Some(m)) => match m.phase {
                        "recording" => Doing::Recording(m.recorded_secs.unwrap_or(0.0) as u64),
                        "rebuilding" => Doing::Building(percent(m.permille.into(), 1000)),
                        "publishing" => Doing::Building(None),
                        _ => {
                            let pct = match total {
                                Some(t) => percent(done, t),
                                None => percent(m.segments_done, m.segments_total),
                            };
                            Doing::Transfer(pct, left(done, total, speed))
                        }
                    },
                    (None, None) => Doing::Transfer(
                        total.and_then(|t| percent(done, t)),
                        left(done, total, speed),
                    ),
                };
                if let Some(t) = total {
                    done_sum += done.min(t);
                    total_sum += t;
                }
                Line {
                    id: **id,
                    name: k.name.clone(),
                    speed,
                    doing,
                    can_finish: k.recording,
                }
            })
            .collect();

        let count = |f: fn(&Kept) -> bool| self.downloads.values().filter(|k| f(k)).count();
        Model {
            speed: if lines.is_empty() { 0 } else { self.speed },
            percent: percent(done_sum, total_sum),
            waiting: count(|k| matches!(k.status, Status::Queued | Status::Waiting)),
            needs_you: count(|k| k.needs_you && k.status != Status::Completed),
            lines,
        }
    }
}

fn percent(done: u64, total: u64) -> Option<u8> {
    (u128::from(done) * 100)
        .checked_div(u128::from(total))
        .map(|p| p.min(100) as u8)
}

fn left(done: u64, total: Option<u64>, speed: u64) -> Option<u64> {
    total?.saturating_sub(done).checked_div(speed)
}

// ---------------------------------------------------------------- words

/// A size as the window writes it (`format.ts`), so the tray and the panel agree.
pub fn bytes(n: u64) -> String {
    let units = ["B", "KB", "MB", "GB", "TB"];
    let (mut v, mut u) = (n as f64, 0);
    while v >= 1024.0 && u < units.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    let text = match (u, v) {
        (0, _) => n.to_string(),
        (_, v) if v >= 100.0 => format!("{v:.0}"),
        (_, v) if v >= 10.0 => format!("{v:.1}"),
        _ => format!("{v:.2}"),
    };
    tr_args(
        "format-bytes",
        vec![("n", text.into()), ("unit", units[u].into())],
    )
}

pub fn rate(n: u64) -> String {
    tr_args("format-per-second", vec![("size", bytes(n).into())])
}

fn duration(secs: u64) -> String {
    match secs {
        0..60 => tr_args("format-duration-s", vec![("s", secs.into())]),
        60..3600 => tr_args(
            "format-duration-ms",
            vec![("m", (secs / 60).into()), ("s", (secs % 60).into())],
        ),
        3600..86400 => tr_args(
            "format-duration-hm",
            vec![
                ("h", (secs / 3600).into()),
                ("m", (secs % 3600 / 60).into()),
            ],
        ),
        _ => tr_args(
            "format-duration-dh",
            vec![
                ("d", (secs / 86400).into()),
                ("h", (secs % 86400 / 3600).into()),
            ],
        ),
    }
}

fn clock(secs: u64) -> String {
    let (h, m, s) = (secs / 3600, secs % 3600 / 60, secs % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

fn bar(pct: u8) -> String {
    let full = usize::from(pct) / 20;
    "▰".repeat(full) + &"▱".repeat(5 - full.min(5))
}

impl Line {
    /// "68% · 12.1 MB/s · 2m 3s left". With `bars`, a text progress bar in front (for the
    /// menu, which has nothing else to draw with).
    pub fn detail(&self, bars: bool) -> String {
        let phase = |id: &str, pct: Option<u8>| match pct {
            Some(p) => format!("{} {p}%", tr(id)),
            None => tr(id),
        };
        let mut parts = Vec::new();
        match self.doing {
            Doing::Transfer(pct, _) => {
                if let Some(p) = pct {
                    parts.push(if bars {
                        format!("{} {p}%", bar(p))
                    } else {
                        format!("{p}%")
                    });
                }
                parts.push(rate(self.speed));
            }
            Doing::Recording(secs) => {
                parts.push(format!("● {}", clock(secs)));
                parts.push(rate(self.speed));
            }
            Doing::Checking(p) => parts.push(phase("tray-checking", p)),
            Doing::Unpacking(p) => parts.push(phase("tray-unpacking", p)),
            Doing::Building(p) => parts.push(phase("tray-building", p)),
        }
        // The menu has no room for the time left beside its bar.
        if !bars && let Doing::Transfer(_, Some(secs)) = self.doing {
            parts.push(tr_args("tray-left", vec![("time", duration(secs).into())]));
        }
        parts.join(" · ")
    }
}

/// One line of plain text, at most `max` characters, cut in the middle so the end (the
/// extension) stays visible.
pub fn short(name: &str, max: usize) -> String {
    let chars: Vec<char> = name
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    if chars.len() <= max {
        return chars.into_iter().collect();
    }
    let tail = (max / 3).max(1);
    let head = max.saturating_sub(tail + 1);
    let mut out: String = chars[..head].iter().collect();
    out.push('…');
    out.extend(&chars[chars.len() - tail..]);
    out
}

/// For tooltips and notification bodies, which desktops show as rich text.
pub fn markup(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            c => out.push(c),
        }
    }
    out
}

/// "2 downloading · 4.86 MB/s · 50% · 1 waiting".
pub fn summary(m: &Model) -> String {
    let mut parts = Vec::new();
    if m.lines.is_empty() {
        parts.push(tr("tray-no-active-downloads"));
    } else {
        parts.push(tr_args(
            "tray-downloading",
            vec![
                ("count", m.lines.len().into()),
                ("speed", bytes(m.speed).into()),
            ],
        ));
        if let Some(p) = m.percent {
            parts.push(format!("{p}%"));
        }
    }
    if m.waiting > 0 {
        parts.push(tr_args("tray-waiting", vec![("count", m.waiting.into())]));
    }
    if m.needs_you > 0 {
        parts.push(tr_args(
            "tray-needs-you",
            vec![("count", m.needs_you.into())],
        ));
    }
    parts.join(" · ")
}

/// How many downloads the tooltip lists.
const IN_TOOLTIP: usize = 6;

/// The tooltip's body, as rich text: the totals, then the downloads.
pub fn tooltip(m: &Model) -> String {
    let mut rows = vec![markup(&summary(m))];
    for l in m.lines.iter().take(IN_TOOLTIP) {
        rows.push(format!(
            "<b>{}</b> {}",
            markup(&short(&l.name, 40)),
            markup(&l.detail(false))
        ));
    }
    if m.lines.len() > IN_TOOLTIP {
        rows.push(markup(&tr_args(
            "tray-and-more",
            vec![("count", (m.lines.len() - IN_TOOLTIP).into())],
        )));
    }
    rows.join("<br>")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kept(name: &str, status: Status, position: i64) -> Kept {
        Kept {
            name: name.into(),
            status,
            position,
            total: None,
            downloaded: 0,
            media: false,
            recording: false,
            needs_you: false,
        }
    }

    fn item(id: i64, downloaded: u64, total_size: Option<u64>, speed: u64) -> ProgressItem {
        ProgressItem {
            id,
            downloaded,
            total_size,
            speed,
            active_connections: 1,
            segments: vec![],
            conns: vec![],
            media: None,
            phase: None,
            source_bytes: vec![],
            traffic: None,
            protocol: None,
        }
    }

    fn two() -> Tracker {
        let mut t = Tracker::default();
        t.downloads.insert(1, kept("b.iso", Status::Downloading, 2));
        t.downloads.insert(2, kept("a.bin", Status::Downloading, 1));
        t.downloads.insert(3, kept("later", Status::Waiting, 3));
        t.apply(&siler_core::Event::Progress {
            items: vec![item(1, 50, Some(100), 5), item(2, 5, None, 1)],
            total_speed: 5_100_000,
        });
        t
    }

    #[test]
    fn totals_in_queue_order() {
        let m = two().model();
        assert_eq!(m.ids(), [2, 1]);
        assert_eq!((m.percent, m.waiting), (Some(50), 1));
        assert_eq!(m.lines[0].doing, Doing::Transfer(None, None));
        assert_eq!(m.lines[1].doing, Doing::Transfer(Some(50), Some(10)));
        assert_eq!(summary(&m), "2 downloading · 4.86 MB/s · 50% · 1 waiting");
        assert_eq!(summary(&Model::default()), "No active downloads");
    }

    #[test]
    fn a_recording_shows_its_time() {
        let mut t = Tracker::default();
        let mut k = kept("live", Status::Downloading, 1);
        (k.media, k.recording) = (true, true);
        t.downloads.insert(1, k);
        let mut i = item(1, 900, None, 2048);
        i.media = Some(siler_core::media::MediaProgress {
            phase: "recording",
            recorded_secs: Some(754.2),
            ..Default::default()
        });
        t.apply(&siler_core::Event::Progress {
            items: vec![i],
            total_speed: 2048,
        });
        let m = t.model();
        assert!(m.recording() && m.lines[0].can_finish);
        assert_eq!(m.percent, None);
        assert_eq!(m.lines[0].detail(true), "● 12:34 · 2.00 KB/s");
    }

    #[test]
    fn names_cant_inject_markup() {
        let mut t = two();
        t.downloads.get_mut(&1).unwrap().name = "<img src=\"http://x\">&\n.iso".into();
        let text = tooltip(&t.model());
        assert!(!text.contains("<img"), "{text}");
        assert!(text.contains("&lt;img") && text.contains("&amp;"), "{text}");
        assert!(!text.contains('\n'));
    }

    #[test]
    fn long_names_keep_their_end() {
        let s = short("a-very-long-name-of-a-file-indeed.tar.zst", 20);
        assert_eq!(s.chars().count(), 20);
        assert!(s.ends_with("ar.zst") || s.ends_with(".zst"), "{s}");
        assert_eq!(short("añ🙂", 3), "añ🙂");
    }

    #[test]
    fn bars() {
        assert_eq!(bar(0), "▱▱▱▱▱");
        assert_eq!(bar(68), "▰▰▰▱▱");
        assert_eq!(bar(100), "▰▰▰▰▰");
    }
}
