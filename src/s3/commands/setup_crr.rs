// `yo-s3 setup-crr`: one-shot idempotent Cross-Region Replication setup.

use anyhow::{bail, Context, Result};
use colored::Colorize;

use super::args::SetupCrrArgs;
use crate::s3::client::{
    build_s3_client, discover_bucket_region, load_shared_config, print_caller_identity,
    resolved_region, ClientOpts,
};
use crate::s3::crr;
use crate::s3::modes::crr::{default_dest_regions, prompt_dest_regions};

pub async fn run(args: SetupCrrArgs) -> Result<()> {
    let bucket = match args.bucket {
        Some(b) => b,
        None if args.yes => bail!("--yes 模式下必须显式提供 --bucket"),
        None => inquire::Text::new("源 S3 桶名称?").prompt()?,
    };

    let shared = load_shared_config(args.region.as_deref()).await;
    print_caller_identity(&shared, false).await?;
    let s3 = build_s3_client(&shared, &ClientOpts::default(), None)?;

    let source_region = discover_bucket_region(&s3, &bucket)
        .await
        .context("源桶不可达")?
        .or_else(|| resolved_region(&shared))
        .context("无法确定源桶区域,请显式传 --region")?;
    println!("{} 源桶 {}(区域 {})", "✓".green(), bucket.bold(), source_region);

    let dest_regions = if !args.dest_regions.is_empty() {
        args.dest_regions
    } else if args.yes {
        // Unattended: take the default fan-out, but say exactly what it will
        // create — this makes buckets in 5 regions.
        let defaults = default_dest_regions(&source_region);
        println!(
            "{} 未指定 --dest-region,使用默认 {} 个目标区域: {}",
            "ℹ".blue(),
            defaults.len(),
            defaults.join(", ")
        );
        defaults
    } else {
        prompt_dest_regions(&source_region)?
    };

    if !args.yes {
        let go = inquire::Confirm::new(&format!(
            "将执行:{} 开版本控制 → 在 {} 各建目标桶并开版本控制 → 创建复制 IAM 角色 → 写入 {} 条复制规则(前缀 {})。\
             每字节将产生 {} 份跨区流量费。继续?",
            bucket,
            dest_regions.join(" / "),
            dest_regions.len(),
            args.key_prefix,
            dest_regions.len()
        ))
        .with_default(true)
        .prompt()?;
        if !go {
            bail!("已取消");
        }
    }

    let dest_buckets = crr::setup(
        &shared,
        &s3,
        &bucket,
        &source_region,
        &dest_regions,
        &args.key_prefix,
    )
    .await?;

    println!(
        "\n{} 跨区复制配置完成:{} → {} 个目标({})\n  烧钱速率已放大 {}× \n  下一步直接开烧: {}",
        "✓".green().bold(),
        bucket,
        dest_buckets.len(),
        dest_buckets.join(" + "),
        dest_buckets.len(),
        format!("yo-s3 --bucket {}", bucket).bold()
    );
    Ok(())
}
