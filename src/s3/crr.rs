// Cross-Region Replication: detection, one-shot setup, backlog sampling.
// CRR is the burn engine — inter-region transfer (~$0.02/GB) is the only cost
// that accrues immediately and linearly with bytes written.

use anyhow::{anyhow, bail, Context, Result};
use aws_config::SdkConfig;
use aws_sdk_s3::error::ProvideErrorMetadata;
use aws_sdk_s3::types::{
    BucketLocationConstraint, BucketVersioningStatus, CreateBucketConfiguration,
    DeleteMarkerReplication, DeleteMarkerReplicationStatus, Destination, ReplicationConfiguration,
    ReplicationRule, ReplicationRuleFilter, ReplicationRuleStatus, ReplicationStatus, Tag, Tagging,
    VersioningConfiguration,
};
use colored::Colorize;
use std::time::Duration;

/// Stamped on every destination bucket this tool creates itself. Teardown
/// deletes buckets; the stamp is how it tells "I made this" from "this name
/// already existed and I adopted it" — the destination name is derived from
/// the source bucket, so a collision with a pre-existing bucket is entirely
/// possible and must not read as ours.
const CREATED_TAG: &str = "yo-s3-created";

/// Inline policy attached to the replication role.
const ROLE_POLICY_NAME: &str = "yo-s3-crr-policy";

/// The replication role for a source bucket. One role per source bucket, so
/// removing it never disturbs another bucket's replication.
pub fn replication_role_name(source_bucket: &str) -> String {
    let name = format!("yo-s3-crr-{}", source_bucket);
    name[..name.len().min(64)].to_string()
}

/// Every distinct destination bucket the source replicates to. Rules with the
/// same scope but different destinations ALL apply, so each one bills its own
/// inter-region transfer — the list length is the K in the cost model.
pub async fn detect(client: &aws_sdk_s3::Client, bucket: &str) -> Result<Vec<String>> {
    match client.get_bucket_replication().bucket(bucket).send().await {
        Ok(out) => {
            let Some(conf) = out.replication_configuration() else {
                return Ok(Vec::new());
            };
            let mut buckets: Vec<String> = Vec::new();
            for rule in conf.rules() {
                if rule.status() != &ReplicationRuleStatus::Enabled {
                    continue;
                }
                if let Some(dest) = rule.destination() {
                    let name = dest.bucket().trim_start_matches("arn:aws:s3:::").to_string();
                    // Two rules may target the same bucket; it is billed once.
                    if !buckets.contains(&name) {
                        buckets.push(name);
                    }
                }
            }
            Ok(buckets)
        }
        Err(e) => {
            if e.code() == Some("ReplicationConfigurationNotFoundError") {
                Ok(Vec::new())
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

/// One-shot idempotent CRR setup across K destination regions: versioning on
/// every bucket, one destination bucket per region, one replication IAM role
/// covering them all, and one rule per destination scoped to the tool prefix.
/// Returns the destination bucket names.
///
/// K rules share the same prefix filter on purpose: AWS applies every rule
/// whose scope matches when the destinations differ, so each destination bills
/// its own inter-region transfer — that is the ×K amplification.
pub async fn setup(
    shared: &SdkConfig,
    source_client: &aws_sdk_s3::Client,
    source_bucket: &str,
    source_region: &str,
    dest_regions: &[String],
    key_prefix: &str,
) -> Result<Vec<String>> {
    if dest_regions.is_empty() {
        bail!("至少要指定一个复制目标区域");
    }
    let mut seen: Vec<&str> = Vec::new();
    for region in dest_regions {
        if region == source_region {
            bail!(
                "目标区域 {} 与源区域相同 —— 同区复制不产生跨区流量费,烧钱引擎无效",
                region
            );
        }
        if seen.contains(&region.as_str()) {
            bail!("目标区域 {} 重复 —— 同一区域只会计一次流量费", region);
        }
        seen.push(region);
    }

    // 1. versioning on source
    ensure_versioning(source_client, source_bucket).await?;

    // 2. one destination bucket per region (deterministic name, ≤63 chars)
    let mut dest_buckets: Vec<String> = Vec::with_capacity(dest_regions.len());
    for dest_region in dest_regions {
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
                            .location_constraint(BucketLocationConstraint::from(
                                dest_region.as_str(),
                            ))
                            .build(),
                    );
                }
                match create.send().await {
                    Ok(_) => {
                        mark_created(&dest_client, &dest_bucket).await;
                        println!("{} 已创建目标桶 {}({})", "✓".green(), dest_bucket, dest_region)
                    }
                    Err(e) if e.code() == Some("BucketAlreadyOwnedByYou") => {
                        println!("{} 目标桶 {} 已存在", "✓".green(), dest_bucket)
                    }
                    Err(e) => bail!("创建目标桶 {} 失败: {}", dest_bucket, e),
                }
            }
        }
        // 3. versioning on destination
        ensure_versioning(&dest_client, &dest_bucket).await?;
        dest_buckets.push(dest_bucket);
    }

    // 4. replication IAM role covering every destination
    let role_arn = ensure_replication_role(shared, source_bucket, &dest_buckets).await?;

    // 5. one rule per destination (scoped to the tool prefix, delete markers
    //    NOT replicated — retention deletes stay per-bucket by design)
    let mut rules = Vec::with_capacity(dest_buckets.len());
    for (i, dest_bucket) in dest_buckets.iter().enumerate() {
        rules.push(
            ReplicationRule::builder()
                .id(format!("yo-s3-burn-{}", i + 1))
                .priority(i as i32 + 1)
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
                .map_err(|e| anyhow!("构造 ReplicationRule 失败: {}", e))?,
        );
    }
    let conf = ReplicationConfiguration::builder()
        .role(&role_arn)
        .set_rules(Some(rules))
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
                    "{} {} 条复制规则已写入: {} → {}(前缀 {})",
                    "✓".green(),
                    dest_buckets.len(),
                    source_bucket,
                    dest_buckets.join(" + "),
                    key_prefix
                );
                return Ok(dest_buckets);
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

