// Resolved run configuration + validation + the snapshot embedded in
// checkpoints + where a run's state lives on disk.

use anyhow::{bail, Context, Result};
use rand::Rng;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use super::modes::ModeId;
use super::{fmt_bytes, GIB, MAX_OBJECT_SIZE, MAX_PARTS_PER_OBJECT, MAX_PART_SIZE, MIB, MIN_PART_SIZE};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub enum RateMode {
    /// Resample the target rate every --rate-resample-interval
    Continuous,
    /// Sample the target rate once at the start of each object
    PerObject,
}

/// How to decide whether uploads take the Transfer Acceleration edge endpoint.
///
/// `Auto` exists because AWS does not bill acceleration it does not deliver:
/// a client in the same region as the bucket is not charged, so forcing it on
/// there would put $0.04/GB into the estimate that never lands on the bill.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, clap::ValueEnum)]
pub enum AccelMode {
    /// 自动:能生效且会真正计费时启用(默认)
    #[default]
    Auto,
    /// 强制启用;桶或参数不支持时直接报错
    On,
    /// 始终不用
    Off,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub enum StopWhen {
    /// Stop only when every configured bound is met
    All,
    /// Stop as soon as any configured bound is met
    Any,
}

/// Fully resolved run configuration (CLI + interactive fill-in).
#[derive(Debug, Clone)]
pub struct BenchConfig {
    /// Which cost engine burns the budget (see s3::modes).
    pub mode: ModeId,
    pub bucket: String,
    pub key_prefix: String,
    pub budget_micro: u64,
    /// Replication destinations to create when the bucket has none yet.
    /// Naming them is what authorizes the tool to provision (buckets + IAM
    /// role) unattended; a bucket that already replicates ignores this.
    pub dest_regions: Vec<String>,
    pub region: Option<String>,
    pub endpoint_url: Option<String>,
    pub path_style: bool,
    pub insecure_skip_tls_verify: bool,
    /// Whether acceleration is actually ARMED for this run (+$0.04/GB), after
    /// resolving `--transfer-acceleration`. Set by preflight, not by the CLI.
    pub transfer_acceleration: bool,

    /// Objects get a random size in [min, max] — real backup jobs do not emit
    /// identically sized files, and the budget ledger does not care either way
    /// (bytes bought is fixed by price, not by how they are chunked).
    pub object_size_min: u64,
    pub object_size_max: u64,
    /// Base name and extension of every object written.
    pub object_name: String,
    pub object_ext: String,
    pub part_size: u64,
    pub pool_size: u64,
    pub concurrent_objects: usize,
    pub concurrent_parts: usize,

    pub rate_min: u64,
    pub rate_max: u64,
    pub rate_mode: RateMode,
    pub rate_resample_interval: Duration,

    pub retain: Duration,
    pub total_size: Option<u64>,
    pub iterations: Option<u64>,
    pub stop_when: StopWhen,
    pub max_duration: Option<Duration>,
    /// `--days`: spread the budget over this many days and cap what any one
    /// UTC day may burn at `budget ÷ days`. None = no daily ceiling.
    pub days: Option<u64>,

