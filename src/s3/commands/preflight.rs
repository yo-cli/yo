// Everything that happens before the first byte is uploaded: interactive
// fill-in of missing required params, credential/bucket/CRR checks, the cost
// estimate + confirmation gate, and the checkpoint resume decision.

use anyhow::{bail, Context, Result};
use aws_config::SdkConfig;
use colored::Colorize;
use std::path::Path;
use uuid::Uuid;

use super::args::RunArgs;
use crate::s3::checkpoint::Checkpoint;
use crate::s3::client::{
    build_s3_client, discover_bucket_region, load_shared_config, print_caller_identity,
    resolved_region, ClientOpts,
};
use crate::s3::config::BenchConfig;
use crate::s3::cost::{pricing_for, print_estimate, write_lifecycle_files, Pricing};
use crate::s3::{crr, fmt_bytes, fmt_usd};

pub struct DestTarget {
    pub bucket: String,
    pub client: aws_sdk_s3::Client,
}

pub struct RunContext {
    pub cfg: BenchConfig,
    pub s3: aws_sdk_s3::Client,
    pub dest: Option<DestTarget>,
    /// CRR drives the budget math. True when replication is configured — or in
    /// --dry-run without it, where the intended engine is simulated so the
    /// rehearsal terminates like a real run would.
    pub crr_active: bool,
    pub pricing: Pricing,
    pub ckpt: Checkpoint,
    pub resumed: bool,
    pub run_id: Uuid,
}

