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
// - The [min, max] the target is sampled from lives HERE rather than in the
//   config, because it is not constant: a run whose target outruns the network
//   clamps its own bounds down into what the link actually delivers.

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

/// Never clamp below this. A run pinned at a few KiB/s is indistinguishable
/// from a hang, and it also stops the clamp from chasing a transient stall
/// all the way to zero.
const RATE_FLOOR: u64 = MIB;

struct Bucket {
    tokens: f64,
    last_refill: Instant,
}

pub struct RateLimiter {
    state: Mutex<Bucket>,
    /// Target rate in bytes/sec. Written by the sampler, read by acquirers.
    rate: AtomicU64,
    /// Bounds the target is sampled from. Lowered by `clamp_to_observed`.
    min: AtomicU64,
    max: AtomicU64,
}

impl RateLimiter {
    pub fn new(min: u64, max: u64) -> Self {
        let limiter = Self {
            state: Mutex::new(Bucket {
                tokens: 0.0,
                last_refill: Instant::now(),
            }),
            rate: AtomicU64::new(min.max(1)),
            min: AtomicU64::new(min.max(1)),
            max: AtomicU64::new(max.max(min).max(1)),
        };
        limiter.resample();
        limiter
    }

    pub fn set_rate(&self, bytes_per_sec: u64) {
        self.rate.store(bytes_per_sec.max(1), Ordering::Relaxed);
    }

    pub fn rate(&self) -> u64 {
        self.rate.load(Ordering::Relaxed)
    }

    /// The bounds currently in force — which is what the run summary should
    /// report, not what the command line asked for.
    pub fn bounds(&self) -> (u64, u64) {
        (
            self.min.load(Ordering::Relaxed),
            self.max.load(Ordering::Relaxed),
        )
    }

    /// Sample a fresh rate uniformly from the current bounds and apply it.
    pub fn resample(&self) -> u64 {
        let (min, max) = self.bounds();
        let r = if min >= max {
            min
        } else {
            rand::rng().random_range(min..=max)
        };
        self.set_rate(r);
        r
    }

