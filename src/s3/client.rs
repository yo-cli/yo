// AWS client construction. Credentials come from the standard chain (EC2 IAM
// Role via IMDS / env vars / ~/.aws); when the chain is empty, auth.rs fills it
// in interactively and can remember the keys as an ~/.aws profile.

use anyhow::{bail, Context, Result};
use aws_config::{BehaviorVersion, Region, SdkConfig};
use aws_sdk_s3::config::retry::RetryConfig;
use aws_sdk_s3::config::timeout::TimeoutConfig;
use aws_sdk_s3::config::RequestChecksumCalculation;
use aws_smithy_types::error::display::DisplayErrorContext;
use colored::Colorize;
use std::time::Duration;

/// Connection options shared by run / cleanup.
#[derive(Debug, Clone, Default)]
pub struct ClientOpts {
    pub endpoint_url: Option<String>,
    pub path_style: bool,
    pub insecure_skip_tls_verify: bool,
    /// Route uploads through the S3 Transfer Acceleration edge endpoint.
    pub accelerate: bool,
}

/// Load the shared AWS config (region from --region / env / profile / IMDS).
pub async fn load_shared_config(
    region_override: Option<&str>,
    profile: Option<&str>,
) -> SdkConfig {
    let mut loader = aws_config::defaults(BehaviorVersion::latest());
    if let Some(r) = region_override {
        loader = loader.region(Region::new(r.to_string()));
    }
    if let Some(p) = profile {
        loader = loader.profile_name(p);
    }
    loader.load().await
}

/// Build the S3 client for the given region (None = the shared config region).
///
/// - SDK retries are fully DISABLED: retry/backoff is owned by uploader.rs so
///   503 SlowDown can be counted and backed off explicitly, without two retry
///   layers amplifying each other.
/// - With a custom endpoint (MinIO/Ceph): path-style addressing and
///   `when_required` checksums are switched on automatically — older S3
///   compatibles reject x-amz-checksum trailers.
/// - No Content-MD5 anywhere: single-core MD5 (a few hundred MB/s) would cap
///   throughput; the SDK's default CRC32 is fine.
pub fn build_s3_client(
    shared: &SdkConfig,
    opts: &ClientOpts,
    region_override: Option<&str>,
) -> Result<aws_sdk_s3::Client> {
    if opts.insecure_skip_tls_verify {
        let https = opts
            .endpoint_url
            .as_deref()
            .map(|e| e.starts_with("https"))
            .unwrap_or(true);
        if https {
            bail!(
                "--insecure-skip-tls-verify 暂不支持 https 自签端点;\
                 自建测试环境请改用 http:// 端点,或把自签 CA 加入系统信任"
            );
        }
        eprintln!(
            "{}",
            "⚠ --insecure-skip-tls-verify 已指定(http 端点本身不走 TLS,该开关无实际作用)"
                .yellow()
        );
    }

    let mut builder = aws_sdk_s3::config::Builder::from(shared)
        .retry_config(RetryConfig::disabled())
        .timeout_config(
            TimeoutConfig::builder()
                .connect_timeout(Duration::from_secs(10))
                .build(),
        );
    if let Some(r) = region_override {
        builder = builder.region(Region::new(r.to_string()));
    }
    if let Some(endpoint) = &opts.endpoint_url {
        builder = builder
            .endpoint_url(endpoint.clone())
            .force_path_style(true)
            .request_checksum_calculation(RequestChecksumCalculation::WhenRequired);
    }
    if opts.path_style {
        builder = builder.force_path_style(true);
    }
    if opts.accelerate {
        // Sends to <bucket>.s3-accelerate.amazonaws.com. Virtual-hosted only,
        // so it is incompatible with path-style (rejected in validate()).
        builder = builder.accelerate(true);
    }
    Ok(aws_sdk_s3::Client::from_conf(builder.build()))
}

pub struct CallerIdentity {
    pub arn: String,
    pub account: String,
}

/// Who are we? This is also the credential validator: it is the cheapest call
/// that proves a credential set actually works, so `auth.rs` gates persistence
/// on it (CLAUDE.md: 凭据先校验、后持久化).
pub async fn caller_identity(shared: &SdkConfig) -> Result<CallerIdentity> {
    match aws_sdk_sts::Client::new(shared).get_caller_identity().send().await {
        Ok(id) => Ok(CallerIdentity {
            arn: id.arn().unwrap_or("<unknown>").to_string(),
            account: id.account().unwrap_or("<unknown>").to_string(),
        }),
        // DisplayErrorContext, not `{}`: the top frame of an SdkError is a
        // useless label like "dispatch failure" — the sentence naming the
        // actual cause ("no IAM role", DNS, timeout) is further down the
        // source chain and is exactly what the user needs to see.
        Err(e) => bail!("{}", DisplayErrorContext(&e)),
    }
}

/// The fallback message for when we cannot ask (unattended, or the user quit).
pub fn credential_hint(err: &anyhow::Error) -> String {
    format!(
        "获取 AWS 身份失败: {}\n  排查:\n  \
         1. EC2 上:实例是否挂了 IAM Role?(控制台 → EC2 → 实例 → 操作 → 安全 → 修改 IAM 角色)\n  \
         2. 本机:环境变量 AWS_ACCESS_KEY_ID / AWS_SECRET_ACCESS_KEY 或 ~/.aws/credentials 是否配置?\n  \
         3. 已有 profile 时加 --profile <名>;交互模式下会直接让你选或粘贴凭据",
        err
    )
}

/// Region actually resolved by the config chain (for the pricing table).
pub fn resolved_region(shared: &SdkConfig) -> Option<String> {
    shared.region().map(|r| r.to_string())
}

/// Discover which region a bucket lives in. Works cross-region: HeadBucket
/// returns the region in its output, and even a 301 redirect error carries an
/// `x-amz-bucket-region` header.
pub enum BucketProbe {
    /// Reachable; carries the region when the API disclosed it.
    Exists(Option<String>),
    /// HeadBucket said 404. Distinct from 403 on purpose: 404 means the name is
    /// free to create, 403 means it exists and is not ours — offering to create
    /// in that case would just fail with BucketAlreadyExists.
    Missing,
}

impl BucketProbe {
    pub fn region(&self) -> Option<String> {
        match self {
            BucketProbe::Exists(r) => r.clone(),
            BucketProbe::Missing => None,
        }
    }
}

pub async fn discover_bucket_region(
    client: &aws_sdk_s3::Client,
    bucket: &str,
) -> Result<BucketProbe> {
    match client.head_bucket().bucket(bucket).send().await {
        Ok(out) => Ok(BucketProbe::Exists(out.bucket_region().map(|s| s.to_string()))),
        Err(sdk_err) => {
            if let Some(raw) = sdk_err.raw_response() {
                // A cross-region bucket answers with a redirect that still
                // names its region — that counts as existing.
                if let Some(region) = raw.headers().get("x-amz-bucket-region") {
                    return Ok(BucketProbe::Exists(Some(region.to_string())));
                }
                if raw.status().as_u16() == 404 {
                    return Ok(BucketProbe::Missing);
                }
            }
            Err(sdk_err).context(format!("HeadBucket {} 失败", bucket))
        }
    }
}
