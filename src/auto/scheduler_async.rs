use crate::auto::config::Task;
use crate::auto::lockscreen_monitor::{LockscreenMonitor, MonitorMode};
use crate::auto::lockscreen_state::StateManager;
use crate::auto::shared_state::SharedState;
use crate::auto::task_executor_async::TaskExecutorAsync;
use crate::auto::tts::VolcengineTtsClient;
use chrono::{Local, NaiveTime, Timelike};
use colored::Colorize;
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;
use thiserror::Error;
use tokio::sync::RwLock;

#[derive(Debug, Error)]
pub enum SchedulerError {
    #[error("Failed to load config: {0}")]
    #[allow(dead_code)]
    ConfigLoadError(String),
    #[error("Task execution failed: {0}")]
    #[allow(dead_code)]
    ExecutionError(String),
    #[error("Invalid time format: {0}")]
    #[allow(dead_code)]
    InvalidTimeFormat(String),
}

pub struct TaskSchedulerAsync {
    state: Arc<RwLock<SharedState>>,
    last_execution: HashMap<String, String>, // task_name -> last_execution_time (YYYY-MM-DD HH:MM)
    lockscreen_states: HashMap<String, StateManager>, // task_name -> StateManager (for unlock tracking)
}

impl TaskSchedulerAsync {
    /// 创建新的异步调度器
    pub async fn new(state: Arc<RwLock<SharedState>>) -> Result<Self, SchedulerError> {
        // 启动时清理 TTS 缓存
        Self::clear_tts_cache();

        let mut lockscreen_states = HashMap::new();
        let mut has_monitored_tasks = false;

        // 从 SharedState 获取配置，注册需要监控的任务
        {
            let shared = state.read().await;
            for task in &shared.config.tasks {
                // 为配置了 max_unlocks 的 lockscreen_repeated 任务注册
                if task.task_type == "lockscreen_repeated" && task.max_unlocks.is_some() {
                    let initial_interval_seconds = task.interval_minutes * 60;

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
                                MonitorMode::Repeated {
                                    max_unlocks: task.max_unlocks,
                                    tts_api_key: task.tts_api_key.clone(),
                                    tts_voice: task.tts_voice.clone(),
                                },
                            );
                            has_monitored_tasks = true;

                            lockscreen_states.insert(task.name.clone(), state_manager);
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

        Ok(Self {
            state,
            last_execution: HashMap::new(),
            lockscreen_states,
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
    pub async fn display_status(&self) {
        let now = Local::now();
        println!();
        println!(
            "{}",
            "=".repeat(60).bright_cyan().bold()
        );
        println!(
            "{}",
            format!("⏰  Current Time: {}", now.format("%Y-%m-%d %H:%M:%S"))
                .bright_yellow()
                .bold()
        );
        println!(
            "{}",
            "=".repeat(60).bright_cyan().bold()
        );
        println!();

        let state = self.state.read().await;

        if state.config.tasks.is_empty() {
            println!("{}", "No tasks configured".yellow());
        } else {
            println!("{}", "📋 Task List:".bright_green().bold());
            for (index, task) in state.config.tasks.iter().enumerate() {
                let status = if task.enabled {
                    "✓ ENABLED".green().bold()
                } else {
                    "✗ DISABLED".red()
                };

                println!(
                    "{}. {} [{}] ({} - {}, every {} min) - {}",
                    index + 1,
                    task.name.bright_white().bold(),
                    status,
                    task.start_time,
                    task.end_time,
                    task.interval_minutes,
                    task.task_type.cyan()
                );

                if let Some(ref desc) = task.description {
                    println!("   {}", desc.bright_black());
                }
            }
        }

        println!();
        println!(
            "{}",
            "=".repeat(60).bright_cyan().bold()
        );
        println!();
    }

    /// 运行调度器主循环
    pub async fn run(&mut self) -> Result<(), SchedulerError> {
        self.display_status().await;

        println!(
            "{}",
            "🚀 Task scheduler started (30-second polling interval)".green().bold()
        );
        println!(
            "{}",
            "💡 Press Ctrl+C to stop".yellow()
        );
        println!();

        let mut last_config_reload_hour = Local::now().hour();

        loop {
            // 检查暂停状态
            let should_skip = {
                let mut state = self.state.write().await;

                if state.pause_state.is_paused() {
                    if state.pause_state.is_expired() {
                        // 暂停时间到，自动恢复
                        println!("{}", "⏰ Pause period ended, resuming...".yellow().bold());
                        state.resume(true).await; // true 表示自动恢复
                        false
                    } else {
                        // 仍在暂停中
                        if let Some(remaining) = state.pause_state.remaining_seconds() {
                            let mins = remaining / 60;
                            let secs = remaining % 60;
                            if secs == 0 {  // 只在整分钟时打印
                                println!(
                                    "{}",
                                    format!("⏸ Paused (remaining: {}:{:02})", mins, secs)
                                        .yellow()
                                );
                            }
                        }
                        true
                    }
                } else {
                    false
                }
            };

            if should_skip {
                tokio::time::sleep(Duration::from_secs(30)).await;
                continue;
            }

            let now = Local::now();
            let current_hour = now.hour();

            // 每小时整点重新加载配置
            if current_hour != last_config_reload_hour && now.minute() == 0 {
                println!(
                    "{}",
                    format!("🔄 Reloading config at {}:00", current_hour)
                        .blue()
                        .bold()
                );

                let mut state = self.state.write().await;
                state.reload_config().await;

                last_config_reload_hour = current_hour;
            }

            // 更新时间窗口状态
            self.update_time_window_state().await;

            // 检查并执行任务
            let tasks_to_execute = self.check_tasks_to_execute().await;

            for task in tasks_to_execute {
                println!(
                    "{}",
                    format!("⏰ Time to execute task: {}", task.name)
                        .cyan()
                        .bold()
                );

                // 对于 lockscreen_repeated 任务，检查是否需要触发关机
                if task.task_type == "lockscreen_repeated" && task.shutdown_on_exceed {
                    // 从 LockscreenState 检查关机状态
                    let should_shutdown = self.should_trigger_shutdown_for_task(&task.name);

                    if should_shutdown {
                        println!(
                            "{}",
                            format!("🔴 [{}] Maximum unlock count exceeded! Triggering shutdown...", task.name)
                                .red()
                                .bold()
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

                        // 执行关机命令
                        Self::execute_shutdown(30);

                        // 重置状态
                        self.reset_shutdown_state(&task.name, task.interval_minutes);

                        continue; // 不再执行锁屏
                    }
                }

                // 执行任务
                if let Err(e) = TaskExecutorAsync::execute_task(&task, self.state.clone()).await {
                    println!(
                        "{}",
                        format!("❌ Task execution failed: {}", e).red().bold()
                    );
                }

                // 更新最后执行时间
                let execution_time = now.format("%Y-%m-%d %H:%M").to_string();
                self.last_execution.insert(task.name.clone(), execution_time);
            }

            // 休眠 30 秒
            tokio::time::sleep(Duration::from_secs(30)).await;
        }
    }

    /// 检查哪些任务需要执行
    async fn check_tasks_to_execute(&self) -> Vec<Task> {
        let state = self.state.read().await;
        let now = Local::now();
        let current_time = now.format("%H:%M").to_string();
        let current_date_time = now.format("%Y-%m-%d %H:%M").to_string();

        let mut tasks_to_execute = Vec::new();

        for task in &state.config.tasks {
            if !task.enabled {
                continue;
            }

            // 检查是否在时间范围内
            if !Self::is_time_in_range(&current_time, &task.start_time, &task.end_time) {
                continue;
            }

            // 检查是否应该执行
            if self.should_execute_task(task, &current_date_time) {
                tasks_to_execute.push(task.clone());
            }
        }

        tasks_to_execute
    }

    /// 判断当前时间是否在任务的时间范围内
    fn is_time_in_range(current: &str, start: &str, end: &str) -> bool {
        let current_time = match NaiveTime::parse_from_str(current, "%H:%M") {
            Ok(t) => t,
            Err(_) => return false,
        };

        let start_time = match NaiveTime::parse_from_str(start, "%H:%M") {
            Ok(t) => t,
            Err(_) => return false,
        };

        let end_time = match NaiveTime::parse_from_str(end, "%H:%M") {
            Ok(t) => t,
            Err(_) => return false,
        };

        // 如果跨午夜（例如 22:00 到 06:00）
        if start_time > end_time {
            current_time >= start_time || current_time < end_time
        } else {
            current_time >= start_time && current_time < end_time
        }
    }

    /// 更新时间窗口状态（用于 lockscreen_repeated 任务）
    async fn update_time_window_state(&mut self) {
        let now = Local::now();
        let current_time = now.format("%H:%M").to_string();

        let mut state = self.state.write().await;

        // 查找配置了 max_unlocks 的 lockscreen_repeated 任务
        for task in &state.config.tasks.clone() {
            if task.task_type != "lockscreen_repeated" || task.max_unlocks.is_none() {
                continue;
            }

            let in_range = Self::is_time_in_range(&current_time, &task.start_time, &task.end_time);

            if in_range && !state.in_lockscreen_window {
                // 进入时间窗口
                state.enter_lockscreen_window(&task.name).await;

                // 同步更新 LockscreenState
                if let Some(state_manager) = self.lockscreen_states.get(&task.name) {
                    state_manager.enter_time_window();
                }

                println!(
                    "{}",
                    format!("🚪 Entered lockscreen window for task '{}' (unlock tracking enabled)", task.name)
                        .cyan()
                        .bold()
                );
            } else if !in_range && state.in_lockscreen_window {
                // 离开时间窗口
                state.exit_lockscreen_window(&task.name).await;

                // 同步更新 LockscreenState
                if let Some(state_manager) = self.lockscreen_states.get(&task.name) {
                    state_manager.exit_time_window(task.interval_minutes * 60);
                    let _ = state_manager.save();
                }

                println!(
                    "{}",
                    format!("🚪 Exited lockscreen window for task '{}' (counters reset)", task.name)
                        .cyan()
                        .bold()
                );
            }
        }
    }

    /// 检查是否应该触发关机（从 LockscreenState 读取）
    fn should_trigger_shutdown_for_task(&self, task_name: &str) -> bool {
        if let Some(state_manager) = self.lockscreen_states.get(task_name) {
            let state_arc = state_manager.get_state_arc();
            let s = state_arc.lock().unwrap();
            s.should_trigger_shutdown()
        } else {
            false
        }
    }

    /// 重置任务的关机状态
    fn reset_shutdown_state(&self, task_name: &str, interval_minutes: u32) {
        if let Some(state_manager) = self.lockscreen_states.get(task_name) {
            state_manager.exit_time_window(interval_minutes * 60);
            let _ = state_manager.save();
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

    /// 判断任务是否应该执行
    fn should_execute_task(&self, task: &Task, current_datetime: &str) -> bool {
        // 检查是否已经执行过
        if let Some(last_exec) = self.last_execution.get(&task.name) {
            if last_exec == current_datetime {
                return false;
            }
        }

        // 解析开始时间
        let start_time = match NaiveTime::parse_from_str(&task.start_time, "%H:%M") {
            Ok(t) => t,
            Err(_) => return false,
        };

        // 解析当前时间
        let current_time = match NaiveTime::parse_from_str(
            &current_datetime[11..16],  // 提取 HH:MM
            "%H:%M"
        ) {
            Ok(t) => t,
            Err(_) => return false,
        };

        // 计算从开始时间经过的分钟数
        let minutes_since_start = if current_time >= start_time {
            (current_time.signed_duration_since(start_time).num_minutes()) as i64
        } else {
            // 跨午夜情况
            let minutes_until_midnight = (NaiveTime::from_hms_opt(23, 59, 0)
                .unwrap()
                .signed_duration_since(start_time)
                .num_minutes()) as i64 + 1;
            let minutes_after_midnight = (current_time
                .signed_duration_since(NaiveTime::from_hms_opt(0, 0, 0).unwrap())
                .num_minutes()) as i64;
            minutes_until_midnight + minutes_after_midnight
        };

        // 检查是否在间隔点上
        let interval_minutes = task.interval_minutes as i64;
        minutes_since_start % interval_minutes == 0
    }
}
