// Everything that happens before the first byte is uploaded: interactive
// fill-in of missing required params, credential/bucket/CRR checks, the cost
// estimate + confirmation gate, and the checkpoint resume decision.

use anyhow::{bail, Context, Result};
use colored::Colorize;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use uuid::Uuid;

use super::args::RunArgs;
use crate::s3::auth;
use crate::s3::budget;
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
use crate::s3::lock::{self, Acquired, HolderInfo, RunLock};
use crate::s3::modes::{BurnMode, ModeCtx};
use crate::s3::quota;
use crate::s3::{accel, crr, fmt_bytes, fmt_usd, naming, netpath, sweep};

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

    // --- 1. fill in the required params, interactively when missing ---
    // Budget then days then bucket: the first two are one decision (how much,
    // and how fast), the third is where.
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
    // How many days to spread it over. Asked right after the budget because it
    // is the other half of the same decision — and because the answer is what
    // decides whether this run finishes tonight or a month from now.
    // `--duration` says the same thing in other units, so naming it skips the
    // question; `--yes` never asks, like every other prompt here.
    let days = match args.days {
        Some(d) => Some(d),
        None if args.duration.is_some() || args.yes => None,
        None => prompt_days(budget_micro, last.days)?,
    };
    let bucket = match args.bucket.clone() {
        Some(b) => b,
        None if args.yes => bail!("--yes 模式下必须显式提供 --bucket"),
        None => prompt_bucket(last.bucket.as_deref())?,
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
    // `--duration` and `--days` plan the pace out of the budget in different
    // units, so an answered or explicit `--days` has to shut `--duration`'s
    // memory off. clap only rejects the pair when BOTH are typed — a remembered
    // `--duration 6h` would otherwise override the number just typed at the
    // prompt, and the daily ceiling would silently apply at the wrong pace.
    let duration = if days.is_some() || args.duration.is_some() {
        args.duration
    } else {
        last.duration()
    };
    recall_str("duration", args.duration.is_none() && duration.is_some());
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
    let mut state = config::state_dir(
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
        days,
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
    let run_lock = loop {
        config::ensure_state_dir(&state)?;
        match lock::try_acquire(&state, "yo-s3 run")? {
            Acquired::Held(l) => break l,
            Acquired::Busy(holder) => {
                // Not a dead end: a different prefix is a different ledger, and
                // asking beats making the user re-type the whole command.
                cfg.key_prefix = prompt_parallel_prefix(&holder, &cfg, &args)?;
                state = config::state_dir(
                    endpoint_url.as_deref(),
                    &cfg.bucket,
                    &cfg.key_prefix,
                    args.dry_run,
                )?;
                cfg.checkpoint_path = resolve_checkpoint(
                    args.checkpoint.as_deref(),
                    args.resume.as_deref(),
                    &state,
                    args.dry_run,
                );
                cfg.summary_out = args
                    .summary_out
                    .clone()
                    .unwrap_or_else(|| path_string(state.join("summary.json")));
                cfg.validate()?;
            }
        }
    };
    println!(
        "{} 单实例锁已获取: {}(仅防本机重复启动;多台机器打同一桶+前缀仍会各花各的预算)",
        "✓".green(),
        state.display()
    );
    if cfg.key_prefix != args.key_prefix {
        // The prefix is now something the user did not type, and every later
        // command that touches this run's data needs it spelled out.
        println!(
            "  {}",
            format!(
                "本次前缀 {}(独立账本);清理用 yo-s3 cleanup --bucket {} --key-prefix {}",
                cfg.key_prefix, cfg.bucket, cfg.key_prefix
            )
            .dimmed()
        );
    }

    // --- 2. credentials + clients ---
    // Credentials are as required as --budget and --bucket, and are the most
    // common thing to block a first run, so they get the same interactive
    // fill-in rather than a hint telling the user to go solve it elsewhere.
    let mut shared = load_shared_config(cfg.region.as_deref(), profile.as_deref()).await;
    let chosen_profile = auth::ensure_credentials(
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
    // A profile chosen at the credential menu is an answer with no default, like
    // the bucket: remembering it is what stops the next run from asking again.
    let profile = chosen_profile.or(profile);
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

    // --- 6.5 --duration / --days: turn a target wall time into a rate ---
    // Done here and not at config time because the byte count depends on the
    // composed cost model, which only exists once the mode is armed.
    // `--days` plans the same way `--duration` does; what it adds on top is the
    // hard per-day ceiling, which lives in the budget meter (`quota.rs`).
    // Carrying the flag text alongside the target keeps the two from being
    // re-derived apart: whichever flag set the wall time is the one every
    // message about it should name.
    let paced = match (duration, days) {
        (Some(target), _) => Some((target, format!("--duration {}", humantime::format_duration(target)))),
        (None, Some(n)) => Some((
            Duration::from_secs(n.saturating_mul(quota::DAY_SECS as u64)),
            format!("--days {}", n),
        )),
        (None, None) => None,
    };
    if let Some((target, flag)) = paced {
        let total_bytes = if cost.budget_drives_stop() {
            cost::budget_bytes(cfg.budget_micro, &cost, &pricing, cfg.part_size)
        } else {
            // No per-byte cost means the budget cannot say how many bytes to
            // write, so there is nothing to spread — the user has to bound it.
            cfg.total_size.with_context(|| {
                format!(
                    "模式 {} 当前没有按字节计费的即时成本,{} 无从推导要写多少字节;\
                     请补 --total-size 指定写入量,或用 --dest-region 配上跨区复制\
                     让流量费成为成本引擎",
                    cfg.mode, flag
                )
            })?
        };
        let (min, max) = config::pace_rate(total_bytes, target)?;
        cfg.rate_min = min;
        cfg.rate_max = max;
        let avg = (min + max) / 2;
        println!(
            "{} 按 {} 规划:平均 {}(区间 {} – {})",
            "ℹ".blue(),
            flag,
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
    // An hour whose ceiling cannot buy the smallest object the scheduler would
    // schedule parks the run forever: every hour it finds nothing plannable,
    // sleeps, and wakes to the same answer. Say so now instead.
    //
    // Both halves have to match what the scheduler actually does, or the guard
    // draws its line somewhere the run does not: the LEANEST hour the band can
    // draw (not the average), priced by the same `object_cost_micro` the meter
    // enforces with (not `budget_bytes`, which leaves out the per-object
    // requests). Getting either wrong leaves exactly the hang this prevents.
    if let (Some(days), Some(cap)) = (days, cfg.daily_cap_micro()) {
        let (leanest_hour, _) = quota::hour_band(cap / 24);
        let smallest = if cost.budget_drives_stop() {
            budget::MIN_TAIL_OBJECT
        } else {
            // Request-only modes never shrink an object; a whole one has to fit.
            cfg.object_size_min
        };
        let need = cost::object_cost_micro(smallest, &cost, &pricing, cfg.part_size);
        if leanest_hour < need {
            // Spelled to 6 decimals, not through `fmt_usd`: every amount in
            // this message is sub-cent by definition, and "只有 $0.00,买不起
            // 一个 $0.00 的对象" is what 2 decimals turns the reason into.
            bail!(
                "预算 {} 摊到 {} 天后,最少的那一小时只有 ${:.6},买不起一个 {} 的对象(${:.6}),\
                 运行会一直空转。请减少 --days 或提高 --budget",
                fmt_usd(cfg.budget_micro),
                days,
                leanest_hour as f64 / 1e6,
                fmt_bytes(smallest),
                need as f64 / 1e6
            );
        }
    }
    // `--max-duration` is one of the remembered params, so it can be in force
    // without being typed today — and a fallback that fires on day 1 of a
    // 30-day plan looks like the tool giving up rather than like a bound.
    if let (Some(days), Some(max)) = (days, max_duration) {
        if max.as_secs() < days.saturating_mul(quota::DAY_SECS as u64) {
            println!(
                "{} --max-duration {} 早于 --days {} 的计划终点:到点会强制停,预算烧不完",
                "⚠".yellow(),
                humantime::format_duration(max),
                days
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
        days,
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

/// Ask how many days the budget should be spread over, and show what that works
/// out to per day and per hour before anything is spent.
///
/// `0` means no limit — what this tool did before `--days` existed, burn as fast
/// as the link allows. It is the cold default so that pressing Enter on a first
/// run keeps the old behaviour; from the second run on, last time's answer is
/// the default, which is what keeps a multi-day plan's ceiling alive across the
/// restarts such a plan is guaranteed to have.
fn prompt_days(budget_micro: u64, last_days: Option<u64>) -> Result<Option<u64>> {
    let days = inquire::CustomType::<u64>::new("分几天烧完?")
        .with_default(last_days.unwrap_or(0))
        .with_help_message(
            "每天最多烧 预算÷天数(硬上限),再摊到每小时:整点重取本小时额度并据此配速。0 = 不限,能多快烧多快",
        )
        .prompt()?;
    if days == 0 {
        return Ok(None);
    }
    let per_day = config::split_over_days(budget_micro, days)?;
    // Spelled out here rather than only on the estimate page: the estimate is
    // several AWS round-trips away, and this is the moment the number was chosen.
    let per_hour = per_day / 24;
    let (lo, hi) = quota::hour_band(per_hour);
    println!(
        "  {}",
        format!(
            "每天 {}(硬上限)· 每小时约 {},整点在 {} – {} 间重取",
            fmt_usd(per_day),
            fmt_usd(per_hour),
            fmt_usd(lo),
            fmt_usd(hi)
        )
        .dimmed()
    );
    Ok(Some(days))
}

/// Ask which bucket to fill. An empty answer is not a mistake worth ending the
/// run over: a missing bucket gets created a few steps below anyway, so the tool
/// can just as well invent the name — nobody wants to sit at a prompt naming a
/// disposable burn bucket, and having no bucket yet is what a first run IS.
fn prompt_bucket(last: Option<&str>) -> Result<String> {
    let mut prompt = inquire::Text::new("目标 S3 桶名称?");
    prompt = match last {
        Some(b) => prompt.with_default(b),
        None => prompt.with_help_message("留空回车 = 从随机候选名里挑一个(桶不存在会自动创建)"),
    };
    let answer = prompt.prompt()?;
    if !answer.trim().is_empty() {
        return Ok(answer.trim().to_string());
    }
    pick_generated_bucket()
}

/// Pick from a batch of invented names, re-rolling until one is liked. Nothing
/// here can end the run: an empty answer at the manual entry lands back on the
/// batch, which is the whole point of this path existing.
fn pick_generated_bucket() -> Result<String> {
    const BATCH: usize = 5;
    const REROLL: &str = "换一批";
    const MANUAL: &str = "自己输入";
    loop {
        let mut options = naming::suggest_bucket_names(BATCH);
        options.push(REROLL.to_string());
        options.push(MANUAL.to_string());
        let choice = inquire::Select::new("挑一个桶名?", options)
            .with_help_message("随机生成,几乎不可能与他人重名;选中后不存在即创建")
            .prompt()?;
        if choice == REROLL {
            continue;
        }
        if choice == MANUAL {
            let typed = inquire::Text::new("目标 S3 桶名称?").prompt()?;
            if !typed.trim().is_empty() {
                return Ok(typed.trim().to_string());
            }
            continue;
        }
        return Ok(choice);
    }
}

/// Another instance holds this state directory's lock. Refusing outright is
/// right about the danger and wrong about the answer: a run under a DIFFERENT
/// key prefix gets its own ledger, its own sweep scope and its own lock — it is
/// exactly the parallel run the refusal used to tell the user to go type by
/// hand. So offer it here, while saying plainly that it is a SECOND budget and
/// not a share of the first.
///
/// Three cases still just fail. `--yes`, because a cron waking up while
/// yesterday's run is still going must never quietly start burning another full
/// budget; and `--resume` / `--checkpoint`, because both pin the ledger to one
/// file — a parallel run writing that same file is the accounting corruption
/// the lock exists to prevent.
fn prompt_parallel_prefix(holder: &HolderInfo, cfg: &BenchConfig, args: &RunArgs) -> Result<String> {
    if cfg.yes || args.resume.is_some() || args.checkpoint.is_some() {
        bail!(
            "已有 {} 在跑,拒绝启动第二个实例。\n  \
             两个实例各记各的账,{} 的硬上限会被花掉两遍。\n  \
             确认它已结束后重试;真要并行请换一个 --key-prefix(各自独立的预算与清扫范围)",
            holder,
            fmt_usd(cfg.budget_micro)
        );
    }
    println!("{} 已有 {} 在跑", "⚠".yellow().bold(), holder);
    println!(
        "  {}{}{}",
        "两个实例各记各的账:另起一个是再花一份 ".yellow(),
        fmt_usd(cfg.budget_micro).yellow().bold(),
        ",不是两个进程分同一份".yellow()
    );
    let suggested = next_free_prefix(cfg, args.dry_run)?;
    let parallel = format!("另起一个并行跑,前缀 {}", suggested);
    const CUSTOM: &str = "另起一个,自己指定前缀";
    const QUIT: &str = "退出,等它跑完";
    let choice = inquire::Select::new(
        "怎么办?",
        vec![parallel, CUSTOM.to_string(), QUIT.to_string()],
    )
    .with_help_message("换前缀 = 换一本账:预算、checkpoint、保留期清扫范围各自独立,互不删对方的数据")
    .prompt()?;
    if choice == QUIT {
        bail!("已取消:等它结束后重试");
    }
    if choice == CUSTOM {
        let typed = inquire::Text::new("新的 --key-prefix?")
            .with_default(&suggested)
            .prompt()?;
        let typed = typed.trim().trim_start_matches('/');
        // A prefix is a delete scope: an empty one would put the whole bucket
        // in reach of the retention sweeper, which a stray Enter must not do.
        if typed.is_empty() {
            return Ok(suggested);
        }
        if typed.ends_with('/') {
            return Ok(typed.to_string());
        }
        return Ok(format!("{}/", typed));
    }
    Ok(suggested)
}

/// The first `<base>-N/` with no state directory yet — a prefix nobody keeps an
/// account under, so the parallel run starts from zero instead of adopting
/// somebody else's stale checkpoint.
fn next_free_prefix(cfg: &BenchConfig, dry_run: bool) -> Result<String> {
    let base = prefix_base(&cfg.key_prefix);
    for n in 2..100 {
        let candidate = format!("{}-{}/", base, n);
        let dir = config::state_dir(cfg.endpoint_url.as_deref(), &cfg.bucket, &candidate, dry_run)?;
        if !dir.exists() {
            return Ok(candidate);
        }
    }
    bail!("{}-2/ 到 {}-99/ 都已有账本,请显式给一个 --key-prefix", base, base)
}

/// The stem a parallel prefix counts up from. `backup/` and `backup-2/` both
/// count from `backup`, so the third run lands on `backup-3/` rather than
/// growing a `backup-2-2/` tail one suffix per parallel run.
fn prefix_base(key_prefix: &str) -> &str {
    let stem = key_prefix.trim_end_matches('/');
    let base = match stem.rsplit_once('-') {
        Some((head, n))
            if !head.is_empty() && !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()) =>
        {
            head
        }
        _ => stem,
    };
    if base.is_empty() {
        "backup"
    } else {
        base
    }
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

#[cfg(test)]
mod tests {
    use super::prefix_base;

    /// However many parallel runs are started, they all count up from the same
    /// stem — otherwise each one would grow another `-2` on the last one's name.
    #[test]
    fn parallel_prefixes_count_from_one_stem() {
        assert_eq!(prefix_base("backup/"), "backup");
        assert_eq!(prefix_base("backup-2/"), "backup");
        assert_eq!(prefix_base("backup-17/"), "backup");
        // A hyphen that is not a counter belongs to the name.
        assert_eq!(prefix_base("db-backup/"), "db-backup");
        assert_eq!(prefix_base("nightly/db/"), "nightly/db");
        // An empty prefix would count up from nothing, so it gets a name.
        assert_eq!(prefix_base(""), "backup");
        assert_eq!(prefix_base("/"), "backup");
    }
}
