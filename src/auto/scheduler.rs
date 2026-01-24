use crate::auto::config::{AutoConfig, ConfigManager, Task};
use crate::auto::lockscreen_monitor::LockscreenMonitor;
use crate::auto::lockscreen_state::StateManager;
use crate::auto::task_executor::TaskExecutor;
use crate::auto::tts::VolcengineTtsClient;
use chrono::{Local, NaiveTime, Timelike};
use colored::Colorize;
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::thread;
use std::time::Duration;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum SchedulerError {
    #[error("Failed to load config: {0}")]
    ConfigLoadError(String),
    #[error("Task execution failed: {0}")]
    #[allow(dead_code)]
    ExecutionError(String),
    #[error("Invalid time format: {0}")]
    InvalidTimeFormat(String),
}

pub struct TaskScheduler {
    config: AutoConfig,
    last_execution: HashMap<String, String>, // task_name -> last_execution_time (YYYY-MM-DD HH:MM)
    adaptive_states: HashMap<String, StateManager>, // task_name -> StateManager (for adaptive_lockscreen tasks)
    repeated_states: HashMap<String, StateManager>, // task_name -> StateManager (for lockscreen_repeated with max_unlocks)
}

impl TaskScheduler {
    /// 创建新的调度器
    pub fn new() -> Result<Self, SchedulerError> {
        let config = ConfigManager::load_config()
            .map_err(|e| SchedulerError::ConfigLoadError(format!("{}", e)))?;

        let mut adaptive_states = HashMap::new();
        let mut repeated_states = HashMap::new();
        let mut has_monitored_tasks = false;

        // 为所有需要监控的任务注册
        for task in &config.tasks {
            if task.task_type == "adaptive_lockscreen" {
                let initial_interval_seconds = task.interval_minutes * 60;
                let min_interval_seconds = task.min_interval_seconds;

                // 创建状态管理器
                match StateManager::new(task.name.clone(), initial_interval_seconds) {
                    Ok(state_manager) => {
                        // 注册到全局监控
                        LockscreenMonitor::register_task(
                            task.name.clone(),
                            state_manager.get_state_arc(),
                            crate::auto::lockscreen_monitor::MonitorMode::Adaptive { min_interval_seconds },
                        );
                        has_monitored_tasks = true;

                        adaptive_states.insert(task.name.clone(), state_manager);
                    }
                    Err(e) => {
                        println!(
                            "{}",
                            format!(
                                "⚠ Failed to create state manager for task '{}': {}",
                                task.name, e
                            )
                            .yellow()
                        );
                    }
                }
            }

            // 为配置了 max_unlocks 的 lockscreen_repeated 任务注册
            if task.task_type == "lockscreen_repeated" && task.max_unlocks.is_some() {
                let initial_interval_seconds = task.interval_minutes * 60;

                // 创建带解锁限制的状态管理器
                match StateManager::new(task.name.clone(), initial_interval_seconds) {
                    Ok(state_manager) => {
                        // 设置 max_unlocks
                        {
                            let state_arc = state_manager.get_state_arc();
                            let mut s = state_arc.lock().unwrap();
                            s.max_unlocks = task.max_unlocks;
                        }

                        // 注册到全局监控
                        LockscreenMonitor::register_task(
                            task.name.clone(),
                            state_manager.get_state_arc(),
                            crate::auto::lockscreen_monitor::MonitorMode::Repeated {
                                max_unlocks: task.max_unlocks,
                                tts_api_key: task.tts_api_key.clone(),
                                tts_voice: task.tts_voice.clone(),
                            },
                        );
                        has_monitored_tasks = true;

                        repeated_states.insert(task.name.clone(), state_manager);
                    }
                    Err(e) => {
                        println!(
                            "{}",
                            format!(
                                "⚠ Failed to create state manager for task '{}': {}",
                                task.name, e
                            )
                            .yellow()
                        );
                    }
                }
            }
        }

        // 如果有需要监控的任务，启动全局监控器
        if has_monitored_tasks {
            if let Err(e) = LockscreenMonitor::start_global_monitor() {
                println!(
                    "{}",
                    format!("⚠ Failed to start global monitor: {}", e).yellow()
                );
            } else {
                println!(
                    "{}",
                    "✓ Global session monitor started".green().bold()
                );
            }
        }

        // 启动时清理 TTS 缓存
        Self::clear_tts_cache();

        Ok(Self {
            config,
            last_execution: HashMap::new(),
            adaptive_states,
            repeated_states,
        })
    }

