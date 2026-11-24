use serde::{Deserialize, Serialize};

/// A byte range `[start, end)` of the target file, of which `[start, pos)` is on disk.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Segment {
    pub start: u64,
    pub end: u64,
    pub pos: u64,
}

impl Segment {
    pub fn new(start: u64, end: u64) -> Self {
        Segment {
            start,
            end,
            pos: start,
        }
    }
    pub fn remaining(&self) -> u64 {
        self.end.saturating_sub(self.pos)
    }
    pub fn done(&self) -> bool {
        self.pos >= self.end
    }
    pub fn downloaded(&self) -> u64 {
        self.pos - self.start
    }
}

/// What the connection working on a segment is doing, as the progress window shows it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConnState {
    /// No connection is working on it.
    #[default]
    Idle,
    /// Held back until its server lets another request through.
    Waiting,
    /// Connecting and sending the request, until the server's answer arrives.
    Connecting,
    Receiving,
    /// Failed; tried again after a pause.
    Retrying,
    Finished,
}

/// Splits `total` bytes into at most `conns` equal segments of at least `min_size` bytes.
pub(crate) fn initial(total: u64, conns: usize, min_size: u64) -> Vec<Segment> {
    if total == 0 {
        return vec![Segment::new(0, 0)];
    }
    let n = (total / min_size.max(1)).clamp(1, conns.max(1) as u64);
    let base = total / n;
    (0..n)
        .map(|i| {
            let start = i * base;
            let end = if i == n - 1 { total } else { start + base };
            Segment::new(start, end)
        })
        .collect()
}

/// Segments plus which of them currently have a worker attached, and what it is doing.
#[derive(Debug, Default)]
pub(crate) struct Table {
    pub segs: Vec<Segment>,
    pub busy: Vec<bool>,
    pub conns: Vec<ConnState>,
    /// Where the write in flight on each segment ends (0 = none). The write itself happens
    /// without the table's lock, so a split must leave these bytes to the segment.
    writing: Vec<u64>,
}

impl Table {
    pub fn new(segs: Vec<Segment>) -> Self {
        let busy = vec![false; segs.len()];
        let conns = segs.iter().map(Self::resting).collect();
        let writing = vec![0; segs.len()];
        Table {
            segs,
            busy,
            conns,
            writing,
        }
    }

    /// Claims up to `len` bytes at segment `i`'s position for a write made after the lock
    /// is let go: returns where they go and how many (0 once the segment is complete).
    pub fn reserve(&mut self, i: usize, len: u64) -> (u64, usize) {
        let seg = self.segs[i];
        let n = len.min(seg.remaining());
        self.writing[i] = if n > 0 { seg.pos + n } else { 0 };
        (seg.pos, n as usize)
    }

    /// The reserved bytes of segment `i` are on disk.
    pub fn commit(&mut self, i: usize, n: u64) {
        self.segs[i].pos += n;
        self.writing[i] = 0;
    }

    /// The reserved write of segment `i` failed.
    pub fn unreserve(&mut self, i: usize) {
        self.writing[i] = 0;
    }

    fn resting(seg: &Segment) -> ConnState {
        if seg.done() {
            ConnState::Finished
        } else {
            ConnState::Idle
        }
    }

    /// The segment's worker let go of it.
    pub fn release(&mut self, i: usize) {
        self.busy[i] = false;
        self.writing[i] = 0;
        self.conns[i] = Self::resting(&self.segs[i]);
    }

    /// Claims the unfinished, unclaimed segment with the most bytes left.
    pub fn take_free(&mut self) -> Option<usize> {
        let i = (0..self.segs.len())
            .filter(|&i| !self.busy[i] && !self.segs[i].done())
            .max_by_key(|&i| self.segs[i].remaining())?;
        self.busy[i] = true;
        self.conns[i] = ConnState::Waiting;
        Some(i)
    }

    /// Halves the claimed segment with the most bytes left and claims the tail half as a
    /// new segment. Refuses when that would create pieces smaller than `min`.
    pub fn split_largest(&mut self, min: u64) -> Option<usize> {
        let i = (0..self.segs.len())
            .filter(|&i| self.busy[i])
            .max_by_key(|&i| self.segs[i].remaining())?;
        let seg = self.segs[i];
        // Bytes being written stay with the segment.
        let from = seg.pos.max(self.writing[i]);
        let rem = seg.end.saturating_sub(from);
        if rem < 2 * min.max(1) {
            return None;
        }
        let mid = from + rem / 2;
        self.segs[i].end = mid;
        self.segs.push(Segment::new(mid, seg.end));
        self.busy.push(true);
        self.conns.push(ConnState::Waiting);
        self.writing.push(0);
        Some(self.segs.len() - 1)
    }

    pub fn all_done(&self) -> bool {
        self.segs.iter().all(Segment::done)
    }

    pub fn downloaded(&self) -> u64 {
        self.segs.iter().map(Segment::downloaded).sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initial_layout() {
        let s = initial(100, 4, 10);
        assert_eq!(s.len(), 4);
        assert_eq!(s[0], Segment::new(0, 25));
        assert_eq!(s[3], Segment::new(75, 100));
        assert_eq!(initial(15, 8, 10).len(), 1, "never below min size");
        assert_eq!(initial(0, 8, 10), vec![Segment::new(0, 0)]);
        let s = initial(103, 4, 1);
        assert_eq!(s.last().unwrap().end, 103);
    }

    #[test]
    fn split_takes_tail_of_largest() {
        let mut t = Table::new(vec![Segment::new(0, 100), Segment::new(100, 120)]);
        assert_eq!(t.take_free(), Some(0));
        assert_eq!(t.take_free(), Some(1));
        assert_eq!(t.take_free(), None);
        t.segs[0].pos = 20;
        let n = t.split_largest(10).unwrap();
        assert_eq!(t.segs[0].end, 60);
        assert_eq!(t.segs[n], Segment::new(60, 100));
        assert!(t.busy[n]);
        assert_eq!(t.conns.len(), t.segs.len());
        // remaining 40/40/20: next split halves a 40 segment; then pieces < 2*min are refused.
        assert!(t.split_largest(10).is_some());
        assert!(t.split_largest(25).is_none());
    }

    #[test]
    fn split_leaves_a_write_in_flight_alone() {
        let mut t = Table::new(vec![Segment::new(0, 100)]);
        assert_eq!(t.take_free(), Some(0));
        assert_eq!(t.reserve(0, 60), (0, 60));
        // Split while the 60 bytes are being written: only the 40 after them are halved.
        let n = t.split_largest(10).unwrap();
        assert_eq!(t.segs[0].end, 80);
        assert_eq!(t.segs[n], Segment::new(80, 100));
        t.commit(0, 60);
        assert_eq!(t.segs[0].pos, 60);
        assert!(!t.segs[0].done());
        // A reservation never runs past the (shortened) end.
        assert_eq!(t.reserve(0, 50), (60, 20));
        t.commit(0, 20);
        assert!(t.segs[0].done());
        assert_eq!(t.reserve(0, 50), (80, 0));
        // Too little left past a write to split.
        let mut t = Table::new(vec![Segment::new(0, 100)]);
        t.take_free();
        t.reserve(0, 90);
        assert!(t.split_largest(10).is_none());
    }
}
