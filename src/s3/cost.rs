// Region pricing table, the pre-run cost estimate page, and the suggested
// lifecycle policy JSON. Prices are built-in approximations (S3 Standard,
// public list prices); actuals come from the AWS bill — the page says so.

use colored::Colorize;
use serde::Serialize;
use std::path::Path;

use super::config::BenchConfig;
use super::modes::BurnMode;
use super::netpath::EgressPath;
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

    pub fn storage_micro_for(&self, bytes: u64, hours: f64) -> u64 {
        let gb_month = bytes as f64 / GIB as f64 * hours / (30.0 * 24.0);
        (gb_month * self.storage_gb_month_usd * 1_000_000.0).round() as u64
    }

    /// Convert a per-GB list price into micro-dollars per byte
    /// (AWS bills a "GB" of transfer as 2^30 bytes).
    pub fn micro_per_byte(per_gb_usd: f64) -> f64 {
        per_gb_usd * 1_000_000.0 / GIB as f64
    }
}

/// One per-byte fee charged on the bytes this run writes. These are what make
/// a budget stoppable: cost accrues the instant bytes move, linearly.
///
/// Fees STACK — the same byte can be billed by several at once (replication to
/// K regions, replication time control on top of that, an accelerated upload
/// endpoint, a NAT gateway on the upload path). Hence a list, not one fee.
#[derive(Debug, Clone)]
pub struct TransferFee {
    /// Fee line label on the estimate page, e.g. "跨区复制流量费".
    pub label: String,
    pub micro_per_byte: f64,
}

impl TransferFee {
    pub fn per_gb(label: impl Into<String>, per_gb_usd: f64) -> Self {
        Self {
            label: label.into(),
            micro_per_byte: Pricing::micro_per_byte(per_gb_usd),
        }
    }

    pub fn per_gb_usd(&self) -> f64 {
        self.micro_per_byte * GIB as f64 / 1_000_000.0
    }
}

/// How a burn mode turns bytes into immediate cost. The budget ledger plans
/// and accounts purely from this — no mode-specific branches in budget.rs.
#[derive(Debug, Clone)]
pub struct CostModel {
    /// Every per-byte fee the written bytes incur. Empty = request fees only,
    /// and the budget cannot stop the run (see `budget_drives_stop`).
    pub transfer: Vec<TransferFee>,
    /// Billable requests per object on top of the per-part uploads
    /// (CreateMultipartUpload + CompleteMultipartUpload + any mode extras).
    pub requests_per_object: u64,
}

impl CostModel {
    /// A mode whose only immediate cost is PUT-class request fees.
    pub fn request_only() -> Self {
        Self {
            transfer: Vec::new(),
            requests_per_object: 2, // create + complete
        }
    }

    /// Total per-byte cost across every stacked fee.
    pub fn micro_per_byte(&self) -> f64 {
        self.transfer.iter().map(|t| t.micro_per_byte).sum()
    }

    pub fn transfer_micro(&self, bytes: u64) -> u64 {
        (bytes as f64 * self.micro_per_byte()).round() as u64
    }

    /// Whether the budget alone can stop the run. False means request fees are
    /// the only immediate cost (~$0.02/TiB) — the run needs --total-size /
    /// --iterations / --max-duration to ever terminate.
    pub fn budget_drives_stop(&self) -> bool {
        self.micro_per_byte() > 0.0
    }
}

/// S3 Transfer Acceleration, data IN accelerated by US/Europe/Japan edge
/// locations. Other edge locations are $0.08/GB — the edge that actually
/// serves the client is not knowable up front, so the estimate page states
/// which rate it assumed.
pub const TA_PER_GB_USD: f64 = 0.04;
pub const TA_OTHER_EDGE_PER_GB_USD: f64 = 0.08;

/// NAT gateway data processing, charged per GB traversing the gateway
/// "regardless of the traffic's source or destination". An S3 gateway VPC
/// endpoint carries the same traffic for free.
pub const NAT_PER_GB_USD: f64 = 0.045;

