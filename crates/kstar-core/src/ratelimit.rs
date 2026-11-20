use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// Token-bucket limiter shared by all connections it governs. A rate of 0 means unlimited.
///
/// Callers take tokens first and then sleep off any debt, so concurrent callers queue up
/// fairly and the long-run rate converges on the configured value.
pub(crate) struct RateLimiter {
    rate: AtomicU64,
    bucket: Mutex<Bucket>,
}

struct Bucket {
    tokens: f64,
    last: Instant,
}

impl RateLimiter {
    pub fn new(bytes_per_sec: u64) -> Self {
        RateLimiter {
            rate: AtomicU64::new(bytes_per_sec),
            bucket: Mutex::new(Bucket {
                tokens: 0.0,
                last: Instant::now(),
            }),
        }
    }

    pub fn set_rate(&self, bytes_per_sec: u64) {
        let mut b = self.bucket.lock().unwrap();
        b.tokens = 0.0;
        b.last = Instant::now();
        self.rate.store(bytes_per_sec, Ordering::Relaxed);
    }

    pub async fn acquire(&self, n: u64) {
        let rate = self.rate.load(Ordering::Relaxed);
        if rate == 0 {
            return;
        }
        let wait = {
            let mut b = self.bucket.lock().unwrap();
            let now = Instant::now();
            let rate_f = rate as f64;
            // Allow a small burst (~100 ms worth) so chunky reads don't stall needlessly.
            let burst = (rate_f * 0.1).max(16_384.0);
            b.tokens = (b.tokens + now.duration_since(b.last).as_secs_f64() * rate_f).min(burst);
            b.last = now;
            b.tokens -= n as f64;
            (b.tokens < 0.0).then(|| Duration::from_secs_f64(-b.tokens / rate_f))
        };
        if let Some(w) = wait {
            tokio::time::sleep(w).await;
        }
    }
}
