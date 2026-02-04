//! Rhai 脚本引擎

use super::{api, default_rules};
use super::types::{CurrentScript, GlobalState, Rule, Trigger};
use crate::auto::config::{keys, GlobalConfig};
use colored::Colorize;
use rhai::{Dynamic, Engine, Map, Scope};
use serde_json::{Map as JsonMap, Value as JsonValue};
use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

/// Rhai 引擎
pub struct RhaiEngine {
    engine: Engine,
    #[allow(dead_code)]
    state: Arc<Mutex<GlobalState>>,
}

impl RhaiEngine {
    pub fn new() -> Self {
        let mut engine = Engine::new();
        let state = Arc::new(Mutex::new(GlobalState::default()));

        // 程序启动时清除所有持久化的脚本状态
        // 每次重启都使用脚本中定义的新 state
        Self::clear_all_states();

        // 加载全局配置
        let config = GlobalConfig::load();
        Self::apply_config(&state, &config);

        api::register_all(&mut engine, state.clone(), config);
        Self { engine, state }
    }

    /// 清除所有持久化的脚本状态
    /// 每次程序重启时调用，确保使用脚本中定义的最新默认值
    fn clear_all_states() {
        let state_dir = Self::get_state_dir();
        if state_dir.exists() {
            if let Ok(entries) = fs::read_dir(&state_dir) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.extension().map(|e| e == "json").unwrap_or(false) {
                        if let Err(e) = fs::remove_file(&path) {
                            println!("{}", format!("  ⚠ Failed to clear state {}: {}", path.display(), e).yellow());
                        }
                    }
                }
            }
            println!("{}", "✓ Cleared all persisted script states".cyan());
        }
    }

    /// 应用全局配置到状态
    fn apply_config(state: &Arc<Mutex<GlobalState>>, config: &GlobalConfig) {
        let mut st = state.lock().unwrap();
        if let Some(api_key) = config.get(keys::TTS_API_KEY) {
            st.tts_api_key = Some(api_key.clone());
            println!("{}", format!("🔑 TTS_API_KEY loaded from config").cyan());
        }
        if let Some(voice) = config.get(keys::TTS_VOICE) {
            st.tts_voice = Some(voice.clone());
            println!("{}", format!("🔊 TTS_VOICE loaded from config: {}", voice).cyan());
        }
    }

    pub fn get_rules_dir() -> PathBuf {
        let home = std::env::var("USERPROFILE")
            .or_else(|_| std::env::var("HOME"))
            .unwrap_or_else(|_| ".".to_string());
        PathBuf::from(home).join(".yo").join("rules")
    }

    pub fn load_rules(&self) -> Result<Vec<Rule>, String> {
        let rules_dir = Self::get_rules_dir();

        if !rules_dir.exists() {
            println!("{}", format!("📁 Creating rules directory: {}", rules_dir.display()).yellow());
            fs::create_dir_all(&rules_dir).map_err(|e| e.to_string())?;
            default_rules::create(&rules_dir)?;
        }

        let mut rules = Vec::new();
        for entry in fs::read_dir(&rules_dir).map_err(|e| e.to_string())? {
            let path = entry.map_err(|e| e.to_string())?.path();
            if path.extension().map(|e| e == "rhai").unwrap_or(false) {
                match self.load_rule(&path) {
                    Ok(rule) => {
                        println!("{}", format!("  📜 Loaded: {} ({:?})", rule.name, rule.trigger.events).green());
                        rules.push(rule);
                    }
                    Err(e) => println!("{}", format!("  ⚠ Failed {}: {}", path.display(), e).yellow()),
                }
            }
        }
        println!("{}", format!("✓ Loaded {} rules", rules.len()).green().bold());
        Ok(rules)
    }

    fn load_rule(&self, path: &PathBuf) -> Result<Rule, String> {
        let name = path.file_stem().and_then(|s| s.to_str()).unwrap_or("unknown").to_string();
        let script = fs::read_to_string(path).map_err(|e| e.to_string())?;
        let ast = self.engine.compile(&script).map_err(|e| e.to_string())?;
        let (trigger, display_name, description) = self.extract_metadata(&ast)?;
        Ok(Rule { name, display_name, description, trigger, ast })
    }

    fn extract_metadata(&self, ast: &rhai::AST) -> Result<(Trigger, Option<String>, Option<String>), String> {
        let mut scope = Scope::new();
        self.engine.run_ast_with_scope(&mut scope, ast).map_err(|e| e.to_string())?;

        let mut trigger = Trigger { enabled: true, events: vec!["tick".to_string()], ..Default::default() };

        if let Some(val) = scope.get_value::<Dynamic>("trigger") {
            if let Some(map) = val.try_cast::<Map>() {
                Self::parse_trigger_map(&map, &mut trigger);
            }
        }

        // 提取 name 和 description 变量
        let display_name = scope.get_value::<String>("name");
        let description = scope.get_value::<String>("description");

        Ok((trigger, display_name, description))
    }

    fn parse_trigger_map(map: &Map, trigger: &mut Trigger) {
        if let Some(range) = map.get("time_range").and_then(|v| v.clone().into_array().ok()) {
            if range.len() >= 2 {
                let start = range[0].clone().into_string().unwrap_or_default();
                let end = range[1].clone().into_string().unwrap_or_default();
                trigger.time_range = Some((start, end));
            }
        }
        if let Some(i) = map.get("interval_minutes").and_then(|v| v.as_int().ok()) {
            trigger.interval_minutes = Some(i as u32);
        }
        if let Some(arr) = map.get("events").and_then(|v| v.clone().into_array().ok()) {
            trigger.events = arr.into_iter().filter_map(|v| v.into_string().ok()).collect();
        }
        if let Some(arr) = map.get("weekdays").and_then(|v| v.clone().into_array().ok()) {
            trigger.weekdays = Some(arr.into_iter().filter_map(|v| v.as_int().ok().map(|i| i as u32)).collect());
        }
        if let Some(b) = map.get("enabled").and_then(|v| v.as_bool().ok()) {
            trigger.enabled = b;
        }
    }

    pub fn call_on_tick(&self, rule: &Rule) -> Result<(), String> {
        self.call_fn_with_state(rule, "on_tick")
    }

    pub fn call_on_unlock(&self, rule: &Rule) -> Result<(), String> {
        self.call_fn_with_state(rule, "on_unlock")
    }

    pub fn call_on_lock(&self, rule: &Rule) -> Result<(), String> {
        self.call_fn_with_state(rule, "on_lock")
    }

    pub fn call_on_mount(&self, rule: &Rule) -> Result<(), String> {
        self.call_fn_with_state(rule, "on_mount")
    }

    pub fn call_on_destroy(&self, rule: &Rule) -> Result<(), String> {
        self.call_fn_with_state(rule, "on_destroy")
    }

    /// 调用函数，支持状态持久化
    /// 只持久化本次执行中实际改变的值
    fn call_fn_with_state(&self, rule: &Rule, fn_name: &str) -> Result<(), String> {
        // 设置当前脚本上下文（用于 generate_script_events）
        {
            let mut gs = self.state.lock().unwrap();
            gs.current_script = Some(CurrentScript {
                name: rule.name.clone(),
                time_range: rule.trigger.time_range.clone(),
                interval_minutes: rule.trigger.interval_minutes.unwrap_or(1),
                ast: rule.ast.clone(),
            });
        }

        let mut scope = Scope::new();

        // 运行脚本获取默认 state 和其他变量
        self.engine.run_ast_with_scope(&mut scope, &rule.ast).map_err(|e| e.to_string())?;

        // 获取脚本定义的默认 state
        let default_state = scope.get_value::<Dynamic>("state")
            .and_then(|v| v.try_cast::<Map>())
            .unwrap_or_default();

        // 从磁盘加载已保存的运行时状态（只包含之前改变过的值）
        let saved_runtime_state = self.load_state_from_disk(&rule.name).unwrap_or_default();

        // 合并：脚本默认值 + 已保存的运行时状态
        let mut final_state = default_state.clone();
        for (k, v) in saved_runtime_state {
            final_state.insert(k, v);
        }

        // 更新 scope 中的 state
        scope.set_value("state", final_state);

        // 调用函数（如果存在）
        let result = self.engine.call_fn::<()>(&mut scope, &rule.ast, fn_name, ());

        // 忽略函数不存在的错误
        if let Err(ref e) = result {
            let err_str = e.to_string();
            if err_str.contains("Function not found") || err_str.contains("not found") {
                // 函数不存在，静默忽略
            } else {
                return Err(err_str);
            }
        }

        // 获取执行后的状态，只保存与默认值不同的部分
        if let Some(state_val) = scope.get_value::<Dynamic>("state") {
            if let Some(state_after) = state_val.try_cast::<Map>() {
                // 只保存与脚本默认值不同的值（运行时改变的状态）
                let mut runtime_state = Map::new();

                for (k, after_val) in &state_after {
                    let default_val = default_state.get(k.as_str());

                    // 只有与默认值不同时才保存
                    let should_save = match default_val {
                        Some(dv) => !Self::dynamic_equals(&after_val, dv),
                        None => true, // 新增的 key
                    };

                    if should_save {
                        runtime_state.insert(k.clone(), after_val.clone());
                    }
                }

                // 保存到 GlobalState
                {
                    let mut gs = self.state.lock().unwrap();
                    gs.script_states.insert(rule.name.clone(), runtime_state.clone());
                }
                // 持久化到磁盘（只保存运行时改变的值）
                self.save_state_to_disk(&rule.name, &runtime_state);
            }
        }

        // 清除当前脚本上下文
        {
            let mut gs = self.state.lock().unwrap();
            gs.current_script = None;
        }

        Ok(())
    }

    /// 比较两个 Dynamic 值是否相等
    fn dynamic_equals(a: &Dynamic, b: &Dynamic) -> bool {
        if let (Ok(a_int), Ok(b_int)) = (a.as_int(), b.as_int()) {
            return a_int == b_int;
        }
        if let (Ok(a_float), Ok(b_float)) = (a.as_float(), b.as_float()) {
            return (a_float - b_float).abs() < f64::EPSILON;
        }
        if let (Ok(a_bool), Ok(b_bool)) = (a.as_bool(), b.as_bool()) {
            return a_bool == b_bool;
        }
        if let (Ok(a_str), Ok(b_str)) = (a.clone().into_string(), b.clone().into_string()) {
            return a_str == b_str;
        }
        false
    }

    /// 获取状态存储目录
    fn get_state_dir() -> PathBuf {
        let home = std::env::var("USERPROFILE")
            .or_else(|_| std::env::var("HOME"))
            .unwrap_or_else(|_| ".".to_string());
        PathBuf::from(home).join(".yo").join("state")
    }

    /// 从磁盘加载脚本状态
    fn load_state_from_disk(&self, script_name: &str) -> Option<Map> {
        let state_dir = Self::get_state_dir();
        let path = state_dir.join(format!("{}.json", script_name));

        if !path.exists() {
            return None;
        }

        let content = fs::read_to_string(&path).ok()?;
        let json: JsonMap<String, JsonValue> = serde_json::from_str(&content).ok()?;

        Some(Self::json_to_rhai_map(&json))
    }

    /// 保存脚本状态到磁盘
    fn save_state_to_disk(&self, script_name: &str, state: &Map) {
        let state_dir = Self::get_state_dir();
        if !state_dir.exists() {
            let _ = fs::create_dir_all(&state_dir);
        }

        let path = state_dir.join(format!("{}.json", script_name));
        let json = Self::rhai_map_to_json(state);

        if let Ok(content) = serde_json::to_string_pretty(&json) {
            let _ = fs::write(&path, content);
        }
    }

    /// JSON Map 转 Rhai Map
    fn json_to_rhai_map(json: &JsonMap<String, JsonValue>) -> Map {
        let mut map = Map::new();
        for (k, v) in json {
            let dynamic = Self::json_to_dynamic(v);
            map.insert(k.clone().into(), dynamic);
        }
        map
    }

    /// JSON Value 转 Rhai Dynamic
    fn json_to_dynamic(value: &JsonValue) -> Dynamic {
        match value {
            JsonValue::Null => Dynamic::UNIT,
            JsonValue::Bool(b) => Dynamic::from(*b),
            JsonValue::Number(n) => {
                if let Some(i) = n.as_i64() {
                    Dynamic::from(i)
                } else if let Some(f) = n.as_f64() {
                    Dynamic::from(f)
                } else {
                    Dynamic::UNIT
                }
            }
            JsonValue::String(s) => Dynamic::from(s.clone()),
            JsonValue::Array(arr) => {
                let vec: Vec<Dynamic> = arr.iter().map(Self::json_to_dynamic).collect();
                Dynamic::from(vec)
            }
            JsonValue::Object(obj) => {
                let map = Self::json_to_rhai_map(obj);
                Dynamic::from(map)
            }
        }
    }

    /// Rhai Map 转 JSON Map
    fn rhai_map_to_json(map: &Map) -> JsonMap<String, JsonValue> {
        let mut json = JsonMap::new();
        for (k, v) in map {
            let value = Self::dynamic_to_json(v);
            json.insert(k.to_string(), value);
        }
        json
    }

    /// Rhai Dynamic 转 JSON Value
    fn dynamic_to_json(value: &Dynamic) -> JsonValue {
        if value.is_unit() {
            JsonValue::Null
        } else if let Some(b) = value.as_bool().ok() {
            JsonValue::Bool(b)
        } else if let Some(i) = value.as_int().ok() {
            JsonValue::Number(i.into())
        } else if let Some(f) = value.as_float().ok() {
            serde_json::Number::from_f64(f)
                .map(JsonValue::Number)
                .unwrap_or(JsonValue::Null)
        } else if let Some(s) = value.clone().into_string().ok() {
            JsonValue::String(s)
        } else if let Some(arr) = value.clone().into_array().ok() {
            let vec: Vec<JsonValue> = arr.iter().map(Self::dynamic_to_json).collect();
            JsonValue::Array(vec)
        } else if let Some(map) = value.clone().try_cast::<Map>() {
            JsonValue::Object(Self::rhai_map_to_json(&map))
        } else {
            JsonValue::Null
        }
    }

    /// 获取全局状态的引用
    pub fn get_state(&self) -> Arc<Mutex<GlobalState>> {
        self.state.clone()
    }
}
