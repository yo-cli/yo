// The spend ceilings behind `--days`: the hour that shapes the burn, and the
// UTC day that bounds it.
//
// `--days N` divides the budget twice — `budget ÷ N` for the day, and that again
// ÷ 24 for the hour:
//
// - The DAY's ceiling is flat and hard. It is the number the user asked not to
//   be exceeded, so nothing is allowed to round it upward.
// - The HOUR's is drawn fresh every hour from ±`HOUR_JITTER` around its average.
//   That is what keeps the burn from being a flat line, and it costs the plan
//   nothing: the mean of a uniform draw is exactly its centre, so 24 of them add
//   up to a day that is only ever "about right" — and the day's own ceiling
//   stops the half of days that would land above it.
//
// Both periods are UTC-aligned. AWS meters daily in UTC, so the day here is the
// day the invoice will show, and the hour rides the same boundaries so the two
// ledgers roll together at midnight.
//
// Ledgers are fed from the same two places budget.rs books money (request fees
// as they land, an object's transfer fee when it completes) and ride along in
// the checkpoint. Without that a run killed at 12:30 and restarted at 12:31
// would draw itself a second hour's quota — and, on the day, a second day's.
//
// A plain Mutex rather than the atomics budget.rs uses: rolling a period has to
// move the clock, redraw the ceiling and zero the ledger as one step, and this
// is touched a few times per part, never per byte.

use chrono::{DateTime, Utc};
use rand::Rng;
use serde::{Deserialize, Serialize};
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

pub const HOUR_SECS: i64 = 3_600;
pub const DAY_SECS: i64 = 86_400;

/// Half-width of the hourly ceiling's draw.
///
/// Deliberately narrower than the rate's ±40%: that one wobbles inside an hour
/// and is what makes the burn look organic, while this one moves a whole hour
/// of spend, and 24 of them are what a day is made of. At ±10% a day lands
/// inside ~±2.5% of its target — "大差不差" without the day's own ceiling having
/// to clip a heavy hour very often.
pub const HOUR_JITTER: f64 = 0.1;

/// The band an hour's ceiling is drawn from. `draw` samples out of this exact
/// band and everything that SHOWS the band reads it from here, so what is
/// printed cannot drift from what is drawn.
pub fn hour_band(base_micro: u64) -> (u64, u64) {
    band(base_micro, HOUR_JITTER)
}

fn band(base_micro: u64, jitter: f64) -> (u64, u64) {
    let base = base_micro as f64;
    (
        (base * (1.0 - jitter)) as u64,
        (base * (1.0 + jitter)) as u64,
    )
}

struct PeriodState {
    /// Unix seconds at the start of the period `burned_micro` belongs to.
    start: i64,
    /// The ceiling drawn for THIS period. It travels with the ledger: a restart
    /// mid-period must spend the ceiling it was already spending, not a fresh
    /// draw that could hand the same hour a second, larger quota.
    cap_micro: u64,
    burned_micro: u64,
}

/// One period's ledger, as the checkpoint keeps it.
pub struct PeriodSnapshot {
    pub start: DateTime<Utc>,
    pub cap_micro: u64,
    pub burned_micro: u64,
}

/// A spend ceiling on one repeating clock period.
pub struct PeriodQuota {
    period_secs: i64,
    /// What the period may burn on average. The drawn ceiling jitters around it.
    base_micro: u64,
    /// Half-width of the draw as a fraction of `base_micro`. 0 = flat ceiling.
    jitter: f64,
    state: Mutex<PeriodState>,
}

impl PeriodQuota {
    fn new(
        period_secs: i64,
        base_micro: u64,
        jitter: f64,
        start: DateTime<Utc>,
        cap_micro: u64,
        burned_micro: u64,
    ) -> Self {
        Self {
            period_secs,
            base_micro,
            jitter,
            state: Mutex::new(PeriodState {
                start: floor_to(start.timestamp(), period_secs),
                // A period with no remembered ceiling gets one now. A remembered
                // one is trimmed to today's band: a resume that lowered
                // --budget would otherwise spend this period against a ceiling
                // drawn for a much richer plan.
                cap_micro: if cap_micro > 0 {
                    cap_micro.min(band(base_micro, jitter).1)
                } else {
                    draw(base_micro, jitter)
                },
                burned_micro,
            }),
        }
    }

