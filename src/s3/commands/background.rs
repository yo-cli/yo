// Background tasks spawned by the run orchestrator: signal handler,
// periodic reporter (progress bar + log line), and the retention sweeper.

use chrono::Utc;
use colored::Colorize;
use indicatif::ProgressBar;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio_util::sync::CancellationToken;

use crate::s3::budget::BudgetMeter;
use crate::s3::config::BenchConfig;
use crate::s3::limiter::RateLimiter;
use crate::s3::metrics::Metrics;
use crate::s3::modes::{BurnMode, DestTarget, ObserveCtx};
use crate::s3::quota::HOUR_SECS;
use crate::s3::{fmt_bytes, fmt_rate, fmt_usd, sweep};

const SWEEP_INTERVAL: Duration = Duration::from_secs(600);

/// Report windows of sustained shortfall before the pace is clamped. Long
/// enough that an object boundary or one slow part cannot trigger it, short
/// enough (3 × --report-interval, 30s by default) to act well before the SDK's
/// stalled-stream protection starts killing connections.
const STARVED_TICKS_BEFORE_CLAMP: u32 = 3;

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
    paused: Arc<AtomicBool>,
) {
    let interval = cfg.report_interval;
    let bucket = cfg.bucket.clone();
    let dry_run = cfg.dry_run;
    let budget_total = cfg.budget_micro;
    tokio::spawn(async move {
        let mut last_bytes: u64 = 0;
        let mut last_tick = Instant::now();
        // Consecutive ticks where actual throughput fell far short of target.
        // Sustained shortfall means the limiter is handing out permission the
        // network cannot deliver, parts queue up, and connections eventually
        // starve — the SDK then kills them as stalled and the run dies on
        // consecutive object failures. Printing advice was not enough: a run
        // under nohup collapses hours before anyone reads it, so the reporter
        // clamps the pace itself.
        let mut starved_ticks: u32 = 0;
        loop {
            tokio::select! {
                _ = cancel.cancelled() => break,
                _ = tokio::time::sleep(interval) => {}
            }
            // Sleeping off a spent hourly or daily ceiling: the pause line already
            // said when the run wakes, and a day of these would bury it.
            if paused.load(Ordering::Relaxed) {
                last_bytes = metrics.bytes_completed.load(Ordering::Relaxed);
                last_tick = Instant::now();
                starved_ticks = 0;
                continue;
            }
            let bytes = metrics.bytes_completed.load(Ordering::Relaxed);
            let dt = last_tick.elapsed().as_secs_f64().max(0.001);
            let inst = ((bytes - last_bytes) as f64 / dt) as u64;
            last_bytes = bytes;
            last_tick = Instant::now();

            let target = limiter.rate();
            // Only meaningful once data is actually moving; a tick during pool
            // generation or between objects is not a shortfall.
            let starved = inst > 0 && target > 0 && inst < target / 2;

            let burned = budget.burned_micro();
            // In-flight is shown but never added to the bar position: the bar
            // tracks money actually committed, and a reservation can still be
            // released by an abort.
            let inflight = budget.reserved_micro();
            let inflight_txt = if inflight > 0 {
                format!("(在途 {})", fmt_usd(inflight))
            } else {
                String::new()
            };
            pb.set_position((burned / 10_000).min(budget_total / 10_000));
            pb.set_message(format!(
                "{}{} / {}",
                fmt_usd(burned),
                inflight_txt,
                fmt_usd(budget_total)
            ));

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
            // With `--days` armed, these are the numbers the operator actually
            // watches: the hour says whether the pace is on plan right now, the
            // day is what the AWS daily bill will say.
            let ceilings_txt = match budget.plan() {
                Some(plan) => format!(
                    " | 本小时 {}/{} | 今日 {}/{}",
                    fmt_usd(plan.hour().burned()),
                    fmt_usd(plan.hour().cap()),
                    fmt_usd(plan.day().burned()),
                    fmt_usd(plan.day().cap())
                ),
                None => String::new(),
            };
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
                    "📊 瞬时 {} | 目标 {} | 已烧 {}{}/{}({:.1}%){} | 对象 {} 完成 | 重试 {} | SlowDown {}{}{}",
                    fmt_rate(inst),
                    fmt_rate(target),
                    fmt_usd(burned),
                    inflight_txt,
                    fmt_usd(budget_total),
                    burned as f64 / budget_total as f64 * 100.0,
                    ceilings_txt,
                    metrics.objects_done.load(Ordering::Relaxed),
                    metrics.parts_retried.load(Ordering::Relaxed),
                    metrics.slowdowns.load(Ordering::Relaxed),
                    backlog_txt,
                    eta
                ),
            );

            if starved {
                starved_ticks += 1;
                if starved_ticks >= STARVED_TICKS_BEFORE_CLAMP {
                    // Reset rather than latch: if the clamped pace still
                    // outruns the link, the next window clamps again until it
                    // converges. `None` = already at or below what the network
                    // delivers, so there is nothing to say.
                    starved_ticks = 0;
                    if let Some((min, max)) = limiter.clamp_to_observed(inst) {
                        say(
                            &pb,
                            format!(
                                "{} 实际吞吐 {} 持续低于目标 {} 的一半 —— 网络已是瓶颈,\
                                 已自动降速到 {} ~ {}。\n  \
                                 (再快只会让 part 排队积压,连接被饿死后 SDK 判定失速并中止上传;\
                                 速率只影响耗时,预算照烧)",
                                "⚠".yellow().bold(),
                                fmt_rate(inst),
                                fmt_rate(target),
                                fmt_rate(min),
                                fmt_rate(max)
                            ),
                        );
                    }
                }
            } else {
                starved_ticks = 0;
            }
        }
    });
}

/// `--days` re-plans the pace at the top of every hour.
///
/// Each hour draws its own ceiling (see s3::quota), and the rate has to follow
/// it: left centred on the average, a heavy hour could never be reached and the
/// plan would run systematically slow — the shortfall would be one-sided,
/// because the lean hours still stop at their own ceiling. The pace is always
/// "this hour's ceiling spread over a whole hour", never over whatever is left
/// of it, so a restart at :55 simply under-burns that hour instead of trying to
/// cram an hour's money into five minutes.
pub fn spawn_hourly_pacer(
    budget: Arc<BudgetMeter>,
    limiter: Arc<RateLimiter>,
    pb: ProgressBar,
    cancel: CancellationToken,
    paused: Arc<AtomicBool>,
) {
    tokio::spawn(async move {
        // Only spawned for a `--days` run, so the plan is there for the whole
        // life of the task — looking it up once says so.
        let Some(plan) = budget.plan() else { return };
        loop {
            let cap = plan.hour().cap();
            if let Some(rate) = budget.rate_for(cap, HOUR_SECS as u64) {
                let (min, max) = limiter.recentre(rate);
                // Silent while the run is sitting out a day-long ceiling: the
                // pace still gets re-planned, but 24 of these lines would bury
                // the pause banner that says when it wakes.
                if !paused.load(Ordering::Relaxed) {
                    say(
                        &pb,
                        format!(
                            "🕐 本小时额度 {},配速 {}(区间 {} – {})",
                            fmt_usd(cap),
                            fmt_rate(rate),
                            fmt_rate(min),
                            fmt_rate(max)
                        ),
                    );
                }
            }
            // A second of slack: waking a hair early would find the hour not
            // yet rolled and re-plan against the ceiling just spent.
            let wait = plan.hour().until_reset(Utc::now()) + Duration::from_secs(1);
            tokio::select! {
                _ = cancel.cancelled() => break,
                _ = tokio::time::sleep(wait) => {}
            }
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
