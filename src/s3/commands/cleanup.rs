// `yo-s3 cleanup`: manual last-resort cleanup — abort orphan multipart
// uploads and (after confirmation) physically delete every tool-created
// object version under the prefix, on the source AND the replication target.

use anyhow::{bail, Result};
use colored::Colorize;

use super::args::CleanupArgs;
use crate::s3::client::{
    build_s3_client, discover_bucket_region, load_shared_config, print_caller_identity, ClientOpts,
};
use crate::s3::config;
use crate::s3::lock::{self, Acquired, RunLock};
use crate::s3::registry::abort_orphans;
use crate::s3::{crr, fmt_bytes, sweep};

pub async fn run(args: CleanupArgs) -> Result<()> {
    let bucket = match args.bucket.clone() {
        Some(b) => b,
        None if args.yes => bail!("--yes 模式下必须显式提供 --bucket"),
        None => inquire::Text::new("要清理的 S3 桶名称?").prompt()?,
    };

    // Incompatible flags fail before credentials, clients or any deletion —
    // finding out at the very end that teardown never applied is worthless.
    if args.all && args.endpoint_url.is_some() {
        bail!("--all 仅适用于 AWS:跨区复制不是 S3 兼容存储的特性,自定义端点下没有额外可删的东西");
    }

    // A live run's in-flight multipart uploads sit under this very prefix, and
    // abort_orphans below cannot tell them from real orphans — aborting them
    // throws away parts whose request and traffic fees are already paid.
    let state = config::state_dir(args.endpoint_url.as_deref(), &bucket, &args.key_prefix, false)?;
    config::ensure_state_dir(&state)?;
    let _lock: Option<RunLock> = match lock::try_acquire(&state, "yo-s3 cleanup")? {
        Acquired::Held(l) => Some(l),
        Acquired::Busy(holder) if args.force => {
            println!(
                "{} {} 正在跑,--force 已指定,继续清理(会 abort 它的在途分段)",
                "⚠".yellow(),
                holder
            );
            None
        }
        Acquired::Busy(holder) => bail!(
            "{} 正在跑,拒绝清理:cleanup 会 abort 它正在传的分段,\
             那些 part 的请求费和流量费已经花掉了。\n  \
             等它结束后重试;确实要打断它请加 --force",
            holder
        ),
    };

    let shared = load_shared_config(args.region.as_deref()).await;
    print_caller_identity(&shared, false).await?;
    let opts = ClientOpts {
        endpoint_url: args.endpoint_url.clone(),
        path_style: args.path_style,
        insecure_skip_tls_verify: args.insecure_skip_tls_verify,
        // Cleanup is list/delete only — the accelerate endpoint serves neither.
        accelerate: false,
    };
    let s3 = build_s3_client(&shared, &opts, None)?;

    // Source + every replication destination. Missing one leaves a full copy
    // of the data billing storage in that region forever.
    let mut dest_targets: Vec<(aws_sdk_s3::Client, String)> = Vec::new();
    if args.endpoint_url.is_none() {
        if let Ok(dest_buckets) = crr::detect(&s3, &bucket).await {
            for dest_bucket in dest_buckets {
                let dest_region = discover_bucket_region(&s3, &dest_bucket).await.unwrap_or(None);
                let dest_client =
                    build_s3_client(&shared, &ClientOpts::default(), dest_region.as_deref())?;
                println!("{} 检测到复制目标桶 {},一并清理", "ℹ".blue(), dest_bucket);
                dest_targets.push((dest_client, dest_bucket));
            }
        }
    }
    let targets: Vec<(aws_sdk_s3::Client, String)> = std::iter::once((s3.clone(), bucket.clone()))
        .chain(dest_targets.iter().cloned())
        .collect();

    for (client, target_bucket) in &targets {
        println!("\n{} 清理 {}(前缀 {})", "📦".to_string(), target_bucket.bold(), args.key_prefix);

        // 1. orphan multipart uploads: always aborted, they only cost money
        match abort_orphans(client, target_bucket, &args.key_prefix).await {
            Ok(0) => println!("{} 无未完成的分段上传", "✓".green()),
            Ok(n) => println!("{} 已 abort {} 个未完成分段上传", "✓".green(), n),
            Err(e) => eprintln!("{} 分段上传清理失败: {:#}", "✗".red(), e),
        }

        // 2. object versions: list, show, confirm, delete
        let remaining = match sweep::count_remaining(client, target_bucket, &args.key_prefix).await {
            Ok(r) => r,
            Err(e) => {
                eprintln!("{} 列举对象失败: {:#}", "✗".red(), e);
                continue;
            }
        };
        if remaining.deleted == 0 {
            println!("{} 前缀下无对象版本", "✓".green());
            continue;
        }
        println!(
            "  发现 {} 个对象版本,共 {}",
            remaining.deleted,
            fmt_bytes(remaining.bytes).bold()
        );
        let confirmed = args.yes
            || inquire::Confirm::new(&format!(
                "物理删除 {} 中这 {} 个版本?(不可恢复)",
                target_bucket, remaining.deleted
            ))
            .with_default(false)
            .prompt()?;
        if !confirmed {
            println!("{} 跳过对象删除", "ℹ".blue());
            continue;
        }
        match sweep::sweep_versions_before(client, target_bucket, &args.key_prefix, chrono::Utc::now())
            .await
        {
            Ok(stats) => println!(
                "{} 已物理删除 {} 个版本({})",
                "✓".green(),
                stats.deleted,
                fmt_bytes(stats.bytes)
            ),
            Err(e) => eprintln!("{} 删除失败: {:#}", "✗".red(), e),
        }
    }

    if args.all {
        teardown(&args, &shared, &s3, &bucket, &dest_targets).await?;
    }

    println!("\n{} 清理完成", "✓".green().bold());
    if !args.all && !dest_targets.is_empty() {
        println!(
            "  {}",
            format!(
                "目标桶与复制角色仍留在账号里(不计费但会一直在);要一并删掉: yo-s3 cleanup --bucket {} --all",
                bucket
            )
            .dimmed()
        );
    }
    Ok(())
}