    pub fn cap_at(&self, now: DateTime<Utc>) -> u64 {
        self.rolled(now).cap_micro
    }

    pub fn burned_at(&self, now: DateTime<Utc>) -> u64 {
        self.rolled(now).burned_micro
    }

    pub fn remaining_at(&self, now: DateTime<Utc>) -> u64 {
        let st = self.rolled(now);
        st.cap_micro.saturating_sub(st.burned_micro)
    }

    pub fn add_at(&self, now: DateTime<Utc>, micro: u64) {
        let mut st = self.rolled(now);
        st.burned_micro = st.burned_micro.saturating_add(micro);
    }

    /// What the checkpoint stores about this period.
    pub fn snapshot_at(&self, now: DateTime<Utc>) -> PeriodSnapshot {
        let st = self.rolled(now);
        PeriodSnapshot {
            start: DateTime::from_timestamp(st.start, 0).unwrap_or(now),
            cap_micro: st.cap_micro,
            burned_micro: st.burned_micro,
        }
    }

    /// Roll onto `now`'s period if the clock has crossed a boundary: new
    /// ceiling, empty ledger. A clock that jumps BACKWARDS keeps the current
    /// period — falling back into one already spent would hand out its quota a
    /// second time.
    fn rolled(&self, now: DateTime<Utc>) -> MutexGuard<'_, PeriodState> {
        let mut st = self.state.lock().unwrap();
        let current = floor_to(now.timestamp(), self.period_secs);
        if current > st.start {
            st.start = current;
            st.cap_micro = draw(self.base_micro, self.jitter);
            st.burned_micro = 0;
        }
        st
    }

    /// When this period's ceiling refills. Measured off the period the ledger
    /// is actually on, not off the wall clock: after a backwards clock step
    /// `rolled` still refuses to refill, and a reset computed from `now` would
    /// promise one that does not come.
    pub fn resets_at(&self, now: DateTime<Utc>) -> DateTime<Utc> {
        let next = self.rolled(now).start + self.period_secs;
        DateTime::from_timestamp(next, 0).unwrap_or(now)
    }

    /// Time until the ceiling refills. Derived from `resets_at` so the two can
    /// never disagree — a pause prints both in one sentence. Never zero: the
    /// scheduler sleeps this long, and a zero-length sleep would spin the loop
    /// until the clock moved on.
    pub fn until_reset(&self, now: DateTime<Utc>) -> Duration {
        let left = self.resets_at(now).timestamp() - now.timestamp();
        Duration::from_secs(left.max(1) as u64)
    }

    // --- wall-clock wrappers, for everything outside the tests ---

    pub fn cap(&self) -> u64 {
        self.cap_at(Utc::now())
    }

    pub fn burned(&self) -> u64 {
        self.burned_at(Utc::now())
    }
}

/// What `--days` produces: an hourly ceiling that wobbles and a daily one that
/// does not.
pub struct DayPlan {
    hour: PeriodQuota,
    day: PeriodQuota,
}

/// The part of the plan that has to survive the process, stored in the
/// checkpoint. Everything else is re-derived from `--budget` and `--days`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct PlanLedger {
    pub hour_start: Option<DateTime<Utc>>,
    pub hour_cap_micro: u64,
    pub hour_burned_micro: u64,
    pub day_start: Option<DateTime<Utc>>,
    pub day_burned_micro: u64,
}

/// Which ceiling stood the scheduler down.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PausedBy {
    Hour,
    Day,
}

impl PausedBy {
    /// Reads inside 「{}额度已烧满」, so it is a noun phrase, not a tag — which
    /// is why the tag itself is the enum and this is only its wording.
    pub fn label(self) -> &'static str {
        match self {
            Self::Hour => "本小时",
            Self::Day => "今日",
        }
    }
}

/// Why the scheduler has to stand down, and until when.
pub struct Pause {
    pub wait: Duration,
    pub period: PausedBy,
    pub burned_micro: u64,
    pub cap_micro: u64,
    pub resets_at: DateTime<Utc>,
}