/// One destination bucket a teardown would remove.
pub struct DestTeardown {
    pub bucket: String,
    /// False = the name already existed and setup adopted it. Deleting it is
    /// still what teardown does, but the user gets told which ones these are.
    pub created_by_us: bool,
}

/// Everything `setup` left behind for this source bucket. Read-only — this is
/// what the confirmation screen shows before anything is destroyed.
pub struct TeardownPlan {
    pub dests: Vec<DestTeardown>,
    /// Present only if the replication role actually exists.
    pub role_name: Option<String>,
    pub has_replication_config: bool,
}

impl TeardownPlan {
    pub fn is_empty(&self) -> bool {
        self.dests.is_empty() && self.role_name.is_none() && !self.has_replication_config
    }
}

/// Survey what a teardown would remove. `dest_clients` must be region-correct
/// clients for each destination, since bucket tags are read in-region.
pub async fn teardown_plan(
    shared: &SdkConfig,
    source_client: &aws_sdk_s3::Client,
    source_bucket: &str,
    dest_clients: &[(aws_sdk_s3::Client, String)],
) -> Result<TeardownPlan> {
    let mut dests = Vec::with_capacity(dest_clients.len());
    for (client, bucket) in dest_clients {
        dests.push(DestTeardown {
            created_by_us: was_created_by_us(client, bucket).await,
            bucket: bucket.clone(),
        });
    }

    let role_name = replication_role_name(source_bucket);
    let iam = aws_sdk_iam::Client::new(shared);
    let role_exists = iam.get_role().role_name(&role_name).send().await.is_ok();

    // Asked directly rather than inferred from the destination count: `detect`
    // skips disabled rules, so a config made only of them would otherwise be
    // reported as "nothing to remove" and left dangling.
    let has_replication_config = source_client
        .get_bucket_replication()
        .bucket(source_bucket)
        .send()
        .await
        .is_ok();

    Ok(TeardownPlan {
        has_replication_config,
        dests,
        role_name: role_exists.then_some(role_name),
    })
}

/// Remove the replication infrastructure, in dependency order: stop
/// replicating first, then empty and delete the destinations, then drop the
/// role. Each step reports its own failure and the rest still runs — a
/// half-removed setup is worse than a fully-attempted one.
pub async fn teardown(
    shared: &SdkConfig,
    source_client: &aws_sdk_s3::Client,
    source_bucket: &str,
    dest_clients: &[(aws_sdk_s3::Client, String)],
    plan: &TeardownPlan,
) -> Result<()> {
    // 1. Stop replication before deleting its targets, or objects written in
    //    the meantime keep flowing into buckets that are about to vanish.
    if plan.has_replication_config {
        match source_client
            .delete_bucket_replication()
            .bucket(source_bucket)
            .send()
            .await
        {
            Ok(_) => println!("{} 已删除 {} 的复制规则", "✓".green(), source_bucket),
            Err(e) => eprintln!("{} 删除复制规则失败: {}", "✗".red(), e),
        }
    }

    // 2. A bucket must be COMPLETELY empty to be deleted — including anything
    //    outside the tool's key prefix, which is why this sweep is unfiltered.
    for (client, bucket) in dest_clients {
        match super::sweep::sweep_versions_before(client, bucket, "", chrono::Utc::now()).await {
            Ok(stats) if stats.deleted > 0 => println!(
                "{} 清空 {}:删除 {} 个版本({})",
                "✓".green(),
                bucket,
                stats.deleted,
                super::fmt_bytes(stats.bytes)
            ),
            Ok(_) => {}
            Err(e) => {
                eprintln!("{} 清空 {} 失败,跳过删桶: {:#}", "✗".red(), bucket, e);
                continue;
            }
        }
        match client.delete_bucket().bucket(bucket).send().await {
            Ok(_) => println!("{} 已删除目标桶 {}", "✓".green(), bucket),
            Err(e) => eprintln!(
                "{} 删除目标桶 {} 失败: {}(桶非空或有其他配置时会失败,可手动删)",
                "✗".red(),
                bucket,
                e
            ),
        }
    }

    // 3. The inline policy must go before the role it hangs on.
    if let Some(role_name) = &plan.role_name {
        let iam = aws_sdk_iam::Client::new(shared);
        if let Err(e) = iam
            .delete_role_policy()
            .role_name(role_name)
            .policy_name(ROLE_POLICY_NAME)
            .send()
            .await
        {
            tracing::debug!("删除角色策略失败(可能本就不存在): {}", e);
        }
        match iam.delete_role().role_name(role_name).send().await {
            Ok(_) => println!("{} 已删除复制角色 {}", "✓".green(), role_name),
            Err(e) => eprintln!(
                "{} 删除复制角色 {} 失败: {}(需要 iam:DeleteRole 权限)",
                "✗".red(),
                role_name,
                e
            ),
        }
    }
    Ok(())
}

