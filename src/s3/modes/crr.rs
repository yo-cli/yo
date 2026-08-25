// Mode `crr`: Cross-Region Replication transfer is the engine. Writing a byte
// to the source bucket immediately bills a replicated byte out of the region
// (~$0.02/GB), which is the only S3 cost that accrues instantly, linearly with
// bytes, and can therefore be stopped precisely on a budget.

use anyhow::{bail, Context, Result};
use async_trait::async_trait;
use colored::Colorize;

use super::{BurnMode, DestTarget, ModeCtx, ModeId, ObserveCtx, Observation};
use crate::s3::client::{build_s3_client, discover_bucket_region, resolved_region, ClientOpts};
use crate::s3::config::BenchConfig;
use crate::s3::cost::{crr_per_gb, CostModel, Pricing, TransferFee};
use crate::s3::crr;

#[derive(Default)]
pub struct CrrMode {
    /// Every destination the source replicates to. Each one bills its own
    /// inter-region transfer, so K destinations = K× the per-byte burn rate.
    dest: Vec<DestTarget>,
    /// --dry-run against a bucket without replication: bill as if the engine
    /// were armed, so the rehearsal terminates the way a real run would.
    assumed: bool,
}

impl CrrMode {
    /// Destinations billing transfer right now (dry-run assumes exactly one).
    fn fanout(&self) -> u64 {
        if self.dest.is_empty() && self.assumed {
            1
        } else {
            self.dest.len() as u64
        }
    }
}

#[async_trait]
impl BurnMode for CrrMode {
    fn id(&self) -> ModeId {
        ModeId::Crr
    }

    fn describe(&self) -> String {
        match (self.dest.len(), self.assumed) {
            (0, true) => "跨区复制流量(dry-run 假设已配置)".to_string(),
            (0, false) => "跨区复制未配置,已退化为纯写入".to_string(),
            (1, _) => format!("跨区复制流量,目标桶 {}", self.dest[0].bucket),
            (k, _) => format!(
                "跨区复制流量 ×{} 个目标区域,每字节计 {} 次跨区传输",
                k, k
            ),
        }
    }

    async fn preflight(&mut self, ctx: &ModeCtx<'_>) -> Result<()> {
        let cfg = ctx.cfg;
        if cfg.endpoint_url.is_some() {
            println!(
                "{} 自定义端点模式:跨区复制为 AWS 原生特性,此处不可用,退化为纯写入(烧钱极慢)。\
                 显式 --mode write-only 可跳过本提示",
                "⚠".yellow()
            );
            return Ok(());
        }

        self.dest = resolve_dest(ctx).await?;
        if self.dest.is_empty() && cfg.dry_run {
            println!(
                "{} dry-run 按「已启用跨区复制」口径模拟烧钱(实跑时加 --dest-region 自动配好)",
                "ℹ".blue()
            );
            self.assumed = true;
        }
        Ok(())
    }

    fn cost_model(&self, pricing: &Pricing) -> CostModel {
        let k = self.fanout();
        if k == 0 {
            // Engine never armed — only request fees accrue.
            return CostModel::request_only();
        }
        // Each destination is billed as its own source→destination pair, and
        // some pairs are discounted, so this is a SUM over destinations rather
        // than one rate × K. A dry-run with no real destinations assumes one
        // at the source's standard rate.
        let per_gb: f64 = if self.dest.is_empty() {
            pricing.crr_per_gb_usd
        } else {
            self.dest
                .iter()
                .map(|d| crr_per_gb(pricing, d.region.as_deref()))
                .sum()
        };
        let label = if k == 1 {
            "跨区复制流量费".to_string()
        } else {
            format!("跨区复制流量费 ×{}", k)
        };
        CostModel {
            transfer: vec![TransferFee::per_gb(label, per_gb)],
            // create + complete + one replication PUT per destination
            // (AWS bills exactly one PUT per object per destination)
            requests_per_object: 2 + k,
        }
    }

