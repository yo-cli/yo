// Read what is already on the bucket. Nothing here changes the account — the
// answers decide whether the burn engine is armed, and at what K.

use anyhow::{anyhow, Context, Result};
use aws_sdk_s3::error::ProvideErrorMetadata;
use aws_sdk_s3::types::{BucketVersioningStatus, ReplicationRule, ReplicationRuleStatus, ReplicationStatus};

/// Every distinct destination bucket the source replicates to.
///
/// `covering` filters to rules whose prefix filter actually matches what we are
/// about to write. That distinction is the difference between a run that burns
/// money and one that writes for hours for free: a rule scoped to some other
/// prefix contributes NO replication traffic, but counting it would arm the
/// cost model at K× and promise a burn that never happens.
pub async fn detect_covering(
    client: &aws_sdk_s3::Client,
    bucket: &str,
    covering: Option<&str>,
) -> Result<Vec<String>> {
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
                if let Some(want) = covering {
                    if !rule_covers(rule, want) {
                        continue;
                    }
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

/// Every destination, regardless of which prefix its rule targets.
pub async fn detect(client: &aws_sdk_s3::Client, bucket: &str) -> Result<Vec<String>> {
    detect_covering(client, bucket, None).await
}

/// Does this rule replicate objects written under `key_prefix`?
///
/// A rule with no filter (or an empty prefix) covers the whole bucket. A rule
/// filtered to `P` covers our writes only when our prefix starts with `P`.
fn rule_covers(rule: &ReplicationRule, key_prefix: &str) -> bool {
    let filter_prefix = rule
        .filter()
        .and_then(|f| f.prefix().or_else(|| f.and().and_then(|a| a.prefix())))
        // V1-style rules carry the prefix on the rule itself.
        .or_else(|| {
            #[allow(deprecated)]
            rule.prefix()
        })
        .unwrap_or("");
    key_prefix.starts_with(filter_prefix)
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
    use aws_sdk_s3::types::{Destination, ReplicationRuleFilter};

    fn rule_with_prefix(p: Option<&str>) -> ReplicationRule {
        let mut b = ReplicationRule::builder()
            .id("r")
            .status(ReplicationRuleStatus::Enabled)
            .destination(
                Destination::builder()
                    .bucket("arn:aws:s3:::d")
                    .build()
                    .unwrap(),
            );
        if let Some(p) = p {
            b = b.filter(ReplicationRuleFilter::builder().prefix(p).build());
        }
        b.build().unwrap()
    }

    /// The quietest way this tool can fail: rules exist so nothing looks
    /// broken, but none match the prefix being written — no replication, no
    /// traffic fee, a run that writes for hours without moving the budget.
    #[test]
    fn rule_coverage_decides_whether_a_write_replicates() {
        // rule scoped to the prefix we write under → covered
        assert!(rule_covers(&rule_with_prefix(Some("backup/")), "backup/"));
        assert!(rule_covers(&rule_with_prefix(Some("backup/")), "backup/db/"));
        // rule scoped elsewhere → NOT covered, must not be counted as a destination
        assert!(!rule_covers(&rule_with_prefix(Some("yo-s3-bench/")), "backup/"));
        // a narrower rule does not cover a broader write prefix
        assert!(!rule_covers(&rule_with_prefix(Some("backup/db/")), "backup/"));
        // no filter = whole bucket
        assert!(rule_covers(&rule_with_prefix(None), "anything/"));
    }
}
