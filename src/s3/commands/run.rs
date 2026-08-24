// The run orchestrator: scheduler loop + background tasks (rate sampler,
// reporter, retention sweeper, signal handler, max-duration watchdog) +
// graceful shutdown with in-flight abort + final summary.

use anyhow::{Context, Result};
use colored::Colorize;
use indicatif::{ProgressBar, ProgressStyle};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

use super::args::RunArgs;
use super::background::{say, spawn_reporter, spawn_signal_handler, spawn_sweeper};
use super::preflight::{self, RunContext};
use crate::s3::budget::BudgetMeter;
use crate::s3::checkpoint::Checkpoint;
use crate::s3::config::{BenchConfig, RateMode, StopWhen};
use crate::s3::limiter::RateLimiter;
use crate::s3::metrics::{Metrics, RunSummary};
use crate::s3::pool::BufferPool;
use crate::s3::registry::{abort_orphans, UploadRegistry};
use crate::s3::uploader::{ObjectOutcome, UploadCtx};
use crate::s3::{fmt_bytes, fmt_rate, fmt_usd, sweep};

const MAX_CONSECUTIVE_FAILURES: u32 = 3;

pub async fn run(args: RunArgs) -> Result<()> {
    let RunContext {
        cfg,
        s3,
        upload_s3,
        mode,
        cost,
        pricing,
        mut ckpt,
        resumed,
        run_id,
        // Held until this function returns: no second instance may share this
        // budget ledger while we are spending against it.
        lock: _run_lock,
    } = preflight::prepare(args).await?;

    // --- data pool (the only data generation in the whole run) ---
    println!("{} 生成 {} 内存数据池...", "ℹ".blue(), fmt_bytes(cfg.pool_size));
    let pool_started = Instant::now();
    let pool_size = cfg.pool_size;
    let pool = Arc::new(
        tokio::task::spawn_blocking(move || BufferPool::generate(pool_size))
            .await
            .context("数据池生成任务失败")?,
    );
    println!("{} 数据池就绪({:.1}s)", "✓".green(), pool_started.elapsed().as_secs_f64());

    // --- shared components ---
    let limiter = Arc::new(RateLimiter::new(cfg.rate_min));
    limiter.resample(cfg.rate_min, cfg.rate_max);
    let metrics = Arc::new(Metrics::new());
    let budget = Arc::new(BudgetMeter::new(
        cfg.budget_micro,
        ckpt.burned_micro,
        pricing.clone(),
        cost,
        cfg.part_size,
    ));
    let registry = Arc::new(UploadRegistry::new());
    let cancel = CancellationToken::new();
    let uctx = Arc::new(UploadCtx {
        client: upload_s3,
        bucket: cfg.bucket.clone(),
        key_prefix: cfg.key_prefix.clone(),
        run_id,
        part_size: cfg.part_size,
        concurrent_parts: cfg.concurrent_parts,
        dry_run: cfg.dry_run,
        pool,
        limiter: limiter.clone(),
        metrics: metrics.clone(),
        budget: budget.clone(),
        registry: registry.clone(),
        cancel: cancel.clone(),
    });

    // Resume: abort orphan multipart uploads a hard-killed process left behind.
    if resumed && !cfg.dry_run {
        match abort_orphans(&s3, &cfg.bucket, &uctx.run_prefix()).await {
            Ok(0) => {}
            Ok(n) => println!("{} 已清理上次残留的 {} 个未完成分段上传", "✓".green(), n),
            Err(e) => eprintln!("{} 孤儿残片清理失败: {:#}", "⚠".yellow(), e),
        }
    }

    // --- bookkeeping baselines (survive across resumes) ---
    let base_active = ckpt.active_secs;
    let base_slowdown = ckpt.slowdown_total;
    let base_errors = ckpt.error_total;
    let session_start = Instant::now();
    let stop_reason: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));

    // --- progress bar (position = burned cents) ---
    let pb = ProgressBar::new((cfg.budget_micro / 10_000).max(1));
    pb.set_style(
        ProgressStyle::with_template("💸 [{bar:30.cyan/blue}] {msg}")
            .unwrap()
            .progress_chars("=>-"),
    );
    pb.set_position((ckpt.burned_micro / 10_000).min(cfg.budget_micro / 10_000));

    // --- background tasks ---
    spawn_signal_handler(cancel.clone(), stop_reason.clone(), cfg.bucket.clone());
    if matches!(cfg.rate_mode, RateMode::Continuous) {
        let limiter = limiter.clone();
        let cancel = cancel.clone();
        let (min, max, interval) = (cfg.rate_min, cfg.rate_max, cfg.rate_resample_interval);
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = cancel.cancelled() => break,
                    _ = tokio::time::sleep(interval) => { limiter.resample(min, max); }
                }
            }
        });
    }
    if let Some(max_duration) = cfg.max_duration {
        let cancel = cancel.clone();
        let stop_reason = stop_reason.clone();
        let remaining = max_duration.saturating_sub(Duration::from_secs(base_active));
        tokio::spawn(async move {
            tokio::select! {
                _ = cancel.cancelled() => {}
                _ = tokio::time::sleep(remaining) => {
                    *stop_reason.lock().unwrap() = Some("max-duration 兜底触发".to_string());
                    cancel.cancel();
                }
            }
        });
    }
    let backlog_pending = Arc::new(AtomicU64::new(0));
    spawn_reporter(
        &cfg,
        metrics.clone(),
        budget.clone(),
        limiter.clone(),
        s3.clone(),
        pb.clone(),
        cancel.clone(),
        mode.clone(),
        backlog_pending.clone(),
    );
    if !cfg.dry_run && cfg.retain > Duration::ZERO {
        spawn_sweeper(&cfg, s3.clone(), mode.destinations(), pb.clone(), cancel.clone());
    }

    // --- scheduler loop ---
    let mut inflight: JoinSet<Result<ObjectOutcome>> = JoinSet::new();
    let mut next_iteration = ckpt.completed_iterations;
    let mut scheduled_bytes = ckpt.completed_bytes;
    let mut consecutive_failures: u32 = 0;
    let mut local_stop: Option<String> = None;

    loop {
        if cancel.is_cancelled() {
            break;
        }
        if let Some(reason) = check_stop(&cfg, &ckpt, &budget) {
            local_stop = Some(reason);
            break;
        }

        // top up in-flight objects
        while inflight.len() < cfg.concurrent_objects && !cancel.is_cancelled() {
            if matches!(cfg.stop_when, StopWhen::Any) {
                if cfg.iterations.is_some_and(|n| next_iteration >= n) {
                    break;
                }
                if cfg.total_size.is_some_and(|t| scheduled_bytes >= t) {
                    break;
                }
            }
            let Some(size) = budget.plan_next_object(cfg.object_size) else {
                break;
            };
            if matches!(cfg.rate_mode, RateMode::PerObject) {
                let r = limiter.resample(cfg.rate_min, cfg.rate_max);
                say(&pb, format!("🎲 obj-{:06} 本对象速率 {}", next_iteration, fmt_rate(r)));
            }
            say(
                &pb,
                format!("⬆ 开始上传 obj-{:06}({})", next_iteration, fmt_bytes(size)),
            );
            let mode = mode.clone();
            let uctx = uctx.clone();
            let iteration = next_iteration;
            inflight.spawn(async move { mode.run_unit(uctx, iteration, size).await });
            next_iteration += 1;
            scheduled_bytes += size;
        }

        if inflight.is_empty() {
            local_stop.get_or_insert_with(|| "预算烧满(硬上限)".to_string());
            break;
        }

        let joined = tokio::select! {
            _ = cancel.cancelled() => break,
            j = inflight.join_next() => j,
        };
        let Some(joined) = joined else { continue };
        match flatten_outcome(joined) {
            Outcome::Completed { key, bytes } => {
                consecutive_failures = 0;
                ckpt.completed_iterations += 1;
                ckpt.completed_bytes += bytes;
                sync_ckpt(&mut ckpt, &budget, &metrics, base_active, base_slowdown, base_errors, session_start);
                if let Err(e) = ckpt.save(Path::new(&cfg.checkpoint_path)) {
                    eprintln!("{} checkpoint 写入失败: {:#}", "⚠".yellow(), e);
                }
                say(
                    &pb,
                    format!(
                        "{} {} 完成({}),累计已烧 {}",
                        "✓".green(),
                        key,
                        fmt_bytes(bytes),
                        fmt_usd(budget.burned_micro())
                    ),
                );
            }
            Outcome::Cancelled => {}
            // A cancel can race the join: a torn-down object is not a failure.
            Outcome::Failed(_) if cancel.is_cancelled() => {}
            Outcome::Failed(desc) => {
                consecutive_failures += 1;
                say(&pb, format!("{} {}", "✗".red(), desc));
                if consecutive_failures >= MAX_CONSECUTIVE_FAILURES {
                    local_stop = Some(format!(
                        "连续 {} 个对象失败,疑似权限/配置问题,停止",
                        MAX_CONSECUTIVE_FAILURES
                    ));
                    cancel.cancel();
                    break;
                }
            }
        }
    }

    // --- drain + cleanup ---
    cancel.cancel();
    while let Some(joined) = inflight.join_next().await {
        if let Outcome::Completed { bytes, .. } = flatten_outcome(joined) {
            ckpt.completed_iterations += 1;
            ckpt.completed_bytes += bytes;
        }
    }
    pb.finish_and_clear();
    if !cfg.dry_run {
        let aborted = registry.abort_all(&s3, &cfg.bucket).await;
        if aborted > 0 {
            println!("{} 已清理 {} 个在途残片", "✓".green(), aborted);
        }
    }
    sync_ckpt(&mut ckpt, &budget, &metrics, base_active, base_slowdown, base_errors, session_start);
    if let Err(e) = ckpt.save(Path::new(&cfg.checkpoint_path)) {
        eprintln!("{} 最终 checkpoint 写入失败: {:#}", "⚠".yellow(), e);
    }

    let reason = stop_reason
        .lock()
        .unwrap()
        .clone()
        .or(local_stop)
        .unwrap_or_else(|| "完成".to_string());

    let dest_buckets: Vec<String> = mode.destinations().iter().map(|d| d.bucket.clone()).collect();
    finish(&cfg, &ckpt, &budget, &metrics, &pricing, dest_buckets, &reason, session_start, base_active, backlog_pending.load(Ordering::Relaxed), &s3).await
}

