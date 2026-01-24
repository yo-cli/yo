//! Rhai 脚本引擎

use super::{api, default_rules};
use super::types::{GlobalState, Rule, Trigger};
use crate::auto::config::{keys, GlobalConfig};
use colored::Colorize;
use rhai::{Dynamic, Engine, Map, Scope};
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

        // 加载全局配置
        let config = GlobalConfig::load();
        Self::apply_config(&state, &config);

        api::register_all(&mut engine, state.clone(), config);
        Self { engine, state }
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
        self.call_fn(rule, "on_tick")
    }

    pub fn call_on_unlock(&self, rule: &Rule) -> Result<(), String> {
        self.call_fn(rule, "on_unlock")
    }

    pub fn call_on_lock(&self, rule: &Rule) -> Result<(), String> {
        self.call_fn(rule, "on_lock")
    }

    fn call_fn(&self, rule: &Rule, fn_name: &str) -> Result<(), String> {
        let mut scope = Scope::new();
        self.engine.run_ast_with_scope(&mut scope, &rule.ast).map_err(|e| e.to_string())?;
        self.engine.call_fn::<()>(&mut scope, &rule.ast, fn_name, ()).map_err(|e| e.to_string())
    }
}
