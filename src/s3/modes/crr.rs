// Mode `crr`: Cross-Region Replication transfer is the engine. Writing a byte
// to the source bucket immediately bills a replicated byte out of the region
// (~$0.02/GB), which is the only S3 cost that accrues instantly, linearly with
// bytes, and can therefore be stopped precisely on a budget.

use anyhow::{bail, Context, Result};
use async_trait::async_trait;
use colored::Colorize;
use rand::seq::IndexedRandom;

use super::{BurnMode, DestTarget, ModeCtx, ModeId, ObserveCtx, Observation};
use crate::s3::client::{build_s3_client, discover_bucket_region, resolved_region, ClientOpts};
use crate::s3::config::BenchConfig;
use crate::s3::cost::{crr_per_gb, pricing_for, CostModel, Pricing, TransferFee};
use crate::s3::crr;

#[derive(Default)]
pub struct CrrMode {
    /// Every destination the source replicates to. Each one bills its own
    /// inter-region transfer, so K destinations = K× the per-byte burn rate.
    dest: Vec<DestTarget>,
    /// --dry-run that could not determine ANY destination region — the config
    /// read failed, the source region is unknown, or existing rules block
    /// auto-provisioning. Bill as a single destination, the least a real run can
    /// end up with, so the rehearsal still terminates the way a real one would.
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