    /// 清理 TTS 缓存目录
    fn clear_tts_cache() {
        let cache_dir = Self::get_cache_dir();
        if cache_dir.exists() {
            match fs::remove_dir_all(&cache_dir) {
                Ok(_) => {
                    println!(
                        "{}",
                        "  ✓ TTS cache cleared on startup".green().bold()
                    );
                }
                Err(e) => {
                    println!(
                        "{}",
                        format!("  ⚠ Failed to clear TTS cache: {}", e).yellow()
                    );
                }
            }
        }
    }

    /// 获取 TTS 缓存目录路径
    fn get_cache_dir() -> PathBuf {
        let home = std::env::var("USERPROFILE")
            .or_else(|_| std::env::var("HOME"))
            .unwrap_or_else(|_| ".".to_string());
        PathBuf::from(home).join(".yo").join("voice").join("cache")
    }

    /// 显示当前时间和任务列表
    pub fn display_status(&self) {
        let now = Local::now();
        println!();
        println!(
            "{}",
            "╔══════════════════════════════════════════════════════════════╗"
                .cyan()
                .bold()
        );
        println!(
            "{} {} {}",
            "║".cyan().bold(),
            format!("  🤖 Yo Auto Task Scheduler").cyan().bold(),
            "                       ║".cyan().bold()
        );
        println!(
            "{}",
            "╠══════════════════════════════════════════════════════════════╣"
                .cyan()
                .bold()
        );
        println!(
            "{} {} {}",
            "║".cyan().bold(),
            format!(
                "  📅 Current Time: {}",
                now.format("%Y-%m-%d %H:%M:%S")
            )
            .white()
            .bold(),
            "                 ║".cyan().bold()
        );
        println!(
            "{}",
            "╠══════════════════════════════════════════════════════════════╣"
                .cyan()
                .bold()
        );
        println!(
            "{} {} {}",
            "║".cyan().bold(),
            "  📋 Scheduled Tasks:".yellow().bold(),
            "                                     ║".cyan().bold()
        );
        println!(
            "{}",
            "╠══════════════════════════════════════════════════════════════╣"
                .cyan()
                .bold()
        );

        if self.config.tasks.is_empty() {
            println!(
                "{} {} {}",
                "║".cyan().bold(),
                "  No tasks scheduled".white(),
                "                                   ║".cyan().bold()
            );
        } else {
            for (idx, task) in self.config.tasks.iter().enumerate() {
                let status = if task.enabled {
                    "✓".green().bold()
                } else {
                    "✗".red().bold()
                };

                let task_info = format!(
                    "  {} [{}-{}] {} - {}",
                    status,
                    task.start_time,
                    task.end_time,
                    task.name,
                    task.task_type
                );

                // 计算需要的填充空格（确保不为负）
                let info_len = task_info.chars().count();
                let padding = if info_len < 60 { 60 - info_len } else { 0 };
                println!(
                    "{} {}{}{}",
                    "║".cyan().bold(),
                    task_info.white(),
                    " ".repeat(padding),
                    "║".cyan().bold()
                );

                if let Some(ref desc) = task.description {
                    let desc_line = format!("      💡 {}", desc);
                    let desc_len = desc_line.chars().count();
                    let desc_padding = if desc_len < 60 { 60 - desc_len } else { 0 };
                    println!(
                        "{} {}{}{}",
                        "║".cyan().bold(),
                        desc_line.blue(),
                        " ".repeat(desc_padding),
                        "║".cyan().bold()
                    );
                }

                if idx < self.config.tasks.len() - 1 {
                    println!(
                        "{} {} {}",
                        "║".cyan().bold(),
                        "  ────────────────────────────────────────────────────".white(),
                        "   ║".cyan().bold()
                    );
                }
            }
        }

        println!(
            "{}",
            "╚══════════════════════════════════════════════════════════════╝"
                .cyan()
                .bold()
        );
        println!();

        // 显示配置文件路径
        if let Ok(config_path) = ConfigManager::get_config_path_str() {
            println!(
                "{}",
                format!("ℹ  Config file: {}", config_path).blue().bold()
            );
        }

        println!(
            "{}",
            "ℹ  Press Ctrl+C to stop the scheduler".yellow().bold()
        );
        println!();
    }