    fn destinations(&self) -> &[DestTarget] {
        &self.dest
    }

    async fn observe(&self, ctx: &ObserveCtx<'_>) -> Option<Observation> {
        if self.dest.is_empty() {
            return None;
        }
        let (pending, failed) = crr::sample_backlog(ctx.s3, ctx.bucket, ctx.keys).await;
        let mut text = format!(" | 复制积压 {}/{}", pending, ctx.keys.len());
        if failed > 0 {
            text.push_str(&format!("(失败 {}!)", failed));
        }
        Some(Observation { text, pending })
    }
}

/// Detect every replication destination, and set them up on the spot when the
/// bucket has none — from `--dest-region` if given, interactively otherwise.
/// Empty = the run degrades to request fees.
async fn resolve_dest(ctx: &ModeCtx<'_>) -> Result<Vec<DestTarget>> {
    let cfg = ctx.cfg;
    // Only rules that actually match what we write generate traffic. Counting
    // a rule scoped elsewhere would arm the cost model at K× for a burn that
    // never happens — the run would write for hours and barely move the budget.
    let detected = match crr::detect_covering(ctx.s3, &cfg.bucket, Some(&cfg.key_prefix)).await {
        Ok(d) => d,
        Err(e) => {
            if cfg.dry_run {
                eprintln!("{} 复制配置读取失败({:#}),--dry-run 继续", "⚠".yellow(), e);
                return Ok(Vec::new());
            }
            return Err(e);
        }
    };

    let dest_buckets = if !detected.is_empty() {
        println!(
            "{} 跨区复制已配置 → {} 个目标桶 {}",
            "✓".green(),
            detected.len(),
            detected.join(" + ").bold()
        );
        if !cfg.dest_regions.is_empty() {
            // Silently ignoring it would leave the user believing they changed
            // the fan-out — and the burn rate — when they did not.
            println!(
                "  {}",
                "桶上已有复制配置,--dest-region 本次不生效(要改目标区域请先删掉现有复制规则)"
                    .dimmed()
            );
        }
        detected
    } else if !cfg.dest_regions.is_empty() {
        // Naming the regions is the authorization: it is the one signal that
        // says "yes, create buckets and an IAM role in those regions".
        if !cfg.yes {
            confirm_provision(cfg, &cfg.dest_regions)?;
        }
        provision(ctx, &cfg.dest_regions).await?
    } else if cfg.dry_run || cfg.yes {
        warn_prefix_mismatch(ctx).await;
        println!(
            "{} 未配置跨区复制(烧钱主引擎缺失)—— 加 {} 可自动配好再开烧",
            "⚠".yellow().bold(),
            "--dest-region <区域,区域,...>".bold()
        );
        Vec::new()
    } else {
        warn_prefix_mismatch(ctx).await;
        println!(
            "{} 未配置跨区复制 —— 它是烧钱主引擎(跨区流量 ~$0.02/GB,每多一个目标区域翻一倍)",
            "⚠".yellow().bold()
        );
        let choice = inquire::Select::new(
            "怎么处理?",
            vec![
                "现在自动配置(建目标桶+复制规则,推荐)",
                "不配置,纯写入继续(烧钱极慢)",
                "退出",
            ],
        )
        .prompt()?;
        match choice {
            // Picking this and then typing the regions IS the confirmation —
            // no second y/N on top of it.
            "现在自动配置(建目标桶+复制规则,推荐)" => {
                let dest_regions = prompt_dest_regions(&source_region(ctx)?)?;
                provision(ctx, &dest_regions).await?
            }
            "退出" => bail!("已取消"),
            _ => Vec::new(),
        }
    };

    let mut targets = Vec::with_capacity(dest_buckets.len());
    for bucket in dest_buckets {
        // Each destination lives in its own region — build its client there,
        // and keep the region: it decides that destination's transfer rate.
        let region = discover_bucket_region(ctx.s3, &bucket)
            .await
            .map(|p| p.region())
            .unwrap_or(None);
        let client = build_s3_client(ctx.shared, &ClientOpts::default(), region.as_deref())?;
        targets.push(DestTarget {
            bucket,
            region,
            client,
        });
    }
    Ok(targets)
}

