// Runtime counters + part latency histogram + the final JSON summary.

use hdrhistogram::Histogram;
use serde::Serialize;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

pub struct Metrics {
    /// Bytes of successfully uploaded parts (drives instant throughput).
    pub bytes_completed: AtomicU64,
    pub parts_ok: AtomicU64,
    pub parts_retried: AtomicU64,
    pub slowdowns: AtomicU64,
    pub errors: AtomicU64,
    pub objects_done: AtomicU64,
    pub objects_aborted: AtomicU64,
    /// Part upload latency in milliseconds.
    latency_ms: Mutex<Histogram<u64>>,
    /// Recently completed object keys, sampled for replication backlog.
    recent_keys: Mutex<VecDeque<String>>,
}

impl Metrics {
    pub fn new() -> Self {
        Self {
            bytes_completed: AtomicU64::new(0),
            parts_ok: AtomicU64::new(0),
            parts_retried: AtomicU64::new(0),
            slowdowns: AtomicU64::new(0),
            errors: AtomicU64::new(0),
            objects_done: AtomicU64::new(0),
            objects_aborted: AtomicU64::new(0),
            // 1 ms .. 1 h, 3 significant digits
            latency_ms: Mutex::new(Histogram::new_with_bounds(1, 3_600_000, 3).unwrap()),
            recent_keys: Mutex::new(VecDeque::with_capacity(32)),
        }
    }

    pub fn record_part(&self, bytes: u64, latency_ms: u64) {
        self.bytes_completed.fetch_add(bytes, Ordering::Relaxed);
        self.parts_ok.fetch_add(1, Ordering::Relaxed);
        let mut h = self.latency_ms.lock().unwrap();
        let _ = h.record(latency_ms.max(1));
    }

    pub fn record_object_done(&self, key: &str) {
        self.objects_done.fetch_add(1, Ordering::Relaxed);
        let mut q = self.recent_keys.lock().unwrap();
        if q.len() >= 32 {
            q.pop_front();
        }
        q.push_back(key.to_string());
    }

    pub fn recent_keys(&self, n: usize) -> Vec<String> {
        let q = self.recent_keys.lock().unwrap();
        q.iter().rev().take(n).cloned().collect()
    }

    pub fn latency_percentiles(&self) -> LatencySummary {
        let h = self.latency_ms.lock().unwrap();
        LatencySummary {
            p50_ms: h.value_at_quantile(0.50),
            p95_ms: h.value_at_quantile(0.95),
            p99_ms: h.value_at_quantile(0.99),
            max_ms: h.max(),
            count: h.len(),
        }
    }
}

impl Default for Metrics {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct LatencySummary {
    pub p50_ms: u64,
    pub p95_ms: u64,
    pub p99_ms: u64,
    pub max_ms: u64,
    pub count: u64,
}

/// Final machine-readable summary, printed and written to --summary-out.
#[derive(Serialize)]
pub struct RunSummary {
    pub run_id: String,
    /// Burn mode that produced this run (see s3::modes).
    pub mode: String,
    pub dry_run: bool,
    pub bucket: String,
    /// Every replication destination; its length is the transfer fee's ×K.
    pub dest_buckets: Vec<String>,
    pub region: String,
    pub started_at: String,
    pub finished_at: String,
    pub active_secs: u64,
    pub stop_reason: String,

    pub budget_usd: f64,
    /// The `--days` ceiling this run was planned against, if any. A multi-day
    /// plan's summary has to say what one day was allowed to cost, or the file
    /// cannot be checked against the daily AWS bill it was sized for.
    pub daily_cap_usd: Option<f64>,
    pub burned_usd: f64,
    pub burned_transfer_usd: f64,
    pub burned_request_usd: f64,
    pub storage_estimate_usd_not_in_budget: f64,

    pub objects_completed: u64,
    pub objects_aborted: u64,
    pub bytes_completed_objects: u64,
    pub bytes_uploaded_parts: u64,
    pub avg_throughput_bytes_per_sec: u64,
    /// The rate bounds in force when the run ended — which is not what was
    /// asked for if the run auto-clamped itself down to the network.
    pub rate_min: u64,
    pub rate_max: u64,

    pub parts_ok: u64,
    pub parts_retried: u64,
    pub slowdown_count: u64,
    pub error_count: u64,
    pub part_latency: LatencySummary,
    pub replication_pending_sampled: Option<u64>,
}
