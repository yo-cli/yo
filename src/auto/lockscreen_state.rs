use chrono::{DateTime, Local};
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum StateError {
    #[error("Failed to create state directory: {0}")]
    DirectoryError(String),
    #[error("Failed to read state file: {0}")]
    ReadError(String),
    #[error("Failed to write state file: {0}")]
    WriteError(String),
    #[error("Failed to parse state: {0}")]
    ParseError(String),
    #[error("HOME environment variable not set")]
    HomeNotSet,
    #[error("IO error: {0}")]
    IoError(#[from] std::io::Error),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LockscreenState {
    /// 当前时间段内解锁次数
    pub unlock_count: u32,
    /// 当前动态间隔（秒）
    pub current_interval_seconds: u32,
    /// 最后一次锁屏时间
    #[serde(default)]
    pub last_lock_time: Option<DateTime<Local>>,
    /// 最后一次解锁时间
    #[serde(default)]
    pub last_unlock_time: Option<DateTime<Local>>,
    /// 当前是否在任务时间段内
    pub in_time_window: bool,
    /// 时间窗口起始时间
    #[serde(default)]
    pub window_start_time: Option<DateTime<Local>>,
    /// 任务名称（用于区分不同时间段的任务）
    pub task_name: String,
    /// 是否应该触发关机（解锁次数超限后下次锁屏时执行）
    #[serde(default)]
    pub should_shutdown: bool,
    /// 最大解锁次数限制（None 表示无限制）
    #[serde(default)]
    pub max_unlocks: Option<u32>,
}

impl Default for LockscreenState {
    fn default() -> Self {
        Self {
            unlock_count: 0,
            current_interval_seconds: 300, // 5 分钟
            last_lock_time: None,
            last_unlock_time: None,
            in_time_window: false,
            window_start_time: None,
            task_name: String::new(),
            should_shutdown: false,
            max_unlocks: None,
        }
    }
}

impl LockscreenState {
    /// 创建新状态
    pub fn new(task_name: String, initial_interval_seconds: u32) -> Self {
        Self {
            unlock_count: 0,
            current_interval_seconds: initial_interval_seconds,
            last_lock_time: None,
            last_unlock_time: None,
            in_time_window: false,
            window_start_time: None,
            task_name,
            should_shutdown: false,
            max_unlocks: None,
        }
    }

    /// 创建带解锁限制的新状态（用于 lockscreen_repeated）
    #[allow(dead_code)]
    pub fn new_with_max_unlocks(task_name: String, initial_interval_seconds: u32, max_unlocks: Option<u32>) -> Self {
        Self {
            unlock_count: 0,
            current_interval_seconds: initial_interval_seconds,
            last_lock_time: None,
            last_unlock_time: None,
            in_time_window: false,
            window_start_time: None,
            task_name,
            should_shutdown: false,
            max_unlocks,
        }
    }

    /// 进入时间窗口
    pub fn enter_time_window(&mut self) {
        if !self.in_time_window {
            self.in_time_window = true;
            self.window_start_time = Some(Local::now());
            self.unlock_count = 0;
        }
    }

    /// 离开时间窗口（重置状态）
    pub fn exit_time_window(&mut self, initial_interval_seconds: u32) {
        self.in_time_window = false;
        self.unlock_count = 0;
        self.current_interval_seconds = initial_interval_seconds;
        self.window_start_time = None;
        self.should_shutdown = false;
    }

    /// 记录锁屏
    pub fn record_lock(&mut self) {
        self.last_lock_time = Some(Local::now());
    }

    /// 记录解锁（减半间隔，用于 adaptive_lockscreen）
    #[allow(dead_code)]
    pub fn record_unlock(&mut self, min_interval_seconds: u32) {
        self.last_unlock_time = Some(Local::now());
        self.unlock_count += 1;

        // 减半当前间隔
        let new_interval = self.current_interval_seconds / 2;
        self.current_interval_seconds = new_interval.max(min_interval_seconds);
    }

    /// 记录解锁（简单计数，用于 lockscreen_repeated）
    /// 返回 (当前解锁次数, 是否达到最大次数)
    pub fn record_unlock_simple(&mut self) -> (u32, bool) {
        self.last_unlock_time = Some(Local::now());
        self.unlock_count += 1;

        // 检查是否达到最大解锁次数
        if let Some(max) = self.max_unlocks {
            if self.unlock_count >= max {
                self.should_shutdown = true;
                return (self.unlock_count, true);
            }
        }

        (self.unlock_count, false)
    }

    /// 检查是否应该关机
    pub fn should_trigger_shutdown(&self) -> bool {
        self.should_shutdown
    }

    /// 获取剩余解锁次数
    #[allow(dead_code)]
    pub fn remaining_unlocks(&self) -> Option<u32> {
        self.max_unlocks.map(|max| max.saturating_sub(self.unlock_count))
    }

    /// 获取当前间隔（秒）
    pub fn get_current_interval_seconds(&self) -> u32 {
        self.current_interval_seconds
    }
}

/// 状态管理器（线程安全）
pub struct StateManager {
    state_file_path: PathBuf,
    state: Arc<Mutex<LockscreenState>>,
}

impl StateManager {
    /// 创建新的状态管理器
    pub fn new(task_name: String, initial_interval_seconds: u32) -> Result<Self, StateError> {
        let state_file_path = Self::get_state_file_path(&task_name)?;

        // 尝试加载现有状态
        let state = if state_file_path.exists() {
            Self::load_state_from_file(&state_file_path)?
        } else {
            LockscreenState::new(task_name, initial_interval_seconds)
        };

        Ok(Self {
            state_file_path,
            state: Arc::new(Mutex::new(state)),
        })
    }

    /// 获取状态文件路径
    fn get_state_file_path(task_name: &str) -> Result<PathBuf, StateError> {
        let home = std::env::var("USERPROFILE")
            .or_else(|_| std::env::var("HOME"))
            .map_err(|_| StateError::HomeNotSet)?;

        let yo_dir = PathBuf::from(home).join(".yo");

        // 确保目录存在
        if !yo_dir.exists() {
            fs::create_dir_all(&yo_dir)
                .map_err(|e| StateError::DirectoryError(format!("{}", e)))?;
        }

        Ok(yo_dir.join(format!("lockscreen_state_{}.json", task_name)))
    }

    /// 从文件加载状态
    fn load_state_from_file(path: &PathBuf) -> Result<LockscreenState, StateError> {
        let content = fs::read_to_string(path)
            .map_err(|e| StateError::ReadError(format!("{}", e)))?;

        serde_json::from_str(&content)
            .map_err(|e| StateError::ParseError(format!("{}", e)))
    }

    /// 保存状态到文件
    pub fn save(&self) -> Result<(), StateError> {
        let state = self.state.lock().unwrap();
        let json = serde_json::to_string_pretty(&*state)
            .map_err(|e| StateError::WriteError(format!("{}", e)))?;

        let mut file = fs::File::create(&self.state_file_path)
            .map_err(|e| StateError::WriteError(format!("{}", e)))?;

        file.write_all(json.as_bytes())
            .map_err(|e| StateError::WriteError(format!("{}", e)))?;

        Ok(())
    }

    /// 获取状态的克隆（用于读取）
    #[allow(dead_code)]
    pub fn get_state(&self) -> LockscreenState {
        self.state.lock().unwrap().clone()
    }

    /// 获取 Arc<Mutex<LockscreenState>> 用于监控线程
    pub fn get_state_arc(&self) -> Arc<Mutex<LockscreenState>> {
        Arc::clone(&self.state)
    }

    /// 进入时间窗口
    pub fn enter_time_window(&self) {
        let mut state = self.state.lock().unwrap();
        state.enter_time_window();
    }

    /// 离开时间窗口
    pub fn exit_time_window(&self, initial_interval_seconds: u32) {
        let mut state = self.state.lock().unwrap();
        state.exit_time_window(initial_interval_seconds);
    }

    /// 记录锁屏
    pub fn record_lock(&self) {
        let mut state = self.state.lock().unwrap();
        state.record_lock();
    }

    /// 记录解锁
    #[allow(dead_code)]
    pub fn record_unlock(&self, min_interval_seconds: u32) {
        let mut state = self.state.lock().unwrap();
        state.record_unlock(min_interval_seconds);
    }

    /// 获取当前间隔
    pub fn get_current_interval_seconds(&self) -> u32 {
        let state = self.state.lock().unwrap();
        state.get_current_interval_seconds()
    }

    /// 检查是否在时间窗口内
    pub fn is_in_time_window(&self) -> bool {
        let state = self.state.lock().unwrap();
        state.in_time_window
    }
}
