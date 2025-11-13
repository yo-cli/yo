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
}

impl PauseState {
    /// 暂停 N 分钟
    pub fn pause(&mut self, minutes: u32) {
        let now = Local::now();
        self.paused = true;
        self.paused_at = Some(now);
        self.pause_until = Some(now + chrono::Duration::minutes(minutes as i64));
    }

    /// 恢复运行
    pub fn resume(&mut self) {
        self.paused = false;
        self.pause_until = None;
        self.paused_at = None;
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
        }
    }

    /// 暂停 N 分钟
    pub async fn pause(&mut self, minutes: u32) {
        self.pause_state.pause(minutes);
        let _ = self.save_pause_state().await;
        self.log_operation("pause", &format!("User paused for {} minutes", minutes))
            .await;
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