    pub checkpoint_path: String,
    pub summary_out: String,
    pub report_interval: Duration,
    pub dry_run: bool,
    pub yes: bool,
}

impl BenchConfig {
    /// Validate S3 hard limits and internal invariants with actionable errors.
    pub fn validate(&self) -> Result<()> {
        if self.budget_micro == 0 {
            bail!("--budget 必须 > 0");
        }
        if self.bucket.is_empty() {
            bail!("--bucket 不能为空");
        }
        if !(MIN_PART_SIZE..=MAX_PART_SIZE).contains(&self.part_size) {
            bail!(
                "part 大小 {} 超出 S3 限制 [{}, {}]",
                fmt_bytes(self.part_size),
                fmt_bytes(MIN_PART_SIZE),
                fmt_bytes(MAX_PART_SIZE)
            );
        }
        if self.object_size_min > self.object_size_max {
            bail!(
                "对象大小区间非法:[{}, {}](要求 min ≤ max)",
                fmt_bytes(self.object_size_min),
                fmt_bytes(self.object_size_max)
            );
        }
        if self.object_size_max > MAX_OBJECT_SIZE {
            bail!(
                "对象大小 {} 超出 S3 单对象上限 {}",
                fmt_bytes(self.object_size_max),
                fmt_bytes(MAX_OBJECT_SIZE)
            );
        }
        if self.object_size_min < MIB {
            bail!("对象大小 {} 太小(至少 1 MiB)", fmt_bytes(self.object_size_min));
        }
        if self.object_name.trim().is_empty() {
            bail!("--object-name 不能为空");
        }
        // The whole range must fit the part limit, so check the big end.
        let parts = self.object_size_max.div_ceil(self.part_size);
        if parts > MAX_PARTS_PER_OBJECT {
            // e.g. 1 TiB / 10_000 ≈ 110 MiB → suggest the next power-of-two part size
            let min_part = self.object_size_max.div_ceil(MAX_PARTS_PER_OBJECT);
            bail!(
                "对象 {} ÷ part {} = {} 个 part,超出 S3 上限 {}。part 至少要 {}",
                fmt_bytes(self.object_size_max),
                fmt_bytes(self.part_size),
                parts,
                MAX_PARTS_PER_OBJECT,
                fmt_bytes(min_part.next_power_of_two())
            );
        }
        if self.pool_size < 2 * self.part_size {
            bail!(
                "内存池 {} 必须 ≥ 2 × part 大小({})",
                fmt_bytes(self.pool_size),
                fmt_bytes(2 * self.part_size)
            );
        }
        if self.pool_size > 64 * GIB {
            bail!("内存池 {} 过大(上限 64 GiB)", fmt_bytes(self.pool_size));
        }
        if self.rate_min == 0 || self.rate_min > self.rate_max {
            bail!(
                "速率区间非法:[{}, {}](要求 0 < min ≤ max)",
                super::fmt_rate(self.rate_min),
                super::fmt_rate(self.rate_max)
            );
        }
        if self.concurrent_objects == 0 || self.concurrent_parts == 0 {
            bail!("并发数必须 ≥ 1");
        }
        if !self.key_prefix.is_empty() && !self.key_prefix.ends_with('/') {
            bail!("--key-prefix 必须以 / 结尾(当前: {})", self.key_prefix);
        }
        if let Some(days) = self.days {
            split_over_days(self.budget_micro, days)?;
        }
        Ok(())
    }

    /// The hard per-UTC-day ceiling `--days` implies, or `None` without it.
    /// `validate` has already rejected a split too thin to buy anything.
    pub fn daily_cap_micro(&self) -> Option<u64> {
        self.days.map(|days| self.budget_micro / days.max(1))
    }

    /// A random size for the next object. Real backup jobs do not emit
    /// identically sized files; the budget is unaffected either way because
    /// bytes-bought is fixed by price, not by how they are chunked.
    pub fn sample_object_size(&self) -> u64 {
        if self.object_size_min >= self.object_size_max {
            return self.object_size_min;
        }
        rand::rng().random_range(self.object_size_min..=self.object_size_max)
    }

    /// Mid-point, for the estimate page which needs a single number.
    pub fn object_size_avg(&self) -> u64 {
        self.object_size_min + (self.object_size_max - self.object_size_min) / 2
    }

