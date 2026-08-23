// Region pricing table, the pre-run cost estimate page, and the suggested
// lifecycle policy JSON. Prices are built-in approximations (S3 Standard,
// public list prices); actuals come from the AWS bill — the page says so.

use colored::Colorize;
use serde::Serialize;
use std::path::Path;

use super::config::BenchConfig;
use super::{fmt_bytes, fmt_rate, fmt_usd, GIB};

#[derive(Debug, Clone, Serialize)]
pub struct Pricing {
    pub region: String,
    /// PUT/COPY/POST/LIST per 1000 requests, USD
    pub put_per_1k_usd: f64,
    /// S3 Standard storage per GB-month, USD
    pub storage_gb_month_usd: f64,
    /// Inter-region replication transfer per GB FROM this region, USD
    pub crr_per_gb_usd: f64,
    /// True when the region was not in the table and us-east-1 prices are used
    pub assumed: bool,
}

impl Pricing {
    pub fn request_micro(&self, n: u64) -> u64 {
        (n as f64 * self.put_per_1k_usd / 1000.0 * 1_000_000.0).round() as u64
    }

    pub fn transfer_micro(&self, bytes: u64) -> u64 {
        (bytes as f64 * self.transfer_micro_per_byte()).round() as u64
    }

    /// micro-dollars per byte (AWS bills GB as 2^30 bytes)
    pub fn transfer_micro_per_byte(&self) -> f64 {
        self.crr_per_gb_usd * 1_000_000.0 / GIB as f64
    }

    pub fn storage_micro_for(&self, bytes: u64, hours: f64) -> u64 {
        let gb_month = bytes as f64 / GIB as f64 * hours / (30.0 * 24.0);
        (gb_month * self.storage_gb_month_usd * 1_000_000.0).round() as u64
    }
}

// (region, put_per_1k, storage_gb_month, crr_transfer_out_per_gb)
const PRICE_TABLE: &[(&str, f64, f64, f64)] = &[
    ("us-east-1", 0.005, 0.023, 0.02),
    ("us-east-2", 0.005, 0.023, 0.02),
    ("us-west-1", 0.0055, 0.026, 0.02),
    ("us-west-2", 0.005, 0.023, 0.02),
    ("eu-west-1", 0.005, 0.023, 0.02),
    ("eu-west-2", 0.0053, 0.024, 0.02),
    ("eu-central-1", 0.0054, 0.0245, 0.02),
    ("ap-northeast-1", 0.0047, 0.025, 0.09),
    ("ap-northeast-2", 0.0045, 0.025, 0.08),
    ("ap-southeast-1", 0.005, 0.025, 0.09),
    ("ap-southeast-2", 0.0055, 0.025, 0.098),
    ("ap-south-1", 0.005, 0.025, 0.086),
];

pub fn pricing_for(region: &str) -> Pricing {
    for (r, put, storage, crr) in PRICE_TABLE {
        if *r == region {
            return Pricing {
                region: region.to_string(),
                put_per_1k_usd: *put,
                storage_gb_month_usd: *storage,
                crr_per_gb_usd: *crr,
                assumed: false,
            };
        }
    }
    Pricing {
        region: region.to_string(),
        put_per_1k_usd: 0.005,
        storage_gb_month_usd: 0.023,
        crr_per_gb_usd: 0.02,
        assumed: true,
    }
}

/// Print the pre-run estimate page: how many bytes the budget buys, the fee
/// breakdown, expected duration, and the traps worth knowing before burning.
pub fn print_estimate(cfg: &BenchConfig, pricing: &Pricing, crr_enabled: bool, dest: Option<&str>) {
    println!("\n{}", "📊 成本预估(内置近似单价,最终以 AWS 账单为准)".cyan().bold());
    if pricing.assumed {
        println!(
            "{}",
            format!("⚠ 区域 {} 不在内置价格表,按 us-east-1 单价估算", pricing.region).yellow()
        );
    }

    let budget = cfg.budget_micro;
    if crr_enabled {
        // bytes the budget buys ≈ budget / (transfer + request fees per byte)
        let per_byte = pricing.transfer_micro_per_byte()
            + pricing.request_micro(1) as f64 / cfg.part_size as f64;
        let total_bytes = (budget as f64 / per_byte) as u64;
        let n_objects = total_bytes.div_ceil(cfg.object_size);
        let n_requests = total_bytes.div_ceil(cfg.part_size) + n_objects * 3;
        let transfer = pricing.transfer_micro(total_bytes);
        let requests = pricing.request_micro(n_requests);
        let avg_rate = (cfg.rate_min + cfg.rate_max) / 2;
        let secs = total_bytes as f64 / avg_rate as f64;
        let retain_hours = cfg.retain.as_secs_f64() / 3600.0;
        // Steady-state stored data = write rate × retention window, both regions
        let window_bytes = ((avg_rate as f64 * cfg.retain.as_secs_f64()) as u64).min(total_bytes);
        let storage = pricing.storage_micro_for(window_bytes * 2, retain_hours.max(secs / 3600.0));

        println!("  预算(硬上限):        {}", fmt_usd(budget).green().bold());
        println!(
            "  ├ 跨区流量费:         {}  (${}/GB × {})",
            fmt_usd(transfer),
            pricing.crr_per_gb_usd,
            fmt_bytes(total_bytes)
        );
        println!("  └ 请求费:             {}  ({} 次请求)", fmt_usd(requests), n_requests);
        println!("  预计写入总量:         {}  (约 {} 个对象)", fmt_bytes(total_bytes).bold(), n_objects);
        println!(
            "  预计耗时:             {}  (按平均速率 {} 估)",
            humantime::format_duration(std::time::Duration::from_secs(secs as u64)).to_string().bold(),
            fmt_rate(avg_rate)
        );
        if let Some(d) = dest {
            println!("  复制目标桶:           {}", d);
        }
        println!(
            "  {} {}  (保留 {:.0}h × 两个区域,不计入预算停止判据,随保留时长另计)",
            "附:存储费估算:".dimmed(),
            fmt_usd(storage),
            retain_hours
        );
    } else {
        let reqs_per_tib = (super::TIB).div_ceil(cfg.part_size) + 3;
        let usd_per_tib = pricing.request_micro(reqs_per_tib);
        println!(
            "{}",
            "⚠ 未启用跨区复制 —— 烧钱主引擎缺失!".yellow().bold()
        );
        println!(
            "  纯写入只产生请求费:每写 1 TiB 仅 {},烧完 {} 需要写约 {}",
            fmt_usd(usd_per_tib),
            fmt_usd(budget),
            fmt_bytes((budget as f64 / usd_per_tib as f64 * super::TIB as f64) as u64)
        );
        println!("  存储费按月缓慢发酵,无法在运行期精确控停。建议先跑 yo-s3 setup-crr。");
    }
    println!(
        "  {} 若本机经 NAT 网关访问 S3,另有 NAT 流量费;用免费的 S3 Gateway Endpoint 可避免(工具无法自动检测,请自查 VPC)",
        "ℹ".blue()
    );
}

