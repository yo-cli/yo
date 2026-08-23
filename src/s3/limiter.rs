// Byte-level async token bucket, shared by all concurrent uploads.
//
// Design notes:
// - Waiting is always `tokio::time::sleep` — never spinning, never blocking.
// - Large acquires (a 256 MiB part) are split into 16 MiB slices internally so
//   the bucket capacity stays small and mid-flight rate updates take effect
//   at fine granularity.
// - Capacity is ~1 second of quota (clamped), so a long idle can burst at most
//   1s worth of bytes; the long-term average equals the integral of the
//   sampled rate and always lands inside [min, max].
// - Sleeps are capped at 200 ms so a rate change never waits behind a stale
//   long sleep computed from the old rate.

use rand::Rng;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use tokio::sync::Mutex;
use tokio::time::Instant;

use super::{GIB, MIB};

const ACQUIRE_SLICE: u64 = 16 * MIB;
const MAX_SLEEP: Duration = Duration::from_millis(200);
const BURST_SECS: f64 = 1.0;
const MAX_BURST_BYTES: f64 = GIB as f64;

struct Bucket {
    tokens: f64,
    last_refill: Instant,
}

pub struct RateLimiter {
    state: Mutex<Bucket>,
    /// Target rate in bytes/sec. Written by the sampler, read by acquirers.
    rate: AtomicU64,
}

impl RateLimiter {
    pub fn new(initial_rate: u64) -> Self {
        Self {
            state: Mutex::new(Bucket {
                tokens: 0.0,
                last_refill: Instant::now(),
            }),
            rate: AtomicU64::new(initial_rate.max(1)),
        }
    }

    pub fn set_rate(&self, bytes_per_sec: u64) {
        self.rate.store(bytes_per_sec.max(1), Ordering::Relaxed);
    }

    pub fn rate(&self) -> u64 {
        self.rate.load(Ordering::Relaxed)
    }

    /// Sample a fresh rate uniformly from [min, max] and apply it.
    pub fn resample(&self, min: u64, max: u64) -> u64 {
        let r = if min >= max {
            min
        } else {
            rand::rng().random_range(min..=max)
        };
        self.set_rate(r);
        r
    }

    /// Acquire `bytes` tokens, sleeping as needed. Cancellation-safe: dropping
    /// the future mid-wait leaves no tokens consumed for the unfinished slice.
    pub async fn acquire(&self, bytes: u64) {
        let mut left = bytes;
        while left > 0 {
            let take = left.min(ACQUIRE_SLICE);
            self.acquire_slice(take).await;
            left -= take;
        }
    }

    async fn acquire_slice(&self, n: u64) {
        loop {
            let wait = {
                let mut st = self.state.lock().await;
                let rate = self.rate.load(Ordering::Relaxed).max(1) as f64;
                // Capacity must be ≥ n or a slice larger than capacity could
                // never accumulate enough tokens and would wait forever.
                let cap = (rate * BURST_SECS).min(MAX_BURST_BYTES).max(n as f64);
                let now = Instant::now();
                let elapsed = now.duration_since(st.last_refill).as_secs_f64();
                st.tokens = (st.tokens + elapsed * rate).min(cap);
                st.last_refill = now;
                if st.tokens >= n as f64 {
                    st.tokens -= n as f64;
                    None
                } else {
                    let deficit = n as f64 - st.tokens;
                    Some(Duration::from_secs_f64(deficit / rate))
                }
            };
            match wait {
                None => return,
                Some(w) => tokio::time::sleep(w.min(MAX_SLEEP)).await,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::rngs::SmallRng;
    use rand::SeedableRng;
    use std::sync::Arc;

    // All tests run under tokio's paused clock: virtual time only advances via
    // sleeps, so hours of wall time simulate instantly — and the tests passing
    // at all proves the limiter never spins (a spin would hang a paused clock).

    #[tokio::test(start_paused = true)]
    async fn fixed_rate_long_term_average_is_exact() {
        let limiter = RateLimiter::new(100 * MIB);
        let start = Instant::now();
        let window = Duration::from_secs(300);
        let mut total: u64 = 0;
        while start.elapsed() < window {
            limiter.acquire(8 * MIB).await;
            total += 8 * MIB;
        }
        let avg = total as f64 / start.elapsed().as_secs_f64();
        let target = (100 * MIB) as f64;
        // Burst capacity adds at most ~1s of quota over a 300s window (≈0.3%)
        assert!(
            (avg - target).abs() / target < 0.05,
            "avg {} vs target {}",
            avg,
            target
        );
    }

    #[tokio::test(start_paused = true)]
    async fn resampled_rate_average_stays_within_bounds() {
        let min = 50 * MIB;
        let max = 150 * MIB;
        let limiter = Arc::new(RateLimiter::new(min));
        let sampler = {
            let limiter = limiter.clone();
            tokio::spawn(async move {
                let mut rng = SmallRng::seed_from_u64(42);
                loop {
                    tokio::time::sleep(Duration::from_secs(10)).await;
                    let r = rng.random_range(min..=max);
                    limiter.set_rate(r);
                }
            })
        };
        let start = Instant::now();
        let window = Duration::from_secs(600);
        let mut total: u64 = 0;
        while start.elapsed() < window {
            limiter.acquire(16 * MIB).await;
            total += 16 * MIB;
        }
        sampler.abort();
        let avg = total as f64 / start.elapsed().as_secs_f64();
        assert!(
            avg >= min as f64 * 0.95 && avg <= max as f64 * 1.05,
            "long-term avg {} escaped [{}, {}]",
            avg,
            min,
            max
        );
    }

    #[tokio::test(start_paused = true)]
    async fn large_acquire_exceeding_capacity_completes() {
        // 256 MiB acquire at 100 MiB/s with a ~100 MiB bucket: must finish in
        // ~2.56 virtual seconds instead of deadlocking on capacity.
        let limiter = RateLimiter::new(100 * MIB);
        let start = Instant::now();
        limiter.acquire(256 * MIB).await;
        let secs = start.elapsed().as_secs_f64();
        assert!(secs > 2.0 && secs < 3.5, "took {}s", secs);
    }

    #[tokio::test(start_paused = true)]
    async fn rate_update_takes_effect_mid_acquire() {
        let limiter = Arc::new(RateLimiter::new(10 * MIB));
        let task = {
            let limiter = limiter.clone();
            tokio::spawn(async move {
                let start = Instant::now();
                // 100 MiB at 10 MiB/s would take ~10s
                limiter.acquire(100 * MIB).await;
                start.elapsed()
            })
        };
        tokio::time::sleep(Duration::from_secs(1)).await;
        limiter.set_rate(200 * MIB);
        let took = task.await.unwrap();
        // ~1s at old rate + ~0.45s at new rate (+200ms sleep granularity)
        assert!(took < Duration::from_secs(3), "took {:?}", took);
    }
}