    /// 解析时间字符串
    fn parse_time(time_str: &str) -> Result<NaiveTime, SchedulerError> {
        NaiveTime::parse_from_str(time_str, "%H:%M")
            .map_err(|_| SchedulerError::InvalidTimeFormat(time_str.to_string()))
    }

    /// 检查任务是否应该执行
    fn should_execute_task(&mut self, task: &Task, now_time: NaiveTime, now_str: &str) -> bool {
        // 检查任务是否启用
        if !task.enabled {
            return false;
        }

        // 解析开始和结束时间
        let start_time = match Self::parse_time(&task.start_time) {
            Ok(time) => time,
            Err(_) => return false,
        };

        let end_time = match Self::parse_time(&task.end_time) {
            Ok(time) => time,
            Err(_) => return false,
        };

        // 检查当前时间是否在时间区间内（处理跨午夜情况）
        let in_time_range = if start_time <= end_time {
            // 不跨午夜: 22:00-23:00
            now_time >= start_time && now_time < end_time
        } else {
            // 跨午夜: 22:00-06:00 (意味着 22:00-23:59 或 00:00-06:00)
            now_time >= start_time || now_time < end_time
        };

        // 处理自适应锁屏任务的时间窗口
        if task.task_type == "adaptive_lockscreen" {
            if let Some(state_manager) = self.adaptive_states.get(&task.name) {
                if in_time_range && !state_manager.is_in_time_window() {
                    // 进入时间窗口
                    state_manager.enter_time_window();
                    println!(
                        "{}",
                        format!("🚪 Task '{}' entered time window", task.name)
                            .cyan()
                            .bold()
                    );
                } else if !in_time_range && state_manager.is_in_time_window() {
                    // 离开时间窗口
                    state_manager.exit_time_window(task.interval_minutes * 60);
                    state_manager.save().ok();
                    println!(
                        "{}",
                        format!("🚪 Task '{}' exited time window (state reset)", task.name)
                            .cyan()
                            .bold()
                    );
                }
            }
        }

        // 处理 lockscreen_repeated 任务的时间窗口（仅当配置了 max_unlocks）
        if task.task_type == "lockscreen_repeated" && task.max_unlocks.is_some() {
            if let Some(state_manager) = self.repeated_states.get(&task.name) {
                if in_time_range && !state_manager.is_in_time_window() {
                    // 进入时间窗口
                    state_manager.enter_time_window();
                    println!(
                        "{}",
                        format!("🚪 Task '{}' entered time window (unlock tracking enabled)", task.name)
                            .cyan()
                            .bold()
                    );
                } else if !in_time_range && state_manager.is_in_time_window() {
                    // 离开时间窗口，重置计数
                    state_manager.exit_time_window(task.interval_minutes * 60);
                    state_manager.save().ok();
                    println!(
                        "{}",
                        format!("🚪 Task '{}' exited time window (unlock count reset)", task.name)
                            .cyan()
                            .bold()
                    );
                }
            }
        }

        if !in_time_range {
            return false;
        }

        // 对于自适应锁屏任务，使用动态间隔（秒）
        if task.task_type == "adaptive_lockscreen" {
            if let Some(state_manager) = self.adaptive_states.get(&task.name) {
                // 检查距离上次执行是否超过当前间隔
                if let Some(last_exec_time_str) = self.last_execution.get(&task.name) {
                    if let Ok(last_exec_time) = chrono::NaiveDateTime::parse_from_str(
                        last_exec_time_str,
                        "%Y-%m-%d %H:%M:%S",
                    ) {
                        let now_datetime = Local::now().naive_local();
                        let elapsed = (now_datetime - last_exec_time).num_seconds() as u32;
                        let current_interval = state_manager.get_current_interval_seconds();

                        if elapsed < current_interval {
                            return false; // 还没到间隔时间
                        }
                    }
                } else {
                    // 首次执行
                    return true;
                }

                return true;
            }
        }

        // 原有逻辑：对于非自适应任务
        // 计算从开始时间到现在的分钟数
        let minutes_since_start = if now_time >= start_time {
            // 同一天
            (now_time - start_time).num_minutes()
        } else {
            // 跨午夜后的时间
            let minutes_to_midnight = (NaiveTime::from_hms_opt(23, 59, 59).unwrap() - start_time).num_minutes() + 1;
            let minutes_after_midnight = (now_time - NaiveTime::from_hms_opt(0, 0, 0).unwrap()).num_minutes();
            minutes_to_midnight + minutes_after_midnight
        };

        // 检查是否是间隔的整数倍（允许30秒误差）
        let interval = task.interval_minutes as i64;
        let is_interval_point = minutes_since_start % interval == 0;

        if !is_interval_point {
            return false;
        }

        // 检查是否在这个间隔点已经执行过
        if let Some(last_exec) = self.last_execution.get(&task.name) {
            if last_exec == now_str {
                return false; // 这个时间点已经执行过了
            }
        }

        true
    }

