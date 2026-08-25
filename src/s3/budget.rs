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

use super::cost::{self, CostModel, Pricing};
use super::quota::{DayPlan, Pause};
use super::MIB;

/// Smallest object worth scheduling when shrinking the tail to fit the budget.
/// Also the resolution of every ceiling here: one that cannot buy this much is
/// a ceiling nothing can be planned against.
pub const MIN_TAIL_OBJECT: u64 = 8 * MIB;

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
    /// `--days`: the hour and day ceilings under the budget. Every micro booked
    /// against the budget is booked against them too.
    plan: Option<DayPlan>,
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
            plan: None,
        }
    }

    /// Arm the `--days` ceilings. Without them the budget is the only bound.
    pub fn with_plan(mut self, plan: DayPlan) -> Self {
        self.plan = Some(plan);
        self
    }

    pub fn plan(&self) -> Option<&DayPlan> {
        self.plan.as_ref()
    }

    /// The pace that spends `micro` over `secs` seconds, in bytes/sec — how
    /// `--days` turns an hour's drawn ceiling into a rate.
    ///
    /// `None` when the mode has no per-byte transfer fee. Request fees alone
    /// still divide into a number, but an absurd one — an hour of them costs
    /// ~$0.02/TiB, so "spend this hour's dollars" would ask for tens of GB/s.
    /// Those runs are paced by `--total-size` instead and must be left alone.
    pub fn rate_for(&self, micro: u64, secs: u64) -> Option<u64> {
        if !self.cost.budget_drives_stop() || secs == 0 {
            return None;
        }
        let bytes = cost::budget_bytes(micro, &self.cost, &self.pricing, self.part_size);
        (bytes > 0).then(|| bytes / secs)
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

    /// Money committed by objects still uploading. Transfer fees only land when
    /// an object completes (CRR never replicates a partial upload), so with
    /// large objects `burned_micro` can sit at zero for a long while — this is
    /// the number that shows the run is in fact spending.
    pub fn reserved_micro(&self) -> u64 {
        self.reserved_micro.load(Ordering::Relaxed)
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

    /// What may still be committed right now: whichever ceiling — the whole
    /// budget, this hour, or today — comes first. In-flight reservations come
    /// off ALL of them, so an object still uploading when the hour turns cannot
    /// be counted a second time against the next one.
    fn spendable_micro(&self) -> u64 {
        let total = self.remaining_micro();
        match &self.plan {
            Some(plan) => total.min(plan.remaining().saturating_sub(self.reserved_micro())),
            None => total,
        }
    }

    /// The cheapest object `plan_next_object` would still schedule. A mode with
    /// a per-byte fee can shrink its tail down to `MIN_TAIL_OBJECT`; one billed
    /// only per request cannot shrink at all, so it needs a whole `next_size`.
    /// Asking the wrong one of these is how a paused run gets mistaken for a
    /// finished one.
    fn smallest_plannable_micro(&self, next_size: u64) -> u64 {
        if self.cost.budget_drives_stop() {
            self.object_cost_micro(MIN_TAIL_OBJECT)
        } else {
            self.object_cost_micro(next_size)
        }
    }

    /// The scheduler has nothing left to plan, and `next_size` is what it would
    /// try next. `Some(pause)` means only a `--days` ceiling is in the way and
    /// the run should wait it out. `None` means waiting cannot help: either the
    /// budget itself is done, or there are no `--days` ceilings to wait on.
    pub fn required_pause(&self, next_size: u64) -> Option<Pause> {
        let plan = self.plan.as_ref()?;
        // If the budget could not afford the next object either, it is the
        // budget that ran out, not the clock — no amount of waiting helps.
        let need = self.smallest_plannable_micro(next_size);
        if self.exhausted() || self.remaining_micro() < need {
            return None;
        }
        plan.wait_for(need, self.reserved_micro())
    }

    /// Book money against the hour and day ceilings as well as the budget.
    fn book_plan(&self, micro: u64) {
        if let Some(plan) = &self.plan {
            plan.add(micro);
        }
    }

    /// Record `n` successful billable requests (PUT-class).
    pub fn record_requests(&self, n: u64) {
        let micro = self.pricing.request_micro(n);
        self.burned_micro.fetch_add(micro, Ordering::Relaxed);
        self.request_micro.fetch_add(micro, Ordering::Relaxed);
        self.book_plan(micro);
    }

    /// Immediate cost of one whole object of `bytes` (transfer + its requests).
    fn object_cost_micro(&self, bytes: u64) -> u64 {
        cost::object_cost_micro(bytes, &self.cost, &self.pricing, self.part_size)
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
            // The --days ceilings still gate: they bound real money either way.
            // Guarded on `plan`, not just on `spendable_micro`: without a plan
            // this path must never refuse an object, however low the budget is.
            let full_cost = self.object_cost_micro(default_size);
            if self.plan.is_some() && full_cost > self.spendable_micro() {
                return None;
            }
            self.reserve(full_cost);
            return Some(default_size);
        }
        let remaining = self.spendable_micro();
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
        let mut bytes = (bytes / MIB) * MIB; // tidy MiB alignment
        // That estimate amortizes per-part request fees over a whole part, but
        // the bill charges per part STARTED — so a tail landing just past a part
        // boundary costs a few micro-dollars more than it was sized for. Step
        // back until it truly fits rather than let a ceiling be crossed at all:
        // "硬上限" has to mean the number, not the number plus rounding.
        while bytes >= MIN_TAIL_OBJECT && self.object_cost_micro(bytes) > remaining {
            bytes -= MIB;
        }
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

    /// Object completed: burn its transfer fee and release the reservation.
    /// (Its request fees were already burned per successful request.)
    ///
    /// Booking comes BEFORE the release on purpose. The two counters are read
    /// separately by `spendable_micro`, so a scheduler read landing between
    /// them sees one state or the other; in this order the in-between state
    /// counts the money twice (conservative), while releasing first would leave
    /// a window where it counts for nothing and a ceiling could be planned past.
    pub fn commit_object(&self, bytes: u64) {
        let micro = self.cost.transfer_micro(bytes);
        if micro > 0 {
            self.burned_micro.fetch_add(micro, Ordering::Relaxed);
            self.transfer_micro.fetch_add(micro, Ordering::Relaxed);
            self.book_plan(micro);
        }
        self.release(bytes);
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
    use crate::s3::quota::{self, DayPlan, PausedBy, PlanLedger};
    use crate::s3::{GIB, MIB};
    use chrono::Utc;
    use std::time::Duration;

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

    /// What the report line leans on: with objects big enough to take an hour,
    /// `burned_micro` stays at zero the whole time and the reservation is the
    /// only evidence the run is spending anything.
    #[test]
    fn reservation_is_visible_while_an_object_is_in_flight() {
        let meter = BudgetMeter::new(1_000_000, 0, pricing(), crr_cost(), GIB);
        assert_eq!(meter.reserved_micro(), 0);

        let size = meter.plan_next_object(10 * GIB).unwrap();
        assert!(meter.reserved_micro() > 0, "在途金额应可见");
        assert_eq!(meter.burned_micro(), 0, "对象未完成前不记流量费");

        meter.commit_object(size);
        assert_eq!(meter.reserved_micro(), 0, "完成后预留应释放");
        assert!(meter.burned_micro() > 0);
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

    /// Drain everything the planner will hand out, the way the scheduler does.
    fn burn_everything_plannable(meter: &BudgetMeter) -> u64 {
        let mut objects = 0;
        while let Some(size) = meter.plan_next_object(10 * GIB) {
            objects += 1;
            meter.record_requests(size.div_ceil(GIB) + 3);
            meter.commit_object(size);
        }
        objects
    }

    /// A meter whose `--days` plan is `budget` spread over `days`.
    fn metered_plan(budget_micro: u64, days: u64, saved: PlanLedger) -> BudgetMeter {
        BudgetMeter::new(budget_micro, 0, pricing(), crr_cost(), GIB)
            .with_plan(DayPlan::new(budget_micro, days, &saved))
    }

    /// The hour is a HARD ceiling, not a pace: once this hour's slice is spent
    /// the planner refuses to schedule more — with the budget nowhere near
    /// exhausted, so the run is only standing down until the next hour.
    #[test]
    fn the_hourly_ceiling_ends_the_hour_without_ending_the_run() {
        // $240 over 10 days → $24/day → ~$1/hour, jittered by HOUR_JITTER.
        let meter = metered_plan(240_000_000, 10, PlanLedger::default());
        let hour_cap = meter.plan().unwrap().hour().cap();
        let (lo, hi) = quota::hour_band(1_000_000);
        assert!((lo..=hi).contains(&hour_cap), "小时额度 {} 不在 {}–{}", hour_cap, lo, hi);
        assert!(burn_everything_plannable(&meter) > 0, "这一小时一个对象都没排上");

        let burned = meter.burned_micro();
        assert!(burned <= hour_cap, "烧穿了本小时的 {}: {}", hour_cap, burned);
        assert!(burned * 20 >= hour_cap * 19, "小时额度远未用满: {}", burned);
        assert!(!meter.exhausted(), "一小时不该烧掉整笔预算");

        let pause = meter.required_pause(10 * GIB).expect("只是本小时满了,应该等到下个整点");
        assert_eq!(pause.period, PausedBy::Hour);
        assert!(pause.wait <= Duration::from_secs(3600));
    }

    /// The day still holds the line the user actually asked for: 24 jittered
    /// hours may add up over it, and the day's own ceiling is what stops them.
    #[test]
    fn the_daily_ceiling_still_backstops_the_hours() {
        let meter = metered_plan(240_000_000, 10, PlanLedger::default());
        let plan = meter.plan().unwrap();
        let day_cap = plan.day().cap();
        assert_eq!(day_cap, 24_000_000, "日上限必须是 预算 ÷ 天数,不抖");

        // Spend the day down to a sliver, leaving this hour's ceiling untouched.
        plan.day().add_at(Utc::now(), day_cap - 1);
        assert!(
            meter.plan_next_object(10 * GIB).is_none(),
            "今日额度见底了还在排对象"
        );
        let pause = meter.required_pause(10 * GIB).expect("今日满了");
        assert_eq!(pause.period, PausedBy::Day, "日上限满时等到下个整点毫无意义");
    }

    /// …and when it is the BUDGET that ran out, waiting for the next hour would
    /// hang the run forever, so no pause must be offered.
    #[test]
    fn a_spent_budget_is_never_mistaken_for_a_spent_hour() {
        let meter = BudgetMeter::new(1_000_000, 0, pricing(), crr_cost(), GIB)
            .with_plan(DayPlan::new(1_000_000_000_000, 1, &PlanLedger::default()));
        burn_everything_plannable(&meter);
        assert!(meter.exhausted() || meter.remaining_micro() < 200_000);
        assert!(meter.required_pause(10 * GIB).is_none(), "预算烧完了,不该再等下一小时");
    }

    /// An hour picked up mid-way gets only what is left of it, at the ceiling it
    /// was already spending: the ledger belongs to the checkpoint, not to each
    /// process that opens it.
    #[test]
    fn a_resumed_hour_only_gets_its_remainder() {
        let saved = PlanLedger {
            hour_start: Some(Utc::now()),
            hour_cap_micro: 1_000_000,
            hour_burned_micro: 800_000,
            day_start: Some(Utc::now()),
            day_burned_micro: 800_000,
        };
        let meter = metered_plan(240_000_000, 10, saved);
        assert_eq!(meter.plan().unwrap().hour().cap(), 1_000_000, "重启重抽了上限");
        burn_everything_plannable(&meter);
        assert!(
            meter.burned_micro() <= 200_000,
            "越过了本小时剩下的 $0.20: {}",
            meter.burned_micro()
        );
    }

    /// Transfer fees only land when an object completes, so the ceilings have to
    /// be charged at RESERVATION time too — otherwise a run with several objects
    /// in the air keeps planning against money already spoken for.
    #[test]
    fn reservations_count_against_the_ceilings_before_they_land() {
        let meter = metered_plan(240_000_000, 10, PlanLedger::default());
        let hour_cap = meter.plan().unwrap().hour().cap();
        let mut inflight = Vec::new();
        while let Some(size) = meter.plan_next_object(10 * GIB) {
            inflight.push(size);
        }
        assert!(!inflight.is_empty());
        assert_eq!(meter.burned_micro(), 0, "还没有对象完成");
        let reserved: u64 = inflight.iter().map(|&b| meter.object_cost_micro(b)).sum();
        assert!(reserved <= hour_cap, "在途预留已超本小时上限: {}", reserved);
    }

    /// Preflight refuses a plan whose leanest hour cannot buy the smallest
    /// object; whatever it lets through must therefore be schedulable. The two
    /// used to price that object differently — preflight via `budget_bytes`
    /// (which omits the per-object requests), the meter via `object_cost_micro`
    /// — so a band of hourly ceilings passed the gate and then never scheduled
    /// anything, waking and sleeping forever on exactly the hang the gate exists
    /// to prevent.
    #[test]
    fn any_hour_the_preflight_guard_admits_can_actually_schedule() {
        let pricing = pricing();
        let cost = crr_cost();
        let smallest = cost::object_cost_micro(MIN_TAIL_OBJECT, &cost, &pricing, GIB);
        // Sweep across the boundary the guard draws, in both directions.
        for hour_cap in (smallest.saturating_sub(40))..(smallest + 40) {
            let admitted = crate::s3::quota::hour_band(hour_cap).0 >= smallest;
            if !admitted {
                continue;
            }
            let meter = BudgetMeter::new(1_000_000_000, 0, pricing.clone(), cost.clone(), GIB)
                .with_plan(DayPlan::new(hour_cap * 24, 1, &PlanLedger::default()));
            assert!(
                meter.plan_next_object(10 * GIB).is_some(),
                "护栏放行了每小时 {} 却排不出对象",
                hour_cap
            );
        }
    }

    /// The pace has to follow the hour's own ceiling, or a fat hour would be
    /// capped by a lean rate and the plan would run systematically slow.
    #[test]
    fn the_hourly_pace_is_derived_from_that_hours_ceiling() {
        let meter = metered_plan(240_000_000, 10, PlanLedger::default());
        let cap = meter.plan().unwrap().hour().cap();
        let rate = meter.rate_for(cap, 3600).expect("crr 有按字节成本");
        // An hour at that rate must buy exactly what the hour is allowed to
        // spend — that closed loop is what keeps the day landing on target.
        let spent = meter.cost.transfer_micro(rate * 3600);
        assert!(
            (spent as f64 / cap as f64 - 1.0).abs() < 0.02,
            "小时配速买不出小时额度: {} vs {}",
            spent,
            cap
        );
        // A mode with no per-byte fee cannot derive a pace from money at all.
        let request_only = BudgetMeter::new(1_000_000, 0, pricing(), CostModel::request_only(), GIB);
        assert_eq!(request_only.rate_for(1_000_000, 3600), None);
    }
}
