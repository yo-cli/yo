use chrono::{DateTime, Local};
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::path::PathBuf;
use tokio::fs;
use tokio::io::AsyncWriteExt;

use super::config::AutoConfig;

/// 暂停状态
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PauseState {
    /// 是否暂停
    pub paused: bool,
    /// 暂停到什么时候（None 表示无限暂停）
    pub pause_until: Option<DateTime<Local>>,
    /// 什么时候暂停的
    pub paused_at: Option<DateTime<Local>>,
    /// 当前时间窗口内的暂停次数
    #[serde(default)]
    pub pause_count: u32,
    /// 时间窗口开始时间（用于重置计数）
    #[serde(default)]
    pub window_start: Option<DateTime<Local>>,
}

impl PauseState {
    /// 暂停 N 分钟（返回当前暂停次数）
    pub fn pause(&mut self, minutes: u32) -> u32 {
        let now = Local::now();
        self.paused = true;
        self.paused_at = Some(now);
        self.pause_until = Some(now + chrono::Duration::minutes(minutes as i64));
        self.pause_count += 1;
        self.pause_count
    }

    /// 恢复运行
    pub fn resume(&mut self) {
        self.paused = false;
        self.pause_until = None;
        self.paused_at = None;
    }

    /// 重置暂停计数（进入新的时间窗口时调用）
    pub fn reset_pause_count(&mut self) {
        self.pause_count = 0;
        self.window_start = Some(Local::now());
    }

    /// 获取暂停次数
    pub fn get_pause_count(&self) -> u32 {
        self.pause_count
    }

    /// 检查是否暂停
    pub fn is_paused(&self) -> bool {
        self.paused
    }

    /// 检查暂停是否已过期
    pub fn is_expired(&self) -> bool {
        if let Some(until) = self.pause_until {
            Local::now() >= until
        } else {
            false
        }
    }

    /// 获取剩余暂停秒数
    pub fn remaining_seconds(&self) -> Option<i64> {
        if let Some(until) = self.pause_until {
            let remaining = (until - Local::now()).num_seconds();
            if remaining > 0 {
                Some(remaining)
            } else {
                Some(0)
            }
        } else {
            None
        }
    }
}

/// 任务执行记录
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskExecution {
    /// 任务名称
    pub task_name: String,
    /// 执行时间
    pub executed_at: DateTime<Local>,
    /// 任务类型
    pub task_type: String,
    /// 是否成功
    pub success: bool,
    /// 消息（错误信息或成功信息）
    pub message: Option<String>,
}

/// 操作日志
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OperationLog {
    /// 时间戳
    pub timestamp: DateTime<Local>,
    /// 操作类型
    pub operation: String,
    /// 详细信息
    pub details: String,
}

/// 关机警告状态
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ShutdownWarning {
    /// 是否即将关机
    pub pending: bool,
    /// 原因（unlock_exceeded 或 pause_exceeded）
    pub reason: Option<String>,
    /// 当前解锁/暂停次数
    pub current_count: u32,
    /// 最大允许次数
    pub max_count: u32,
    /// 相关任务名称
    pub task_name: Option<String>,
}

/// 共享状态
#[derive(Debug, Clone)]
pub struct SharedState {
    /// 暂停状态
    pub pause_state: PauseState,
    /// 任务执行历史（保留最近 100 条）
    pub task_history: VecDeque<TaskExecution>,
    /// 操作日志（内存中保留最近 50 条）
    pub operation_logs: VecDeque<OperationLog>,
    /// 配置
    pub config: AutoConfig,
    /// 解锁计数（任务名 -> 计数）
    pub unlock_counts: std::collections::HashMap<String, u32>,
    /// 关机警告状态
    pub shutdown_warning: ShutdownWarning,
    /// 是否在锁屏时间窗口内
    pub in_lockscreen_window: bool,
}

impl SharedState {
    /// 加载状态
    pub async fn load() -> Self {
        let pause_state = Self::load_pause_state().await.unwrap_or_default();
        let task_history = Self::load_task_history().await.unwrap_or_default();
        let config = Self::load_config().await.unwrap_or_default();

        Self {
            pause_state,
            task_history,
            operation_logs: VecDeque::new(),
            config,
            unlock_counts: std::collections::HashMap::new(),
            shutdown_warning: ShutdownWarning::default(),
            in_lockscreen_window: false,
        }
    }

    /// 暂停 N 分钟
    /// 返回 (暂停次数, 是否超过限制)
    pub async fn pause(&mut self, minutes: u32) -> (u32, bool) {
        let pause_count = self.pause_state.pause(minutes);
        let _ = self.save_pause_state().await;

        // 检查是否在锁屏任务的时间窗口内，并且暂停次数超过 2 次
        let max_pauses = 2u32;
        let exceeded = self.in_lockscreen_window && pause_count > max_pauses;

        if exceeded {
            // 设置关机警告
            self.shutdown_warning = ShutdownWarning {
                pending: true,
                reason: Some("pause_exceeded".to_string()),
                current_count: pause_count,
                max_count: max_pauses,
                task_name: None,
            };
            self.log_operation("pause_exceeded", &format!(
                "Pause count ({}) exceeded limit ({}), shutdown will be triggered",
                pause_count, max_pauses
            )).await;
        } else {
            self.log_operation("pause", &format!(
                "User paused for {} minutes (count: {}/{})",
                minutes, pause_count, max_pauses
            )).await;
        }

        (pause_count, exceeded)
    }

    /// 恢复运行
    pub async fn resume(&mut self, is_auto: bool) {
        self.pause_state.resume();
        let _ = self.save_pause_state().await;

        let msg = if is_auto {
            "Auto resumed after pause period"
        } else {
            "User resumed manually"
        };
        self.log_operation("resume", msg).await;
    }