/// Fees charged on the upload path itself, independent of which burn mode is
/// running — they bill the same bytes any mode already writes.
///
/// `egress` is detected, not declared: see s3::netpath.
pub fn path_surcharges(transfer_acceleration: bool, egress: EgressPath) -> Vec<TransferFee> {
    let mut fees = Vec::new();
    if transfer_acceleration {
        fees.push(TransferFee::per_gb("传输加速费", TA_PER_GB_USD));
    }
    if egress.bills_data_processing() {
        fees.push(TransferFee::per_gb("NAT 网关处理费", NAT_PER_GB_USD));
    }
    fees
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

/// Inter-region transfer pairs that are cheaper than the source region's
/// standard rate. AWS meters transfer per pair (`{src}-{dst}-AWS-Out-Bytes`)
/// and the rate is set by the source region — except for these documented
/// exceptions, so a K-way fan-out is the SUM of K pair rates, not one rate × K.
/// Symmetric: AWS prices "data sent between" these regions at this rate.
const DISCOUNTED_PAIRS: &[(&str, &str, f64)] = &[("us-east-1", "us-east-2", 0.01)];

/// Per-GB replication transfer price from `pricing.region` to `dest_region`.
/// An unknown destination falls back to the source's standard rate, which is
/// the highest it can be — discounts only move the price down, so the budget
/// never under-counts.
pub fn crr_per_gb(pricing: &Pricing, dest_region: Option<&str>) -> f64 {
    let Some(dest) = dest_region else {
        return pricing.crr_per_gb_usd;
    };
    for (a, b, rate) in DISCOUNTED_PAIRS {
        let matches = (pricing.region == *a && dest == *b) || (pricing.region == *b && dest == *a);
        if matches {
            return *rate;
        }
    }
    pricing.crr_per_gb_usd
}

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

/// Pad `label:` out to the estimate page's value column. Fee labels come from
/// the active mode, so the width has to be computed rather than hardcoded —
/// CJK glyphs occupy two terminal columns.
fn pad_label(label: &str) -> String {
    const VALUE_COLUMN: usize = 20;
    let cols: usize = label.chars().map(|c| if c.is_ascii() { 1 } else { 2 }).sum::<usize>() + 1;
    format!("{}:{}", label, " ".repeat(VALUE_COLUMN.saturating_sub(cols)))
}

/// Print the pre-run estimate page: how many bytes the budget buys, the fee
/// breakdown, expected duration, and the traps worth knowing before burning.
/// `cost` is the composed model (mode engine + upload-path surcharges), not
/// `mode.cost_model()` — the mode does not know what the path adds.
pub fn print_estimate(cfg: &BenchConfig, pricing: &Pricing, mode: &dyn BurnMode, cost: &CostModel) {
    println!("\n{}", "📊 成本预估(内置近似单价,最终以 AWS 账单为准)".cyan().bold());
    if pricing.assumed {
        println!(
            "{}",
            format!("⚠ 区域 {} 不在内置价格表,按 us-east-1 单价估算", pricing.region).yellow()
        );
    }
    println!("  烧钱模式:             {}  ({})", mode.id().to_string().bold(), mode.describe());

    let budget = cfg.budget_micro;
    let dests = mode.destinations();
    if cost.budget_drives_stop() {
        // bytes the budget buys ≈ budget / (transfer + request fees per byte)
        let per_byte =
            cost.micro_per_byte() + pricing.request_micro(1) as f64 / cfg.part_size as f64;
        let total_bytes = (budget as f64 / per_byte) as u64;
        let n_objects = total_bytes.div_ceil(cfg.object_size);
        let n_requests = total_bytes.div_ceil(cfg.part_size) + n_objects * cost.requests_per_object;
        let requests = pricing.request_micro(n_requests);
        let avg_rate = (cfg.rate_min + cfg.rate_max) / 2;
        let secs = total_bytes as f64 / avg_rate as f64;
        let retain_hours = cfg.retain.as_secs_f64() / 3600.0;
        // Steady-state stored data = write rate × retention window, both regions
        let window_bytes = ((avg_rate as f64 * cfg.retain.as_secs_f64()) as u64).min(total_bytes);
        // Source plus one stored copy per replication destination.
        let copies = 1 + dests.len() as u64;
        let storage =
            pricing.storage_micro_for(window_bytes * copies, retain_hours.max(secs / 3600.0));

        println!("  预算(硬上限):        {}", fmt_usd(budget).green().bold());
        if cost.transfer.len() > 1 {
            println!(
                "  ├ 每字节合计:         ${:.4}/GB  ({} 项叠加)",
                cost.micro_per_byte() * GIB as f64 / 1_000_000.0,
                cost.transfer.len()
            );
        }
        for fee in &cost.transfer {
            println!(
                "  ├ {}{}  (${:.4}/GB × {})",
                pad_label(&fee.label),
                fmt_usd((total_bytes as f64 * fee.micro_per_byte).round() as u64),
                fee.per_gb_usd(),
                fmt_bytes(total_bytes)
            );
        }
        println!("  └ {}{}  ({} 次请求)", pad_label("请求费"), fmt_usd(requests), n_requests);
        println!("  预计写入总量:         {}  (约 {} 个对象)", fmt_bytes(total_bytes).bold(), n_objects);
        println!(
            "  预计耗时:             {}  (按平均速率 {} 估)",
            humantime::format_duration(std::time::Duration::from_secs(secs as u64)).to_string().bold(),
            fmt_rate(avg_rate)
        );
        if !dests.is_empty() {
            // Rates can differ per destination (discounted pairs), so show the
            // one each bucket actually bills rather than implying a flat ×K.
            println!("  复制目标桶:           {} 个", dests.len());
            for dest in dests {
                println!(
                    "    · {}  ({}, ${:.4}/GB)",
                    dest.bucket,
                    dest.region.as_deref().unwrap_or("区域未知,按源区域标准价计"),
                    crr_per_gb(pricing, dest.region.as_deref())
                );
            }
        }
        println!(
            "  {} {}  (保留 {:.0}h × {} 份,不计入预算停止判据,随保留时长另计)",
            "附:存储费估算:".dimmed(),
            fmt_usd(storage),
            retain_hours,
            copies
        );
    } else {
        let reqs_per_tib = (super::TIB).div_ceil(cfg.part_size) + cost.requests_per_object;
        let usd_per_tib = pricing.request_micro(reqs_per_tib);
        println!(
            "{}",
            format!("⚠ 模式 {} 没有按字节计费的即时成本 —— 烧钱主引擎缺失!", mode.id())
                .yellow()
                .bold()
        );
        println!(
            "  纯写入只产生请求费:每写 1 TiB 仅 {},烧完 {} 需要写约 {}",
            fmt_usd(usd_per_tib),
            fmt_usd(budget),
            fmt_bytes((budget as f64 / usd_per_tib as f64 * super::TIB as f64) as u64)
        );
        println!("  存储费按月缓慢发酵,无法在运行期精确控停。建议加 --dest-region 启用跨区复制。");
    }
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
/// Every replication destination gets its own file — an unswept destination
/// bills storage just like the source does.
pub fn write_lifecycle_files(prefix: &str, source_bucket: &str, dest_buckets: &[String]) {
    let json = lifecycle_json(prefix);
    let mut targets = vec![("./yo-s3-lifecycle-source.json".to_string(), source_bucket.to_string())];
    for (i, d) in dest_buckets.iter().enumerate() {
        let file = if dest_buckets.len() == 1 {
            "./yo-s3-lifecycle-dest.json".to_string()
        } else {
            format!("./yo-s3-lifecycle-dest-{}.json", i + 1)
        };
        targets.push((file, d.clone()));
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

    /// The discount applies to "data sent between" the pair, so it must hold
    /// in both directions and not leak onto unrelated destinations.
    #[test]
    fn discounted_pair_is_symmetric_and_scoped() {
        let use1 = pricing_for("us-east-1");
        let use2 = pricing_for("us-east-2");
        assert_eq!(crr_per_gb(&use1, Some("us-east-2")), 0.01);
        assert_eq!(crr_per_gb(&use2, Some("us-east-1")), 0.01);
        // Any other destination keeps the source's standard rate
        assert_eq!(crr_per_gb(&use1, Some("us-west-2")), 0.02);
        assert_eq!(crr_per_gb(&use1, Some("eu-west-1")), 0.02);
        // Unknown destination falls back to the standard (highest) rate
        assert_eq!(crr_per_gb(&use1, None), 0.02);
        // Expensive source regions are unaffected by the pair table
        assert_eq!(crr_per_gb(&pricing_for("ap-northeast-1"), Some("us-east-2")), 0.09);
    }

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
        let cost = CostModel {
            transfer: vec![TransferFee::per_gb("跨区复制流量费", p.crr_per_gb_usd)],
            requests_per_object: 3,
        };
        // 1 GiB at $0.02/GB → exactly $0.02 = 20_000 micro
        assert_eq!(cost.transfer_micro(GIB), 20_000);
        assert!(cost.budget_drives_stop());
        // 1000 requests at $0.005/1k → $0.005
        assert_eq!(p.request_micro(1000), 5_000);
    }

    #[test]
    fn fees_stack_on_the_same_byte() {
        // Replication to 2 regions + an accelerated upload endpoint all bill
        // the same written byte: 0.02×2 + 0.04 = $0.08/GB.
        let cost = CostModel {
            transfer: vec![
                TransferFee::per_gb("跨区复制流量费 ×2", 0.04),
                TransferFee::per_gb("传输加速费", 0.04),
            ],
            requests_per_object: 4,
        };
        assert_eq!(cost.transfer_micro(GIB), 80_000);
        assert!(cost.budget_drives_stop());
    }

    #[test]
    fn ta_surcharge_stacks_onto_the_engine() {
        // crr(K=1) $0.02 + TA $0.04 = $0.06/GB on the same byte.
        let mut cost = CostModel {
            transfer: vec![TransferFee::per_gb("跨区复制流量费", 0.02)],
            requests_per_object: 3,
        };
        cost.transfer.extend(path_surcharges(true, EgressPath::NotOnEc2));
        assert_eq!(cost.transfer.len(), 2);
        assert_eq!(cost.transfer_micro(GIB), 60_000);
        // TA does not add requests, only per-byte cost
        assert_eq!(cost.requests_per_object, 3);
    }

    #[test]
    fn ta_alone_can_drive_the_stop() {
        // write-only has no per-byte fee, but TA gives it one.
        let mut cost = CostModel::request_only();
        assert!(!cost.budget_drives_stop());
        cost.transfer.extend(path_surcharges(true, EgressPath::NotOnEc2));
        assert!(cost.budget_drives_stop());
        assert_eq!(cost.transfer_micro(GIB), 40_000);
    }

    #[test]
    fn nat_path_is_surcharged_without_being_asked() {
        // Detected NAT adds $0.045/GB on its own; the free path adds nothing.
        let nat = path_surcharges(false, EgressPath::NatGateway);
        assert_eq!(nat.len(), 1);
        assert_eq!(nat[0].label, "NAT 网关处理费");

        for free in [
            EgressPath::NotOnEc2,
            EgressPath::GatewayEndpoint,
            EgressPath::InternetGateway,
            EgressPath::Unknown,
        ] {
            assert!(path_surcharges(false, free).is_empty(), "{:?}", free);
        }
    }

    #[test]
    fn every_path_fee_stacks() {
        // crr K=3 ($0.06) + TA ($0.04) + NAT ($0.045) = $0.145/GB
        let mut cost = CostModel {
            transfer: vec![TransferFee::per_gb("跨区复制流量费 ×3", 0.06)],
            requests_per_object: 5,
        };
        cost.transfer.extend(path_surcharges(true, EgressPath::NatGateway));
        assert_eq!(cost.transfer.len(), 3);
        assert_eq!(cost.transfer_micro(GIB), 145_000);
    }

    #[test]
    fn request_only_cannot_stop_on_budget() {
        let cost = CostModel::request_only();
        assert_eq!(cost.transfer_micro(GIB), 0);
        assert!(!cost.budget_drives_stop());
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
