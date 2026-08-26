// Which bucket is ours. Destination names are derived from the source bucket
// rather than random, so a collision with a bucket the user already owned is
// entirely possible — and teardown deletes what this module says is ours.
// Hence two independent kinds of evidence live here together: the name has to
// round-trip exactly, and the bucket has to carry our tag.

use aws_sdk_s3::types::{Tag, Tagging};

/// Stamped on every bucket this tool creates itself. Teardown deletes buckets;
/// the stamp is how it tells "I made this" from "this name already existed and
/// I adopted it".
const CREATED_TAG: &str = "yo-s3-created";

/// Bytes reserved for the region name inside a destination suffix. Deliberately
/// wider than any region in service (`ap-northeast-1` is 14, `us-isof-south-1`
/// is 15): the errors are not symmetric. A budget that is too generous only
/// shortens the prefix, returning a few extra buckets that the round-trip check
/// then rejects — while one byte too tight drops a real orphan out of the
/// listing entirely, which is the exact leak `find_orphan_dests` exists to close.
const REGION_LEN_BUDGET: usize = 24;

/// The destination bucket for one region. Derived from the source name rather
/// than random, so it is the same on every run and stays inside the 63-char
/// bucket limit.
pub fn dest_bucket_name(source_bucket: &str, dest_region: &str) -> String {
    let suffix = format!("-crr-{}", dest_region);
    let max_src = 63usize.saturating_sub(suffix.len());
    let src = &source_bucket[..source_bucket.len().min(max_src)];
    format!("{}{}", src.trim_end_matches('-'), suffix)
}

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

/// Best effort: a bucket we created but failed to stamp is merely treated as
/// adopted by teardown, which is the safe direction to fail in.
pub(super) async fn mark_created(client: &aws_sdk_s3::Client, bucket: &str) {
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

/// The tag check. `client` must be region-correct: bucket tags are readable
/// only in the bucket's own region.
pub(super) async fn was_created_by_us(client: &aws_sdk_s3::Client, bucket: &str) -> bool {
    match client.get_bucket_tagging().bucket(bucket).send().await {
        Ok(out) => out.tag_set().iter().any(|t| t.key() == CREATED_TAG),
        Err(_) => false, // no tag set at all, or unreadable → treat as adopted
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Destination names must stay inside the 63-char S3 bucket limit even when
    /// the source name is long, or setup fails at create time.
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
            "us-isof-south-1".to_string(),   // 15, in service today
            "a".repeat(REGION_LEN_BUDGET),   // the widest the budget allows
        ];
        let regions: Vec<String> = crate::s3::modes::crr::DEST_POOL
            .iter()
            .map(|r| r.to_string())
            .chain(long_names)
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