    /// 执行任务
    fn execute_task(&mut self, task: &Task, _now_str: String) {
        println!(
            "{}",
            format!("⏰ [{}] Triggering task: {}", Local::now().format("%H:%M:%S"), task.name)
                .yellow()
                .bold()
        );

        // 对于自适应锁屏任务，在执行前记录锁屏
        if task.task_type == "adaptive_lockscreen" {
            if let Some(state_manager) = self.adaptive_states.get(&task.name) {
                state_manager.record_lock();
                let current_interval = state_manager.get_current_interval_seconds();
                println!(
                    "{}",
                    format!(
                        "🔒 Current interval: {} seconds ({} min {} sec)",
                        current_interval,
                        current_interval / 60,
                        current_interval % 60
                    )
                    .blue()
                    .bold()
                );
            }
        }

        // 对于 lockscreen_repeated 任务，检查是否需要触发关机
        if task.task_type == "lockscreen_repeated" && task.shutdown_on_exceed {
            if let Some(state_manager) = self.repeated_states.get(&task.name) {
                let should_shutdown = {
                    let state_arc = state_manager.get_state_arc();
                    let s = state_arc.lock().unwrap();
                    s.should_trigger_shutdown()
                };

                if should_shutdown {
                    println!(
                        "{}",
                        "🔴 Maximum unlock count exceeded! Triggering shutdown...".red().bold()
                    );

                    // 播放关机警告语音
                    if let (Some(api_key), Some(voice)) = (&task.tts_api_key, &task.tts_voice) {
                        let client = VolcengineTtsClient::new(api_key.clone());
                        if let Err(e) = client.synthesize_and_play("已超过最大解锁次数，30秒后关机", voice) {
                            println!(
                                "{}",
                                format!("⚠ Failed to play shutdown warning TTS: {}", e).yellow()
                            );
                        }
                    }

                    // 执行关机命令（30秒延迟）
                    Self::execute_shutdown(30);

                    // 重置状态
                    state_manager.exit_time_window(task.interval_minutes * 60);
                    state_manager.save().ok();

                    return; // 不再执行锁屏
                }
            }
        }

        match TaskExecutor::execute_task(task) {
            Ok(_) => {
                println!(
                    "{}",
                    format!("✓ Task '{}' completed successfully", task.name)
                        .green()
                        .bold()
                );

                // 使用更精确的时间格式用于自适应任务
                let now_precise = Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
                self.last_execution.insert(task.name.clone(), now_precise);

                // 对于自适应锁屏任务，保存状态
                if task.task_type == "adaptive_lockscreen" {
                    if let Some(state_manager) = self.adaptive_states.get(&task.name) {
                        if let Err(e) = state_manager.save() {
                            println!(
                                "{}",
                                format!("⚠ Failed to save state for task '{}': {}", task.name, e)
                                    .yellow()
                            );
                        }
                    }
                }
            }
            Err(e) => {
                println!(
                    "{}",
                    format!("✗ Task '{}' failed: {}", task.name, e)
                        .red()
                        .bold()
                );
            }
        }
    }

