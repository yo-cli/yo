// Single-object multipart upload: create → concurrent parts → complete.
// Retry policy lives HERE (SDK retries are disabled): explicit 503 SlowDown
// counting + exponential backoff with full jitter, so throttling is visible
// in the final report instead of being silently absorbed by the SDK.

use anyhow::{anyhow, bail, Result};
use aws_sdk_s3::error::{ProvideErrorMetadata, SdkError};
use aws_sdk_s3::types::{CompletedMultipartUpload, CompletedPart};
use aws_smithy_types::error::display::DisplayErrorContext;
use rand::Rng;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Semaphore;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use super::body::{chunks_len, replayable_stream};
use super::budget::BudgetMeter;
use super::limiter::RateLimiter;
use super::metrics::Metrics;
use super::pool::{object_header, BufferPool};
use super::registry::UploadRegistry;
use super::OBJECT_HEADER_LEN;

const MAX_ATTEMPTS: u32 = 8;
const BACKOFF_CAP: Duration = Duration::from_secs(60);

pub struct UploadCtx {
    pub client: aws_sdk_s3::Client,
    pub bucket: String,
    pub key_prefix: String,
    pub run_id: Uuid,
    pub part_size: u64,
    pub concurrent_parts: usize,
    pub dry_run: bool,
    pub pool: Arc<BufferPool>,
    pub limiter: Arc<RateLimiter>,
    pub metrics: Arc<Metrics>,
    pub budget: Arc<BudgetMeter>,
    pub registry: Arc<UploadRegistry>,
    /// Run-level cancellation (SIGINT / --max-duration).
    pub cancel: CancellationToken,
}

impl UploadCtx {
    pub fn object_key(&self, iteration: u64) -> String {
        format!("{}obj-{:06}", self.run_prefix(), iteration)
    }

    /// Prefix shared by every object of this run (for orphan sweeps).
    pub fn run_prefix(&self) -> String {
        let run_hex = self.run_id.simple().to_string();
        format!("{}{}/", self.key_prefix, &run_hex[..8])
    }
}

pub enum ObjectOutcome {
    Completed { key: String, bytes: u64 },
    Aborted { key: String, cancelled: bool },
}

enum ErrClass {
    SlowDown,
    Retryable,
    Fatal,
}

fn classify<E: ProvideErrorMetadata>(err: &SdkError<E>) -> ErrClass {
    match err {
        SdkError::ServiceError(ctx) => {
            let status = ctx.raw().status().as_u16();
            let code = err.code().unwrap_or("");
            if code == "SlowDown" || status == 503 {
                ErrClass::SlowDown
            } else if status >= 500 || code == "RequestTimeout" || code == "InternalError" {
                ErrClass::Retryable
            } else {
                // 4xx: auth/config problems — retrying cannot help
                ErrClass::Fatal
            }
        }
        SdkError::TimeoutError(_) | SdkError::DispatchFailure(_) | SdkError::ResponseError(_) => {
            ErrClass::Retryable
        }
        _ => ErrClass::Fatal,
    }
}

/// Full-jitter exponential backoff: random(0, base × 2^attempt), capped.
fn backoff_delay(attempt: u32, slow_down: bool) -> Duration {
    let base_ms: u64 = if slow_down { 1000 } else { 500 };
    let ceiling = base_ms
        .saturating_mul(1u64 << attempt.min(10))
        .min(BACKOFF_CAP.as_millis() as u64);
    Duration::from_millis(rand::rng().random_range(0..=ceiling))
}

