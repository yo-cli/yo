// Everything that happens before the first byte is uploaded: interactive
// fill-in of missing required params, credential/bucket/CRR checks, the cost
// estimate + confirmation gate, and the checkpoint resume decision.

use anyhow::{bail, Context, Result};
use colored::Colorize;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use uuid::Uuid;

use super::args::RunArgs;
use crate::s3::auth;
use crate::s3::checkpoint::Checkpoint;
use crate::s3::client::{
    build_s3_client, discover_bucket_region, load_shared_config, resolved_region, BucketProbe,
    ClientOpts,
};
use crate::s3::config::{self, AccelMode, BenchConfig};
use crate::s3::cost::{
    self, path_surcharges, pricing_for, print_estimate, write_lifecycle_files, CostModel, Pricing,
};
use crate::s3::lastrun::{self, LastRun};
use crate::s3::lock::{self, Acquired, RunLock};
use crate::s3::modes::{BurnMode, ModeCtx};
use crate::s3::{accel, crr, fmt_bytes, fmt_usd, netpath, sweep};

/// Where a pre-`~/.yo/s3` version of the tool kept its checkpoint.
const LEGACY_CHECKPOINT: &str = "./yo-s3.ckpt.json";

pub struct RunContext {
    pub cfg: BenchConfig,
    /// Control-plane client: discovery, sweeps, backlog sampling, cleanup.
    pub s3: aws_sdk_s3::Client,
    /// Object-upload client. Same as `s3` unless Transfer Acceleration is on,
    /// in which case it targets the accelerate endpoint.
    pub upload_s3: aws_sdk_s3::Client,
    /// The armed cost engine: it owns the replication destination (if any),
    /// the per-object work, and the live backlog sampling.
    pub mode: Arc<dyn BurnMode>,
    /// The armed mode's cost shape, resolved once after preflight.
    pub cost: CostModel,
    pub pricing: Pricing,
    pub ckpt: Checkpoint,
    pub resumed: bool,
    pub run_id: Uuid,
    /// Single-instance guard, held for the whole run and never read: dropping
    /// it (or dying) is what releases the state directory.
    pub lock: RunLock,
}