    /// 添加任务执行历史
    pub async fn add_history(&mut self, execution: TaskExecution) {
        // 保留最近 100 条
        if self.task_history.len() >= 100 {
            self.task_history.pop_front();
        }
        self.task_history.push_back(execution);
        let _ = self.save_task_history().await;
    }

    /// 记录操作日志
    pub async fn log_operation(&mut self, operation: &str, details: &str) {
        let log = OperationLog {
            timestamp: Local::now(),
            operation: operation.to_string(),
            details: details.to_string(),
        };

        // 内存中保留最近 50 条
        if self.operation_logs.len() >= 50 {
            self.operation_logs.pop_front();
        }
        self.operation_logs.push_back(log.clone());

        // 异步写入文件
        let _ = Self::append_log(&log).await;
    }

    /// 重新加载配置
    pub async fn reload_config(&mut self) {
        if let Ok(config) = Self::load_config().await {
            self.config = config;
            self.log_operation("config_reload", "Configuration reloaded")
                .await;
        }
    }

    /// 进入锁屏时间窗口
    pub async fn enter_lockscreen_window(&mut self, task_name: &str) {
        if !self.in_lockscreen_window {
            self.in_lockscreen_window = true;
            self.pause_state.reset_pause_count();
            self.unlock_counts.insert(task_name.to_string(), 0);
            self.shutdown_warning = ShutdownWarning::default();
            self.log_operation("enter_window", &format!(
                "Entered lockscreen time window for task '{}'",
                task_name
            )).await;
        }
    }

    /// 离开锁屏时间窗口
    pub async fn exit_lockscreen_window(&mut self, task_name: &str) {
        if self.in_lockscreen_window {
            self.in_lockscreen_window = false;
            self.pause_state.reset_pause_count();
            self.unlock_counts.remove(task_name);
            self.shutdown_warning = ShutdownWarning::default();
            self.log_operation("exit_window", &format!(
                "Exited lockscreen time window for task '{}', counters reset",
                task_name
            )).await;
        }
    }

    /// 记录解锁（用于 lockscreen_repeated 任务）
    /// 返回 (解锁次数, 是否超过限制)
    #[allow(dead_code)]
    pub fn record_unlock(&mut self, task_name: &str, max_unlocks: u32) -> (u32, bool) {
        let count = self.unlock_counts.entry(task_name.to_string()).or_insert(0);
        *count += 1;
        let current = *count;
        let exceeded = current >= max_unlocks;

        if exceeded {
            self.shutdown_warning = ShutdownWarning {
                pending: true,
                reason: Some("unlock_exceeded".to_string()),
                current_count: current,
                max_count: max_unlocks,
                task_name: Some(task_name.to_string()),
            };
        }

        (current, exceeded)
    }

    /// 获取关机警告状态
    #[allow(dead_code)]
    pub fn get_shutdown_warning(&self) -> &ShutdownWarning {
        &self.shutdown_warning
    }

    /// 清除关机警告
    #[allow(dead_code)]
    pub fn clear_shutdown_warning(&mut self) {
        self.shutdown_warning = ShutdownWarning::default();
    }

    /// 检查是否应该触发关机
    #[allow(dead_code)]
    pub fn should_trigger_shutdown(&self) -> bool {
        self.shutdown_warning.pending
    }

    // ===== 文件操作 =====

    fn get_yo_dir() -> PathBuf {
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
        PathBuf::from(home).join(".yo")
    }

    /// 加载暂停状态
    async fn load_pause_state() -> Result<PauseState, Box<dyn std::error::Error + Send + Sync>> {
        let path = Self::get_yo_dir().join("pause_state.json");
        if !path.exists() {
            return Ok(PauseState::default());
        }
        let content = fs::read_to_string(path).await?;
        Ok(serde_json::from_str(&content)?)
    }

    /// 保存暂停状态
    async fn save_pause_state(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let path = Self::get_yo_dir().join("pause_state.json");
        let content = serde_json::to_string_pretty(&self.pause_state)?;
        fs::write(path, content).await?;
        Ok(())
    }

    /// 加载任务历史
    async fn load_task_history() -> Result<VecDeque<TaskExecution>, Box<dyn std::error::Error + Send + Sync>> {
        let path = Self::get_yo_dir().join("task_history.json");
        if !path.exists() {
            return Ok(VecDeque::new());
        }
        let content = fs::read_to_string(path).await?;
        let vec: Vec<TaskExecution> = serde_json::from_str(&content)?;
        Ok(vec.into_iter().collect())
    }

    /// 保存任务历史
    async fn save_task_history(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let path = Self::get_yo_dir().join("task_history.json");
        let vec: Vec<_> = self.task_history.iter().cloned().collect();
        let content = serde_json::to_string_pretty(&vec)?;
        fs::write(path, content).await?;
        Ok(())
    }

    /// 追加操作日志
    async fn append_log(log: &OperationLog) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let path = Self::get_yo_dir().join("scheduler.log");
        let mut file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .await?;

        let line = serde_json::to_string(log)? + "\n";
        file.write_all(line.as_bytes()).await?;
        Ok(())
    }

    /// 加载配置
    async fn load_config() -> Result<AutoConfig, Box<dyn std::error::Error + Send + Sync>> {
        let path = Self::get_yo_dir().join("auto_config.json");
        if !path.exists() {
            let default_config = AutoConfig::default();
            let content = serde_json::to_string_pretty(&default_config)?;
            fs::write(path, content).await?;
            return Ok(default_config);
        }
        let content = fs::read_to_string(path).await?;
        Ok(serde_json::from_str(&content)?)
    }
}
