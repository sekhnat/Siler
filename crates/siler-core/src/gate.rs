//! What a server said about being busy, shared by every download from it.
//!
//! A 429 or 503 used to teach only the download that got it. Here it reaches the others:
//! a server that refuses Siler outright ("come back in 60 s") holds every new request to it
//! until then, and a server that refuses one connection too many lowers the connection
//! limit all of its downloads share. Servers that aren't complaining are never held.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::oneshot;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

/// One server, by `host_key`.
struct HostGate {
    /// Nothing new is sent to the server before this.
    cooldown_until: Mutex<Option<Instant>>,
    slots: Mutex<Slots>,
}

/// Requests in progress against what the server takes, and who waits for a turn.
struct Slots {
    live: usize,
    /// How many requests at once the server took before refusing; `usize::MAX` = no limit
    /// learned.
    cap: usize,
    /// First come, first served.
    queue: VecDeque<(u64, oneshot::Sender<()>)>,
    next: u64,
}

impl Slots {
    /// Hands free turns to the waiters, in order.
    fn grant(&mut self) {
        while self.live < self.cap {
            let Some((_, tx)) = self.queue.pop_front() else {
                return;
            };
            if tx.send(()).is_ok() {
                self.live += 1;
            }
        }
    }
}

impl HostGate {
    fn new() -> HostGate {
        HostGate {
            cooldown_until: Mutex::new(None),
            slots: Mutex::new(Slots {
                live: 0,
                cap: usize::MAX,
                queue: VecDeque::new(),
                next: 0,
            }),
        }
    }

    fn cooling(&self, now: Instant) -> Option<Instant> {
        self.cooldown_until.lock().unwrap().filter(|&t| t > now)
    }

    /// A turn: at once when one is free and nobody waits, else in order. None when
    /// `cancel` fires first.
    async fn turn(self: &Arc<Self>, cancel: &CancellationToken) -> Option<Pass> {
        let (id, rx) = {
            let mut s = self.slots.lock().unwrap();
            if s.queue.is_empty() && s.live < s.cap {
                s.live += 1;
                return Some(Pass(self.clone()));
            }
            let (tx, rx) = oneshot::channel();
            let id = s.next;
            s.next += 1;
            s.queue.push_back((id, tx));
            (id, rx)
        };
        tokio::select! {
            r = rx => r.ok().map(|()| Pass(self.clone())),
            _ = cancel.cancelled() => {
                let mut s = self.slots.lock().unwrap();
                let before = s.queue.len();
                s.queue.retain(|(i, _)| *i != id);
                if s.queue.len() == before {
                    // Granted as it was cancelled: pass the turn on.
                    s.live -= 1;
                    s.grant();
                }
                None
            }
        }
    }
}

/// Held while a request to the server is in progress.
pub(crate) struct Pass(Arc<HostGate>);

impl Drop for Pass {
    fn drop(&mut self) {
        let mut s = self.0.slots.lock().unwrap();
        s.live -= 1;
        s.grant();
    }
}

#[derive(Default)]
pub(crate) struct HostGates {
    gates: Mutex<HashMap<String, Arc<HostGate>>>,
    /// Servers whose limit changed, for the engine to share out again.
    dirty: Mutex<HashSet<String>>,
}

impl HostGates {
    fn gate(&self, host: &str) -> Arc<HostGate> {
        self.gates
            .lock()
            .unwrap()
            .entry(host.to_string())
            .or_insert_with(|| Arc::new(HostGate::new()))
            .clone()
    }

    /// Waits out the server's cooldown and for a turn within what it takes, then counts a
    /// request to it until the pass is dropped. None when `cancel` fires first.
    pub async fn enter(&self, host: &str, cancel: &CancellationToken) -> Option<Pass> {
        let gate = self.gate(host);
        loop {
            // A later refusal can push the cooldown further while this one waits.
            while let Some(until) = gate.cooling(Instant::now()) {
                tokio::select! {
                    _ = tokio::time::sleep_until(until) => {}
                    _ = cancel.cancelled() => return None,
                }
            }
            if cancel.is_cancelled() {
                return None;
            }
            let pass = gate.turn(cancel).await?;
            if gate.cooling(Instant::now()).is_none() {
                return Some(pass);
            }
        }
    }