    /// 运行调度器（持续运行）
    pub fn run(&mut self) -> Result<(), SchedulerError> {
        self.display_status();

        println!(
            "{}",
            "🚀 Task scheduler started...".green().bold()
        );
        println!();

        loop {
            let now = Local::now();
            let now_time = now.time();
            let now_str = now.format("%Y-%m-%d %H:%M").to_string();

            // 检查所有任务
            let tasks = self.config.tasks.clone();
            for task in tasks.iter() {
                if self.should_execute_task(task, now_time, &now_str) {
                    self.execute_task(task, now_str.clone());
                }
            }

            // 每 30 秒检查一次
            thread::sleep(Duration::from_secs(30));

            // 每小时重新加载配置（支持动态更新）
            if now.minute() == 0 && now.second() < 30 {
                if let Ok(new_config) = ConfigManager::load_config() {
                    self.config = new_config;
                    println!(
                        "{}",
                        format!("🔄 [{}] Configuration reloaded", now.format("%H:%M:%S"))
                            .blue()
                            .bold()
                    );
                }
            }
        }
    }

    /// 执行系统关机命令
    fn execute_shutdown(delay_seconds: u32) {
        println!(
            "{}",
            format!("⚠️ System will shutdown in {} seconds...", delay_seconds)
                .red()
                .bold()
        );

        #[cfg(target_os = "windows")]
        {
            let result = Command::new("shutdown")
                .args(&["/s", "/t", &delay_seconds.to_string()])
                .spawn();

            match result {
                Ok(_) => {
                    println!(
                        "{}",
                        format!("✓ Shutdown scheduled in {} seconds", delay_seconds)
                            .green()
                            .bold()
                    );
                }
                Err(e) => {
                    println!(
                        "{}",
                        format!("✗ Failed to schedule shutdown: {}", e).red().bold()
                    );
                }
            }
        }

        #[cfg(target_os = "linux")]
        {
            let result = Command::new("shutdown")
                .args(&["-h", &format!("+{}", delay_seconds / 60)])
                .spawn();

            match result {
                Ok(_) => {
                    println!(
                        "{}",
                        format!("✓ Shutdown scheduled in {} seconds", delay_seconds)
                            .green()
                            .bold()
                    );
                }
                Err(e) => {
                    println!(
                        "{}",
                        format!("✗ Failed to schedule shutdown: {}", e).red().bold()
                    );
                }
            }
        }

        #[cfg(target_os = "macos")]
        {
            let result = Command::new("sudo")
                .args(&["shutdown", "-h", &format!("+{}", delay_seconds / 60)])
                .spawn();

            match result {
                Ok(_) => {
                    println!(
                        "{}",
                        format!("✓ Shutdown scheduled in {} seconds", delay_seconds)
                            .green()
                            .bold()
                    );
                }
                Err(e) => {
                    println!(
                        "{}",
                        format!("✗ Failed to schedule shutdown: {}", e).red().bold()
                    );
                }
            }
        }
    }
}