impl DayPlan {
    pub fn new(budget_micro: u64, days: u64, saved: &PlanLedger) -> Self {
        let now = Utc::now();
        let day_cap = budget_micro / days.max(1);
        Self {
            hour: PeriodQuota::new(
                HOUR_SECS,
                day_cap / 24,
                HOUR_JITTER,
                saved.hour_start.unwrap_or(now),
                saved.hour_cap_micro,
                saved.hour_burned_micro,
            ),
            // Flat: this is the ceiling the user named, and a jittered version
            // of it would be a different promise.
            day: PeriodQuota::new(
                DAY_SECS,
                day_cap,
                0.0,
                saved.day_start.unwrap_or(now),
                day_cap,
                saved.day_burned_micro,
            ),
        }
    }

    pub fn hour(&self) -> &PeriodQuota {
        &self.hour
    }

    pub fn day(&self) -> &PeriodQuota {
        &self.day
    }

    /// What may still be committed under both ceilings at once.
    pub fn remaining(&self) -> u64 {
        let now = Utc::now();
        self.hour.remaining_at(now).min(self.day.remaining_at(now))
    }

    /// Book money against both ledgers.
    pub fn add(&self, micro: u64) {
        let now = Utc::now();
        self.hour.add_at(now, micro);
        self.day.add_at(now, micro);
    }

    pub fn ledger(&self) -> PlanLedger {
        let now = Utc::now();
        let hour = self.hour.snapshot_at(now);
        // The day's ceiling is not stored: it is flat, so `DayPlan::new`
        // re-derives it from --budget and --days on the way back in.
        let day = self.day.snapshot_at(now);
        PlanLedger {
            hour_start: Some(hour.start),
            hour_cap_micro: hour.cap_micro,
            hour_burned_micro: hour.burned_micro,
            day_start: Some(day.start),
            day_burned_micro: day.burned_micro,
        }
    }

    /// How long before `need_micro` could fit again, given `reserved_micro`
    /// already spoken for by objects in the air. The binding period is whichever
    /// refills LAST — waiting out the hour is pointless when it is the day that
    /// is full.
    pub fn wait_for(&self, need_micro: u64, reserved_micro: u64) -> Option<Pause> {
        let now = Utc::now();
        let is_short = |q: &PeriodQuota| {
            q.remaining_at(now).saturating_sub(reserved_micro) < need_micro
        };
        let (quota, period) = if is_short(&self.day) {
            (&self.day, PausedBy::Day)
        } else if is_short(&self.hour) {
            (&self.hour, PausedBy::Hour)
        } else {
            return None;
        };
        Some(Pause {
            wait: quota.until_reset(now),
            period,
            burned_micro: quota.burned_at(now),
            cap_micro: quota.cap_at(now),
            resets_at: quota.resets_at(now),
        })
    }
}

fn floor_to(ts: i64, period_secs: i64) -> i64 {
    ts - ts.rem_euclid(period_secs)
}

