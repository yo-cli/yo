// Build the fan-out: versioning everywhere, one destination bucket per region,
// one IAM role covering them all, and one replication rule per destination.
// Every step is idempotent — a bucket or role already in place is reported and
// reused, never treated as an error.

use anyhow::{anyhow, bail, Context, Result};
use aws_config::SdkConfig;
use aws_sdk_s3::error::ProvideErrorMetadata;
use aws_sdk_s3::types::{
    BucketLocationConstraint, BucketVersioningStatus, CreateBucketConfiguration,
    DeleteMarkerReplication, DeleteMarkerReplicationStatus, Destination, ReplicationConfiguration,
    ReplicationRule, ReplicationRuleFilter, ReplicationRuleStatus, VersioningConfiguration,
};
use colored::Colorize;
use std::time::Duration;

use super::detect::versioning_enabled;
use super::identity::{dest_bucket_name, mark_created};
use super::role::ensure_replication_role;
use super::retrying_region_client;

/// The gate a fan-out must pass before anything is created — and, just as
/// importantly, before a `--dry-run` bills it. Same-region replication produces
/// no cross-region traffic and a repeated region is billed once, so either would
/// make the cost model promise a burn AWS will never charge for.
///
/// `source_region` is optional because a rehearsal may not have been able to
/// determine it; the duplicate check still applies.
pub fn validate_dest_regions(source_region: Option<&str>, dest_regions: &[String]) -> Result<()> {
    if dest_regions.is_empty() {
        bail!("至少要指定一个复制目标区域");
    }
    let mut seen: Vec<&str> = Vec::new();
    for region in dest_regions {
        if Some(region.as_str()) == source_region {
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
    Ok(())
}

/// One-shot idempotent CRR setup across K destination regions. Returns the
/// destination bucket names.
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
    validate_dest_regions(Some(source_region), dest_regions)?;

    // 1. versioning on source
    ensure_versioning(source_client, source_bucket).await?;

    // 2. one destination bucket per region (deterministic name, ≤63 chars)
    let mut dest_buckets: Vec<String> = Vec::with_capacity(dest_regions.len());
    for dest_region in dest_regions {
        let dest_bucket = dest_bucket_name(source_bucket, dest_region);
        let dest_client = retrying_region_client(shared, dest_region);
        match dest_client.head_bucket().bucket(&dest_bucket).send().await {
            Ok(_) => println!("{} 目标桶 {} 已存在({})", "✓".green(), dest_bucket, dest_region),
            Err(_) => {
                create_bucket(&dest_client, &dest_bucket, dest_region)
                    .await
                    .with_context(|| format!("创建目标桶 {} 失败", dest_bucket))?;
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

/// Create a bucket in `region` and stamp it as ours. Idempotent: a bucket we
/// already own is reported, not treated as an error.
pub async fn create_bucket(client: &aws_sdk_s3::Client, bucket: &str, region: &str) -> Result<()> {
    let mut create = client.create_bucket().bucket(bucket);
    // us-east-1 must NOT carry a LocationConstraint — AWS rejects it there.
    if region != "us-east-1" {
        create = create.create_bucket_configuration(
            CreateBucketConfiguration::builder()
                .location_constraint(BucketLocationConstraint::from(region))
                .build(),
        );
    }
    match create.send().await {
        Ok(_) => {
            mark_created(client, bucket).await;
            println!("{} 已创建桶 {}({})", "✓".green(), bucket.bold(), region);
            Ok(())
        }
        Err(e) if e.code() == Some("BucketAlreadyOwnedByYou") => {
            println!("{} 桶 {} 已存在", "✓".green(), bucket);
            Ok(())
        }
        Err(e) => bail!("{}", e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// This gate has to be reachable WITHOUT running `setup`, because a
    /// `--dry-run` bills the fan-out without ever creating it: if the check
    /// lived only inside `setup`, a rehearsal would happily price a fan-out the
    /// real run refuses to build.
    #[test]
    fn a_fanout_the_real_run_would_refuse_never_passes_the_gate() {
        let ok = vec!["us-west-2".to_string(), "eu-west-1".to_string()];
        assert!(validate_dest_regions(Some("us-east-1"), &ok).is_ok());

        // Same region as the source: no cross-region traffic, no burn.
        let same = vec!["us-west-2".to_string(), "us-east-1".to_string()];
        assert!(validate_dest_regions(Some("us-east-1"), &same).is_err());

        // A repeat is billed once, so counting it twice over-promises the burn.
        let dup = vec!["us-west-2".to_string(), "us-west-2".to_string()];
        assert!(validate_dest_regions(Some("us-east-1"), &dup).is_err());

        // Nothing to replicate to at all.
        assert!(validate_dest_regions(Some("us-east-1"), &[]).is_err());

        // A rehearsal may not know the source region; the duplicate check still
        // applies, and the same-region one simply cannot fire.
        assert!(validate_dest_regions(None, &dup).is_err());
        assert!(validate_dest_regions(None, &same).is_ok());
    }
}