    pub fn snapshot(&self) -> ConfigSnapshot {
        ConfigSnapshot {
            mode: self.mode,
            transfer_acceleration: self.transfer_acceleration,
            bucket: self.bucket.clone(),
            key_prefix: self.key_prefix.clone(),
            budget_micro: self.budget_micro,
            daily_cap_micro: self.daily_cap_micro().unwrap_or(0),
            endpoint_url: self.endpoint_url.clone(),
            object_size_min: self.object_size_min,
            object_size_max: self.object_size_max,
            object_name: self.object_name.clone(),
            object_ext: self.object_ext.clone(),
            part_size: self.part_size,
            retain_secs: self.retain.as_secs(),
        }
    }
}

/// A copy of the config a run started with, kept so a resume can tell what
/// changed.
///
/// **Almost nothing here blocks a resume.** The money is a scalar that only
/// accumulates — `burned_micro` is bytes × price — so renaming objects, or
/// resizing them, or repricing future bytes cannot make it wrong. The only
/// change with a real consequence is moving WHERE the data lives: objects
/// written under the old bucket/prefix fall outside what the retention sweeper
/// and `cleanup` look at, and then bill storage forever with nothing coming
/// back for them. That, and only that, refuses to resume.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConfigSnapshot {
    /// Absent in checkpoints written before modes existed — those runs were
    /// always the CRR engine, which is what `ModeId::default()` is.
    #[serde(default)]
    pub mode: ModeId,
    /// Changes the per-byte rate, so it must not flip across a resume.
    #[serde(default)]
    pub transfer_acceleration: bool,
    pub bucket: String,
    pub key_prefix: String,
    pub budget_micro: u64,
    /// The `--days` ceiling in force, 0 = none. Recorded so that a resume which
    /// silently drops `--days` says so: the ledger it guards lives in the same
    /// checkpoint, and a hard cap that disappears without a word is the worst
    /// kind of change for a tool that spends real money.
    #[serde(default)]
    pub daily_cap_micro: u64,
    pub endpoint_url: Option<String>,
    /// Absent in checkpoints written before object size became a range and
    /// before objects were named — zero / empty means "not recorded", which
    /// `diff` reports as no change rather than as a change from nothing.
    #[serde(default)]
    pub object_size_min: u64,
    #[serde(default)]
    pub object_size_max: u64,
    #[serde(default)]
    pub object_name: String,
    #[serde(default)]
    pub object_ext: String,
    pub part_size: u64,
    // Rate is deliberately ABSENT. It is not "where data goes, how it is laid
    // out, or what done means": burned_micro is bytes × price and the pace
    // never enters it. Guarding it here only blocked the one adjustment a
    // long run actually needs — slowing down when the network cannot keep up —
    // and forced the user to choose between an unusable pace and throwing the
    // already-burned budget away.
    pub retain_secs: u64,
}

/// What changed between the checkpoint's config and the current one.
#[derive(Debug, Default)]
pub struct SnapshotDiff {
    /// Moves the data somewhere the sweeper no longer looks. Refuses resume.
    pub blocking: Vec<String>,
    /// Changes what future bytes cost or look like. Reported, then allowed.
    pub notes: Vec<String>,
}

impl SnapshotDiff {
    pub fn is_clean(&self) -> bool {
        self.blocking.is_empty() && self.notes.is_empty()
    }
}

