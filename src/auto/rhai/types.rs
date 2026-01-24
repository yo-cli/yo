//! Rhai 类型定义

use rhai::AST;
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
    pub counters: HashMap<String, i64>,
    pub flags: HashMap<String, bool>,
    pub tts_api_key: Option<String>,
    pub tts_voice: Option<String>,
}
