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
}

/// 当前脚本信息
#[derive(Clone)]
pub struct CurrentScript {
    pub name: String,
    pub time_range: Option<(String, String)>,
    pub interval_minutes: u32,
    pub ast: rhai::AST,
}