impl ConfigSnapshot {
    /// Field-by-field diff, split by whether it can actually break the run.
    pub fn diff(&self, other: &ConfigSnapshot) -> SnapshotDiff {
        let mut out = SnapshotDiff::default();
        let mut blocking = |name: &str, a: String, b: String| {
            if a != b {
                out.blocking
                    .push(format!("{}: checkpoint={} 当前={}", name, a, b));
            }
        };
        blocking("bucket", self.bucket.clone(), other.bucket.clone());
        blocking("key_prefix", self.key_prefix.clone(), other.key_prefix.clone());
        blocking(
            "endpoint_url",
            format!("{:?}", self.endpoint_url),
            format!("{:?}", other.endpoint_url),
        );

        let mut d = Vec::new();
        let mut cmp = |name: &str, a: String, b: String| {
            if a != b {
                d.push(format!("{}: checkpoint={} 当前={}", name, a, b));
            }
        };
        cmp("mode", self.mode.to_string(), other.mode.to_string());
        cmp(
            "transfer_acceleration",
            self.transfer_acceleration.to_string(),
            other.transfer_acceleration.to_string(),
        );
        cmp(
            "budget",
            super::fmt_usd(self.budget_micro),
            super::fmt_usd(other.budget_micro),
        );
        cmp(
            "daily_cap",
            fmt_daily_cap(self.daily_cap_micro),
            fmt_daily_cap(other.daily_cap_micro),
        );
        // Zero / empty means the checkpoint predates the field, not that the
        // value was zero — reporting "checkpoint=0 B 当前=1.00 GiB" would read
        // like a defect rather than like an older file.
        if self.object_size_min != 0 {
            cmp(
                "object_size_min",
                fmt_bytes(self.object_size_min),
                fmt_bytes(other.object_size_min),
            );
        }
        if self.object_size_max != 0 {
            cmp(
                "object_size_max",
                fmt_bytes(self.object_size_max),
                fmt_bytes(other.object_size_max),
            );
        }
        if !self.object_name.is_empty() {
            cmp("object_name", self.object_name.clone(), other.object_name.clone());
        }
        if !self.object_ext.is_empty() {
            cmp("object_ext", self.object_ext.clone(), other.object_ext.clone());
        }
        cmp("part_size", fmt_bytes(self.part_size), fmt_bytes(other.part_size));
        cmp(
            "retain",
            format!("{}s", self.retain_secs),
            format!("{}s", other.retain_secs),
        );
        out.notes = d;
        out
    }
}

/// The hard per-UTC-day ceiling `budget ÷ days`. The division is the whole
/// definition of `--days`, and it lives here alone so the prompt that offers
/// the split and the validation that accepts it can never disagree about which
/// splits are usable — or word the refusal two different ways.
pub fn split_over_days(budget_micro: u64, days: u64) -> Result<u64> {
    if days == 0 {
        bail!("--days 必须 ≥ 1");
    }
    let per_day = budget_micro / days;
    if per_day == 0 {
        bail!(
            "预算 {} 摊到 {} 天,每天不足 $0.000001 —— 天数太多或预算太小",
            super::fmt_usd(budget_micro),
            days
        );
    }
    Ok(per_day)
}

/// "无" rather than "$0.00": a run without `--days` has no daily ceiling at
/// all, which is a different thing from one whose ceiling rounds to nothing.
fn fmt_daily_cap(micro: u64) -> String {
    if micro == 0 {
        "无".to_string()
    } else {
        super::fmt_usd(micro)
    }
}

/// Jitter half-width around a planned average. The mean of a uniform draw over
/// `[0.6x, 1.4x]` is exactly x, so the plan still lands on target while keeping
/// the wobble that is part of how this tool writes. One shape for both places
/// that wobble: the rate, and the hourly ceiling `--days` draws (see s3::quota).
pub const PACE_JITTER: f64 = 0.4;

/// The `[min, max]` a planned average is sampled from.
pub fn jitter_bounds(avg: u64) -> (u64, u64) {
    let avg = avg as f64;
    (
        (avg * (1.0 - PACE_JITTER)) as u64,
        (avg * (1.0 + PACE_JITTER)) as u64,
    )
}

/// Highest paced rate worth believing on one instance (10 Gbps). Above this the
/// plan is network-bound rather than budget-bound, so the run just takes longer
/// than asked — worth saying out loud, not worth refusing.
pub const IMPLAUSIBLE_RATE: u64 = 1280 * MIB;

/// The rate range that spends `total_bytes` evenly across `duration`.
///
/// This is exact, not a feedback loop: the byte count a budget buys is fixed by
/// the cost model and does not depend on the rate, so the rate is a plain
/// division. It also makes resumes self-correcting — a constant rate over a
/// proportionally smaller remainder lands on the same total active time.
pub fn pace_rate(total_bytes: u64, duration: Duration) -> Result<(u64, u64)> {
    let secs = duration.as_secs_f64();
    if secs <= 0.0 {
        bail!("--duration 必须 > 0");
    }
    let (min, max) = jitter_bounds((total_bytes as f64 / secs) as u64);
    if min == 0 {
        bail!(
            "--duration {} 太长:摊下来平均速率不足 1 B/s(总写入量 {})",
            humantime::format_duration(duration),
            fmt_bytes(total_bytes)
        );
    }
    Ok((min, max))
}