/// Run one SDK call under our retry policy. `op` builds a fresh future per
/// attempt (bodies are replayable, so resending is safe). `cancel` is the
/// object-level token for parts, the run-level token for control calls.
async fn with_retry<T, E, Fut, F>(
    metrics: &Metrics,
    cancel: &CancellationToken,
    what: &str,
    mut op: F,
) -> Result<T>
where
    E: ProvideErrorMetadata + std::error::Error + 'static,
    Fut: std::future::Future<Output = Result<T, SdkError<E>>>,
    F: FnMut() -> Fut,
{
    for attempt in 0..MAX_ATTEMPTS {
        let result = tokio::select! {
            _ = cancel.cancelled() => bail!("cancelled"),
            r = op() => r,
        };
        match result {
            Ok(v) => return Ok(v),
            Err(e) => {
                let class = classify(&e);
                let slow_down = matches!(class, ErrClass::SlowDown);
                if slow_down {
                    metrics.slowdowns.fetch_add(1, Ordering::Relaxed);
                }
                match class {
                    ErrClass::Fatal => {
                        metrics.errors.fetch_add(1, Ordering::Relaxed);
                        return Err(anyhow!("{} 失败(不可重试): {}", what, DisplayErrorContext(&e)));
                    }
                    _ if attempt + 1 >= MAX_ATTEMPTS => {
                        metrics.errors.fetch_add(1, Ordering::Relaxed);
                        return Err(anyhow!(
                            "{} 重试 {} 次后仍失败: {}",
                            what,
                            MAX_ATTEMPTS,
                            DisplayErrorContext(&e)
                        ));
                    }
                    _ => {
                        metrics.parts_retried.fetch_add(1, Ordering::Relaxed);
                        let delay = backoff_delay(attempt, slow_down);
                        tracing::debug!(what, attempt, ?delay, "retrying: {}", DisplayErrorContext(&e));
                        tokio::select! {
                            _ = cancel.cancelled() => bail!("cancelled"),
                            _ = tokio::time::sleep(delay) => {}
                        }
                    }
                }
            }
        }
    }
    unreachable!("retry loop always returns")
}

/// Upload one part: pace via the global token bucket FIRST, then send.
async fn upload_part(
    ctx: &UploadCtx,
    cancel: &CancellationToken,
    key: &str,
    upload_id: &str,
    part_number: i32,
    len: u64,
    iteration: u64,
) -> Result<CompletedPart> {
    // Rate limiting sits BEFORE the send — this is the control point that
    // keeps aggregate write throughput inside [rate-min, rate-max].
    tokio::select! {
        _ = cancel.cancelled() => bail!("cancelled"),
        _ = ctx.limiter.acquire(len) => {}
    }

    // Compose the part from refcounted pool slices (zero-copy). Part #1 gets
    // the 64-byte unique header so object contents are globally distinct.
    let offset = rand::rng().random_range(0..ctx.pool.len());
    let chunks = if part_number == 1 {
        let mut c = vec![object_header(&ctx.run_id, iteration)];
        c.extend(ctx.pool.ring_chunks(offset, len - OBJECT_HEADER_LEN));
        c
    } else {
        ctx.pool.ring_chunks(offset, len)
    };
    debug_assert_eq!(chunks_len(&chunks), len);

    let started = std::time::Instant::now();
    let etag = if ctx.dry_run {
        format!("\"dry-run-{}\"", part_number)
    } else {
        let out = with_retry(&ctx.metrics, cancel, "UploadPart", || {
            ctx.client
                .upload_part()
                .bucket(&ctx.bucket)
                .key(key)
                .upload_id(upload_id)
                .part_number(part_number)
                .content_length(len as i64)
                .body(replayable_stream(chunks.clone()))
                .send()
        })
        .await?;
        out.e_tag().unwrap_or_default().to_string()
    };

    ctx.budget.record_requests(1);
    ctx.metrics
        .record_part(len, started.elapsed().as_millis() as u64);
    Ok(CompletedPart::builder()
        .part_number(part_number)
        .e_tag(etag)
        .build())
}