    /// Requests to the server in progress, from every download.
    pub fn live(&self, host: &str) -> usize {
        self.gates
            .lock()
            .unwrap()
            .get(host)
            .map_or(0, |g| g.slots.lock().unwrap().live)
    }

    /// The server refused Siler: nothing new goes to it for `wait`. An earlier, longer
    /// cooldown stays: a short wait after a long one must not let everything through early.
    pub fn throttled(&self, host: &str, wait: Duration) {
        let gate = self.gate(host);
        let until = Instant::now() + wait;
        let mut cooldown = gate.cooldown_until.lock().unwrap();
        if cooldown.is_none_or(|t| t < until) {
            *cooldown = Some(until);
            tracing::info!(
                host,
                wait_s = wait.as_secs_f32(),
                "server busy: its downloads wait"
            );
        }
    }

    /// The server refused one request too many: its downloads share what it took then,
    /// the requests still in progress (at least one).
    pub fn lower_cap(&self, host: &str) {
        let gate = self.gate(host);
        let mut s = gate.slots.lock().unwrap();
        let n = s.live.max(1);
        if n < s.cap {
            s.cap = n;
            tracing::info!(
                host,
                cap = n,
                "server limits connections: its downloads share them"
            );
            self.dirty.lock().unwrap().insert(host.to_string());
        }
    }

    /// The learned limit for a server (`usize::MAX` = none).
    pub fn cap(&self, host: &str) -> usize {
        self.gates
            .lock()
            .unwrap()
            .get(host)
            .map_or(usize::MAX, |g| g.slots.lock().unwrap().cap)
    }

    /// Forgets a server's learned limit: its last download left, or the user asked for a
    /// different number of connections. A cooldown stays until it ends.
    pub fn reset_cap(&self, host: &str) {
        if let Some(g) = self.gates.lock().unwrap().get(host) {
            let mut s = g.slots.lock().unwrap();
            s.cap = usize::MAX;
            s.grant();
        }
    }

    /// The servers whose limit changed since the last call.
    pub fn take_dirty(&self) -> Vec<String> {
        self.dirty.lock().unwrap().drain().collect()
    }

    /// Drops what's known about servers that no download uses, once their cooldown is over
    /// and nothing is sent to them.
    pub fn prune(&self, in_use: impl Fn(&str) -> bool) {
        let now = Instant::now();
        self.gates.lock().unwrap().retain(|host, g| {
            let s = g.slots.lock().unwrap();
            in_use(host) || s.live > 0 || !s.queue.is_empty() || g.cooling(now).is_some()
        });
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.gates.lock().unwrap().len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(start_paused = true)]
    async fn waits_for_the_cooldown_only_on_its_server() {
        let gates = HostGates::default();
        let cancel = CancellationToken::new();
        gates.throttled("a.example", Duration::from_secs(5));
        let start = Instant::now();
        drop(gates.enter("b.example", &cancel).await.unwrap());
        assert_eq!(start.elapsed(), Duration::ZERO);
        drop(gates.enter("a.example", &cancel).await.unwrap());
        assert!(start.elapsed() >= Duration::from_secs(5));
    }

    #[tokio::test(start_paused = true)]
    async fn a_shorter_wait_never_shortens_the_cooldown() {
        let gates = HostGates::default();
        let cancel = CancellationToken::new();
        gates.throttled("a.example", Duration::from_secs(60));
        gates.throttled("a.example", Duration::from_secs(1));
        let start = Instant::now();
        drop(gates.enter("a.example", &cancel).await.unwrap());
        assert!(start.elapsed() >= Duration::from_secs(60));
    }