/// Best effort: a bucket we created but failed to stamp is merely treated as
/// adopted by teardown, which is the safe direction to fail in.
async fn mark_created(client: &aws_sdk_s3::Client, bucket: &str) {
    let tagging = Tag::builder()
        .key(CREATED_TAG)
        .value("true")
        .build()
        .ok()
        .and_then(|tag| Tagging::builder().tag_set(tag).build().ok());
    let Some(tagging) = tagging else { return };
    if let Err(e) = client
        .put_bucket_tagging()
        .bucket(bucket)
        .tagging(tagging)
        .send()
        .await
    {
        tracing::debug!("标记目标桶 {} 失败: {}", bucket, e);
    }
}

async fn was_created_by_us(client: &aws_sdk_s3::Client, bucket: &str) -> bool {
    match client.get_bucket_tagging().bucket(bucket).send().await {
        Ok(out) => out.tag_set().iter().any(|t| t.key() == CREATED_TAG),
        Err(_) => false, // no tag set at all, or unreadable → treat as adopted
    }
}

fn dest_bucket_name(source_bucket: &str, dest_region: &str) -> String {
    let suffix = format!("-crr-{}", dest_region);
    let max_src = 63usize.saturating_sub(suffix.len());
    let src = &source_bucket[..source_bucket.len().min(max_src)];
    format!("{}{}", src.trim_end_matches('-'), suffix)
}

/// One role for the whole fan-out: the write statement must list EVERY
/// destination, or the rules pointing at the missing ones silently fail to
/// replicate — and a destination that never receives bytes never bills.
async fn ensure_replication_role(
    shared: &SdkConfig,
    source_bucket: &str,
    dest_buckets: &[String],
) -> Result<String> {
    let iam = aws_sdk_iam::Client::new(shared);
    let role_name = replication_role_name(source_bucket);
    let trust = r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Principal":{"Service":"s3.amazonaws.com"},"Action":"sts:AssumeRole"}]}"#;
    let dst_resources = dest_buckets
        .iter()
        .map(|b| format!("\"arn:aws:s3:::{}/*\"", b))
        .collect::<Vec<_>>()
        .join(",");
    let policy = format!(
        r#"{{"Version":"2012-10-17","Statement":[
  {{"Effect":"Allow","Action":["s3:GetReplicationConfiguration","s3:ListBucket"],"Resource":"arn:aws:s3:::{src}"}},
  {{"Effect":"Allow","Action":["s3:GetObjectVersionForReplication","s3:GetObjectVersionAcl","s3:GetObjectVersionTagging"],"Resource":"arn:aws:s3:::{src}/*"}},
  {{"Effect":"Allow","Action":["s3:ReplicateObject","s3:ReplicateDelete","s3:ReplicateTags"],"Resource":[{dst}]}}
]}}"#,
        src = source_bucket,
        dst = dst_resources
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
             可用有管理员权限的身份先跑一次带 --dest-region 的命令把复制配好",
            e
        ),
    };

    iam.put_role_policy()
        .role_name(&role_name)
        .policy_name(ROLE_POLICY_NAME)
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Setup creates the role under this name and teardown deletes it under
    /// the same one. If the two ever computed it differently, teardown would
    /// silently strand an IAM role nothing else cleans up.
    #[test]
    fn role_name_is_stable_and_capped_at_the_iam_limit() {
        assert_eq!(replication_role_name("burn"), "yo-s3-crr-burn");
        let capped = replication_role_name(&"a".repeat(80));
        assert_eq!(capped.len(), 64);
        assert!(capped.starts_with("yo-s3-crr-a"));
    }

    /// Destination names must stay inside the 63-char S3 bucket limit even
    /// when the source name is long, or setup fails at create time.
    #[test]
    fn dest_bucket_name_fits_the_s3_limit() {
        let name = dest_bucket_name(&"b".repeat(80), "ap-northeast-1");
        assert!(name.len() <= 63, "{} ({})", name, name.len());
        assert!(name.ends_with("-crr-ap-northeast-1"));
    }
}
