// S3 Transfer Acceleration: the upload-path surcharge.
//
// Unlike CRR, this bills the leg the tool already writes — every uploaded byte
// costs $0.04/GB extra when it arrives through an edge location. It stacks on
// top of whatever burn mode is running.
//
// THE TRAP this module exists to prevent: AWS does not charge for Transfer
// Acceleration when it decides the transfer would not have been faster than a
// direct one ("we will not charge ... and we may bypass the S3 Transfer
// Acceleration system for that upload"). A client in the same region as the
// bucket is exactly that case — the surcharge silently never materializes and
// the tool would report burning money it never spent. Hence the loud
// same-region warning below.

use anyhow::{bail, Context, Result};
use aws_sdk_s3::types::{BucketAccelerateStatus, AccelerateConfiguration};
use colored::Colorize;

/// Regions where Transfer Acceleration can be enabled on a bucket.
/// A bucket outside this list simply cannot use the accelerate endpoint.
const SUPPORTED_REGIONS: &[&str] = &[
    "us-east-1",
    "us-east-2",
    "us-west-1",
    "us-west-2",
    "ca-central-1",
    "eu-west-1",
    "eu-west-2",
    "eu-west-3",
    "eu-central-1",
    "ap-northeast-1",
    "ap-northeast-2",
    "ap-south-1",
    "ap-southeast-1",
    "ap-southeast-2",
    "sa-east-1",
];

pub fn region_supported(region: &str) -> bool {
    SUPPORTED_REGIONS.contains(&region)
}

/// Is Transfer Acceleration currently Enabled on the bucket?
pub async fn enabled(client: &aws_sdk_s3::Client, bucket: &str) -> Result<bool> {
    let out = client
        .get_bucket_accelerate_configuration()
        .bucket(bucket)
        .send()
        .await
        .with_context(|| {
            format!(
                "读取 {} 的传输加速状态失败(需要 s3:GetAccelerateConfiguration 权限)",
                bucket
            )
        })?;
    Ok(out.status() == Some(&BucketAccelerateStatus::Enabled))
}

/// Turn Transfer Acceleration on. Idempotent.
pub async fn enable(client: &aws_sdk_s3::Client, bucket: &str) -> Result<()> {
    client
        .put_bucket_accelerate_configuration()
        .bucket(bucket)
        .accelerate_configuration(
            AccelerateConfiguration::builder()
                .status(BucketAccelerateStatus::Enabled)
                .build(),
        )
        .send()
        .await
        .with_context(|| {
            format!(
                "为 {} 开启传输加速失败。当前身份需要 s3:PutAccelerateConfiguration 权限,\
                 且必须是桶的所有者;也可手动执行:\n  \
                 aws s3api put-bucket-accelerate-configuration --bucket {} \
                 --accelerate-configuration Status=Enabled",
                bucket, bucket
            )
        })?;
    println!("{} {} 传输加速已开启", "✓".green(), bucket);
    println!(
        "  {}",
        "注意:开启后最多 20 分钟才完全生效,这段时间内加速与计费可能都还没起来".dimmed()
    );
    Ok(())
}

/// Reject the configurations where the accelerate endpoint cannot work at all.
/// These are hard AWS constraints, so failing here beats failing per-request.
pub fn validate(bucket: &str, path_style: bool, endpoint_url: Option<&str>) -> Result<()> {
    if endpoint_url.is_some() {
        bail!("--transfer-acceleration 是 AWS 原生特性,不能与 --endpoint-url 同用");
    }
    if path_style {
        bail!("传输加速只支持 virtual-hosted 寻址,不能与 --path-style 同用");
    }
    if bucket.contains('.') {
        bail!(
            "传输加速要求桶名不含点号(当前: {}) —— 这是 AWS 硬性限制,该桶无法使用加速端点",
            bucket
        );
    }
    Ok(())
}

/// Warn when the surcharge is unlikely to actually be billed.
pub fn warn_if_not_accelerated(bucket_region: Option<&str>, client_region: Option<&str>) {
    if let (Some(b), Some(c)) = (bucket_region, client_region) {
        if b == c {
            println!(
                "{} 客户端与桶同在 {} —— AWS 判定加速无收益时{}(官方原话:not likely to be faster 就不计费)。\
                 想让这项真正烧钱,客户端要离桶足够远(如 EC2 在 us-east-1、桶在 ap-southeast-2)",
                "⚠".yellow().bold(),
                b,
                "不会收取加速费,预估里的这一项会落空".bold()
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dotted_bucket_is_rejected() {
        let err = validate("my.bucket", false, None).unwrap_err().to_string();
        assert!(err.contains("点号"), "{}", err);
        assert!(validate("my-bucket", false, None).is_ok());
    }

    #[test]
    fn incompatible_switches_are_rejected() {
        assert!(validate("b", true, None).is_err()); // path-style
        assert!(validate("b", false, Some("http://minio:9000")).is_err());
    }

    #[test]
    fn region_support_list() {
        assert!(region_supported("us-east-1"));
        assert!(region_supported("ap-southeast-2"));
        // Transfer Acceleration is not offered everywhere
        assert!(!region_supported("eu-north-1"));
        assert!(!region_supported("mars-north-1"));
    }
}
