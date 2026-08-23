// Ledger of in-flight multipart uploads. Every failure path (retry exhaustion,
// panic, SIGINT, --max-duration) funnels into abort_all(): leftover multipart
// parts never expire on their own, are invisible in the console, and bill
// forever — so aborting them is non-negotiable.

use anyhow::Result;
use colored::Colorize;
use std::collections::HashMap;
use std::sync::Mutex;

#[derive(Default)]
pub struct UploadRegistry {
    inflight: Mutex<HashMap<String, String>>, // key → upload_id
}

impl UploadRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&self, key: &str, upload_id: &str) {
        self.inflight
            .lock()
            .unwrap()
            .insert(key.to_string(), upload_id.to_string());
    }

    pub fn deregister(&self, key: &str) {
        self.inflight.lock().unwrap().remove(key);
    }

    pub fn snapshot(&self) -> Vec<(String, String)> {
        self.inflight
            .lock()
            .unwrap()
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect()
    }

    /// Abort every registered in-flight upload (best effort, logs failures).
    pub async fn abort_all(&self, client: &aws_sdk_s3::Client, bucket: &str) -> usize {
        let entries = self.snapshot();
        let mut aborted = 0;
        for (key, upload_id) in entries {
            match client
                .abort_multipart_upload()
                .bucket(bucket)
                .key(&key)
                .upload_id(&upload_id)
                .send()
                .await
            {
                Ok(_) => {
                    aborted += 1;
                    self.deregister(&key);
                }
                Err(e) => eprintln!(
                    "{} abort 残片失败 {} ({}): {}(可稍后用 yo-s3 cleanup 清理)",
                    "⚠".yellow(),
                    key,
                    upload_id,
                    e
                ),
            }
        }
        aborted
    }
}

/// Abort ALL unfinished multipart uploads under a prefix — the orphan sweep
/// run on resume/cleanup, covering uploads left by a hard-killed process.
pub async fn abort_orphans(
    client: &aws_sdk_s3::Client,
    bucket: &str,
    prefix: &str,
) -> Result<usize> {
    let mut aborted = 0;
    let mut key_marker: Option<String> = None;
    let mut id_marker: Option<String> = None;
    loop {
        let resp = client
            .list_multipart_uploads()
            .bucket(bucket)
            .prefix(prefix)
            .set_key_marker(key_marker.clone())
            .set_upload_id_marker(id_marker.clone())
            .send()
            .await?;
        for upload in resp.uploads() {
            let (Some(key), Some(upload_id)) = (upload.key(), upload.upload_id()) else {
                continue;
            };
            match client
                .abort_multipart_upload()
                .bucket(bucket)
                .key(key)
                .upload_id(upload_id)
                .send()
                .await
            {
                Ok(_) => aborted += 1,
                Err(e) => eprintln!("{} abort 孤儿残片失败 {}: {}", "⚠".yellow(), key, e),
            }
        }
        if resp.is_truncated() == Some(true) {
            key_marker = resp.next_key_marker().map(|s| s.to_string());
            id_marker = resp.next_upload_id_marker().map(|s| s.to_string());
        } else {
            return Ok(aborted);
        }
    }
}