/// The nastiest failure this tool can have: rules exist, so nothing looks
/// broken, but none of them match the prefix we write — so no replication, no
/// traffic fee, and a run that writes all day without moving the budget. Say it
/// out loud rather than reporting a bland "未配置".
async fn warn_prefix_mismatch(ctx: &ModeCtx<'_>) {
    let all = crr::detect(ctx.s3, &ctx.cfg.bucket).await.unwrap_or_default();
    if all.is_empty() {
        return;
    }
    println!(
        "{} 桶上有 {} 条复制规则,但没有一条覆盖前缀 {} —— \n  \
         写进这个前缀的对象不会被复制,也就不产生跨区流量费,预算几乎不动。\n  \
         要么把 --key-prefix 改成规则覆盖的前缀,要么删掉现有规则重新配置",
        "⚠".yellow().bold(),
        all.len(),
        ctx.cfg.key_prefix.bold()
    );
}

fn source_region(ctx: &ModeCtx<'_>) -> Result<String> {
    ctx.bucket_region
        .map(|s| s.to_string())
        .or_else(|| resolved_region(ctx.shared))
        .context("无法确定源桶区域,请显式传 --region")
}

/// Spell out every resource about to be created. This is the only gate before
/// buckets and an IAM role appear in K regions, and nothing in this tool tears
/// them back down afterwards.
fn confirm_provision(cfg: &BenchConfig, dest_regions: &[String]) -> Result<()> {
    let go = inquire::Confirm::new(&format!(
        "将执行:{} 开版本控制 → 在 {} 各建目标桶并开版本控制 → 创建复制 IAM 角色 → \
         写入 {} 条复制规则(前缀 {})。每字节将产生 {} 份跨区流量费。继续?",
        cfg.bucket,
        dest_regions.join(" / "),
        dest_regions.len(),
        cfg.key_prefix,
        dest_regions.len()
    ))
    .with_default(true)
    .prompt()?;
    if !go {
        bail!("已取消");
    }
    Ok(())
}

async fn provision(ctx: &ModeCtx<'_>, dest_regions: &[String]) -> Result<Vec<String>> {
    let cfg = ctx.cfg;
    let dest_buckets = crr::setup(
        ctx.shared,
        ctx.s3,
        &cfg.bucket,
        &source_region(ctx)?,
        dest_regions,
        &cfg.key_prefix,
    )
    .await?;
    println!(
        "{} 跨区复制配置完成 → {} 个目标({}),烧钱速率 {}×",
        "✓".green().bold(),
        dest_buckets.len(),
        dest_buckets.join(" + "),
        dest_buckets.len()
    );
    Ok(dest_buckets)
}

/// Default fan-out width. Five destinations burn ~5× faster than one while
/// keeping the cleanup surface (5 extra buckets) manageable.
pub const DEFAULT_FANOUT: usize = 5;

/// Candidates for the default fan-out, in preference order. All are
/// enabled-by-default commercial regions — opt-in regions (af-south-1,
/// ap-east-1, me-*, eu-south-*, …) would fail for accounts that never enabled
/// them. us-east-2 is deliberately absent: its pair with us-east-1 is
/// half-price, which would slow the burn rather than speed it up.
const DEFAULT_DEST_CANDIDATES: &[&str] = &[
    "us-west-2",
    "eu-west-1",
    "ap-northeast-1",
    "ap-southeast-2",
    "sa-east-1",
    "eu-central-1",
    "ap-south-1",
    "us-east-1",
];