enum Outcome {
    Completed { key: String, bytes: u64 },
    Cancelled,
    Failed(String),
}

fn flatten_outcome(joined: Result<Result<ObjectOutcome>, tokio::task::JoinError>) -> Outcome {
    match joined {
        Ok(Ok(ObjectOutcome::Completed { key, bytes })) => Outcome::Completed { key, bytes },
        Ok(Ok(ObjectOutcome::Aborted { cancelled: true, .. })) => Outcome::Cancelled,
        Ok(Ok(ObjectOutcome::Aborted { key, cancelled: false })) => {
            Outcome::Failed(format!("{} 上传失败已中止(详见上方日志)", key))
        }
        Ok(Err(e)) => Outcome::Failed(format!("对象任务失败: {:#}", e)),
        Err(join_err) => Outcome::Failed(format!("对象任务 panic: {}", join_err)),
    }
}

/// Budget is a HARD ceiling and always stops the run. Secondary bounds stop it
/// early only in `--stop-when any` mode.
fn check_stop(cfg: &BenchConfig, ckpt: &Checkpoint, budget: &BudgetMeter) -> Option<String> {
    if budget.exhausted() {
        return Some("预算烧满(硬上限)".to_string());
    }
    if matches!(cfg.stop_when, StopWhen::Any) {
        if cfg.iterations.is_some_and(|n| ckpt.completed_iterations >= n) {
            return Some("迭代次数达标(--stop-when any)".to_string());
        }
        if cfg.total_size.is_some_and(|t| ckpt.completed_bytes >= t) {
            return Some("总写入量达标(--stop-when any)".to_string());
        }
    }
    None
}

