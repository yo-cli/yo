// Resolved run configuration + validation + the snapshot embedded in checkpoints.

use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use std::time::Duration;

use super::{fmt_bytes, GIB, MAX_OBJECT_SIZE, MAX_PARTS_PER_OBJECT, MAX_PART_SIZE, MIB, MIN_PART_SIZE};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub enum RateMode {
    /// Resample the target rate every --rate-resample-interval
    Continuous,
    /// Sample the target rate once at the start of each object
    PerObject,
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
    pub bucket: String,
    pub key_prefix: String,
    pub budget_micro: u64,
    pub region: Option<String>,
    pub endpoint_url: Option<String>,
    pub path_style: bool,
    pub insecure_skip_tls_verify: bool,

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
            bucket: "b".into(),
            key_prefix: "yo-s3-bench/".into(),
            budget_micro: 500_000_000,
            region: None,
            endpoint_url: None,
            path_style: false,
            insecure_skip_tls_verify: false,
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
}