pub async fn prepare(args: RunArgs) -> Result<RunContext> {
    // --- 0. last run's answers, for the params that have no default ---
    // Unattended runs deliberately ignore it: a cron job must be reproducible
    // from its command line alone, never from local state that drifted.
    let last = if args.yes { LastRun::default() } else { lastrun::load() };

    // --- 1. fill in the two required params, interactively when missing ---
    let budget_micro = match args.budget {
        Some(b) => b,
        None if args.yes => bail!("--yes 模式下必须显式提供 --budget"),
        None => {
            let default_usd = last
                .budget_micro
                .map(|m| m as f64 / 1_000_000.0)
                .unwrap_or(500.0);
            let usd = inquire::CustomType::<f64>::new("要烧掉多少预算(美元)?")
                .with_default(default_usd)
                .with_help_message("这是硬上限,烧够即停")
                .prompt()?;
            if usd <= 0.0 {
                bail!("预算必须 > 0");
            }
            (usd * 1_000_000.0).round() as u64
        }
    };
    let bucket = match args.bucket.clone() {
        Some(b) => b,
        None if args.yes => bail!("--yes 模式下必须显式提供 --bucket"),
        None => {
            let mut prompt = inquire::Text::new("目标 S3 桶名称?");
            if let Some(b) = last.bucket.as_deref() {
                prompt = prompt.with_default(b);
            }
            prompt.prompt()?
        }
    };

    // Params with no documented default fall back to last time. Explicit flags
    // always win, and whatever gets recalled is printed — recalled state that
    // nobody can see is worse than retyping the flag.
    let mut reused: Vec<&str> = Vec::new();
    let mut recall_str = |name: &'static str, empty: bool| {
        if empty {
            reused.push(name);
        }
    };
    recall_str("region", args.region.is_none() && last.region.is_some());
    recall_str("profile", args.profile.is_none() && last.profile.is_some());
    recall_str(
        "endpoint-url",
        args.endpoint_url.is_none() && last.endpoint_url.is_some(),
    );
    recall_str(
        "dest-region",
        args.dest_regions.is_empty() && !last.dest_regions.is_empty(),
    );
    recall_str("duration", args.duration.is_none() && last.duration().is_some());
    recall_str(
        "max-duration",
        args.max_duration.is_none() && last.max_duration().is_some(),
    );
    recall_str("total-size", args.total_size.is_none() && last.total_size.is_some());
    recall_str("iterations", args.iterations.is_none() && last.iterations.is_some());

    let region = args.region.clone().or_else(|| last.region.clone());
    let profile = args.profile.clone().or_else(|| last.profile.clone());
    let endpoint_url = args.endpoint_url.clone().or_else(|| last.endpoint_url.clone());
    let dest_regions = if args.dest_regions.is_empty() {
        last.dest_regions.clone()
    } else {
        args.dest_regions.clone()
    };
    let duration = args.duration.or_else(|| last.duration());
    let total_size = args.total_size.or(last.total_size);
    let iterations = args.iterations.or(last.iterations);
    let max_duration = args.max_duration.or_else(|| last.max_duration());

    if !reused.is_empty() {
        let flags = last.describe_reused(&reused);
        if !flags.is_empty() {
            println!("{} 沿用上次参数: {}", "ℹ".blue(), flags.bold());
            if let Ok(p) = lastrun::path() {
                println!(
                    "  {}",
                    format!("显式传参可覆盖;不想要就 rm {}", p.display()).dimmed()
                );
            }
        }
    }

    // --- 1.5 state directory: checkpoint + summary + the lock all live here ---
    let state = config::state_dir(
        endpoint_url.as_deref(),
        &bucket,
        &args.key_prefix,
        args.dry_run,
    )?;

    let mut cfg = BenchConfig {
        mode: args.mode,
        bucket,
        key_prefix: args.key_prefix.clone(),
        dest_regions: dest_regions.clone(),
        budget_micro,
        region: region.clone(),
        endpoint_url: endpoint_url.clone(),
        path_style: args.path_style,
        insecure_skip_tls_verify: args.insecure_skip_tls_verify,
        // Resolved below, once the bucket's region is known.
        transfer_acceleration: false,
        object_size_min: args.object_size_min,
        object_size_max: args.object_size_max,
        object_name: args.object_name.clone(),
        object_ext: args.object_ext.clone(),
        part_size: args.part_size,
        pool_size: args.pool_size,
        concurrent_objects: args.concurrent_objects,
        concurrent_parts: args.concurrent_parts,
        rate_min: args.rate_min,
        rate_max: args.rate_max,
        rate_mode: args.rate_mode,
        rate_resample_interval: args.rate_resample_interval,
        retain: args.retain,
        total_size,
        iterations,
        stop_when: args.stop_when,
        max_duration,
        checkpoint_path: resolve_checkpoint(
            args.checkpoint.as_deref(),
            args.resume.as_deref(),
            &state,
            args.dry_run,
        ),
        summary_out: args
            .summary_out
            .clone()
            .unwrap_or_else(|| path_string(state.join("summary.json"))),
        report_interval: args.report_interval,
        dry_run: args.dry_run,
        yes: args.yes,
    };
    cfg.validate()?;

    // The lock is taken before the first AWS call on purpose: a second instance
    // must be turned away before it can enable acceleration on the bucket or
    // create replication destinations, let alone spend.
    config::ensure_state_dir(&state)?;
    let run_lock = match lock::try_acquire(&state, "yo-s3 run")? {
        Acquired::Held(l) => l,
        Acquired::Busy(holder) => bail!(
            "已有 {} 在跑,拒绝启动第二个实例。\n  \
             两个实例各记各的账,{} 的硬上限会被花掉两遍。\n  \
             确认它已结束后重试;真要并行请换一个 --key-prefix(各自独立的预算与清扫范围)",
            holder,
            fmt_usd(cfg.budget_micro)
        ),
    };
    println!(
        "{} 单实例锁已获取: {}(仅防本机重复启动;多台机器打同一桶+前缀仍会各花各的预算)",
        "✓".green(),
        state.display()
    );

    // --- 2. credentials + clients ---
    // Credentials are as required as --budget and --bucket, and are the most
    // common thing to block a first run, so they get the same interactive
    // fill-in rather than a hint telling the user to go solve it elsewhere.
    let mut shared = load_shared_config(cfg.region.as_deref(), profile.as_deref()).await;
    auth::ensure_credentials(
        &mut shared,
        &auth::AuthOpts {
            region: cfg.region.as_deref(),
            profile: profile.as_deref(),
            yes: cfg.yes,
            lenient: cfg.dry_run,
        },
    )
    .await?;
    let shared = shared;
    // The plain client drives every control-plane call (discovery, replication
    // config, sweeps, backlog sampling). The accelerated upload client is built
    // at the end, once acceleration is resolved — the accelerate endpoint is an
    // upload-path thing and does not serve bucket-configuration operations.
    let plain_opts = ClientOpts {
        endpoint_url: cfg.endpoint_url.clone(),
        path_style: cfg.path_style,
        insecure_skip_tls_verify: cfg.insecure_skip_tls_verify,
        accelerate: false,
    };
    let s3 = build_s3_client(&shared, &plain_opts, None)?;

    // --- 3. bucket reachability + region (drives the pricing table) ---
    let bucket_region = match discover_bucket_region(&s3, &cfg.bucket).await {
        Ok(probe @ BucketProbe::Exists(_)) => {
            println!(
                "{} 桶 {} 可达{}",
                "✓".green(),
                cfg.bucket.bold(),
                probe
                    .region()
                    .map(|x| format!("(区域 {})", x))
                    .unwrap_or_default()
            );
            probe.region()
        }
        // The tool already creates K destination buckets on its own; refusing
        // to create the one source bucket it is about to fill with disposable
        // data would be an odd place to stop and send the user to the console.
        Ok(BucketProbe::Missing) if !cfg.dry_run => {
            let region = resolved_region(&shared).unwrap_or_else(|| "us-east-1".to_string());
            println!("{} 桶 {} 不存在", "ℹ".blue(), cfg.bucket.bold());
            if !cfg.yes {
                let go = inquire::Confirm::new(&format!(
                    "现在在 {} 创建它?(源区域决定跨区复制单价与默认目标区域)",
                    region
                ))
                .with_default(true)
                .prompt()?;
                if !go {
                    bail!("已取消:请换一个已存在的 --bucket,或允许创建");
                }
            }
            crr::create_bucket(&s3, &cfg.bucket, &region)
                .await
                .with_context(|| {
                    format!(
                        "创建桶 {} 失败。排查:桶名是否合法且全球唯一 / 当前身份是否有 s3:CreateBucket",
                        cfg.bucket
                    )
                })?;
            Some(region)
        }
        Ok(BucketProbe::Missing) => {
            eprintln!("{} 桶 {} 不存在,--dry-run 继续", "⚠".yellow(), cfg.bucket);
            None
        }
        Err(e) if cfg.dry_run => {
            eprintln!("{} 桶不可达({:#}),--dry-run 继续", "⚠".yellow(), e);
            None
        }
        Err(e) => {
            return Err(e).context(format!(
                "目标桶 {} 不可达。排查:桶名拼写 / 区域 / 当前身份是否有 s3:ListBucket 权限",
                cfg.bucket
            ))
        }
    };
    // The default prefix is now a natural-looking `backup/`, which a real
    // bucket may genuinely already use. Everything under it is treated as this
    // tool's disposable data — the retention sweeper deletes it, `cleanup`
    // deletes it. Say so before the first byte, not after.
    let fresh_run = !Path::new(&cfg.checkpoint_path).exists();
    if !cfg.dry_run && fresh_run {
        if let Ok(existing) = sweep::count_remaining(&s3, &cfg.bucket, &cfg.key_prefix).await {
            if existing.deleted > 0 {
                println!(
                    "{} 前缀 {} 下已有 {} 个对象({}),不是本次运行写的",
                    "⚠".yellow().bold(),
                    cfg.key_prefix.bold(),
                    existing.deleted,
                    fmt_bytes(existing.bytes)
                );
                println!(
                    "  {}",
                    format!(
                        "保留期清扫与 yo-s3 cleanup 会把该前缀下的一切当作本工具的数据删掉 —— \
                         真实数据请换一个 --key-prefix(当前 --retain {:?})",
                        cfg.retain
                    )
                    .yellow()
                );
                if !cfg.yes {
                    let go = inquire::Confirm::new("确认这个前缀下的东西可以被删除?")
                        .with_default(false)
                        .prompt()?;
                    if !go {
                        bail!("已取消:请换一个空的 --key-prefix");
                    }
                }
            }
        }
    }

    let pricing_region = bucket_region
        .clone()
        .or_else(|| resolved_region(&shared))
        .unwrap_or_else(|| "us-east-1".to_string());
    let pricing = pricing_for(&pricing_region);

    // --- 4. versioning (retention sweeps depend on it, whatever the mode) ---
    if cfg.endpoint_url.is_none() {
        match crr::versioning_enabled(&s3, &cfg.bucket).await {
            Ok(true) => println!(
                "{} 版本控制已开启:清扫按版本号物理删除(普通删除只盖 delete marker,旧版本会继续计费)",
                "ℹ".blue()
            ),
            Ok(false) => {}
            Err(e) => {
                if !cfg.dry_run {
                    eprintln!("{} 无法读取版本控制状态: {:#}", "⚠".yellow(), e);
                }
            }
        }
    }

    // --- 5. upload-path surcharges: acceleration + NAT, both auto-resolved ---
    let accelerate = resolve_acceleration(
        &s3,
        &cfg,
        args.transfer_acceleration,
        bucket_region.as_deref(),
        resolved_region(&shared).as_deref(),
    )
    .await?;
    cfg.transfer_acceleration = accelerate;
    // Detected, never asked: the user should not have to know their VPC
    // topology for the budget to be right.
    let egress = netpath::detect(&shared, bucket_region.as_deref()).await;
    if let Some(line) = egress.describe() {
        println!("{}", line);
    }

    // --- 6. arm the burn engine ---
    let mut mode = cfg.mode.build();
    mode.preflight(&ModeCtx {
        shared: &shared,
        s3: &s3,
        cfg: &cfg,
        bucket_region: bucket_region.as_deref(),
    })
    .await?;
    // Engine cost + path surcharges. The mode owns what its engine bills; the
    // upload path is not its business, so the surcharge is composed on here.
    let mut cost = mode.cost_model(&pricing);
    cost.transfer.extend(path_surcharges(cfg.transfer_acceleration, egress));

    // No per-byte fee + unattended + nothing else bounds the run = never stops.
    if !cost.budget_drives_stop()
        && cfg.yes
        && cfg.total_size.is_none()
        && cfg.iterations.is_none()
        && cfg.max_duration.is_none()
        && !cfg.dry_run
    {
        bail!(
            "模式 {} 当前没有按字节计费的即时成本,纯请求费烧不动预算,运行将永不停止。\
             请加 --dest-region <区域,区域,...> 让它自动配好跨区复制,\
             或提供 --total-size / --iterations / --max-duration 之一作为边界",
            cfg.mode
        );
    }

    // --- 6.5 --duration: turn a target wall time into a rate ---
    // Done here and not at config time because the byte count depends on the
    // composed cost model, which only exists once the mode is armed.
    if let Some(target) = duration {
        let total_bytes = if cost.budget_drives_stop() {
            cost::budget_bytes(cfg.budget_micro, &cost, &pricing, cfg.part_size)
        } else {
            // No per-byte cost means the budget cannot say how many bytes to
            // write, so there is nothing to spread — the user has to bound it.
            cfg.total_size.context(
                "模式 {} 没有按字节计费的即时成本,--duration 无从推导写入量;\
                 请补 --total-size,或换用 crr 模式",
            )?
        };
        let (min, max) = config::pace_rate(total_bytes, target)?;
        cfg.rate_min = min;
        cfg.rate_max = max;
        let avg = (min + max) / 2;
        println!(
            "{} 按 --duration {} 规划:平均 {}(区间 {} – {})",
            "ℹ".blue(),
            humantime::format_duration(target),
            crate::s3::fmt_rate(avg).bold(),
            crate::s3::fmt_rate(min),
            crate::s3::fmt_rate(max)
        );
        if avg > config::IMPLAUSIBLE_RATE {
            println!(
                "{} 这个速率超过单机常见网络上限(10 Gbps),实际多半达不到 —— \
                 届时预算照样烧完,只是比 {} 更久",
                "⚠".yellow(),
                humantime::format_duration(target)
            );
        }
    }

    // --- 7. estimate + lifecycle files + the confirmation gate ---
    let dest_buckets: Vec<String> = mode.destinations().iter().map(|d| d.bucket.clone()).collect();
    print_estimate(&cfg, &pricing, mode.as_ref(), &cost);
    write_lifecycle_files(&cfg.key_prefix, &cfg.bucket, &dest_buckets);
    if cfg.dry_run {
        println!("{} --dry-run:不会发出任何真实写入", "ℹ".blue());
    }
    if !cfg.yes {
        let go = inquire::Confirm::new(&format!(
            "确认开始?预算 {} 为硬上限,花出去的钱不可撤回",
            fmt_usd(cfg.budget_micro)
        ))
        .with_default(false)
        .prompt()?;
        if !go {
            bail!("已取消");
        }
    }

    // --- 8. checkpoint: fresh, or resume with strict snapshot validation ---
    let snapshot = cfg.snapshot();
    let (ckpt, resumed) = if let Some(resume_path) = &args.resume {
        let ckpt = Checkpoint::load(Path::new(resume_path))?;
        ckpt.validate_config(&snapshot)?;
        (ckpt, true)
    } else {
        let default_path = Path::new(&cfg.checkpoint_path);
        if default_path.exists() {
            let ckpt = Checkpoint::load(default_path)?;
            ckpt.validate_config(&snapshot)?;
            let resume = if cfg.yes {
                true // unattended rerun after a crash: resuming is the sane default
            } else {
                let choice = inquire::Select::new(
                    &format!(
                        "发现上次的进度(完成 {} 个对象 / {},已烧 {}),怎么继续?",
                        ckpt.completed_iterations,
                        fmt_bytes(ckpt.completed_bytes),
                        fmt_usd(ckpt.burned_micro)
                    ),
                    vec!["继续上次进度", "重新开始(旧数据留待清扫)"],
                )
                .prompt()?;
                choice == "继续上次进度"
            };
            if resume {
                (ckpt, true)
            } else {
                std::fs::remove_file(default_path).ok();
                (Checkpoint::new(Uuid::new_v4().to_string(), snapshot.clone()), false)
            }
        } else {
            (Checkpoint::new(Uuid::new_v4().to_string(), snapshot.clone()), false)
        }
    };
    let run_id = Uuid::parse_str(&ckpt.run_id).context("checkpoint 中的 run_id 非法")?;
    if resumed {
        println!(
            "{} 续跑 run {}:已完成 {} 对象,已烧 {}",
            "✓".green(),
            &ckpt.run_id[..8],
            ckpt.completed_iterations,
            fmt_usd(ckpt.burned_micro)
        );
    }

    let upload_s3 = if cfg.transfer_acceleration {
        let accel_opts = ClientOpts { accelerate: true, ..plain_opts.clone() };
        build_s3_client(&shared, &accel_opts, None)?
    } else {
        s3.clone()
    };

    let remember_bucket = cfg.bucket.clone();
    let remember_budget = cfg.budget_micro;
    let ctx = RunContext {
        cfg,
        s3,
        upload_s3,
        mode: Arc::from(mode),
        cost,
        pricing,
        ckpt,
        resumed,
        run_id,
        lock: run_lock,
    };

    // Recorded only after the confirmation gate: a cancelled run must not
    // rewrite what "last time" means.
    lastrun::save(&LastRun {
        bucket: Some(remember_bucket),
        budget_micro: Some(remember_budget),
        duration_secs: duration.map(|d| d.as_secs()),
        region: region.clone(),
        profile: profile.clone(),
        dest_regions: dest_regions.clone(),
        endpoint_url: endpoint_url.clone(),
        total_size,
        iterations,
        max_duration_secs: max_duration.map(|d| d.as_secs()),
    });
    Ok(ctx)
}

