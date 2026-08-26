// Take the fan-out back down, and find what an earlier teardown missed.
//
// Every step reports its own failure and the rest still runs: a half-removed
// setup is worse than a fully-attempted one. That tolerance is also why orphan
// discovery exists — the step that survives a failure here is a bucket nothing
// points at any more, quietly billing storage in some region forever.

use anyhow::{Context, Result};
use aws_config::SdkConfig;
use colored::Colorize;

use super::identity::{dest_bucket_prefix, dest_region_of, was_created_by_us};
use super::retrying_region_client;
use super::role::{delete_replication_role, replication_role_name, role_exists};
use crate::s3::{fmt_bytes, sweep};

/// A destination bucket found by asking the account rather than by reading the
/// source bucket's replication rules.
pub struct OrphanDest {
    pub bucket: String,
    pub region: String,
}

/// Destination buckets of this source that exist in the account but no
/// replication rule points at — what a half-finished teardown leaves behind
/// (deleting the rules succeeds, deleting a bucket fails, and the survivors
/// bill storage in that region forever).
///
/// Asked of the account rather than derived from a region list: destinations are
/// drawn at random, so their names can no longer be guessed, and a list would go
/// stale the moment the pool changes. `ListBuckets` filters by name prefix
/// server-side and already carries each bucket's region.
///
/// Only buckets carrying our created tag are returned. The name is derived from
/// the source bucket, so colliding with one the user already owned is entirely
/// possible, and nothing discovered this way should reach a deletion list on the
/// strength of its name alone.
pub async fn find_orphan_dests(
    shared: &SdkConfig,
    source_client: &aws_sdk_s3::Client,
    source_bucket: &str,
    known: &[String],
) -> Result<Vec<OrphanDest>> {
    let prefix = dest_bucket_prefix(source_bucket);
    let mut found = Vec::new();
    let mut token: Option<String> = None;
    loop {
        let out = source_client
            .list_buckets()
            .prefix(&prefix)
            .set_continuation_token(token)
            .send()
            .await
            .context("列举账号下的桶失败(需要 s3:ListAllMyBuckets 权限)")?;
        for listed in out.buckets() {
            if let Some(orphan) = orphan_dest_of(shared, source_bucket, known, listed).await {
                found.push(orphan);
            }
        }
        let Some(next) = out.continuation_token().filter(|t| !t.is_empty()) else {
            return Ok(found);
        };
        token = Some(next.to_string());
    }
}

/// Is this account-listed bucket a destination of `source_bucket` that no
/// replication rule points at any more? `None` for anything that fails a check:
/// no name, still named by a rule, a name that does not round-trip, or missing
/// our created tag.
async fn orphan_dest_of(
    shared: &SdkConfig,
    source_bucket: &str,
    known: &[String],
    listed: &aws_sdk_s3::types::Bucket,
) -> Option<OrphanDest> {
    let name = listed.name()?;
    if known.iter().any(|k| k == name) {
        return None;
    }
    let claimed_region = dest_region_of(source_bucket, name)?;
    // ListBuckets reports where the bucket really is; the name only claims it.
    let actual_region = listed.bucket_region().unwrap_or(&claimed_region).to_string();
    let dest_client = retrying_region_client(shared, &actual_region);
    if !was_created_by_us(&dest_client, name).await {
        return None;
    }
    Some(OrphanDest {
        bucket: name.to_string(),
        region: actual_region,
    })
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
    /// The source bucket carries our tag, i.e. the tool created it rather than
    /// the user bringing their own. Only then may teardown remove it.
    pub source_created: bool,
}

impl TeardownPlan {
    pub fn is_empty(&self) -> bool {
        self.dests.is_empty()
            && self.role_name.is_none()
            && !self.has_replication_config
            && !self.source_created
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
    let role_exists = role_exists(shared, &role_name).await;

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
        source_created: was_created_by_us(source_client, source_bucket).await,
    })
}

/// Remove the replication infrastructure, in dependency order: stop replicating
/// first, then empty and delete the destinations, then drop the role.
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

    // 2. Destinations.
    for (client, bucket) in dest_clients {
        if empty_bucket(client, bucket).await {
            delete_bucket(client, bucket).await;
        }
    }

    // 3. The source bucket, but only when we created it — a bucket the user
    //    brought along is theirs, tagged or not, and must survive teardown.
    if plan.source_created && empty_bucket(source_client, source_bucket).await {
        match source_client.delete_bucket().bucket(source_bucket).send().await {
            Ok(_) => println!("{} 已删除源桶 {}(本工具创建)", "✓".green(), source_bucket),
            Err(e) => eprintln!("{} 删除源桶 {} 失败: {}", "✗".red(), source_bucket, e),
        }
    }

    // 4. The role, inline policy first.
    if let Some(role_name) = &plan.role_name {
        delete_replication_role(shared, role_name).await;
    }
    Ok(())
}

/// A bucket must be COMPLETELY empty to be deleted — including anything outside
/// the tool's key prefix, which is why this sweep is unfiltered. Returns whether
/// deleting the bucket is now worth attempting.
async fn empty_bucket(client: &aws_sdk_s3::Client, bucket: &str) -> bool {
    match sweep::sweep_versions_before(client, bucket, "", chrono::Utc::now()).await {
        Ok(stats) => {
            if stats.deleted > 0 {
                println!(
                    "{} 清空 {}:删除 {} 个版本({})",
                    "✓".green(),
                    bucket,
                    stats.deleted,
                    fmt_bytes(stats.bytes)
                );
            }
            true
        }
        Err(e) => {
            eprintln!("{} 清空 {} 失败,跳过删桶: {:#}", "✗".red(), bucket, e);
            false
        }
    }
}

async fn delete_bucket(client: &aws_sdk_s3::Client, bucket: &str) {
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