pub async fn prepare(args: RunArgs) -> Result<RunContext> {
    // --- 1. fill in the two required params, interactively when missing ---
    let budget_micro = match args.budget {
        Some(b) => b,
        None if args.yes => bail!("--yes 模式下必须显式提供 --budget"),
        None => {
            let usd = inquire::CustomType::<f64>::new("要烧掉多少预算(美元)?")
                .with_default(500.0)
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
        None => inquire::Text::new("目标 S3 桶名称?").prompt()?,
    };

    let cfg = BenchConfig {
        bucket,
        key_prefix: args.key_prefix.clone(),
        budget_micro,
        region: args.region.clone(),
        endpoint_url: args.endpoint_url.clone(),
        path_style: args.path_style,
        insecure_skip_tls_verify: args.insecure_skip_tls_verify,
        object_size: args.object_size,
        part_size: args.part_size,
        pool_size: args.pool_size,
        concurrent_objects: args.concurrent_objects,
        concurrent_parts: args.concurrent_parts,
        rate_min: args.rate_min,
        rate_max: args.rate_max,
        rate_mode: args.rate_mode,
        rate_resample_interval: args.rate_resample_interval,
        retain: args.retain,
        total_size: args.total_size,
        iterations: args.iterations,
        stop_when: args.stop_when,
        max_duration: args.max_duration,
        checkpoint_path: args.checkpoint.clone(),
        summary_out: args.summary_out.clone(),
        report_interval: args.report_interval,
        dry_run: args.dry_run,
        yes: args.yes,
    };
    cfg.validate()?;

    // --- 2. credentials + clients ---
    let shared = load_shared_config(cfg.region.as_deref()).await;
    print_caller_identity(&shared, cfg.dry_run).await?;
    let opts = ClientOpts {
        endpoint_url: cfg.endpoint_url.clone(),
        path_style: cfg.path_style,
        insecure_skip_tls_verify: cfg.insecure_skip_tls_verify,
    };
    let s3 = build_s3_client(&shared, &opts, None)?;

    // --- 3. bucket reachability + region (drives the pricing table) ---
    let bucket_region = match discover_bucket_region(&s3, &cfg.bucket).await {
        Ok(r) => {
            println!(
                "{} 桶 {} 可达{}",
                "✓".green(),
                cfg.bucket.bold(),
                r.as_deref().map(|x| format!("(区域 {})", x)).unwrap_or_default()
            );
            r
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
    let pricing_region = bucket_region
        .clone()
        .or_else(|| resolved_region(&shared))
        .unwrap_or_else(|| "us-east-1".to_string());
    let pricing = pricing_for(&pricing_region);

    // --- 4. versioning + CRR detection (AWS only; skipped for S3-compat) ---
    let mut dest: Option<DestTarget> = None;
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
        dest = resolve_crr(&shared, &s3, &cfg, bucket_region.as_deref()).await?;
    } else {
        println!(
            "{} 自定义端点模式:跨区复制为 AWS 原生特性,此处不可用,退化为纯写入(烧钱极慢)",
            "⚠".yellow()
        );
    }

    // No engine + unattended + nothing else bounds the run = it would never stop.
    if dest.is_none()
        && cfg.yes
        && cfg.total_size.is_none()
        && cfg.iterations.is_none()
        && cfg.max_duration.is_none()
        && !cfg.dry_run
    {
        bail!(
            "未启用跨区复制时纯请求费烧不动预算,运行将永不停止。\
             请先跑 yo-s3 setup-crr,或提供 --total-size / --iterations / --max-duration 之一作为边界"
        );
    }

    // --- 5. estimate + lifecycle files + the confirmation gate ---
    let crr_assumed_for_dry = cfg.dry_run && dest.is_none() && cfg.endpoint_url.is_none();
    if crr_assumed_for_dry {
        println!(
            "{} dry-run 按「已启用跨区复制」口径模拟烧钱(实跑前先 yo-s3 setup-crr)",
            "ℹ".blue()
        );
    }
    let crr_active = dest.is_some() || crr_assumed_for_dry;
    print_estimate(&cfg, &pricing, crr_active, dest.as_ref().map(|d| d.bucket.as_str()));
    write_lifecycle_files(
        &cfg.key_prefix,
        &cfg.bucket,
        dest.as_ref().map(|d| d.bucket.as_str()),
    );
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

    // --- 6. checkpoint: fresh, or resume with strict snapshot validation ---
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

    Ok(RunContext {
        cfg,
        s3,
        dest,
        crr_active,
        pricing,
        ckpt,
        resumed,
        run_id,
    })
}

/// Detect CRR; when missing, offer to set it up on the spot (interactive only).
async fn resolve_crr(
    shared: &SdkConfig,
    s3: &aws_sdk_s3::Client,
    cfg: &BenchConfig,
    bucket_region: Option<&str>,
) -> Result<Option<DestTarget>> {
    let detected = match crr::detect(s3, &cfg.bucket).await {
        Ok(d) => d,
        Err(e) => {
            if cfg.dry_run {
                eprintln!("{} 复制配置读取失败({:#}),--dry-run 继续", "⚠".yellow(), e);
                return Ok(None);
            }
            return Err(e);
        }
    };

    let dest_bucket = match detected {
        Some(info) => {
            println!("{} 跨区复制已配置 → 目标桶 {}", "✓".green(), info.dest_bucket.bold());
            Some(info.dest_bucket)
        }
        None if cfg.dry_run || cfg.yes => {
            println!("{} 未配置跨区复制(烧钱主引擎缺失)", "⚠".yellow().bold());
            None
        }
        None => {
            println!("{} 未配置跨区复制 —— 它是烧钱主引擎(跨区流量 ~$0.02/GB)", "⚠".yellow().bold());
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
                "现在自动配置(建目标桶+复制规则,推荐)" => {
                    let source_region = bucket_region
                        .map(|s| s.to_string())
                        .or_else(|| resolved_region(shared))
                        .context("无法确定源桶区域,请显式传 --region")?;
                    let suggested = if source_region == "us-west-2" { "us-east-1" } else { "us-west-2" };
                    let dest_region = inquire::Text::new("复制目标区域?")
                        .with_default(suggested)
                        .prompt()?;
                    let dest = crr::setup(
                        shared,
                        s3,
                        &cfg.bucket,
                        &source_region,
                        &dest_region,
                        &cfg.key_prefix,
                    )
                    .await?;
                    Some(dest)
                }
                "退出" => bail!("已取消"),
                _ => None,
            }
        }
    };

    match dest_bucket {
        None => Ok(None),
        Some(bucket) => {
            // Destination may live in another region — build its client there.
            let dest_region = discover_bucket_region(s3, &bucket).await.unwrap_or(None);
            let opts = ClientOpts::default();
            let client = build_s3_client(shared, &opts, dest_region.as_deref())?;
            Ok(Some(DestTarget { bucket, client }))
        }
    }
}