/// The default destination regions for a given source, excluding the source.
pub fn default_dest_regions(source_region: &str) -> Vec<String> {
    DEFAULT_DEST_CANDIDATES
        .iter()
        .filter(|r| **r != source_region)
        .take(DEFAULT_FANOUT)
        .map(|r| r.to_string())
        .collect()
}

/// Ask for destination regions, defaulting to a K=5 fan-out. Every extra
/// region adds a full copy of the transfer fee, so the prompt says so.
pub fn prompt_dest_regions(source_region: &str) -> Result<Vec<String>> {
    let suggested = default_dest_regions(source_region).join(",");
    let answer = inquire::Text::new("复制目标区域?(逗号分隔,每多一个区域烧钱速度加一倍)")
        .with_default(&suggested)
        .with_help_message("默认 5 个目标 ≈ 5× 烧钱速度;删掉几个可减少目标桶数量")
        .prompt()?;
    Ok(parse_regions(&answer))
}

fn parse_regions(raw: &str) -> Vec<String> {
    raw.split(',')
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::s3::cost::pricing_for;

    #[test]
    fn unarmed_engine_reports_request_only_cost() {
        let mode = CrrMode::default();
        let cost = mode.cost_model(&pricing_for("us-east-1"));
        assert!(!cost.budget_drives_stop());
        assert_eq!(cost.requests_per_object, 2);
    }

    #[test]
    fn dry_run_assumption_arms_the_transfer_fee() {
        let mode = CrrMode {
            dest: Vec::new(),
            assumed: true,
        };
        let cost = mode.cost_model(&pricing_for("us-east-1"));
        assert!(cost.budget_drives_stop());
        assert_eq!(cost.requests_per_object, 3);
        // us-east-1 replicates out at $0.02/GB
        assert_eq!(cost.transfer_micro(crate::s3::GIB), 20_000);
    }

    /// K destinations must multiply BOTH the per-byte transfer and the
    /// per-object replication PUTs, or the budget stops at the wrong dollar.
    #[test]
    fn fanout_multiplies_transfer_and_requests() {
        let pricing = pricing_for("us-east-1");
        for k in 1..=4u64 {
            let mode = CrrMode {
                dest: (0..k).map(|i| fake_dest(i)).collect(),
                assumed: false,
            };
            let cost = mode.cost_model(&pricing);
            // $0.02/GB out of us-east-1, once per destination region
            assert_eq!(cost.transfer_micro(crate::s3::GIB), 20_000 * k, "k={}", k);
            // create + complete + one replication PUT per destination
            assert_eq!(cost.requests_per_object, 2 + k, "k={}", k);
        }
    }

    #[test]
    fn fanout_label_names_the_multiplier() {
        let mode = CrrMode {
            dest: (0..3).map(fake_dest).collect(),
            assumed: false,
        };
        let cost = mode.cost_model(&pricing_for("us-east-1"));
        assert_eq!(cost.transfer[0].label, "跨区复制流量费 ×3");
    }

    /// Transfer is billed per source→destination pair, and us-east-1↔us-east-2
    /// is half price. Charging the source's flat rate × K would over-count it.
    #[test]
    fn discounted_pair_is_priced_lower_than_the_flat_rate() {
        let pricing = pricing_for("us-east-1");
        let mode = CrrMode {
            dest: vec![dest_in("cheap", Some("us-east-2"))],
            assumed: false,
        };
        // $0.01/GB, not the standard $0.02
        assert_eq!(mode.cost_model(&pricing).transfer_micro(crate::s3::GIB), 10_000);
    }

    /// A mixed fan-out must SUM the real per-destination rates.
    #[test]
    fn mixed_rate_fanout_sums_per_destination() {
        let pricing = pricing_for("us-east-1");
        let mode = CrrMode {
            dest: vec![
                dest_in("a", Some("us-east-2")),   // $0.01 (discounted pair)
                dest_in("b", Some("us-west-2")),   // $0.02
                dest_in("c", Some("eu-west-1")),   // $0.02
            ],
            assumed: false,
        };
        let cost = mode.cost_model(&pricing);
        // 0.01 + 0.02 + 0.02 = $0.05/GB — NOT 0.02 × 3
        assert_eq!(cost.transfer_micro(crate::s3::GIB), 50_000);
        assert_eq!(cost.requests_per_object, 5);
    }

    /// An undiscoverable region falls back to the source's standard rate,
    /// which is the highest possible — the budget never under-counts.
    #[test]
    fn unknown_destination_region_uses_the_standard_rate() {
        let pricing = pricing_for("us-east-1");
        let mode = CrrMode {
            dest: vec![dest_in("mystery", None)],
            assumed: false,
        };
        assert_eq!(mode.cost_model(&pricing).transfer_micro(crate::s3::GIB), 20_000);
    }

    #[test]
    fn default_fanout_is_five_and_excludes_the_source() {
        for source in ["us-east-1", "us-west-2", "eu-west-1", "ap-northeast-1"] {
            let dests = default_dest_regions(source);
            assert_eq!(dests.len(), DEFAULT_FANOUT, "source={}", source);
            assert!(!dests.contains(&source.to_string()), "source={}", source);
            // No duplicates — a repeated region is billed once, not twice.
            let mut sorted = dests.clone();
            sorted.sort();
            sorted.dedup();
            assert_eq!(sorted.len(), dests.len(), "source={}", source);
        }
    }

    /// The default set must avoid the half-price pair, which would slow the
    /// burn instead of speeding it up.
    #[test]
    fn default_fanout_avoids_the_discounted_pair() {
        assert!(!default_dest_regions("us-east-1").contains(&"us-east-2".to_string()));
    }

    /// Pin the actual default sets: these create real buckets in real regions,
    /// so a silent change here changes what the tool provisions.
    #[test]
    fn default_fanout_sets_are_pinned() {
        assert_eq!(
            default_dest_regions("us-east-1"),
            ["us-west-2", "eu-west-1", "ap-northeast-1", "ap-southeast-2", "sa-east-1"]
        );
        // Source drops out and the next candidate fills the slot
        assert_eq!(
            default_dest_regions("us-west-2"),
            ["eu-west-1", "ap-northeast-1", "ap-southeast-2", "sa-east-1", "eu-central-1"]
        );
    }

    /// Every default destination must be an enabled-by-default commercial
    /// region: an opt-in region would fail for accounts that never enabled it.
    #[test]
    fn default_candidates_are_not_opt_in_regions() {
        const OPT_IN: &[&str] = &[
            "af-south-1", "ap-east-1", "ap-south-2", "ap-southeast-3", "ap-southeast-4",
            "ca-west-1", "eu-central-2", "eu-south-1", "eu-south-2", "il-central-1",
            "me-central-1", "me-south-1",
        ];
        for region in DEFAULT_DEST_CANDIDATES {
            assert!(!OPT_IN.contains(region), "{} 是 opt-in 区域", region);
        }
    }

    #[test]
    fn regions_parse_from_comma_separated_input() {
        assert_eq!(parse_regions("us-west-2"), vec!["us-west-2"]);
        assert_eq!(
            parse_regions(" us-west-2 , eu-west-1 ,, ap-south-1 "),
            vec!["us-west-2", "eu-west-1", "ap-south-1"]
        );
        assert!(parse_regions("  ,  ").is_empty());
    }

    fn fake_dest(i: u64) -> DestTarget {
        dest_in(&format!("dest-{}", i), Some("us-west-2"))
    }

    fn dest_in(bucket: &str, region: Option<&str>) -> DestTarget {
        let cfg = aws_sdk_s3::Config::builder()
            .region(aws_config::Region::new("us-east-1"))
            .behavior_version_latest()
            .build();
        DestTarget {
            bucket: bucket.to_string(),
            region: region.map(String::from),
            client: aws_sdk_s3::Client::from_conf(cfg),
        }
    }
}
