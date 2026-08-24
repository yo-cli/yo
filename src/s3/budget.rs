// Real-time immediate-cost meter. All money is tracked as integer
// micro-dollars in atomics (no float atomics, no drift from repeated adds).
//
// Budget scope (confirmed with the user): only IMMEDIATE costs count toward
// the stop condition — request fees + the active mode's per-byte transfer fee.
// Storage cost accrues with time after the tool exits and cannot be stopped
// precisely, so it is estimated and displayed but never part of the decision.
//
// The ledger knows nothing about which mode is running: the mode's `CostModel`
// is the only thing that turns bytes into money here.

use std::sync::atomic::{AtomicU64, Ordering};

use super::cost::{CostModel, Pricing};
use super::MIB;

/// Smallest object worth scheduling when shrinking the tail to fit the budget.
const MIN_TAIL_OBJECT: u64 = 8 * MIB;

pub struct BudgetMeter {
    budget_micro: u64,
    pricing: Pricing,
    cost: CostModel,
    part_size: u64,
    /// Burned so far: request fees (on success) + committed transfer fees.
    burned_micro: AtomicU64,
    /// Reserved by objects currently in flight (released on commit/abort).
    reserved_micro: AtomicU64,
    // breakdown for the final report
    request_micro: AtomicU64,
    transfer_micro: AtomicU64,
}

impl BudgetMeter {
    pub fn new(
        budget_micro: u64,
        already_burned_micro: u64,
        pricing: Pricing,
        cost: CostModel,
        part_size: u64,
    ) -> Self {
        Self {
            budget_micro,
            pricing,
            cost,
            part_size,
            burned_micro: AtomicU64::new(already_burned_micro),
            reserved_micro: AtomicU64::new(0),
            request_micro: AtomicU64::new(0),
            transfer_micro: AtomicU64::new(0),
        }
    }

    pub fn pricing(&self) -> &Pricing {
        &self.pricing
    }

    pub fn cost(&self) -> &CostModel {
        &self.cost
    }

    pub fn budget_micro(&self) -> u64 {
        self.budget_micro
    }

    pub fn burned_micro(&self) -> u64 {
        self.burned_micro.load(Ordering::Relaxed)
    }

    pub fn request_micro(&self) -> u64 {
        self.request_micro.load(Ordering::Relaxed)
    }

    pub fn transfer_micro(&self) -> u64 {
        self.transfer_micro.load(Ordering::Relaxed)
    }

    pub fn remaining_micro(&self) -> u64 {
        self.budget_micro
            .saturating_sub(self.burned_micro())
            .saturating_sub(self.reserved_micro.load(Ordering::Relaxed))
    }

    pub fn exhausted(&self) -> bool {
        self.burned_micro() >= self.budget_micro
    }

    /// Record `n` successful billable requests (PUT-class).
    pub fn record_requests(&self, n: u64) {
        let micro = self.pricing.request_micro(n);
        self.burned_micro.fetch_add(micro, Ordering::Relaxed);
        self.request_micro.fetch_add(micro, Ordering::Relaxed);
    }

    /// Immediate cost of one whole object of `bytes` (transfer + its requests).
    fn object_cost_micro(&self, bytes: u64) -> u64 {
        let parts = bytes.div_ceil(self.part_size);
        let reqs = parts + self.cost.requests_per_object;
        self.cost.transfer_micro(bytes) + self.pricing.request_micro(reqs)
    }

    /// Decide the size of the next object so the run lands exactly on budget:
    /// full-size while affordable, then one shrunken tail object, then None.
    /// Reserves the object's cost; pair with `commit_object` / `abort_object`.
    pub fn plan_next_object(&self, default_size: u64) -> Option<u64> {
        if self.exhausted() {
            return None;
        }
        if !self.cost.budget_drives_stop() {
            // Request fees alone burn ~$0.02/TiB — budget can't drive sizing.
            // Scheduling continues; secondary bounds / budget exhaustion stop it.
            self.reserve(self.object_cost_micro(default_size));
            return Some(default_size);
        }
        let remaining = self.remaining_micro();
        let full_cost = self.object_cost_micro(default_size);
        if full_cost <= remaining {
            self.reserve(full_cost);
            return Some(default_size);
        }
        // Shrink the tail: bytes affordable with the remaining budget.
        // Per-byte cost includes per-part request fees amortized over the part
        // size, so the hard budget ceiling holds to the micro-dollar.
        let per_byte = self.cost.micro_per_byte()
            + self.pricing.request_micro(1) as f64 / self.part_size as f64;
        let overhead = self.pricing.request_micro(self.cost.requests_per_object);
        let avail = remaining.saturating_sub(overhead) as f64;
        let bytes = (avail / per_byte) as u64;
        let bytes = (bytes / MIB) * MIB; // tidy MiB alignment
        if bytes < MIN_TAIL_OBJECT {
            return None;
        }
        self.reserve(self.object_cost_micro(bytes));
        Some(bytes)
    }

    fn reserve(&self, micro: u64) {
        self.reserved_micro.fetch_add(micro, Ordering::Relaxed);
    }

