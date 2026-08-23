// `yo-s3 cleanup`: manual last-resort cleanup — abort orphan multipart
// uploads and (after confirmation) physically delete every tool-created
// object version under the prefix, on the source AND the replication target.

use anyhow::{bail, Result};
use colored::Colorize;

use super::args::CleanupArgs;
use crate::s3::client::{
    build_s3_client, discover_bucket_region, load_shared_config, print_caller_identity, ClientOpts,
};
use crate::s3::registry::abort_orphans;
use crate::s3::{crr, fmt_bytes, sweep};

pub async fn run(args: CleanupArgs) -> Result<()> {
    let bucket = match args.bucket.clone() {
        Some(b) => b,
        None if args.yes => bail!("--yes 模式下必须显式提供 --bucket"),
        None => inquire::Text::new("要清理的 S3 桶名称?").prompt()?,
    };

    let shared = load_shared_config(args.region.as_deref()).await;
    print_caller_identity(&shared, false).await?;
    let opts = ClientOpts {
        endpoint_url: args.endpoint_url.clone(),
        path_style: args.path_style,
        insecure_skip_tls_verify: args.insecure_skip_tls_verify,
    };
    let s3 = build_s3_client(&shared, &opts, None)?;

    // Source + (when replication is configured) destination
    let mut targets: Vec<(aws_sdk_s3::Client, String)> = vec![(s3.clone(), bucket.clone())];
    if args.endpoint_url.is_none() {
        if let Ok(Some(info)) = crr::detect(&s3, &bucket).await {
            let dest_region = discover_bucket_region(&s3, &info.dest_bucket).await.unwrap_or(None);
            let dest_client = build_s3_client(&shared, &ClientOpts::default(), dest_region.as_deref())?;
            println!("{} 检测到复制目标桶 {},一并清理", "ℹ".blue(), info.dest_bucket);
            targets.push((dest_client, info.dest_bucket));
        }
    }

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

    println!("\n{} 清理完成", "✓".green().bold());
    Ok(())
}