/// Where a run keeps its state: checkpoint, summary, single-instance lock.
///
/// The identity is `(endpoint, bucket, key_prefix)` — the thing one budget
/// ledger belongs to. The old default keyed it on the working directory
/// (`./yo-s3.ckpt.json`), which meant two runs launched from two directories
/// kept two ledgers against one budget and quietly burned it twice.
///
/// Pure path computation; `ensure_state_dir` creates it.
pub fn state_dir(
    endpoint_url: Option<&str>,
    bucket: &str,
    key_prefix: &str,
    dry_run: bool,
) -> Result<PathBuf> {
    let home = dirs_next::home_dir().context("无法定位 home 目录,状态目录 ~/.yo/s3 不可用")?;
    let mut hasher = Sha256::new();
    for part in [endpoint_url.unwrap_or(""), bucket, key_prefix] {
        hasher.update(part.as_bytes());
        hasher.update([0u8]); // separator: ("a","bc") must not collide with ("ab","c")
    }
    let digest = hasher.finalize();
    let id: String = digest[..4].iter().map(|b| format!("{:02x}", b)).collect();

    let mut dir = home
        .join(".yo")
        .join("s3")
        .join(format!("{}-{}", dir_safe(bucket), id));
    if dry_run {
        // A rehearsal must never touch the real ledger: --dry-run still walks
        // the scheduler and books the burn of objects it never uploaded, so
        // sharing a state dir would write money that was never spent into the
        // real checkpoint. Its own directory also means a dry run neither
        // takes nor waits on the real run's lock.
        dir.push("dry-run");
    }
    Ok(dir)
}

/// Create the state directory, private to the user like `~/.yo/github`.
pub fn ensure_state_dir(dir: &Path) -> Result<()> {
    fs::create_dir_all(dir).with_context(|| format!("创建状态目录失败: {}", dir.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(dir, fs::Permissions::from_mode(0o700))
            .with_context(|| format!("设置状态目录权限失败: {}", dir.display()))?;
    }
    Ok(())
}

/// Keep a bucket name readable as a directory component. Custom endpoints
/// (MinIO/Ceph) do not enforce S3 bucket naming, so nothing here is assumed;
/// the hash appended by `state_dir` carries the actual identity.
fn dir_safe(bucket: &str) -> String {
    let safe: String = bucket
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '.' {
                c
            } else {
                '_'
            }
        })
        .take(40)
        .collect();
    if safe.is_empty() {
        "bucket".to_string()
    } else {
        safe
    }
}

/// Parse a human size like "1TiB" / "256MiB" / "2GB" into bytes.
pub fn parse_size(s: &str) -> Result<u64, String> {
    byte_unit::Byte::parse_str(s, true)
        .map(|b| b.as_u64())
        .map_err(|e| format!("无法解析大小 '{}': {}", s, e))
}

/// Parse a rate like "200MiB" or "200MiB/s" into bytes per second.
pub fn parse_rate(s: &str) -> Result<u64, String> {
    let trimmed = s.trim().trim_end_matches("/s").trim_end_matches("/S");
    parse_size(trimmed)
}

/// Parse a duration like "30s" / "48h".
pub fn parse_duration(s: &str) -> Result<Duration, String> {
    humantime::parse_duration(s).map_err(|e| format!("无法解析时长 '{}': {}", s, e))
}