    #[tokio::test(start_paused = true)]
    async fn a_longer_wait_during_the_wait_is_honoured() {
        let gates = Arc::new(HostGates::default());
        let cancel = CancellationToken::new();
        gates.throttled("a.example", Duration::from_secs(2));
        let g = gates.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(1)).await;
            g.throttled("a.example", Duration::from_secs(10));
        });
        let start = Instant::now();
        drop(gates.enter("a.example", &cancel).await.unwrap());
        assert!(start.elapsed() >= Duration::from_secs(11));
    }

    #[tokio::test(start_paused = true)]
    async fn cancelling_stops_the_wait() {
        let gates = HostGates::default();
        let cancel = CancellationToken::new();
        gates.throttled("a.example", Duration::from_secs(300));
        let c = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(100)).await;
            c.cancel();
        });
        let start = Instant::now();
        assert!(gates.enter("a.example", &cancel).await.is_none());
        assert!(start.elapsed() < Duration::from_secs(1));
    }

    #[tokio::test]
    async fn the_cap_is_what_was_in_progress() {
        let gates = HostGates::default();
        let cancel = CancellationToken::new();
        let passes: Vec<_> = enter_n(&gates, &cancel, 3).await;
        assert_eq!(gates.cap("a.example"), usize::MAX);
        gates.lower_cap("a.example");
        assert_eq!(gates.cap("a.example"), 3);
        assert_eq!(gates.take_dirty(), vec!["a.example".to_string()]);
        assert!(gates.take_dirty().is_empty());
        drop(passes);
        // Only ever lowered, and never below one.
        gates.lower_cap("a.example");
        assert_eq!(gates.cap("a.example"), 1);
        gates.reset_cap("a.example");
        assert_eq!(gates.cap("a.example"), usize::MAX);
    }

    async fn enter_n(gates: &HostGates, cancel: &CancellationToken, n: usize) -> Vec<Pass> {
        let mut v = Vec::new();
        for _ in 0..n {
            v.push(gates.enter("a.example", cancel).await.unwrap());
        }
        v
    }

    #[tokio::test]
    async fn within_a_learned_limit_turns_go_in_order() {
        let gates = Arc::new(HostGates::default());
        let cancel = CancellationToken::new();
        let first = gates.enter("a.example", &cancel).await.unwrap();
        gates.lower_cap("a.example");
        assert_eq!(gates.cap("a.example"), 1);
        let order = Arc::new(Mutex::new(Vec::new()));
        let mut waiting = Vec::new();
        for i in 0..3 {
            let (g, c, o) = (gates.clone(), cancel.clone(), order.clone());
            waiting.push(tokio::spawn(async move {
                let pass = g.enter("a.example", &c).await.unwrap();
                o.lock().unwrap().push(i);
                tokio::task::yield_now().await;
                drop(pass);
            }));
            // Queued in this order.
            while gates.gate("a.example").slots.lock().unwrap().queue.len() < i + 1 {
                tokio::task::yield_now().await;
            }
        }
        drop(first);
        for w in waiting {
            w.await.unwrap();
        }
        assert_eq!(*order.lock().unwrap(), vec![0, 1, 2]);
        assert_eq!(gates.live("a.example"), 0);
    }

    #[tokio::test]
    async fn a_cancelled_waiter_gives_up_its_place() {
        let gates = Arc::new(HostGates::default());
        let first = gates
            .enter("a.example", &CancellationToken::new())
            .await
            .unwrap();
        gates.lower_cap("a.example");
        let cancel = CancellationToken::new();
        let (g, c) = (gates.clone(), cancel.clone());
        let waiter = tokio::spawn(async move { g.enter("a.example", &c).await.is_none() });
        while gates
            .gate("a.example")
            .slots
            .lock()
            .unwrap()
            .queue
            .is_empty()
        {
            tokio::task::yield_now().await;
        }
        cancel.cancel();
        assert!(waiter.await.unwrap());
        assert!(
            gates
                .gate("a.example")
                .slots
                .lock()
                .unwrap()
                .queue
                .is_empty()
        );
        drop(first);
        assert_eq!(gates.live("a.example"), 0);
        // The turn is free again.
        drop(
            gates
                .enter("a.example", &CancellationToken::new())
                .await
                .unwrap(),
        );
    }

    #[tokio::test(start_paused = true)]
    async fn unused_servers_are_forgotten_after_their_cooldown() {
        let gates = HostGates::default();
        let cancel = CancellationToken::new();
        for i in 0..100 {
            drop(
                gates
                    .enter(&format!("h{i}.example"), &cancel)
                    .await
                    .unwrap(),
            );
        }
        gates.throttled("busy.example", Duration::from_secs(5));
        let held = gates.enter("held.example", &cancel).await.unwrap();
        gates.prune(|h| h == "used.example");
        assert_eq!(
            gates.len(),
            2,
            "the cooling and the in-progress server stay"
        );
        tokio::time::sleep(Duration::from_secs(6)).await;
        drop(held);
        gates.prune(|_| false);
        assert_eq!(gates.len(), 0);
    }
}