    fn release(&self, bytes: u64) {
        let micro = self.object_cost_micro(bytes);
        // Saturating: never underflow if commit races a shutdown path.
        let mut cur = self.reserved_micro.load(Ordering::Relaxed);
        loop {
            let next = cur.saturating_sub(micro);
            match self.reserved_micro.compare_exchange_weak(
                cur,
                next,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => return,
                Err(now) => cur = now,
            }
        }
    }

    /// Object completed: release the reservation and burn its transfer fee.
    /// (Its request fees were already burned per successful request.)
    pub fn commit_object(&self, bytes: u64) {
        self.release(bytes);
        let micro = self.cost.transfer_micro(bytes);
        if micro > 0 {
            self.burned_micro.fetch_add(micro, Ordering::Relaxed);
            self.transfer_micro.fetch_add(micro, Ordering::Relaxed);
        }
    }

    /// Object aborted: release the reservation. Request fees already spent stay.
    pub fn abort_object(&self, bytes: u64) {
        self.release(bytes);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::s3::cost::{Pricing, TransferFee};
    use crate::s3::{GIB, MIB};

    fn pricing() -> Pricing {
        Pricing {
            region: "us-east-1".into(),
            put_per_1k_usd: 0.005,
            storage_gb_month_usd: 0.023,
            crr_per_gb_usd: 0.02,
            assumed: false,
        }
    }

    /// The `crr` mode's cost shape: $0.02/GB transfer + 3 requests per object.
    fn crr_cost() -> CostModel {
        CostModel {
            transfer: vec![TransferFee::per_gb("跨区复制流量费", 0.02)],
            requests_per_object: 3,
        }
    }

    #[test]
    fn full_objects_then_shrunken_tail_then_stop() {
        // $1 budget, 10 GiB objects at $0.02/GiB-ish → ~5 full objects
        let meter = BudgetMeter::new(1_000_000, 0, pricing(), crr_cost(), GIB);
        let mut planned: Vec<u64> = Vec::new();
        while let Some(size) = meter.plan_next_object(10 * GIB) {
            planned.push(size);
            // simulate the object completing with its request fees billed
            let parts = size.div_ceil(GIB);
            meter.record_requests(parts + 3);
            meter.commit_object(size);
        }
        assert!(meter.exhausted() || meter.remaining_micro() < 200_000);
        let full = planned.iter().filter(|&&s| s == 10 * GIB).count();
        assert!(full >= 4, "planned: {:?}", planned);
        // tail object (if any) must be smaller and MiB aligned
        if let Some(&tail) = planned.last() {
            if tail != 10 * GIB {
                assert_eq!(tail % MIB, 0);
                assert!(tail < 10 * GIB);
            }
        }
        // total burn must land on budget within one tail-object of slack
        let burned = meter.burned_micro();
        assert!(burned <= 1_010_000, "burned {}", burned);
        assert!(burned >= 950_000, "burned {}", burned);
    }

    /// The budget is a hard ceiling at every fan-out width. K changes both the
    /// per-byte rate and the per-object request count, which the tail-shrink
    /// math depends on — an off-by-one there overshoots real money.
    #[test]
    fn hard_ceiling_holds_at_every_fanout() {
        for k in 1..=4u64 {
            let cost = CostModel {
                transfer: vec![TransferFee::per_gb("crr", 0.02 * k as f64)],
                requests_per_object: 2 + k,
            };
            let meter = BudgetMeter::new(1_000_000, 0, pricing(), cost, GIB);
            let mut total_bytes = 0u64;
            while let Some(size) = meter.plan_next_object(10 * GIB) {
                total_bytes += size;
                meter.record_requests(size.div_ceil(GIB) + 2 + k);
                meter.commit_object(size);
            }
            let burned = meter.burned_micro();
            assert!(burned <= 1_000_000, "k={} 超出预算硬上限: {}", k, burned);
            assert!(burned >= 940_000, "k={} 远未烧满: {}", k, burned);
            // K× the rate must buy ~1/K the bytes for the same dollar.
            let expected = 50 * GIB / k;
            let ratio = total_bytes as f64 / expected as f64;
            assert!((0.85..=1.15).contains(&ratio), "k={} 写入量偏离: {}", k, ratio);
        }
    }

    #[test]
    fn abort_releases_reservation() {
        let meter = BudgetMeter::new(1_000_000, 0, pricing(), crr_cost(), GIB);
        let size = meter.plan_next_object(10 * GIB).unwrap();
        let before = meter.remaining_micro();
        meter.abort_object(size);
        assert!(meter.remaining_micro() > before);
        assert_eq!(meter.burned_micro(), 0);
    }

    #[test]
    fn resume_restores_burned_amount() {
        let meter = BudgetMeter::new(1_000_000, 999_999, pricing(), crr_cost(), GIB);
        assert!(!meter.exhausted());
        meter.record_requests(1000); // $0.005 → crosses the line
        assert!(meter.exhausted());
    }

    #[test]
    fn request_only_mode_never_shrinks() {
        let meter = BudgetMeter::new(1_000_000, 0, pricing(), CostModel::request_only(), GIB);
        assert_eq!(meter.plan_next_object(10 * GIB), Some(10 * GIB));
        // No per-byte fee: completing an object burns nothing beyond requests.
        meter.commit_object(10 * GIB);
        assert_eq!(meter.transfer_micro(), 0);
    }
}