        let resolved = resolve_dest(ctx).await?;
        self.dest = resolved.dest;
        // Only when the tool could not FIND OUT. Having decided not to configure
        // replication is not the same thing: the real run would have no
        // destinations either, so the rehearsal must bill none too.
        if self.dest.is_empty() && cfg.dry_run && resolved.undetermined {
            println!(
                "{} dry-run 无法确定目标区域,按单个目标的口径模拟烧钱",
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

/// What arming the engine concluded.
struct Resolved {
    dest: Vec<DestTarget>,
    /// The tool could not find OUT where this run would replicate to — an
    /// unreadable replication config, or a source region it cannot determine.
    /// This is the only case where a rehearsal may bill an assumed destination;
    /// deciding not to configure any is a different thing entirely.
    undetermined: bool,
}

/// How this run got permission to create buckets and an IAM role in K regions.
enum Authorization {
    /// The regions were named on the command line, but the user has not seen
    /// the inventory yet.
    NeedsConfirmation,
    /// `--yes`: there is no gate left to hold, so the inventory goes to the log.
    Unattended,
    /// The interactive prompt WAS the gate — a second y/N on top is noise.
    AlreadyGiven,
}

/// The regions this run will replicate to, and what authorized creating them.
struct DestPlan {
    regions: Vec<String>,
    authorization: Authorization,
    /// Provisioning will REPLACE replication rules already on the bucket:
    /// `put_bucket_replication` has no partial update. Whatever announces this
    /// plan has to say so — it is the only irreversible part of the operation.
    replaces_existing: bool,
}

/// Where this run should replicate to, when nothing already covers our prefix.
enum DestDecision {
    Provision(DestPlan),
    /// Deliberately leaving replication unconfigured. A real run would have no
    /// destinations, so neither may a rehearsal.
    Skip,
    /// Could not find out. A rehearsal may assume a destination here.
    Unknown,
}

/// Detect every replication destination, and set them up on the spot when the
/// bucket has none — from `--dest-region` if given; from a random draw under
/// `--yes` (really provisioned) or `--dry-run` (simulated only); interactively
/// otherwise. No destinations = the run degrades to request fees.
async fn resolve_dest(ctx: &ModeCtx<'_>) -> Result<Resolved> {
    let cfg = ctx.cfg;
    // Only rules that actually match what we write generate traffic. Counting
    // a rule scoped elsewhere would arm the cost model at K× for a burn that
    // never happens — the run would write for hours and barely move the budget.
    let detected = match crr::detect_covering(ctx.s3, &cfg.bucket, Some(&cfg.key_prefix)).await {
        Ok(d) => d,
        Err(e) if cfg.dry_run => {
            eprintln!("{} 复制配置读取失败({:#}),--dry-run 继续", "⚠".yellow(), e);
            return Ok(Resolved { dest: Vec::new(), undetermined: true });
        }
        Err(e) => return Err(e),
    };

    if !detected.is_empty() {
        report_existing_dests(cfg, &detected);
        return Ok(Resolved {
            dest: build_targets(ctx, detected).await?,
            undetermined: false,
        });
    }

    let plan = match plan_dests(ctx).await? {
        DestDecision::Provision(plan) => plan,
        DestDecision::Skip => return Ok(Resolved { dest: Vec::new(), undetermined: false }),
        DestDecision::Unknown => return Ok(Resolved { dest: Vec::new(), undetermined: true }),
    };
    if plan.regions.is_empty() {
        // The interactive prompt can come back empty — that is "no thanks".
        return Ok(Resolved { dest: Vec::new(), undetermined: false });
    }
    // The gate `crr::setup` applies, hoisted ABOVE the dry-run shortcut: a
    // rehearsal that bills a fan-out the real run would refuse to build is
    // worse than no rehearsal at all.
    crr::validate_dest_regions(source_region(ctx).ok().as_deref(), &plan.regions)?;

    // A rehearsal must not leave buckets, an IAM role and replication rules
    // behind — but it still has to bill as if it had, or the estimate would
    // promise a K=1 burn for a run that is about to arm K=5.
    if cfg.dry_run {
        let dest = planned_targets(ctx, &plan.regions)?;
        println!(
            "{} dry-run:按将要配置的 {} 个目标模拟计费,账号里不创建任何资源",
            "ℹ".blue(),
            dest.len()
        );
        return Ok(Resolved { dest, undetermined: false });
    }

    match plan.authorization {
        Authorization::NeedsConfirmation => confirm_provision(cfg, &plan)?,
        Authorization::Unattended => announce_provision(cfg, &plan),
        Authorization::AlreadyGiven => {}
    }
    provision(ctx, &plan.regions).await?;
    Ok(Resolved {
        dest: planned_targets(ctx, &plan.regions)?,
        undetermined: false,
    })
}

fn report_existing_dests(cfg: &BenchConfig, detected: &[String]) {
    println!(
        "{} 跨区复制已配置 → {} 个目标桶 {}",
        "✓".green(),
        detected.len(),
        detected.join(" + ").bold()
    );
    if !cfg.dest_regions.is_empty() {
        // Silently ignoring it would leave the user believing they changed the
        // fan-out — and the burn rate — when they did not.
        println!(
            "  {}",
            "桶上已有复制配置,--dest-region 本次不生效(要改目标区域请先删掉现有复制规则)"
                .dimmed()
        );
    }
}

/// Which regions this run replicates to when nothing covers our prefix, and
/// what authorizes creating resources there.
async fn plan_dests(ctx: &ModeCtx<'_>) -> Result<DestDecision> {
    let cfg = ctx.cfg;
    if !cfg.dest_regions.is_empty() {
        // Naming the regions is the authorization. What it authorizes includes
        // REPLACING whatever replication config the bucket already carries, so
        // that has to be said before the y/N, not discovered afterwards.
        let uncovering = uncovering_rule_dests(ctx).await;
        if !uncovering.is_empty() {
            warn_prefix_mismatch(ctx, uncovering.len());
        }
        return Ok(DestDecision::Provision(DestPlan {
            regions: cfg.dest_regions.clone(),
            authorization: if cfg.yes {
                Authorization::Unattended
            } else {
                Authorization::NeedsConfirmation
            },
            replaces_existing: !uncovering.is_empty(),
        }));
    }
    if cfg.dry_run || cfg.yes {
        return unattended_plan(ctx).await;
    }
    interactive_plan(ctx).await
}

/// Nobody is at the terminal to name regions, so draw them. Not configuring
/// replication at all is the worse failure here — it burns nothing.
async fn unattended_plan(ctx: &ModeCtx<'_>) -> Result<DestDecision> {
    let uncovering = uncovering_rule_dests(ctx).await;
    if !uncovering.is_empty() {
        warn_prefix_mismatch(ctx, uncovering.len());
        // put_bucket_replication REPLACES the whole configuration, so
        // provisioning here would silently drop the rules already on the
        // bucket. Deleting someone else's replication setup is well past what
        // "burn my budget unattended" authorizes; naming the regions
        // explicitly is how the user says that trade is acceptable.
        println!(
            "{} 桶上已有复制规则,不自动配置(自动配置会整体替换现有规则)—— \
             确实要改请显式传 {}",
            "⚠".yellow().bold(),
            "--dest-region <区域,区域,...>".bold()
        );
        return Ok(DestDecision::Skip);
    }
    // The draw excludes the source region and its discounted partners, so it
    // needs to know where the source is. Not knowing degrades the run to
    // request fees, which is bad — but killing an unattended run outright over
    // a region lookup is worse.
    let Ok(source) = source_region(ctx) else {
        println!(
            "{} 无法确定源桶区域,跳过自动配置(加 {} 或 {})",
            "⚠".yellow().bold(),
            "--region".bold(),
            "--dest-region".bold()
        );
        return Ok(DestDecision::Unknown);
    };
    let regions = default_dest_regions(&source);
    println!(
        "{} 未配置跨区复制,随机选取 {} 个目标区域: {}",
        "ℹ".blue(),
        regions.len(),
        regions.join(" / ").bold()
    );
    Ok(DestDecision::Provision(DestPlan {
        regions,
        authorization: Authorization::Unattended,
        replaces_existing: false,
    }))
}

async fn interactive_plan(ctx: &ModeCtx<'_>) -> Result<DestDecision> {
    let uncovering = uncovering_rule_dests(ctx).await;
    if !uncovering.is_empty() {
        warn_prefix_mismatch(ctx, uncovering.len());
    }
    println!(
        "{} 未配置跨区复制 —— 它是烧钱主引擎(跨区流量 ~$0.02/GB,每多一个目标区域翻一倍)",
        "⚠".yellow().bold()
    );
    const AUTO: &str = "现在自动配置(建目标桶+复制规则,推荐)";
    let choice = inquire::Select::new(
        "怎么处理?",
        vec![AUTO, "不配置,纯写入继续(烧钱极慢)", "退出"],
    )
    .prompt()?;
    match choice {
        AUTO => Ok(DestDecision::Provision(DestPlan {
            regions: prompt_dest_regions(&source_region(ctx)?)?,
            // Picking this and then accepting the regions IS the gate.
            authorization: Authorization::AlreadyGiven,
            replaces_existing: !uncovering.is_empty(),
        })),
        "退出" => bail!("已取消"),
        _ => Ok(DestDecision::Skip),
    }
}

/// Resolve each destination bucket to a region-correct client. The region is
/// kept because it, not the bucket, decides that destination's transfer rate.
/// Used only for destinations read off EXISTING rules, where the region really
/// is unknown — after provisioning, see `planned_targets`.
async fn build_targets(ctx: &ModeCtx<'_>, dest_buckets: Vec<String>) -> Result<Vec<DestTarget>> {
    let mut targets = Vec::with_capacity(dest_buckets.len());
    for bucket in dest_buckets {
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

/// The destinations a plan describes, derived from the plan instead of looked
/// up — without touching the account at all.
///
/// `crr::setup` creates exactly one bucket per region under `dest_bucket_name`,
/// so once a plan exists the mapping is already known. Asking S3 anyway is
/// worse than redundant: `discover_bucket_region` right after `CreateBucket`
/// can hit eventual consistency, and its failure path leaves `region: None`,
/// which quietly points that destination's client at the SOURCE region.
fn planned_targets(ctx: &ModeCtx<'_>, dest_regions: &[String]) -> Result<Vec<DestTarget>> {
    dest_regions
        .iter()
        .map(|region| {
            Ok(DestTarget {
                bucket: crr::dest_bucket_name(&ctx.cfg.bucket, region),
                region: Some(region.clone()),
                client: build_s3_client(ctx.shared, &ClientOpts::default(), Some(region))?,
            })
        })
        .collect()
}

/// Destinations of replication rules already on the bucket. Every caller
/// reaches this only after `detect_covering` came back empty, so anything here
/// is a rule that does NOT cover the prefix we write — which makes a non-empty
/// answer mean two things at once: the run will not replicate, and
/// provisioning would replace these rules.
async fn uncovering_rule_dests(ctx: &ModeCtx<'_>) -> Vec<String> {
    crr::detect(ctx.s3, &ctx.cfg.bucket).await.unwrap_or_default()
}

/// The nastiest failure this tool can have: rules exist, so nothing looks
/// broken, but none of them match the prefix we write — so no replication, no
/// traffic fee, and a run that writes all day without moving the budget. Say it
/// out loud rather than reporting a bland "未配置".
fn warn_prefix_mismatch(ctx: &ModeCtx<'_>, rule_count: usize) {
    println!(
        "{} 桶上有 {} 条复制规则,但没有一条覆盖前缀 {} —— \n  \
         写进这个前缀的对象不会被复制,也就不产生跨区流量费,预算几乎不动。\n  \
         要么把 --key-prefix 改成规则覆盖的前缀,要么删掉现有规则重新配置",
        "⚠".yellow().bold(),
        rule_count,
        ctx.cfg.key_prefix.bold()
    );
}

fn source_region(ctx: &ModeCtx<'_>) -> Result<String> {
    ctx.bucket_region
        .map(|s| s.to_string())
        .or_else(|| resolved_region(ctx.shared))
        .context("无法确定源桶区域,请显式传 --region")
}

/// Everything `provision` is about to do to the account. One sentence, shared
/// by the interactive gate and the unattended announcement: what a user says
/// yes to and what a cron log keeps must never describe different things.
fn provision_inventory(cfg: &BenchConfig, plan: &DestPlan) -> String {
    let mut text = format!(
        "{} 开版本控制 → 在 {} 各建目标桶并开版本控制 → 创建复制 IAM 角色 {} → \
         写入 {} 条复制规则(前缀 {})",
        cfg.bucket,
        plan.regions.join(" / "),
        crr::replication_role_name(&cfg.bucket),
        plan.regions.len(),
        cfg.key_prefix
    );
    if plan.replaces_existing {
        // The only irreversible part of the operation, and the easiest to miss.
        text.push_str("。桶上现有的复制规则会被整体替换(AWS 不支持只改其中一条)");
    }
    text
}

/// Spell out every resource about to be created. This is the only gate before
/// buckets and an IAM role appear in K regions, and nothing in this tool tears
/// them back down afterwards.
fn confirm_provision(cfg: &BenchConfig, plan: &DestPlan) -> Result<()> {
    let go = inquire::Confirm::new(&format!(
        "将执行:{}。每字节将产生 {} 份跨区流量费。继续?",
        provision_inventory(cfg, plan),
        plan.regions.len()
    ))
    .with_default(true)
    .prompt()?;
    if !go {
        bail!("已取消");
    }
    Ok(())
}

/// Same inventory, stated instead of asked. `--yes` provisions without a gate,
/// so the only defence left is that the cron log says exactly which buckets and
/// which role appeared in the account, and where.
fn announce_provision(cfg: &BenchConfig, plan: &DestPlan) {
    println!(
        "{} 无人值守自动配置,将执行:{}",
        "ℹ".blue(),
        provision_inventory(cfg, plan)
    );
    println!(
        "  {}",
        format!("跑完拆干净: yo-s3 cleanup --bucket {} --all", cfg.bucket).dimmed()
    );
}

/// Create the fan-out. The buckets it returns are exactly what `planned_targets`
/// derives from the same regions, so the caller builds its targets from the plan
/// rather than from here.
async fn provision(ctx: &ModeCtx<'_>, dest_regions: &[String]) -> Result<()> {
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
    Ok(())
}

/// Default fan-out width. Five destinations burn ~5× faster than one while
/// keeping the cleanup surface (5 extra buckets) manageable.
pub const DEFAULT_FANOUT: usize = 5;

/// Every region the fan-out may create a bucket in. All are enabled-by-default
/// commercial regions — opt-in regions (af-south-1, ap-east-1, me-*, eu-south-*,
/// …) fail outright for accounts that never enabled them.
///
/// Unordered on purpose: destinations are drawn at random, so this is the set of
/// regions the tool is allowed to touch, not a preference list. Which is also
/// why it is pinned by a test — it decides where real buckets appear.
pub(crate) const DEST_POOL: &[&str] = &[
    "us-east-1",
    "us-east-2",
    "us-west-1",
    "us-west-2",
    "ca-central-1",
    "eu-west-1",
    "eu-west-2",
    "eu-west-3",
    "eu-central-1",
    "eu-north-1",
    "ap-south-1",
    "ap-northeast-1",
    "ap-northeast-2",
    "ap-southeast-1",
    "ap-southeast-2",
    "sa-east-1",
];

/// Regions that may serve as a destination for this source: the pool minus the
/// source itself, minus anything billed BELOW the source's standard rate.
///
/// The second filter reads the price table instead of naming regions, so a newly
/// discovered discounted pair is excluded the day it is added. The hardcoded
/// "just leave us-east-2 out of the list" it replaces only ever worked in one
/// direction — a source in us-east-2 was spared purely because us-east-1 sat
/// past the cut in an ordered candidate list, which a random draw would undo.
fn eligible_dests(source_region: &str) -> Vec<&'static str> {
    let pricing = pricing_for(source_region);
    DEST_POOL
        .iter()
        .copied()
        .filter(|r| *r != source_region)
        .filter(|r| crr_per_gb(&pricing, Some(r)) >= pricing.crr_per_gb_usd)
        .collect()
}

/// K destination regions drawn at random for a given source.
///
/// Random rather than a fixed list because the fan-out's per-byte rate is set by
/// the SOURCE region — every eligible destination bills exactly the same — so
/// the choice is free, and spreading it keeps the tool from stamping the same
/// five regions onto every account that runs it. Same reasoning as the random
/// object sizes and rates: randomize whatever the budget math does not depend on.
pub fn default_dest_regions(source_region: &str) -> Vec<String> {
    eligible_dests(source_region)
        .choose_multiple(&mut rand::rng(), DEFAULT_FANOUT)
        .map(|r| r.to_string())
        .collect()
}

/// Ask for destination regions, pre-filled with a random K=5 draw. Every extra
/// region adds a full copy of the transfer fee, so the prompt says so.
pub fn prompt_dest_regions(source_region: &str) -> Result<Vec<String>> {
    let suggested = default_dest_regions(source_region).join(",");
    let answer = inquire::Text::new("复制目标区域?(逗号分隔,每多一个区域烧钱速度加一倍)")
        .with_default(&suggested)
        .with_help_message(
            "默认随机取 5 个目标 ≈ 5× 烧钱速度;费率由源区域决定,换哪几个目标都一样快,可自行增删",
        )
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

    /// Pin the pool itself. The draw is random, but WHERE the tool is allowed
    /// to create real buckets is not — a silent edit here changes which regions
    /// a run can leave resources in.
    #[test]
    fn dest_pool_is_pinned() {
        assert_eq!(
            DEST_POOL,
            [
                "us-east-1", "us-east-2", "us-west-1", "us-west-2", "ca-central-1",
                "eu-west-1", "eu-west-2", "eu-west-3", "eu-central-1", "eu-north-1",
                "ap-south-1", "ap-northeast-1", "ap-northeast-2", "ap-southeast-1",
                "ap-southeast-2", "sa-east-1",
            ]
        );
    }

    /// Every property the random draw must hold for EVERY source, checked over
    /// enough draws that a rare bad pick cannot slip through.
    #[test]
    fn random_dests_hold_every_invariant() {
        for source in DEST_POOL {
            let pricing = pricing_for(source);
            for _ in 0..200 {
                let dests = default_dest_regions(source);
                assert_eq!(dests.len(), DEFAULT_FANOUT, "source={}", source);
                assert!(!dests.contains(&source.to_string()), "source={}", source);
                // A repeated region is billed once, not twice — a duplicate
                // would inflate the cost model above what AWS will charge.
                let mut sorted = dests.clone();
                sorted.sort();
                sorted.dedup();
                assert_eq!(sorted.len(), dests.len(), "source={}", source);
                for dest in &dests {
                    assert!(DEST_POOL.contains(&dest.as_str()), "{} 不在池里", dest);
                    // The whole reason a random destination is free: every
                    // eligible one bills the source's standard rate. A cheaper
                    // pick would silently halve the burn rate.
                    assert_eq!(
                        crr_per_gb(&pricing, Some(dest)),
                        pricing.crr_per_gb_usd,
                        "{} → {} 不是标准价",
                        source,
                        dest
                    );
                }
            }
        }
    }

    /// The discounted pair must be excluded in BOTH directions. The ordered
    /// candidate list this replaced only ever managed one of them: a source in
    /// us-east-2 was spared purely because us-east-1 sat past the cut.
    #[test]
    fn discounted_pair_is_excluded_from_both_ends() {
        for _ in 0..200 {
            assert!(!default_dest_regions("us-east-1").contains(&"us-east-2".to_string()));
            assert!(!default_dest_regions("us-east-2").contains(&"us-east-1".to_string()));
        }
        // …and only for the pair: both are ordinary destinations elsewhere.
        let elsewhere = eligible_dests("eu-west-1");
        assert!(elsewhere.contains(&"us-east-1"));
        assert!(elsewhere.contains(&"us-east-2"));
    }

    /// If the draw ever stopped being random the tool would silently go back to
    /// stamping one fixed set of regions onto every account.
    #[test]
    fn the_draw_actually_varies() {
        let first = default_dest_regions("us-east-1");
        assert!(
            (0..50).any(|_| default_dest_regions("us-east-1") != first),
            "50 次抽样结果完全相同 —— 随机没生效"
        );
    }

    /// Whatever the source, the pool must still afford a full fan-out after the
    /// source and its discounted partners are removed.
    #[test]
    fn pool_covers_the_fanout_for_every_source() {
        for source in DEST_POOL {
            assert!(
                eligible_dests(source).len() >= DEFAULT_FANOUT,
                "source={} 只剩 {} 个可选目标",
                source,
                eligible_dests(source).len()
            );
        }
    }

    /// Every pool member must be an enabled-by-default commercial region: an
    /// opt-in region fails outright for accounts that never enabled it, and the
    /// random draw would hit it eventually.
    #[test]
    fn pool_holds_no_opt_in_regions() {
        const OPT_IN: &[&str] = &[
            "af-south-1", "ap-east-1", "ap-south-2", "ap-southeast-3", "ap-southeast-4",
            "ap-southeast-5", "ap-southeast-7", "ca-west-1", "eu-central-2", "eu-south-1",
            "eu-south-2", "il-central-1", "me-central-1", "me-south-1", "mx-central-1",
        ];
        for region in DEST_POOL {
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
