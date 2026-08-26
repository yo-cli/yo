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
    // Bucket tags are readable only in the bucket's own region.
    let dest_client = retrying_region_client(shared, &actual_region);
    if !was_created_by_us(&dest_client, name).await {
        return None;
    }
    Some(OrphanDest {
        bucket: name.to_string(),
        region: actual_region,
    })
}

/// A region-pinned client that KEEPS the SDK's default retries, unlike
/// `client::build_s3_client`, which disables them so the burn loop can account
/// for every request exactly. Provisioning and discovery are one-shot slow-path
/// control-plane calls with no retry layer of their own: a retried CreateBucket
/// is strictly better than a replication target that failed to appear.
fn retrying_region_client(shared: &SdkConfig, region: &str) -> aws_sdk_s3::Client {
    let builder = aws_sdk_s3::config::Builder::from(shared)
        .region(aws_config::Region::new(region.to_string()));
    aws_sdk_s3::Client::from_conf(builder.build())
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
        source_created: was_created_by_us(source_client, source_bucket).await,
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

    // 3. The source bucket, but only when we created it — a bucket the user
    //    brought along is theirs, tagged or not, and must survive teardown.
    if plan.source_created {
        match super::sweep::sweep_versions_before(source_client, source_bucket, "", chrono::Utc::now()).await
        {
            Ok(stats) if stats.deleted > 0 => println!(
                "{} 清空 {}:删除 {} 个版本({})",
                "✓".green(),
                source_bucket,
                stats.deleted,
                super::fmt_bytes(stats.bytes)
            ),
            Ok(_) => {}
            Err(e) => eprintln!("{} 清空 {} 失败,跳过删桶: {:#}", "✗".red(), source_bucket, e),
        }
        match source_client.delete_bucket().bucket(source_bucket).send().await {
            Ok(_) => println!("{} 已删除源桶 {}(本工具创建)", "✓".green(), source_bucket),
            Err(e) => eprintln!("{} 删除源桶 {} 失败: {}", "✗".red(), source_bucket, e),
        }
    }

    // 4. The inline policy must go before the role it hangs on.
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

/// Create a bucket in `region` and stamp it as ours. Idempotent: a bucket we
/// already own is reported, not treated as an error.
pub async fn create_bucket(
    client: &aws_sdk_s3::Client,
    bucket: &str,
    region: &str,
) -> Result<()> {
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

/// The destination bucket for one region. Derived from the source name rather
/// than random, so it is the same on every run and stays inside the 63-char
/// bucket limit.
pub fn dest_bucket_name(source_bucket: &str, dest_region: &str) -> String {
    let suffix = format!("-crr-{}", dest_region);
    let max_src = 63usize.saturating_sub(suffix.len());
    let src = &source_bucket[..source_bucket.len().min(max_src)];
    format!("{}{}", src.trim_end_matches('-'), suffix)
}

/// Bytes reserved for the region name inside a destination suffix. Deliberately
/// wider than any region in service (`ap-northeast-1` is 14, `us-isof-south-1`
/// is 15): the errors are not symmetric. A budget that is too generous only
/// shortens the prefix, returning a few extra buckets that the round-trip check
/// then rejects — while one byte too tight drops a real orphan out of the
/// listing entirely, which is the exact leak `find_orphan_dests` exists to close.
pub(crate) const REGION_LEN_BUDGET: usize = 24;

/// The bucket-name prefix EVERY destination of this source starts with,
/// whichever region it landed in. `dest_bucket_name` truncates the source name
/// by the length of each region's suffix, so the longest suffix leaves the
/// shortest head — and only that head is a prefix of all the others.
pub fn dest_bucket_prefix(source_bucket: &str) -> String {
    let max_src = 63usize.saturating_sub(REGION_LEN_BUDGET + "-crr-".len());
    source_bucket[..source_bucket.len().min(max_src)]
        .trim_end_matches('-')
        .to_string()
}

/// Read a destination bucket back: which region does this name claim, and is it
/// really a destination of `source_bucket`?
///
/// Round-tripping through `dest_bucket_name` is what makes the answer safe to
/// act on — a bucket that merely happens to contain "-crr-" does not reproduce,
/// and teardown must never delete a bucket on a name coincidence.
pub fn dest_region_of(source_bucket: &str, bucket: &str) -> Option<String> {
    let (_, region) = bucket.rsplit_once("-crr-")?;
    if !looks_like_region(region) || dest_bucket_name(source_bucket, region) != bucket {
        return None;
    }
    Some(region.to_string())
}

/// `us-east-1`, `ap-northeast-1`, … — two letters, one or more words, a digit.
/// Cheap shape check so a user bucket that merely ends in "-crr-something" does
/// not read as one of ours before the tag is even consulted.
fn looks_like_region(s: &str) -> bool {
    let parts: Vec<&str> = s.split('-').collect();
    parts.len() >= 3
        && parts[0].len() == 2
        && parts[0].bytes().all(|b| b.is_ascii_lowercase())
        && parts[1..parts.len() - 1]
            .iter()
            .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_lowercase()))
        && parts[parts.len() - 1].bytes().all(|b| b.is_ascii_digit())
        && !parts[parts.len() - 1].is_empty()
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

    #[test]
    fn dest_bucket_name_fits_the_s3_limit() {
        let name = dest_bucket_name(&"b".repeat(80), "ap-northeast-1");
        assert!(name.len() <= 63, "{} ({})", name, name.len());
        assert!(name.ends_with("-crr-ap-northeast-1"));
    }

    /// Orphan discovery reads the region back OUT of the bucket name, so the
    /// round trip has to hold for every region the pool can produce — including
    /// the long ones, where the source name gets truncated.
    #[test]
    fn dest_name_round_trips_to_its_region() {
        for source in ["burn", "my-burn-bucket", &"b".repeat(80)] {
            for region in ["us-east-1", "ap-northeast-1", "ca-central-1", "sa-east-1"] {
                let name = dest_bucket_name(source, region);
                assert_eq!(
                    dest_region_of(source, &name).as_deref(),
                    Some(region),
                    "{} / {}",
                    source,
                    region
                );
            }
        }
    }

    /// Teardown deletes what this identifies, so a bucket that merely looks the
    /// part must not round-trip. The name is derived from the source bucket,
    /// which makes collisions with a user's own bucket entirely possible.
    #[test]
    fn a_name_that_is_not_ours_does_not_round_trip() {
        // Belongs to a different source bucket
        assert!(dest_region_of("burn", "other-crr-us-east-1").is_none());
        // "-crr-" followed by something that is not a region
        assert!(dest_region_of("burn", "burn-crr-archive").is_none());
        assert!(dest_region_of("burn", "burn-crr-us-east").is_none());
        assert!(dest_region_of("burn", "burn-crr-backup-1").is_none());
        // No marker at all
        assert!(dest_region_of("burn", "burn-us-east-1").is_none());
        // A source whose own name contains the marker still resolves correctly
        assert_eq!(
            dest_region_of("my-crr-bucket", "my-crr-bucket-crr-eu-west-2").as_deref(),
            Some("eu-west-2")
        );
    }

    /// The ListBuckets filter has to match every destination of this source,
    /// whichever region it landed in. A prefix one byte too long drops that
    /// bucket out of the listing entirely and the orphan is never found again —
    /// so this walks the whole pool plus region names longer than any in
    /// service, at the source lengths where truncation actually bites.
    #[test]
    fn dest_prefix_matches_every_region_variant() {
        let long_names = [
            "us-isof-south-1",              // 15, in service today
            &"a".repeat(REGION_LEN_BUDGET), // the widest the budget allows
        ];
        let regions: Vec<&str> = crate::s3::modes::crr::DEST_POOL
            .iter()
            .copied()
            .chain(long_names.iter().copied())
            .collect();
        for source in ["burn", "my-burn-bucket", &"b".repeat(45), &"b".repeat(80), &"c".repeat(50)] {
            let prefix = dest_bucket_prefix(source);
            for region in &regions {
                let name = dest_bucket_name(source, region);
                assert!(
                    name.starts_with(&prefix),
                    "{} 不以 {} 开头(source={}, region={})",
                    name,
                    prefix,
                    source,
                    region
                );
            }
        }
    }

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

    /// The budget is only honoured if every region the tool can pick fits in it.
    #[test]
    fn every_pool_region_fits_the_budget() {
        for region in crate::s3::modes::crr::DEST_POOL {
            assert!(
                region.len() <= REGION_LEN_BUDGET,
                "{} 超出 REGION_LEN_BUDGET({})",
                region,
                REGION_LEN_BUDGET
            );
        }
    }
}