/// A ceiling for one period: `base` jittered by ±`jitter`, or `base` flat.
/// Sampled out of `band` rather than off a fresh float multiply, so a drawn
/// ceiling can never land outside the band the user was shown.
fn draw(base_micro: u64, jitter: f64) -> u64 {
    if jitter <= 0.0 || base_micro == 0 {
        return base_micro;
    }
    let (lo, hi) = band(base_micro, jitter);
    rand::rng().random_range(lo..=hi)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(s: &str) -> DateTime<Utc> {
        s.parse().unwrap()
    }

    fn plan(budget: u64, days: u64) -> DayPlan {
        DayPlan::new(budget, days, &PlanLedger::default())
    }

    /// Assert against the band the code actually draws from, never a hand-typed
    /// pair — the two would drift the moment HOUR_JITTER changed.
    fn hour_range(base_micro: u64) -> std::ops::RangeInclusive<u64> {
        let (lo, hi) = hour_band(base_micro);
        lo..=hi
    }

    #[test]
    fn spending_draws_a_period_down_to_its_ceiling_and_no_further() {
        let q = PeriodQuota::new(HOUR_SECS, 1_000_000, 0.0, at("2026-08-25T09:00:00Z"), 0, 0);
        let now = at("2026-08-25T09:30:00Z");
        q.add_at(now, 400_000);
        assert_eq!(q.remaining_at(now), 600_000);
        // Overshooting saturates instead of wrapping into a fresh quota.
        q.add_at(now, 99_000_000);
        assert_eq!(q.remaining_at(now), 0);
    }

    #[test]
    fn crossing_the_boundary_refills_the_ceiling() {
        let q = PeriodQuota::new(HOUR_SECS, 1_000_000, 0.0, at("2026-08-25T09:00:00Z"), 0, 0);
        q.add_at(at("2026-08-25T09:59:59Z"), 1_000_000);
        assert_eq!(q.remaining_at(at("2026-08-25T09:59:59Z")), 0);
        assert_eq!(q.remaining_at(at("2026-08-25T10:00:00Z")), 1_000_000);
    }

    /// The whole reason the ledger is persisted: a crash at 12:30 must not hand
    /// 12:31 a second hour's quota.
    #[test]
    fn a_restart_inside_a_period_resumes_the_same_ledger_and_ceiling() {
        let q = PeriodQuota::new(
            HOUR_SECS,
            1_000_000,
            HOUR_JITTER,
            at("2026-08-25T09:00:00Z"),
            1_050_000, // the ceiling this hour had already drawn
            900_000,
        );
        let now = at("2026-08-25T09:31:00Z");
        assert_eq!(q.cap_at(now), 1_050_000, "重启不该重抽本小时的上限");
        assert_eq!(q.remaining_at(now), 150_000);
        // The next hour draws its own.
        let next = at("2026-08-25T10:00:00Z");
        assert!(hour_range(1_000_000).contains(&q.cap_at(next)));
        assert_eq!(q.burned_at(next), 0);
    }

    /// A remembered ceiling is honoured, but not blindly: resuming a plan whose
    /// budget was cut would otherwise spend this hour against a ceiling drawn
    /// when the whole plan was ten times richer.
    #[test]
    fn a_remembered_ceiling_is_trimmed_to_the_plan_in_force() {
        let q = PeriodQuota::new(
            HOUR_SECS,
            1_000_000,
            HOUR_JITTER,
            at("2026-08-25T09:00:00Z"),
            9_000_000, // drawn under a far bigger --budget
            0,
        );
        let now = at("2026-08-25T09:31:00Z");
        assert_eq!(q.cap_at(now), hour_band(1_000_000).1, "没被压回当前的抖动带");
        assert!(hour_range(1_000_000).contains(&q.cap_at(now)));
    }

    /// An NTP correction that steps the clock back must not look like a new hour.
    #[test]
    fn a_backwards_clock_never_refills_a_period() {
        let q = PeriodQuota::new(HOUR_SECS, 1_000_000, 0.0, at("2026-08-25T09:00:00Z"), 0, 0);
        q.add_at(at("2026-08-25T11:00:00Z"), 1_000_000);
        assert_eq!(q.remaining_at(at("2026-08-25T10:00:00Z")), 0, "回拨不该退回旧的一小时");
    }

    #[test]
    fn a_period_resets_on_its_own_boundary() {
        let q = PeriodQuota::new(HOUR_SECS, 1, 0.0, at("2026-08-25T09:00:00Z"), 0, 0);
        let now = at("2026-08-25T09:20:00Z");
        assert_eq!(q.resets_at(now), at("2026-08-25T10:00:00Z"));
        assert_eq!(q.until_reset(now), Duration::from_secs(2400));
        // Exactly on the boundary the ceiling has just refilled: a whole period
        // to go, never a zero-length sleep.
        assert_eq!(
            q.until_reset(at("2026-08-25T10:00:00Z")),
            Duration::from_secs(3600)
        );
    }

    /// The hour wobbles by design — but its mean has to be the average, or the
    /// plan would systematically run fast or slow.
    #[test]
    fn the_hourly_ceiling_wobbles_around_its_average() {
        let q = PeriodQuota::new(HOUR_SECS, 1_000_000, HOUR_JITTER, at("2026-08-25T00:00:00Z"), 0, 0);
        let mut caps = Vec::new();
        for hour in 0..240 {
            let now = DateTime::from_timestamp(
                at("2026-08-25T00:00:00Z").timestamp() + hour * HOUR_SECS,
                0,
            )
            .unwrap();
            caps.push(q.cap_at(now));
        }
        assert!(caps.iter().all(|&c| hour_range(1_000_000).contains(&c)), "越界");
        assert!(caps.iter().collect::<std::collections::HashSet<_>>().len() > 200, "没在抖");
        let mean = caps.iter().sum::<u64>() as f64 / caps.len() as f64;
        assert!(
            (mean - 1_000_000.0).abs() < 20_000.0,
            "均值偏离小时额度: {}",
            mean
        );
    }

    /// The day's ceiling is the promise, so it must NOT wobble.
    #[test]
    fn the_daily_ceiling_is_flat_and_exact() {
        let p = plan(300_000_000, 30);
        assert_eq!(p.day().cap(), 10_000_000);
        assert_eq!(p.day().cap(), 10_000_000, "第二次读也必须是同一个数");
        // The hour, by contrast, is a draw around 1/24 of it.
        let hour_avg = 10_000_000 / 24;
        assert!(
            hour_range(hour_avg).contains(&p.hour().cap()),
            "小时额度 {} 不在 {} 的抖动带内",
            p.hour().cap(),
            hour_avg
        );
    }

    /// 24 hourly draws add up to a day near its target without any one of them
    /// being exact — the whole point of spreading by the hour. At ±10% the day's
    /// spread is ≈1.2% one sigma, and one-sided in practice because the day's
    /// own flat ceiling clips the heavy half. What must NOT drift is the
    /// average, or the plan would run systematically fast or slow.
    #[test]
    fn days_of_hourly_draws_average_out_to_the_daily_target() {
        let day_cap = 10_000_000u64;
        let q = PeriodQuota::new(
            HOUR_SECS,
            day_cap / 24,
            HOUR_JITTER,
            at("2026-08-25T00:00:00Z"),
            0,
            0,
        );
        let days: Vec<u64> = (0..200i64)
            .map(|day| {
                let base = at("2026-08-25T00:00:00Z").timestamp() + day * DAY_SECS;
                (0..24)
                    .map(|h| q.cap_at(DateTime::from_timestamp(base + h * HOUR_SECS, 0).unwrap()))
                    .sum()
            })
            .collect();
        let mean = days.iter().sum::<u64>() as f64 / days.len() as f64;
        assert!(
            (mean / day_cap as f64 - 1.0).abs() < 0.006,
            "日均偏离日额度: {}",
            mean
        );
        let worst = days
            .iter()
            .map(|&d| (d as f64 / day_cap as f64 - 1.0).abs())
            .fold(0.0, f64::max);
        assert!(worst < 0.08, "单日偏离过大: {:.1}%", worst * 100.0);
        assert!(worst > 0.005, "根本没在抖");
    }

    /// Whichever ceiling is full decides the wait — sleeping to the top of the
    /// hour is useless when the day is what ran out.
    #[test]
    fn the_binding_ceiling_decides_how_long_to_wait() {
        let p = plan(240_000_000, 24); // $10/day, ~$0.42/hour
        assert!(p.wait_for(1, 0).is_none(), "两道线都还有余量");

        p.hour().add_at(Utc::now(), 99_000_000);
        let pause = p.wait_for(1, 0).expect("小时满了");
        assert_eq!(pause.period, PausedBy::Hour);
        assert!(pause.wait <= Duration::from_secs(3600));

        p.day().add_at(Utc::now(), 99_000_000);
        let pause = p.wait_for(1, 0).expect("今日满了");
        assert_eq!(pause.period, PausedBy::Day, "日上限满时等到下个整点毫无意义");
        assert!(pause.wait <= Duration::from_secs(86_400));
    }

    /// In-flight money is spoken for on both ledgers, or a run with several
    /// objects in the air keeps planning against fees that have not landed.
    #[test]
    fn reserved_money_counts_against_both_ceilings() {
        let p = plan(240_000_000, 24);
        let hour_cap = p.hour().cap();
        assert!(p.wait_for(hour_cap, 0).is_none());
        assert!(p.wait_for(hour_cap, 1).is_some(), "在途金额没被扣掉");
    }

    #[test]
    fn the_ledger_round_trips_through_the_checkpoint_shape() {
        let p = plan(240_000_000, 24);
        p.add(123_456);
        let saved = p.ledger();
        assert_eq!(saved.hour_burned_micro, 123_456);
        assert_eq!(saved.day_burned_micro, 123_456);

        let restored = DayPlan::new(240_000_000, 24, &saved);
        assert_eq!(restored.hour().cap(), saved.hour_cap_micro);
        assert_eq!(restored.hour().burned(), 123_456);
        assert_eq!(restored.day().burned(), 123_456);
    }
}
