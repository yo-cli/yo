// Cross-Region Replication: detection, one-shot setup, backlog sampling.
// CRR is the burn engine — inter-region transfer (~$0.02/GB) is the only cost
// that accrues immediately and linearly with bytes written.

use anyhow::{anyhow, bail, Context, Result};
use aws_config::SdkConfig;
use aws_sdk_s3::error::ProvideErrorMetadata;
use aws_sdk_s3::types::{
    BucketLocationConstraint, BucketVersioningStatus, CreateBucketConfiguration,
    DeleteMarkerReplication, DeleteMarkerReplicationStatus, Destination, ReplicationConfiguration,
    ReplicationRule, ReplicationRuleFilter, ReplicationRuleStatus, ReplicationStatus,
    VersioningConfiguration,
};
use colored::Colorize;
use std::time::Duration;

#[derive(Debug, Clone)]
pub struct CrrInfo {
    pub dest_bucket: String,
    pub dest_region: Option<String>,
}

/// Read the bucket's replication config; None when replication is not set up.
pub async fn detect(client: &aws_sdk_s3::Client, bucket: &str) -> Result<Option<CrrInfo>> {
    match client.get_bucket_replication().bucket(bucket).send().await {
        Ok(out) => {
            let Some(conf) = out.replication_configuration() else {
                return Ok(None);
            };
            for rule in conf.rules() {
                if rule.status() != &ReplicationRuleStatus::Enabled {
                    continue;
                }
                if let Some(dest) = rule.destination() {
                    let arn = dest.bucket();
                    let dest_bucket = arn.trim_start_matches("arn:aws:s3:::").to_string();
                    return Ok(Some(CrrInfo {
                        dest_bucket,
                        dest_region: None, // discovered later via HeadBucket
                    }));
                }
            }
            Ok(None)
        }
        Err(e) => {
            if e.code() == Some("ReplicationConfigurationNotFoundError") {
                Ok(None)
            } else {
                Err(anyhow!("读取复制配置失败: {}", e))
            }
        }
    }
}

/// Is versioning enabled on the bucket?
pub async fn versioning_enabled(client: &aws_sdk_s3::Client, bucket: &str) -> Result<bool> {
    let out = client
        .get_bucket_versioning()
        .bucket(bucket)
        .send()
        .await
        .with_context(|| format!("读取 {} 版本控制状态失败", bucket))?;
    Ok(out.status() == Some(&BucketVersioningStatus::Enabled))
}

async fn ensure_versioning(client: &aws_sdk_s3::Client, bucket: &str) -> Result<()> {
    if versioning_enabled(client, bucket).await? {
        println!("{} {} 版本控制已开启", "✓".green(), bucket);
        return Ok(());
    }
    client
        .put_bucket_versioning()
        .bucket(bucket)
        .versioning_configuration(
            VersioningConfiguration::builder()
                .status(BucketVersioningStatus::Enabled)
                .build(),
        )
        .send()
        .await
        .with_context(|| format!("为 {} 开启版本控制失败", bucket))?;
    println!("{} {} 版本控制已开启(CRR 必需)", "✓".green(), bucket);
    Ok(())
}

/// One-shot idempotent CRR setup: versioning on both buckets, destination
/// bucket in the target region, replication IAM role, replication rule
/// scoped to the tool prefix. Returns the destination bucket name.
pub async fn setup(
    shared: &SdkConfig,
    source_client: &aws_sdk_s3::Client,
    source_bucket: &str,
    source_region: &str,
    dest_region: &str,
    key_prefix: &str,
) -> Result<String> {
    if dest_region == source_region {
        bail!(
            "目标区域 {} 与源区域相同 —— 同区复制不产生跨区流量费,烧钱引擎无效",
            dest_region
        );
    }

    // 1. versioning on source
    ensure_versioning(source_client, source_bucket).await?;

    // 2. destination bucket (deterministic name, ≤63 chars)
    let dest_bucket = dest_bucket_name(source_bucket, dest_region);
    let dest_client = {
        let builder = aws_sdk_s3::config::Builder::from(shared)
            .region(aws_config::Region::new(dest_region.to_string()));
        aws_sdk_s3::Client::from_conf(builder.build())
    };
    match dest_client.head_bucket().bucket(&dest_bucket).send().await {
        Ok(_) => println!("{} 目标桶 {} 已存在({})", "✓".green(), dest_bucket, dest_region),
        Err(_) => {
            let mut create = dest_client.create_bucket().bucket(&dest_bucket);
            // us-east-1 must NOT carry a LocationConstraint
            if dest_region != "us-east-1" {
                create = create.create_bucket_configuration(
                    CreateBucketConfiguration::builder()
                        .location_constraint(BucketLocationConstraint::from(dest_region))
                        .build(),
                );
            }
            match create.send().await {
                Ok(_) => println!("{} 已创建目标桶 {}({})", "✓".green(), dest_bucket, dest_region),
                Err(e) if e.code() == Some("BucketAlreadyOwnedByYou") => {
                    println!("{} 目标桶 {} 已存在", "✓".green(), dest_bucket)
                }
                Err(e) => bail!("创建目标桶 {} 失败: {}", dest_bucket, e),
            }
        }
    }

    // 3. versioning on destination
    ensure_versioning(&dest_client, &dest_bucket).await?;

    // 4. replication IAM role
    let role_arn = ensure_replication_role(shared, source_bucket, &dest_bucket).await?;

    // 5. replication rule (scoped to the tool prefix, delete markers NOT
    //    replicated — retention deletes stay per-bucket by design)
    let rule = ReplicationRule::builder()
        .id("yo-s3-burn")
        .priority(1)
        .filter(ReplicationRuleFilter::builder().prefix(key_prefix).build())
        .status(ReplicationRuleStatus::Enabled)
        .delete_marker_replication(
            DeleteMarkerReplication::builder()
                .status(DeleteMarkerReplicationStatus::Disabled)
                .build(),
        )
        .destination(
            Destination::builder()
                .bucket(format!("arn:aws:s3:::{}", dest_bucket))
                .build()
                .map_err(|e| anyhow!("构造 Destination 失败: {}", e))?,
        )
        .build()
        .map_err(|e| anyhow!("构造 ReplicationRule 失败: {}", e))?;
    let conf = ReplicationConfiguration::builder()
        .role(&role_arn)
        .rules(rule)
        .build()
        .map_err(|e| anyhow!("构造 ReplicationConfiguration 失败: {}", e))?;

    // IAM roles propagate eventually — retry the put for up to ~1 minute.
    let mut last_err = None;
    for attempt in 0..6 {
        match source_client
            .put_bucket_replication()
            .bucket(source_bucket)
            .replication_configuration(conf.clone())
            .send()
            .await
        {
            Ok(_) => {
                println!(
                    "{} 复制规则已写入: {} → {}(前缀 {})",
                    "✓".green(),
                    source_bucket,
                    dest_bucket,
                    key_prefix
                );
                return Ok(dest_bucket);
            }
            Err(e) => {
                last_err = Some(e);
                if attempt < 5 {
                    println!("{} 等待 IAM 角色生效后重试({}/5)...", "ℹ".blue(), attempt + 1);
                    tokio::time::sleep(Duration::from_secs(10)).await;
                }
            }
        }
    }
    bail!("写入复制规则失败: {}", last_err.unwrap())
}