    /// Pull the bounds down to what the network actually delivers, and apply a
    /// target from them at once — per-object mode has no resample timer to wait
    /// for, and even in continuous mode the old target would stand for another
    /// full interval.
    ///
    /// Only ever lowers. Raising back after an apparent recovery would just
    /// re-saturate the link and oscillate, and pace is not what the run is
    /// measured on — the budget is. `None` means the bounds already sat at or
    /// below what was observed and nothing moved.
    pub fn clamp_to_observed(&self, observed: u64) -> Option<(u64, u64)> {
        let (min, max) = bounds_for(observed);
        if max >= self.max.load(Ordering::Relaxed) {
            return None;
        }
        self.min.store(min, Ordering::Relaxed);
        self.max.store(max, Ordering::Relaxed);
        self.resample();
        Some((min, max))
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

/// The bounds a measured throughput implies: aim under what the link actually
/// delivered, keeping the jitter spread the tool is built around. Deliberately
/// the same 60%–90% the shortfall warning used to print as advice, so the tool
/// now does what it used to only recommend.
fn bounds_for(observed: u64) -> (u64, u64) {
    let max = (observed / 10 * 9).max(RATE_FLOOR);
    let min = (observed / 5 * 3).max(RATE_FLOOR).min(max);
    (min, max)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    // All tests run under tokio's paused clock: virtual time only advances via
    // sleeps, so hours of wall time simulate instantly — and the tests passing
    // at all proves the limiter never spins (a spin would hang a paused clock).

    #[tokio::test(start_paused = true)]
    async fn fixed_rate_long_term_average_is_exact() {
        let limiter = RateLimiter::new(100 * MIB, 100 * MIB);
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
        let limiter = Arc::new(RateLimiter::new(min, max));
        let sampler = {
            let limiter = limiter.clone();
            tokio::spawn(async move {
                loop {
                    tokio::time::sleep(Duration::from_secs(10)).await;
                    limiter.resample();
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
        let limiter = RateLimiter::new(100 * MIB, 100 * MIB);
        let start = Instant::now();
        limiter.acquire(256 * MIB).await;
        let secs = start.elapsed().as_secs_f64();
        assert!(secs > 2.0 && secs < 3.5, "took {}s", secs);
    }

    #[tokio::test(start_paused = true)]
    async fn rate_update_takes_effect_mid_acquire() {
        let limiter = Arc::new(RateLimiter::new(10 * MIB, 10 * MIB));
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

    /// The failure this exists to prevent: target 466 MiB/s on a link that
    /// delivers 126 MiB/s starves every connection until the SDK kills them as
    /// stalled. One clamp must land the whole range under what was measured.
    #[test]
    fn clamping_lands_the_range_under_the_measurement() {
        let limiter = RateLimiter::new(200 * MIB, 500 * MIB);
        let (min, max) = limiter.clamp_to_observed(126 * MIB).expect("must clamp");
        assert!(max < 126 * MIB, "max {} still above the measurement", max);
        assert!(min < max, "range collapsed: [{}, {}]", min, max);
        assert_eq!(limiter.bounds(), (min, max));
        // Per-object mode never resamples on a timer: the new target has to be
        // live the moment the clamp returns, not one interval later.
        assert!(
            (min..=max).contains(&limiter.rate()),
            "target {} outside the clamped range",
            limiter.rate()
        );
    }

    /// Clamping repeatedly must settle instead of chasing a shrinking number
    /// to zero — every window that still looks starved calls in again.
    #[test]
    fn repeated_clamping_converges() {
        let limiter = RateLimiter::new(200 * MIB, 500 * MIB);
        let mut observed = 126 * MIB;
        let mut clamps = 0;
        for _ in 0..200 {
            match limiter.clamp_to_observed(observed) {
                // Keep feeding back a throughput below the new pace: the worst
                // case, where the link degrades every single window.
                Some((_, max)) => {
                    clamps += 1;
                    observed = max / 2;
                }
                None => break,
            }
        }
        // Neither a single clamp (the loop would prove nothing) nor 200 of them
        // (that is a runaway, not convergence).
        assert!((2..200).contains(&clamps), "{} clamps before settling", clamps);
        let (min, max) = limiter.bounds();
        assert!(min <= max, "range inverted: [{}, {}]", min, max);
        assert!(max >= RATE_FLOOR, "clamped below the floor: {}", max);
        assert_eq!(limiter.clamp_to_observed(0), None, "floor must be terminal");
    }

    /// Only ever downward: raising the bounds back after an apparent recovery
    /// would re-saturate the link and oscillate.
    #[test]
    fn clamping_never_raises_the_bounds() {
        let limiter = RateLimiter::new(50 * MIB, 100 * MIB);
        assert_eq!(limiter.clamp_to_observed(900 * MIB), None);
        assert_eq!(limiter.bounds(), (50 * MIB, 100 * MIB));

        let (_, lowered) = limiter.clamp_to_observed(40 * MIB).expect("must clamp");
        assert_eq!(limiter.clamp_to_observed(900 * MIB), None, "must not rebound");
        assert_eq!(limiter.bounds().1, lowered);
    }

    /// `--rate-min == --rate-max` is a fixed pace; sampling it must not panic
    /// on an empty range, before or after a clamp.
    #[test]
    fn a_degenerate_range_is_a_fixed_rate() {
        let limiter = RateLimiter::new(70 * MIB, 70 * MIB);
        assert_eq!(limiter.resample(), 70 * MIB);
        let (min, max) = limiter.clamp_to_observed(30 * MIB).expect("must clamp");
        assert!(min <= max, "range inverted: [{}, {}]", min, max);
        assert!((min..=max).contains(&limiter.resample()));
    }
}
