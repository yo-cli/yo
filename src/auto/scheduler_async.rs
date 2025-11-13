use crate::auto::config::{AutoConfig, Task};
use crate::auto::shared_state::SharedState;
use crate::auto::task_executor_async::TaskExecutorAsync;
use chrono::{Local, NaiveTime, Timelike};
use colored::Colorize;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use thiserror::Error;
use tokio::sync::RwLock;

#[derive(Debug, Error)]
pub enum SchedulerError {
    #[error("Failed to load config: {0}")]
    ConfigLoadError(String),
    #[error("Task execution failed: {0}")]
    ExecutionError(String),
    #[error("Invalid time format: {0}")]
    InvalidTimeFormat(String),
}

pub struct TaskSchedulerAsync {
    state: Arc<RwLock<SharedState>>,
    last_execution: HashMap<String, String>, // task_name -> last_execution_time (YYYY-MM-DD HH:MM)
}

impl TaskSchedulerAsync {
    /// 创建新的异步调度器
    pub async fn new(state: Arc<RwLock<SharedState>>) -> Result<Self, SchedulerError> {
        Ok(Self {
            state,
            last_execution: HashMap::new(),
        })
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

            // 检查并执行任务
            let tasks_to_execute = self.check_tasks_to_execute().await;

            for task in tasks_to_execute {
                println!(
                    "{}",
                    format!("⏰ Time to execute task: {}", task.name)
                        .cyan()
                        .bold()
                );

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
        let mut minutes_since_start = if current_time >= start_time {
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
