// Background tasks spawned by the run orchestrator: signal handler,
// periodic reporter (progress bar + log line), and the retention sweeper.

use colored::Colorize;
use indicatif::ProgressBar;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio_util::sync::CancellationToken;

use crate::s3::budget::BudgetMeter;
use crate::s3::config::BenchConfig;
use crate::s3::limiter::RateLimiter;
use crate::s3::metrics::Metrics;
use crate::s3::modes::{BurnMode, DestTarget, ObserveCtx};
use crate::s3::{fmt_bytes, fmt_rate, fmt_usd, sweep};

const SWEEP_INTERVAL: Duration = Duration::from_secs(600);

/// Print through the progress bar when visible, plainly otherwise (nohup logs).
pub fn say(pb: &ProgressBar, msg: String) {
    if pb.is_hidden() {
        println!("{}", msg);
    } else {
        pb.println(msg);
    }
}

pub fn spawn_signal_handler(
    cancel: CancellationToken,
    stop_reason: Arc<Mutex<Option<String>>>,
    bucket: String,
) {
    tokio::spawn(async move {
        wait_for_terminate().await;
        *stop_reason.lock().unwrap() = Some("收到中断信号".to_string());
        eprintln!(
            "\n{} 收到中断:停止调度并清理在途残片(再按一次 Ctrl-C 强制退出,会留残片)",
            "⚠".yellow().bold()
        );
        cancel.cancel();
        let _ = tokio::signal::ctrl_c().await;
        eprintln!(
            "{} 强制退出。残片可能残留,稍后运行: yo-s3 cleanup --bucket {}",
            "✗".red().bold(),
            bucket
        );
        std::process::exit(130);
    });
}

async fn wait_for_terminate() {
    #[cfg(unix)]
    {
        let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("install SIGTERM handler");
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = term.recv() => {}
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

#[allow(clippy::too_many_arguments)]
pub fn spawn_reporter(
    cfg: &BenchConfig,
    metrics: Arc<Metrics>,
    budget: Arc<BudgetMeter>,
    limiter: Arc<RateLimiter>,
    s3: aws_sdk_s3::Client,
    pb: ProgressBar,
    cancel: CancellationToken,
    mode: Arc<dyn BurnMode>,
    backlog_pending: Arc<AtomicU64>,
) {
    let interval = cfg.report_interval;
    let bucket = cfg.bucket.clone();
    let dry_run = cfg.dry_run;
    let budget_total = cfg.budget_micro;
    tokio::spawn(async move {
        let mut last_bytes: u64 = 0;
        let mut last_tick = Instant::now();
        loop {
            tokio::select! {
                _ = cancel.cancelled() => break,
                _ = tokio::time::sleep(interval) => {}
            }
            let bytes = metrics.bytes_completed.load(Ordering::Relaxed);
            let dt = last_tick.elapsed().as_secs_f64().max(0.001);
            let inst = ((bytes - last_bytes) as f64 / dt) as u64;
            last_bytes = bytes;
            last_tick = Instant::now();

            let burned = budget.burned_micro();
            pb.set_position((burned / 10_000).min(budget_total / 10_000));
            pb.set_message(format!("{} / {}", fmt_usd(burned), fmt_usd(budget_total)));

            let mut backlog_txt = String::new();
            if !dry_run {
                let keys = metrics.recent_keys(5);
                if !keys.is_empty() {
                    let observed = mode
                        .observe(&ObserveCtx {
                            s3: &s3,
                            bucket: &bucket,
                            keys: &keys,
                        })
                        .await;
                    if let Some(obs) = observed {
                        backlog_pending.store(obs.pending, Ordering::Relaxed);
                        backlog_txt = obs.text;
                    }
                }
            }
            // ETA only means something when cost accrues per byte written.
            let per_byte = budget.cost().micro_per_byte();
            let eta = if per_byte > 0.0 && inst > 0 {
                let rem = budget_total.saturating_sub(burned) as f64;
                let secs = rem / (inst as f64 * per_byte);
                format!(
                    " | 预计剩余 {}",
                    humantime::format_duration(Duration::from_secs(secs as u64))
                )
            } else {
                String::new()
            };
            say(
                &pb,
                format!(
                    "📊 瞬时 {} | 目标 {} | 已烧 {}/{}({:.1}%) | 对象 {} 完成 | SlowDown {}{}{}",
                    fmt_rate(inst),
                    fmt_rate(limiter.rate()),
                    fmt_usd(burned),
                    fmt_usd(budget_total),
                    burned as f64 / budget_total as f64 * 100.0,
                    metrics.objects_done.load(Ordering::Relaxed),
                    metrics.slowdowns.load(Ordering::Relaxed),
                    backlog_txt,
                    eta
                ),
            );
        }
    });
}

/// Every 10 minutes, physically delete tool-created versions older than
/// --retain, on the source and on every replication destination.
pub fn spawn_sweeper(
    cfg: &BenchConfig,
    s3: aws_sdk_s3::Client,
    dests: &[DestTarget],
    pb: ProgressBar,
    cancel: CancellationToken,
) {
    let retain = cfg.retain;
    let prefix = cfg.key_prefix.clone();
    let mut targets = vec![(s3, cfg.bucket.clone())];
    for d in dests {
        targets.push((d.client.clone(), d.bucket.clone()));
    }
    tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = cancel.cancelled() => break,
                _ = tokio::time::sleep(SWEEP_INTERVAL) => {}
            }
            let cutoff = chrono::Utc::now() - chrono::Duration::from_std(retain).unwrap_or_default();
            for (client, bucket) in &targets {
                match sweep::sweep_versions_before(client, bucket, &prefix, cutoff).await {
                    Ok(stats) if stats.deleted > 0 => say(
                        &pb,
                        format!(
                            "🧹 清扫 {}: 物理删除 {} 个超期版本({})",
                            bucket,
                            stats.deleted,
                            fmt_bytes(stats.bytes)
                        ),
                    ),
                    Ok(_) => {}
                    Err(e) => say(&pb, format!("{} 清扫 {} 失败: {:#}", "⚠".yellow(), bucket, e)),
                }
            }
        }
    });
}