fn dest_bucket_name(source_bucket: &str, dest_region: &str) -> String {
    let suffix = format!("-crr-{}", dest_region);
    let max_src = 63usize.saturating_sub(suffix.len());
    let src = &source_bucket[..source_bucket.len().min(max_src)];
    format!("{}{}", src.trim_end_matches('-'), suffix)
}

async fn ensure_replication_role(
    shared: &SdkConfig,
    source_bucket: &str,
    dest_bucket: &str,
) -> Result<String> {
    let iam = aws_sdk_iam::Client::new(shared);
    let role_name = {
        let name = format!("yo-s3-crr-{}", source_bucket);
        name[..name.len().min(64)].to_string()
    };
    let trust = r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Principal":{"Service":"s3.amazonaws.com"},"Action":"sts:AssumeRole"}]}"#;
    let policy = format!(
        r#"{{"Version":"2012-10-17","Statement":[
  {{"Effect":"Allow","Action":["s3:GetReplicationConfiguration","s3:ListBucket"],"Resource":"arn:aws:s3:::{src}"}},
  {{"Effect":"Allow","Action":["s3:GetObjectVersionForReplication","s3:GetObjectVersionAcl","s3:GetObjectVersionTagging"],"Resource":"arn:aws:s3:::{src}/*"}},
  {{"Effect":"Allow","Action":["s3:ReplicateObject","s3:ReplicateDelete","s3:ReplicateTags"],"Resource":"arn:aws:s3:::{dst}/*"}}
]}}"#,
        src = source_bucket,
        dst = dest_bucket
    );

    let role_arn = match iam
        .create_role()
        .role_name(&role_name)
        .assume_role_policy_document(trust)
        .send()
        .await
    {
        Ok(out) => {
            let arn = out
                .role()
                .map(|r| r.arn().to_string())
                .ok_or_else(|| anyhow!("CreateRole 未返回角色"))?;
            println!("{} 已创建复制角色 {}", "✓".green(), role_name);
            arn
        }
        Err(e) if e.code() == Some("EntityAlreadyExists") => {
            let out = iam
                .get_role()
                .role_name(&role_name)
                .send()
                .await
                .with_context(|| format!("读取已有角色 {} 失败", role_name))?;
            println!("{} 复制角色 {} 已存在", "✓".green(), role_name);
            out.role()
                .map(|r| r.arn().to_string())
                .ok_or_else(|| anyhow!("GetRole 未返回角色"))?
        }
        Err(e) => bail!(
            "创建复制角色失败: {}\n  提示:当前身份需要 IAM 权限(iam:CreateRole / iam:PutRolePolicy);\
             可在有管理员权限的机器上跑一次 yo-s3 setup-crr",
            e
        ),
    };

    iam.put_role_policy()
        .role_name(&role_name)
        .policy_name("yo-s3-crr-policy")
        .policy_document(policy)
        .send()
        .await
        .context("写入复制角色策略失败")?;
    Ok(role_arn)
}

/// Sample replication status of recently completed objects via HeadObject.
/// Returns (pending, failed) among the sampled keys.
pub async fn sample_backlog(
    client: &aws_sdk_s3::Client,
    bucket: &str,
    keys: &[String],
) -> (u64, u64) {
    let mut pending = 0;
    let mut failed = 0;
    for key in keys {
        if let Ok(out) = client.head_object().bucket(bucket).key(key).send().await {
            match out.replication_status() {
                Some(ReplicationStatus::Pending) => pending += 1,
                Some(ReplicationStatus::Failed) => failed += 1,
                _ => {}
            }
        }
    }
    (pending, failed)
}
