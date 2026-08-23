// `yo-s3 setup-crr`: one-shot idempotent Cross-Region Replication setup.

use anyhow::{bail, Context, Result};
use colored::Colorize;

use super::args::SetupCrrArgs;
use crate::s3::client::{
    build_s3_client, discover_bucket_region, load_shared_config, print_caller_identity,
    resolved_region, ClientOpts,
};
use crate::s3::crr;

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

    let dest_region = match args.dest_region {
        Some(r) => r,
        None if args.yes => bail!("--yes 模式下必须显式提供 --dest-region"),
        None => {
            let suggested = if source_region == "us-west-2" { "us-east-1" } else { "us-west-2" };
            inquire::Text::new("复制目标区域?")
                .with_default(suggested)
                .with_help_message("必须与源区域不同,跨区流量才产生费用")
                .prompt()?
        }
    };

    if !args.yes {
        let go = inquire::Confirm::new(&format!(
            "将执行:{} 开版本控制 → {} 区域建目标桶并开版本控制 → 创建复制 IAM 角色 → 写入复制规则(前缀 {})。继续?",
            bucket, dest_region, args.key_prefix
        ))
        .with_default(true)
        .prompt()?;
        if !go {
            bail!("已取消");
        }
    }

    let dest_bucket = crr::setup(
        &shared,
        &s3,
        &bucket,
        &source_region,
        &dest_region,
        &args.key_prefix,
    )
    .await?;

    println!(
        "\n{} 跨区复制配置完成:{} → {}\n  下一步直接开烧: {}",
        "✓".green().bold(),
        bucket,
        dest_bucket,
        format!("yo-s3 --bucket {}", bucket).bold()
    );
    Ok(())
}