/// Parse a dollar amount like "500" / "12.5" into micro-dollars.
pub fn parse_usd(s: &str) -> Result<u64, String> {
    let v: f64 = s
        .trim()
        .trim_start_matches('$')
        .parse()
        .map_err(|e| format!("无法解析金额 '{}': {}", s, e))?;
    if !(v > 0.0) || v > 1_000_000.0 {
        return Err(format!("金额 {} 超出合理范围 (0, 1000000]", v));
    }
    Ok((v * 1_000_000.0).round() as u64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::s3::{GIB, MIB, TIB};

    fn base() -> BenchConfig {
        BenchConfig {
            mode: ModeId::Crr,
            bucket: "b".into(),
            key_prefix: "yo-s3-bench/".into(),
            budget_micro: 500_000_000,
            dest_regions: Vec::new(),
            region: None,
            endpoint_url: None,
            path_style: false,
            insecure_skip_tls_verify: false,
            transfer_acceleration: false,
            object_size_min: GIB,
            object_size_max: 10 * GIB,
            object_name: "db-backup".into(),
            object_ext: "tar.gz".into(),
            part_size: 256 * MIB,
            pool_size: 2 * GIB,
            concurrent_objects: 1,
            concurrent_parts: 4,
            rate_min: 200 * MIB,
            rate_max: 500 * MIB,
            rate_mode: RateMode::Continuous,
            rate_resample_interval: Duration::from_secs(30),
            retain: Duration::from_secs(86400),
            total_size: None,
            iterations: None,
            stop_when: StopWhen::All,
            max_duration: None,
            days: None,
            checkpoint_path: "ckpt.json".into(),
            summary_out: "summary.json".into(),
            report_interval: Duration::from_secs(10),
            dry_run: false,
            yes: true,
        }
    }

    #[test]
    fn valid_default_config_passes() {
        base().validate().unwrap();
    }

    /// The part limit must be checked against the BIG end of the size range —
    /// a range whose max blows the limit is unusable even if its min is fine.
    #[test]
    fn too_many_parts_rejected_with_suggestion() {
        let mut c = base();
        c.object_size_max = TIB;
        c.part_size = 5 * MIB; // 1 TiB / 5 MiB ≫ 10_000
        let err = c.validate().unwrap_err().to_string();
        assert!(err.contains("10000"), "{}", err);
    }

    #[test]
    fn inverted_size_range_is_rejected() {
        let mut c = base();
        c.object_size_min = 10 * GIB;
        c.object_size_max = GIB;
        let err = c.validate().unwrap_err().to_string();
        assert!(err.contains("min ≤ max"), "{}", err);
    }

    /// Sizes must vary — a fixed size is what made every object identical and
    /// the console listing look machine-generated.
    #[test]
    fn sampled_sizes_stay_in_range_and_vary() {
        let c = base(); // 1 GiB – 10 GiB
        let mut seen = std::collections::HashSet::new();
        for _ in 0..50 {
            let s = c.sample_object_size();
            assert!((GIB..=10 * GIB).contains(&s), "{} 越界", s);
            seen.insert(s);
        }
        assert!(seen.len() > 40, "取样几乎不变化: {} 个不同值", seen.len());
        assert_eq!(c.object_size_avg(), GIB + (10 * GIB - GIB) / 2);
    }

    /// A degenerate range is a fixed size, not an error.
    #[test]
    fn equal_bounds_give_a_fixed_size() {
        let mut c = base();
        c.object_size_min = 4 * GIB;
        c.object_size_max = 4 * GIB;
        c.validate().unwrap();
        assert_eq!(c.sample_object_size(), 4 * GIB);
    }

    #[test]
    fn pool_must_cover_two_parts() {
        let mut c = base();
        c.pool_size = 300 * MIB;
        assert!(c.validate().is_err());
    }

    #[test]
    fn parse_helpers() {
        assert_eq!(parse_size("256MiB").unwrap(), 256 * MIB);
        assert_eq!(parse_rate("200MiB/s").unwrap(), 200 * MIB);
        assert_eq!(parse_usd("500").unwrap(), 500_000_000);
        assert_eq!(parse_duration("30s").unwrap(), Duration::from_secs(30));
    }

    /// Changing what future bytes cost or look like is reported, not refused —
    /// none of it can make the money already burned wrong.
    #[test]
    fn pricing_and_layout_changes_are_notes_not_blockers() {
        let a = base().snapshot();
        let mut c = base();
        c.part_size = 512 * MIB;
        c.budget_micro = 100_000_000;
        c.object_name = "other".into();
        c.object_size_max = 20 * GIB;
        let d = a.diff(&c.snapshot());
        assert!(d.blocking.is_empty(), "不该拦: {:?}", d.blocking);
        assert_eq!(d.notes.len(), 4, "{:?}", d.notes);
    }

    /// Moving the data IS refused: objects under the old prefix would fall out
    /// of the retention sweeper's scope and bill storage forever.
    #[test]
    fn moving_the_data_blocks_resume() {
        let a = base().snapshot();
        for mutate in [
            (|c: &mut BenchConfig| c.bucket = "other".into()) as fn(&mut BenchConfig),
            |c: &mut BenchConfig| c.key_prefix = "elsewhere/".into(),
            |c: &mut BenchConfig| c.endpoint_url = Some("http://minio:9000".into()),
        ] {
            let mut c = base();
            mutate(&mut c);
            assert_eq!(a.diff(&c.snapshot()).blocking.len(), 1);
        }
    }

    /// Acceleration defaults to auto, never to a forced on/off — forcing it on
    /// would add a fee AWS does not charge when client and bucket share a
    /// region, which is this tool's most common deployment.
    #[test]
    fn acceleration_defaults_to_auto() {
        assert_eq!(AccelMode::default(), AccelMode::Auto);
    }

    /// TA changes what future bytes cost. Already-burned money stays valid —
    /// it was real money at the price in force when it was spent.
    #[test]
    fn toggling_acceleration_is_reported_not_refused() {
        let a = base().snapshot();
        let mut c = base();
        c.transfer_acceleration = true;
        let d = a.diff(&c.snapshot());
        assert!(d.blocking.is_empty());
        assert_eq!(d.notes.len(), 1);
        assert!(d.notes[0].contains("transfer_acceleration"), "{:?}", d.notes);
    }

    /// The whole point of `--duration`: the estimate derives wall time from
    /// `(rate_min + rate_max) / 2`, so the paced range must feed that formula
    /// back the number the user asked for.
    #[test]
    fn duration_plan_reproduces_the_requested_wall_time() {
        let total_bytes = 5 * TIB;
        for hours in [1u64, 4, 24, 168] {
            let target = Duration::from_secs(hours * 3600);
            let (min, max) = pace_rate(total_bytes, target).unwrap();
            let avg = (min + max) / 2;
            let secs = total_bytes as f64 / avg as f64;
            let want = target.as_secs_f64();
            assert!(
                (secs - want).abs() / want < 0.001,
                "{}h 规划反推出 {}s(应为 {}s)",
                hours,
                secs,
                want
            );
            assert!(min < max);
        }
    }

    /// Pacing must not flatten the rate — the random wobble is part of how this
    /// tool writes, `--duration` only moves its centre.
    #[test]
    fn paced_range_stays_jittery_and_centred() {
        let (min, max) = pace_rate(100 * GIB, Duration::from_secs(3600)).unwrap();
        let avg = (min + max) / 2;
        assert!(avg.abs_diff(100 * GIB / 3600) <= 1);
        assert!(max > min * 2, "抖动被压平了: {} – {}", min, max);
    }

    #[test]
    fn impossible_durations_are_refused_with_a_reason() {
        assert!(pace_rate(TIB, Duration::ZERO).is_err());
        // Spread a kilobyte over a year and the floor of the range rounds to 0.
        let err = pace_rate(1024, Duration::from_secs(86_400 * 365))
            .unwrap_err()
            .to_string();
        assert!(err.contains("--duration"), "{}", err);
    }

    /// One budget ledger per (endpoint, bucket, prefix) — and the same inputs
    /// must always land on the same directory, or a resume silently restarts
    /// from zero and burns the budget again.
    #[test]
    fn state_dir_identity_is_endpoint_bucket_prefix() {
        let d = |b, p| state_dir(None, b, p, false).unwrap();
        assert_eq!(d("bkt", "yo-s3-bench/"), d("bkt", "yo-s3-bench/"));
        assert_ne!(d("bkt", "yo-s3-bench/"), d("bkt", "other/"));
        assert_ne!(d("bkt", "yo-s3-bench/"), d("other-bkt", "yo-s3-bench/"));
        assert_ne!(
            state_dir(None, "bkt", "p/", false).unwrap(),
            state_dir(Some("http://minio:9000"), "bkt", "p/", false).unwrap()
        );
        // Field boundaries must be real: ("ab","c/") is not ("a","bc/").
        assert_ne!(d("ab", "c/"), d("a", "bc/"));
    }

    /// A dry run books burn for objects it never uploaded, so it must not be
    /// able to reach the real checkpoint or the real lock.
    #[test]
    fn dry_run_state_is_isolated() {
        let real = state_dir(None, "bkt", "p/", false).unwrap();
        let dry = state_dir(None, "bkt", "p/", true).unwrap();
        assert_ne!(real, dry);
        assert!(dry.starts_with(&real));
    }

    /// Custom endpoints do not enforce S3 bucket naming, so the bucket string
    /// must never be able to walk out of ~/.yo/s3.
    #[test]
    fn bucket_name_stays_one_path_component() {
        let dir = state_dir(None, "../../etc/evil", "p/", false).unwrap();
        let root = dirs_next::home_dir().unwrap().join(".yo").join("s3");
        assert_eq!(dir.parent().unwrap(), root);
        assert!(!dir.file_name().unwrap().to_string_lossy().contains('/'));
    }

    /// `--days` is just the budget divided by the days — the estimate page, the
    /// checkpoint ledger and the planner all read this one number.
    #[test]
    fn the_daily_ceiling_is_the_budget_divided_by_the_days() {
        let mut c = base(); // $500
        assert_eq!(c.daily_cap_micro(), None);
        c.days = Some(30);
        c.validate().unwrap();
        assert_eq!(c.daily_cap_micro(), Some(500_000_000 / 30));
    }

    /// Spreading a budget so thin that a day buys nothing is a typo, not a plan.
    #[test]
    fn a_budget_too_thin_to_split_is_refused_with_the_arithmetic() {
        let mut c = base();
        c.budget_micro = 100; // $0.0001
        c.days = Some(365);
        let err = c.validate().unwrap_err().to_string();
        assert!(err.contains("365"), "{}", err);
    }

    /// A resume that quietly drops `--days` must not take the ceiling with it
    /// in silence — it is a note, not a blocker, because the money already
    /// burned stays valid either way.
    #[test]
    fn dropping_the_daily_ceiling_is_reported_not_refused() {
        let mut with_cap = base();
        with_cap.days = Some(30);
        let d = with_cap.snapshot().diff(&base().snapshot());
        assert!(d.blocking.is_empty(), "不该拦: {:?}", d.blocking);
        assert_eq!(d.notes.len(), 1, "{:?}", d.notes);
        assert!(d.notes[0].contains("daily_cap"), "{:?}", d.notes);
        assert!(d.notes[0].contains("无"), "取消日上限要说清楚: {:?}", d.notes);
    }

    #[test]
    fn switching_mode_is_reported_not_refused() {
        let a = base().snapshot();
        let mut c = base();
        c.mode = ModeId::WriteOnly;
        let d = a.diff(&c.snapshot());
        assert!(d.blocking.is_empty());
        assert!(d.notes[0].contains("mode"), "{:?}", d.notes);
    }
}
