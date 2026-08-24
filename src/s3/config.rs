// Resolved run configuration + validation + the snapshot embedded in
// checkpoints + where a run's state lives on disk.

use anyhow::{bail, Context, Result};
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

    pub object_size: u64,
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
        if self.object_size > MAX_OBJECT_SIZE {
            bail!(
                "对象大小 {} 超出 S3 单对象上限 {}",
                fmt_bytes(self.object_size),
                fmt_bytes(MAX_OBJECT_SIZE)
            );
        }
        if self.object_size < MIB {
            bail!("对象大小 {} 太小(至少 1 MiB)", fmt_bytes(self.object_size));
        }
        let parts = self.object_size.div_ceil(self.part_size);
        if parts > MAX_PARTS_PER_OBJECT {
            // e.g. 1 TiB / 10_000 ≈ 110 MiB → suggest the next power-of-two part size
            let min_part = self.object_size.div_ceil(MAX_PARTS_PER_OBJECT);
            bail!(
                "对象 {} ÷ part {} = {} 个 part,超出 S3 上限 {}。part 至少要 {}",
                fmt_bytes(self.object_size),
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
        Ok(())
    }

    pub fn snapshot(&self) -> ConfigSnapshot {
        ConfigSnapshot {
            mode: self.mode,
            transfer_acceleration: self.transfer_acceleration,
            bucket: self.bucket.clone(),
            key_prefix: self.key_prefix.clone(),
            budget_micro: self.budget_micro,
            endpoint_url: self.endpoint_url.clone(),
            object_size: self.object_size,
            part_size: self.part_size,
            rate_min: self.rate_min,
            rate_max: self.rate_max,
            rate_mode: self.rate_mode,
            retain_secs: self.retain.as_secs(),
        }
    }
}

/// The subset of config that must not change across --resume: fields that
/// affect where data goes, how it is laid out, or what "done" means.
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
    pub endpoint_url: Option<String>,
    pub object_size: u64,
    pub part_size: u64,
    pub rate_min: u64,
    pub rate_max: u64,
    pub rate_mode: RateMode,
    pub retain_secs: u64,
}

impl ConfigSnapshot {
    /// Field-by-field diff for the resume mismatch error.
    pub fn diff(&self, other: &ConfigSnapshot) -> Vec<String> {
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
        cmp("bucket", self.bucket.clone(), other.bucket.clone());
        cmp("key_prefix", self.key_prefix.clone(), other.key_prefix.clone());
        cmp(
            "budget",
            super::fmt_usd(self.budget_micro),
            super::fmt_usd(other.budget_micro),
        );
        cmp(
            "endpoint_url",
            format!("{:?}", self.endpoint_url),
            format!("{:?}", other.endpoint_url),
        );
        cmp("object_size", fmt_bytes(self.object_size), fmt_bytes(other.object_size));
        cmp("part_size", fmt_bytes(self.part_size), fmt_bytes(other.part_size));
        cmp("rate_min", super::fmt_rate(self.rate_min), super::fmt_rate(other.rate_min));
        cmp("rate_max", super::fmt_rate(self.rate_max), super::fmt_rate(other.rate_max));
        cmp("rate_mode", format!("{:?}", self.rate_mode), format!("{:?}", other.rate_mode));
        cmp(
            "retain",
            format!("{}s", self.retain_secs),
            format!("{}s", other.retain_secs),
        );
        d
    }
}

/// Jitter half-width around the paced average. The mean of a uniform draw over
/// `[0.6R, 1.4R]` is exactly R, so the planned duration still lands on target
/// while the rate keeps the wobble that is part of how this tool writes.
const PACE_JITTER: f64 = 0.4;

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
    let avg = total_bytes as f64 / secs;
    let min = (avg * (1.0 - PACE_JITTER)) as u64;
    let max = (avg * (1.0 + PACE_JITTER)) as u64;
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
            object_size: TIB,
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

    #[test]
    fn too_many_parts_rejected_with_suggestion() {
        let mut c = base();
        c.part_size = 5 * MIB; // 1 TiB / 5 MiB ≫ 10_000
        let err = c.validate().unwrap_err().to_string();
        assert!(err.contains("10000"), "{}", err);
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

    #[test]
    fn snapshot_diff_lists_changed_fields() {
        let a = base().snapshot();
        let mut c = base();
        c.part_size = 512 * MIB;
        c.budget_micro = 100_000_000;
        let d = a.diff(&c.snapshot());
        assert_eq!(d.len(), 2);
    }

    /// Acceleration defaults to auto, never to a forced on/off — forcing it on
    /// would add a fee AWS does not charge when client and bucket share a
    /// region, which is this tool's most common deployment.
    #[test]
    fn acceleration_defaults_to_auto() {
        assert_eq!(AccelMode::default(), AccelMode::Auto);
    }

    #[test]
    fn toggling_acceleration_blocks_resume() {
        // TA changes the per-byte rate; burned_micro from the other setting
        // would be accounted at the wrong price.
        let a = base().snapshot();
        let mut c = base();
        c.transfer_acceleration = true;
        let d = a.diff(&c.snapshot());
        assert_eq!(d.len(), 1);
        assert!(d[0].contains("transfer_acceleration"), "{:?}", d);
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

    #[test]
    fn switching_mode_blocks_resume() {
        // Different engine = different cost accounting; burned_micro from the
        // old mode would be meaningless. Must surface as a diff, not silence.
        let a = base().snapshot();
        let mut c = base();
        c.mode = ModeId::WriteOnly;
        let d = a.diff(&c.snapshot());
        assert_eq!(d.len(), 1);
        assert!(d[0].contains("mode"), "{:?}", d);
    }
}