/// Upload one whole object. Any failure path aborts the multipart upload —
/// leftover parts bill forever and are invisible in the console.
pub async fn upload_object(
    ctx: Arc<UploadCtx>,
    iteration: u64,
    object_size: u64,
) -> Result<ObjectOutcome> {
    let key = ctx.object_key(iteration);

    let upload_id = if ctx.dry_run {
        format!("dry-run-{}", iteration)
    } else {
        // Any failure before parts start must still release the budget
        // reservation made by the scheduler, or remaining budget leaks away.
        let created = with_retry(&ctx.metrics, &ctx.cancel, "CreateMultipartUpload", || {
            ctx.client
                .create_multipart_upload()
                .bucket(&ctx.bucket)
                .key(&key)
                .send()
        })
        .await
        .and_then(|out| {
            out.upload_id()
                .map(|id| id.to_string())
                .ok_or_else(|| anyhow!("CreateMultipartUpload 未返回 upload_id"))
        });
        match created {
            Ok(id) => {
                ctx.budget.record_requests(1);
                id
            }
            Err(e) => {
                ctx.budget.abort_object(object_size);
                return Err(e);
            }
        }
    };
    ctx.registry.register(&key, &upload_id);

    // Per-object child token: the first part failure stops sibling parts fast,
    // and a run-level cancel propagates into it automatically.
    let object_cancel = ctx.cancel.child_token();
    let semaphore = Arc::new(Semaphore::new(ctx.concurrent_parts));
    let n_parts = object_size.div_ceil(ctx.part_size);
    let mut tasks: JoinSet<Result<CompletedPart>> = JoinSet::new();
    for i in 0..n_parts {
        let part_number = (i + 1) as i32;
        let len = (object_size - i * ctx.part_size).min(ctx.part_size);
        let ctx = ctx.clone();
        let key = key.clone();
        let upload_id = upload_id.clone();
        let semaphore = semaphore.clone();
        let object_cancel = object_cancel.clone();
        tasks.spawn(async move {
            let _permit = semaphore.acquire_owned().await?;
            upload_part(&ctx, &object_cancel, &key, &upload_id, part_number, len, iteration).await
        });
    }

    let mut parts: Vec<CompletedPart> = Vec::with_capacity(n_parts as usize);
    let mut failure: Option<anyhow::Error> = None;
    while let Some(joined) = tasks.join_next().await {
        let outcome = match joined {
            Ok(r) => r,
            // A panic inside a part task still must abort the whole upload.
            Err(join_err) => Err(anyhow!("part 任务 panic: {}", join_err)),
        };
        match outcome {
            Ok(part) => parts.push(part),
            Err(e) => {
                if failure.is_none() {
                    object_cancel.cancel();
                    failure = Some(e);
                }
            }
        }
    }

    if let Some(e) = failure {
        let cancelled = ctx.cancel.is_cancelled();
        abort_upload(&ctx, &key, &upload_id).await;
        ctx.budget.abort_object(object_size);
        ctx.metrics.objects_aborted.fetch_add(1, Ordering::Relaxed);
        if !cancelled {
            tracing::warn!("对象 {} 上传失败,已 abort: {:#}", key, e);
        }
        return Ok(ObjectOutcome::Aborted { key, cancelled });
    }

    parts.sort_by_key(|p| p.part_number());
    if !ctx.dry_run {
        let completed = CompletedMultipartUpload::builder().set_parts(Some(parts)).build();
        let complete_result = with_retry(&ctx.metrics, &ctx.cancel, "CompleteMultipartUpload", || {
            ctx.client
                .complete_multipart_upload()
                .bucket(&ctx.bucket)
                .key(&key)
                .upload_id(&upload_id)
                .multipart_upload(completed.clone())
                .send()
        })
        .await;
        match complete_result {
            Ok(_) => ctx.budget.record_requests(1),
            Err(e) => {
                abort_upload(&ctx, &key, &upload_id).await;
                ctx.budget.abort_object(object_size);
                ctx.metrics.objects_aborted.fetch_add(1, Ordering::Relaxed);
                let cancelled = ctx.cancel.is_cancelled();
                if !cancelled {
                    tracing::warn!("Complete 失败,已 abort {}: {:#}", key, e);
                }
                return Ok(ObjectOutcome::Aborted { key, cancelled });
            }
        }
    }

    ctx.registry.deregister(&key);
    ctx.budget.commit_object(object_size);
    ctx.metrics.record_object_done(&key);
    Ok(ObjectOutcome::Completed { key, bytes: object_size })
}

async fn abort_upload(ctx: &UploadCtx, key: &str, upload_id: &str) {
    if ctx.dry_run {
        ctx.registry.deregister(key);
        return;
    }
    // Deliberately NOT guarded by the cancel token: cleanup must run even
    // during shutdown.
    match ctx
        .client
        .abort_multipart_upload()
        .bucket(&ctx.bucket)
        .key(key)
        .upload_id(upload_id)
        .send()
        .await
    {
        Ok(_) => ctx.registry.deregister(key),
        // Keep it registered: the exit-path abort_all retries it; yo-s3
        // cleanup covers whatever still survives.
        Err(e) => tracing::warn!("abort {} 失败(退出时重试): {}", key, DisplayErrorContext(&e)),
    }
}