fn sync_ckpt(
    ckpt: &mut Checkpoint,
    budget: &BudgetMeter,
    metrics: &Metrics,
    base_active: u64,
    base_slowdown: u64,
    base_errors: u64,
    session_start: Instant,
) {
    ckpt.burned_micro = budget.burned_micro();
    ckpt.active_secs = base_active + session_start.elapsed().as_secs();
    ckpt.slowdown_total = base_slowdown + metrics.slowdowns.load(Ordering::Relaxed);
    ckpt.error_total = base_errors + metrics.errors.load(Ordering::Relaxed);
}

#[allow(clippy::too_many_arguments)]
async fn finish(
    cfg: &BenchConfig,
    ckpt: &Checkpoint,
    budget: &BudgetMeter,
    metrics: &Metrics,
    pricing: &crate::s3::cost::Pricing,
    dest_buckets: Vec<String>,
    reason: &str,
    session_start: Instant,
    base_active: u64,
    backlog_pending: u64,
    s3: &aws_sdk_s3::Client,
) -> Result<()> {
    let latency = metrics.latency_percentiles();
    let active_secs = base_active + session_start.elapsed().as_secs();
    let session_bytes = metrics.bytes_completed.load(Ordering::Relaxed);
    let avg_bps = if active_secs > 0 {
        // cross-resume average over completed-object bytes
        ckpt.completed_bytes / active_secs.max(1)
    } else {
        0
    };
    let retain_hours = cfg.retain.as_secs_f64() / 3600.0;
    // Source plus one stored copy per replication destination.
    let copies = 1 + dest_buckets.len() as u64;
    let storage_est = pricing.storage_micro_for(ckpt.completed_bytes * copies, retain_hours);

    let summary = RunSummary {
        run_id: ckpt.run_id.clone(),
        mode: cfg.mode.to_string(),
        dry_run: cfg.dry_run,
        bucket: cfg.bucket.clone(),
        dest_buckets: dest_buckets.clone(),
        region: pricing.region.clone(),
        started_at: ckpt.started_at.to_rfc3339(),
        finished_at: chrono::Utc::now().to_rfc3339(),
        active_secs,
        stop_reason: reason.to_string(),
        budget_usd: cfg.budget_micro as f64 / 1e6,
        burned_usd: budget.burned_micro() as f64 / 1e6,
        burned_transfer_usd: budget.transfer_micro() as f64 / 1e6,
        burned_request_usd: budget.request_micro() as f64 / 1e6,
        storage_estimate_usd_not_in_budget: storage_est as f64 / 1e6,
        objects_completed: ckpt.completed_iterations,
        objects_aborted: metrics.objects_aborted.load(Ordering::Relaxed),
        bytes_completed_objects: ckpt.completed_bytes,
        bytes_uploaded_parts: session_bytes,
        avg_throughput_bytes_per_sec: avg_bps,
        rate_min: cfg.rate_min,
        rate_max: cfg.rate_max,
        parts_ok: metrics.parts_ok.load(Ordering::Relaxed),
        parts_retried: metrics.parts_retried.load(Ordering::Relaxed),
        slowdown_count: ckpt.slowdown_total,
        error_count: ckpt.error_total,
        part_latency: latency,
        replication_pending_sampled: (!dest_buckets.is_empty()).then_some(backlog_pending),
    };

    println!("\n{}", "🏁 运行结束".cyan().bold());
    println!("  结束原因:   {}", summary.stop_reason.bold());
    println!(
        "  已烧金额:   {}(其中本次:流量 {} + 请求 {}),预算 {}",
        fmt_usd(budget.burned_micro()).green().bold(),
        fmt_usd(budget.transfer_micro()),
        fmt_usd(budget.request_micro()),
        fmt_usd(cfg.budget_micro)
    );
    println!(
        "  写入:       {} 个对象 / {},平均吞吐 {}",
        summary.objects_completed,
        fmt_bytes(summary.bytes_completed_objects),
        fmt_rate(avg_bps)
    );
    println!(
        "  part 延迟:  p50 {}ms / p95 {}ms / p99 {}ms / max {}ms({} 个 part)",
        summary.part_latency.p50_ms,
        summary.part_latency.p95_ms,
        summary.part_latency.p99_ms,
        summary.part_latency.max_ms,
        summary.part_latency.count
    );
    println!(
        "  错误:       SlowDown {} / 重试 {} / 失败 {}",
        summary.slowdown_count, summary.parts_retried, summary.error_count
    );
    println!(
        "  {} 存储费(不计入预算):约 {}({} 份保留 {:.0}h 口径,以账单为准)",
        "ℹ".blue(),
        fmt_usd(storage_est),
        copies,
        retain_hours
    );

    let json = serde_json::to_string_pretty(&summary)?;
    std::fs::write(&cfg.summary_out, &json)
        .with_context(|| format!("写入摘要失败: {}", cfg.summary_out))?;
    println!("{} JSON 摘要已写入 {}", "✓".green(), cfg.summary_out.bold());

    // What's still stored (bills until swept / lifecycle kicks in)
    if !cfg.dry_run {
        if let Ok(remaining) = sweep::count_remaining(s3, &cfg.bucket, &cfg.key_prefix).await {
            if remaining.deleted > 0 {
                println!(
                    "{} 源桶仍存有 {} 个版本({}):超过保留期会被下次运行清扫;立即清理: {}",
                    "⏳".yellow(),
                    remaining.deleted,
                    fmt_bytes(remaining.bytes),
                    format!("yo-s3 cleanup --bucket {}", cfg.bucket).bold()
                );
            }
        }
    }
    Ok(())
}
