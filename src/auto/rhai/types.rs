//! Rhai 类型定义

use rhai::{AST, Map};
use std::collections::HashMap;

/// 触发条件（从脚本解析）
#[derive(Debug, Clone, Default)]
pub struct Trigger {
    pub time_range: Option<(String, String)>,
    pub interval_minutes: Option<u32>,
    pub events: Vec<String>,
    pub weekdays: Option<Vec<u32>>,
    pub enabled: bool,
}

/// 规则（一个脚本文件）
#[derive(Clone)]
pub struct Rule {
    /// 文件名（不含扩展名），作为 ID
    pub name: String,
    /// 显示名称（从脚本的 name 变量读取）
    pub display_name: Option<String>,
    /// 描述（从脚本的 description 变量读取）
    pub description: Option<String>,
    pub trigger: Trigger,
    pub ast: AST,
}

/// 运行模式
#[derive(Default, Clone)]
pub enum RunMode {
    /// 正常执行：播放音频、执行副作用
    #[default]
    Real,
    /// TTS 缓存模式：只生成缓存不播放，跳过其他副作用
    CacheTts { hour: u32, minute: u32 },
    /// 事件生成模式：只收集 speak 文本，不调用 TTS
    GenerateEvents { hour: u32, minute: u32 },
}

impl RunMode {
    /// 是否为真实执行模式
    pub fn is_real(&self) -> bool {
        matches!(self, RunMode::Real)
    }

    /// 获取模拟时间，Real 模式返回 None
    pub fn sim_time(&self) -> Option<(u32, u32)> {
        match self {
            RunMode::Real => None,
            RunMode::CacheTts { hour, minute } => Some((*hour, *minute)),
            RunMode::GenerateEvents { hour, minute } => Some((*hour, *minute)),
        }
    }
}

/// 事件类型
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum EventType {
    /// speak() 调用
    Speak,
    /// chime() 报时
    Chime,
}

/// 收集的事件
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct CollectedEvent {
    pub time: String,
    pub text: String,
    pub event_type: EventType,
}

/// 时间解析工具函数
/// 将 "HH:MM" 格式转换为分钟数
pub fn parse_time_to_minutes(time_str: &str) -> i64 {
    let parts: Vec<&str> = time_str.split(':').collect();
    if parts.len() >= 2 {
        parts[0].parse::<i64>().unwrap_or(0) * 60 + parts[1].parse::<i64>().unwrap_or(0)
    } else {
        0
    }
}

/// 获取用户主目录
pub fn get_home_dir() -> String {
    std::env::var("USERPROFILE")
        .or_else(|_| std::env::var("HOME"))
        .unwrap_or_else(|_| ".".to_string())
}

/// 计算结束时间
/// 给定开始时间和持续分钟数，返回结束时间字符串
pub fn calculate_end_time(start: &str, duration_mins: u32) -> String {
    let parts: Vec<&str> = start.split(':').collect();
    if parts.len() >= 2 {
        let h: u32 = parts[0].parse().unwrap_or(0);
        let m: u32 = parts[1].parse().unwrap_or(0);
        let total = h * 60 + m + duration_mins;
        let end_h = (total / 60) % 24;
        let end_m = total % 60;
        format!("{:02}:{:02}", end_h, end_m)
    } else {
        start.to_string()
    }
}

/// 执行上下文
#[derive(Default, Clone)]
pub struct ExecuteContext {
    /// 运行模式
    pub mode: RunMode,
    /// GenerateEvents 模式下收集的事件
    pub collected_events: Vec<CollectedEvent>,
}

/// 全局状态
#[derive(Default)]
pub struct GlobalState {
    pub tts_api_key: Option<String>,
    pub tts_voice: Option<String>,
    /// 每个脚本的状态（script_name -> state Map）
    pub script_states: HashMap<String, Map>,
    /// 跟踪每个脚本是否在时间范围内（用于检测 on_destroy）
    pub script_in_range: HashMap<String, bool>,
    /// 当前正在执行的脚本（用于 generate_script_events）
    pub current_script: Option<CurrentScript>,
    /// 执行上下文
    pub exec_ctx: ExecuteContext,
}

/// 当前脚本信息
#[derive(Clone)]
pub struct CurrentScript {
    pub name: String,
    pub time_range: Option<(String, String)>,
    pub interval_minutes: u32,
    pub weekdays: Option<Vec<u32>>,
    pub ast: rhai::AST,
}

/// 日历事件（用于 Web UI 和事件生成）
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CalendarEvent {
    pub id: String,
    pub name: String,
    pub start_time: String,
    pub end_time: String,
    pub color: String,
    pub weekdays: Option<Vec<u32>>,
    pub enabled: bool,
}