#[derive(Serialize)]
struct LifecycleRule<'a> {
    #[serde(rename = "ID")]
    id: &'a str,
    #[serde(rename = "Filter")]
    filter: serde_json::Value,
    #[serde(rename = "Status")]
    status: &'a str,
    #[serde(rename = "Expiration")]
    expiration: serde_json::Value,
    #[serde(rename = "NoncurrentVersionExpiration")]
    noncurrent: serde_json::Value,
    #[serde(rename = "AbortIncompleteMultipartUpload")]
    abort_mpu: serde_json::Value,
}

/// Suggested lifecycle policy: the last line of defense if the tool is killed
/// hard and nobody sweeps — caps the damage at ~2 days of storage.
pub fn lifecycle_json(prefix: &str) -> String {
    let rule = LifecycleRule {
        id: "yo-s3-burn-cleanup",
        filter: serde_json::json!({ "Prefix": prefix }),
        status: "Enabled",
        expiration: serde_json::json!({ "Days": 2 }),
        noncurrent: serde_json::json!({ "NoncurrentDays": 1 }),
        abort_mpu: serde_json::json!({ "DaysAfterInitiation": 1 }),
    };
    serde_json::to_string_pretty(&serde_json::json!({ "Rules": [rule] })).unwrap()
}

/// Write the suggested lifecycle JSON files and print how to apply them.
pub fn write_lifecycle_files(prefix: &str, source_bucket: &str, dest_bucket: Option<&str>) {
    let json = lifecycle_json(prefix);
    let mut targets = vec![("./yo-s3-lifecycle-source.json".to_string(), source_bucket.to_string())];
    if let Some(d) = dest_bucket {
        targets.push(("./yo-s3-lifecycle-dest.json".to_string(), d.to_string()));
    }
    for (file, bucket) in &targets {
        if std::fs::write(Path::new(file), &json).is_ok() {
            println!(
                "{} 建议的 lifecycle 兜底规则已生成: {}(手动应用: aws s3api put-bucket-lifecycle-configuration --bucket {} --lifecycle-configuration file://{})",
                "ℹ".blue(),
                file.bold(),
                bucket,
                file
            );
        }
    }
    println!(
        "  {}",
        "该规则仅作用于本工具前缀;若桶上已有 lifecycle 配置,手动合并后再应用,避免整体覆盖".dimmed()
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::s3::GIB;

    #[test]
    fn known_region_and_fallback() {
        assert!(!pricing_for("us-east-1").assumed);
        let p = pricing_for("mars-north-1");
        assert!(p.assumed);
        assert_eq!(p.crr_per_gb_usd, 0.02);
    }

    #[test]
    fn transfer_cost_math() {
        let p = pricing_for("us-east-1");
        // 1 GiB at $0.02/GB → exactly $0.02 = 20_000 micro
        assert_eq!(p.transfer_micro(GIB), 20_000);
        // 1000 requests at $0.005/1k → $0.005
        assert_eq!(p.request_micro(1000), 5_000);
    }

    #[test]
    fn lifecycle_json_contains_all_three_rules() {
        let j = lifecycle_json("yo-s3-bench/");
        assert!(j.contains("Expiration"));
        assert!(j.contains("NoncurrentVersionExpiration"));
        assert!(j.contains("AbortIncompleteMultipartUpload"));
        assert!(j.contains("yo-s3-bench/"));
    }
}