/// Remove what the automatic CRR setup created. Separate from object cleanup
/// because it destroys infrastructure rather than data, and is irreversible.
async fn teardown(
    args: &CleanupArgs,
    shared: &aws_config::SdkConfig,
    s3: &aws_sdk_s3::Client,
    bucket: &str,
    dest_targets: &[(aws_sdk_s3::Client, String)],
) -> Result<()> {
    let plan = crr::teardown_plan(shared, s3, bucket, dest_targets).await?;
    if plan.is_empty() {
        println!("\n{} 没有可拆除的复制配置", "✓".green());
        return Ok(());
    }

    println!("\n{} 将删除以下资源(不可恢复):", "⚠".yellow().bold());
    if plan.has_replication_config {
        println!("  · {} 上的复制规则", bucket);
    }
    for dest in &plan.dests {
        // Adopted buckets get deleted too, but never silently: the name is
        // derived from the source bucket, so one may predate this tool.
        let note = if dest.created_by_us {
            "本工具创建".green()
        } else {
            "非本工具创建,将被整桶清空后删除".yellow().bold()
        };
        println!("  · 目标桶 {}({})", dest.bucket.bold(), note);
    }
    if let Some(role) = &plan.role_name {
        println!("  · IAM 角色 {} 及其内联策略", role);
    }
    // "--all" invites exactly one worry; answer it before asking for a yes.
    println!("  {}", format!("源桶 {} 本身不会被删除", bucket).dimmed());

    let confirmed = args.yes
        || inquire::Confirm::new("确认删除?目标桶会被整桶清空后删除")
            .with_default(false)
            .prompt()?;
    if !confirmed {
        println!("{} 跳过", "ℹ".blue());
        return Ok(());
    }

    crr::teardown(shared, s3, bucket, dest_targets, &plan).await
}
