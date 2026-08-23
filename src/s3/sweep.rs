// Retention sweeper: physically delete tool-created objects older than the
// cutoff. With versioning on (required by CRR) a plain DeleteObject only adds
// a delete marker while old versions keep billing — so deletion is always by
// version id, and delete markers themselves are swept too.

use anyhow::Result;
use aws_sdk_s3::types::{Delete, ObjectIdentifier};
use chrono::{DateTime, Utc};

pub struct SweepStats {
    pub deleted: u64,
    pub bytes: u64,
}

/// Delete every version + delete marker under `prefix` last-modified before
/// `cutoff`. Batched 1000 per DeleteObjects call. Only ever touches the tool's
/// own prefix — never anything else in the bucket.
pub async fn sweep_versions_before(
    client: &aws_sdk_s3::Client,
    bucket: &str,
    prefix: &str,
    cutoff: DateTime<Utc>,
) -> Result<SweepStats> {
    let cutoff_secs = cutoff.timestamp();
    let mut stats = SweepStats { deleted: 0, bytes: 0 };
    let mut key_marker: Option<String> = None;
    let mut vid_marker: Option<String> = None;
    loop {
        let resp = client
            .list_object_versions()
            .bucket(bucket)
            .prefix(prefix)
            .set_key_marker(key_marker.clone())
            .set_version_id_marker(vid_marker.clone())
            .send()
            .await?;

        let mut batch: Vec<ObjectIdentifier> = Vec::new();
        let mut batch_bytes: u64 = 0;
        for v in resp.versions() {
            let old = v.last_modified().map(|t| t.secs() < cutoff_secs).unwrap_or(false);
            if !old {
                continue;
            }
            if let Some(key) = v.key() {
                let mut b = ObjectIdentifier::builder().key(key);
                if let Some(vid) = v.version_id() {
                    b = b.version_id(vid);
                }
                if let Ok(id) = b.build() {
                    batch_bytes += v.size().unwrap_or(0).max(0) as u64;
                    batch.push(id);
                }
            }
        }
        for m in resp.delete_markers() {
            let old = m.last_modified().map(|t| t.secs() < cutoff_secs).unwrap_or(false);
            if !old {
                continue;
            }
            if let Some(key) = m.key() {
                let mut b = ObjectIdentifier::builder().key(key);
                if let Some(vid) = m.version_id() {
                    b = b.version_id(vid);
                }
                if let Ok(id) = b.build() {
                    batch.push(id);
                }
            }
        }

        for chunk in batch.chunks(1000) {
            let delete = Delete::builder()
                .set_objects(Some(chunk.to_vec()))
                .quiet(true)
                .build()?;
            client
                .delete_objects()
                .bucket(bucket)
                .delete(delete)
                .send()
                .await?;
            stats.deleted += chunk.len() as u64;
        }
        stats.bytes += batch_bytes;

        if resp.is_truncated() == Some(true) {
            key_marker = resp.next_key_marker().map(|s| s.to_string());
            vid_marker = resp.next_version_id_marker().map(|s| s.to_string());
        } else {
            return Ok(stats);
        }
    }
}

/// Count what is still stored under the prefix (for the exit reminder).
pub async fn count_remaining(
    client: &aws_sdk_s3::Client,
    bucket: &str,
    prefix: &str,
) -> Result<SweepStats> {
    let mut stats = SweepStats { deleted: 0, bytes: 0 };
    let mut key_marker: Option<String> = None;
    let mut vid_marker: Option<String> = None;
    loop {
        let resp = client
            .list_object_versions()
            .bucket(bucket)
            .prefix(prefix)
            .set_key_marker(key_marker.clone())
            .set_version_id_marker(vid_marker.clone())
            .send()
            .await?;
        for v in resp.versions() {
            stats.deleted += 1;
            stats.bytes += v.size().unwrap_or(0).max(0) as u64;
        }
        if resp.is_truncated() == Some(true) {
            key_marker = resp.next_key_marker().map(|s| s.to_string());
            vid_marker = resp.next_version_id_marker().map(|s| s.to_string());
        } else {
            return Ok(stats);
        }
    }
}