fn path_string(path: PathBuf) -> String {
    path.to_string_lossy().into_owned()
}

/// An explicit `--checkpoint` wins. Otherwise the state directory — except
/// when an older version left a checkpoint in the working directory: adopting
/// it keeps a multi-day run resumable instead of silently restarting from zero,
/// which would burn the budget a second time.
fn resolve_checkpoint(
    explicit: Option<&str>,
    resume: Option<&str>,
    state: &Path,
    dry_run: bool,
) -> String {
    if let Some(p) = explicit {
        return p.to_string();
    }
    // --resume names the ledger being continued, so progress belongs back in
    // it. Writing elsewhere would leave the next --resume reading a stale file.
    if let Some(p) = resume {
        return p.to_string();
    }
    let default = state.join("ckpt.json");
    // Never for a dry run: its fake burn must not reach the real ledger.
    if !dry_run && !default.exists() && Path::new(LEGACY_CHECKPOINT).exists() {
        println!(
            "{} 沿用当前目录的旧 checkpoint {}(新的默认位置是 {})",
            "ℹ".blue(),
            LEGACY_CHECKPOINT,
            default.display()
        );
        return LEGACY_CHECKPOINT.to_string();
    }
    path_string(default)
}

/// Decide whether uploads take the accelerate endpoint, and return whether the
/// $0.04/GB surcharge is actually armed.
///
/// The hard part is that AWS bills acceleration only when it decides the
/// transfer was faster than a direct one — a client in the same region as the
/// bucket is not charged. So `Auto` refuses to arm a fee that will not land on
/// the invoice, and every "cannot apply" path degrades to `false` with one
/// line of explanation instead of failing the run. `On` keeps the strict
/// behaviour: if the user asked for it explicitly, an impossible setup is an
/// error rather than a silent downgrade.
async fn resolve_acceleration(
    s3: &aws_sdk_s3::Client,
    cfg: &BenchConfig,
    mode: AccelMode,
    bucket_region: Option<&str>,
    client_region: Option<&str>,
) -> Result<bool> {
    if matches!(mode, AccelMode::Off) {
        return Ok(false);
    }
    let strict = matches!(mode, AccelMode::On);

    // Hard AWS constraints: virtual-hosted only, no dots, AWS endpoints only.
    if let Err(e) = accel::validate(&cfg.bucket, cfg.path_style, cfg.endpoint_url.as_deref()) {
        if strict {
            return Err(e);
        }
        tracing::debug!("传输加速不适用: {:#}", e);
        return Ok(false);
    }
    if let Some(region) = bucket_region {
        if !accel::region_supported(region) {
            if strict {
                bail!(
                    "桶所在区域 {} 不支持传输加速。请换到支持的区域,或用 --transfer-acceleration off",
                    region
                );
            }
            println!(
                "{} 区域 {} 不支持传输加速,本次不启用",
                "ℹ".blue(),
                region
            );
            return Ok(false);
        }
    }

    // The fee only materializes when acceleration actually helps.
    if bucket_region.is_some() && bucket_region == client_region {
        if !strict {
            println!(
                "{} 客户端与桶同在 {},AWS 不会收取加速费,本次不启用\
                 (想用加速请让客户端远离桶所在区域,或强制 --transfer-acceleration on)",
                "ℹ".blue(),
                bucket_region.unwrap_or("?")
            );
            return Ok(false);
        }
        accel::warn_if_not_accelerated(bucket_region, client_region);
    }

    match accel::enabled(s3, &cfg.bucket).await {
        Ok(true) => {
            println!("{} 传输加速已开启,计入 +$0.04/GB", "✓".green());
            Ok(true)
        }
        Ok(false) => {
            if cfg.yes || cfg.dry_run {
                // Never mutate bucket config unattended; say exactly how to fix.
                let hint = format!(
                    "桶 {} 未开启传输加速。开启:\n  \
                     aws s3api put-bucket-accelerate-configuration --bucket {} \
                     --accelerate-configuration Status=Enabled",
                    cfg.bucket, cfg.bucket
                );
                if strict && !cfg.dry_run {
                    bail!("{}", hint);
                }
                println!("{} {},本次不启用", "ℹ".blue(), hint);
                Ok(false)
            } else {
                let go = inquire::Confirm::new(&format!(
                    "桶 {} 未开启传输加速。现在开启?(每字节 +$0.04/GB,烧钱更快)",
                    cfg.bucket
                ))
                .with_default(true)
                .prompt()?;
                if !go {
                    if strict {
                        bail!("已取消:未开启传输加速时请用 --transfer-acceleration off");
                    }
                    return Ok(false);
                }
                accel::enable(s3, &cfg.bucket).await?;
                Ok(true)
            }
        }
        Err(e) => {
            if strict && !cfg.dry_run {
                return Err(e);
            }
            eprintln!("{} 传输加速状态读取失败({:#}),本次不启用", "⚠".yellow(), e);
            Ok(false)
        }
    }
}
