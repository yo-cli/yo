//! Rhai Web 服务器

use super::types::WebState;
use crate::auto::config::GlobalConfig;
use crate::auto::rhai::api::simulate_script;
use crate::auto::rhai::engine::RhaiEngine;
use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{Html, IntoResponse, Json},
    routing::get,
    Router,
};
use chrono::Local;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::net::SocketAddr;
use std::sync::Arc;

/// 规则信息（用于 JSON 响应）
#[derive(Debug, Serialize)]
pub struct RuleInfo {
    pub name: String,
    pub display_name: Option<String>,
    pub description: Option<String>,
    pub time_range: Option<(String, String)>,
    pub interval_minutes: Option<u32>,
    pub events: Vec<String>,
    pub weekdays: Option<Vec<u32>>,
    pub enabled: bool,
}

/// 状态响应
#[derive(Debug, Serialize)]
pub struct StatusResponse {
    pub current_time: String,
    pub rules_count: usize,
}

/// 规则列表响应
#[derive(Debug, Serialize)]
pub struct RulesResponse {
    pub rules: Vec<RuleInfo>,
}

/// 脚本文件信息
#[derive(Debug, Serialize)]
pub struct ScriptInfo {
    /// 文件名（不含扩展名），作为 ID
    pub name: String,
    /// 完整文件名
    pub filename: String,
    /// 显示名称（从脚本 name 变量提取）
    pub display_name: Option<String>,
    /// 描述（从脚本 description 变量提取）
    pub description: Option<String>,
}

/// 脚本列表响应
#[derive(Debug, Serialize)]
pub struct ScriptsResponse {
    pub scripts: Vec<ScriptInfo>,
}

/// 脚本内容响应
#[derive(Debug, Serialize)]
pub struct ScriptContentResponse {
    pub name: String,
    pub content: String,
}

/// 保存脚本请求
#[derive(Debug, Deserialize)]
pub struct SaveScriptRequest {
    pub content: String,
}

/// 通用结果响应
#[derive(Debug, Serialize)]
pub struct ResultResponse {
    pub success: bool,
    pub message: String,
}

/// 配置响应
#[derive(Debug, Serialize)]
pub struct ConfigResponse {
    pub env: HashMap<String, String>,
}

/// 全局状态响应
#[derive(Debug, Serialize)]
pub struct StateResponse {
    pub scripts: HashMap<String, serde_json::Value>,
}

/// 设置环境变量请求
#[derive(Debug, Deserialize)]
pub struct SetEnvRequest {
    pub key: String,
    pub value: String,
}

/// 删除环境变量请求
#[derive(Debug, Deserialize)]
pub struct DeleteEnvRequest {
    pub key: String,
}

/// 导出脚本响应
#[derive(Debug, Serialize)]
pub struct ExportScriptResponse {
    pub filename: String,
    pub content: String,
}

/// 导出所有脚本响应
#[derive(Debug, Serialize)]
pub struct ExportAllResponse {
    pub scripts: Vec<ExportScriptResponse>,
}

/// 导入脚本请求
#[derive(Debug, Deserialize)]
pub struct ImportScriptRequest {
    pub filename: String,
    pub content: String,
    /// 冲突时的处理方式: "check" | "overwrite" | "rename" | "skip"
    pub conflict_action: Option<String>,
}

/// 重命名脚本请求
#[derive(Debug, Deserialize)]
pub struct RenameScriptRequest {
    pub new_name: String,
}

/// 日历事件（独立存储，与脚本分离）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CalendarEvent {
    pub id: String,
    pub name: String,
    pub start_time: String,  // "HH:MM"
    pub end_time: String,    // "HH:MM"
    pub color: String,
    pub weekdays: Option<Vec<u32>>,  // 1-7, None 表示每天
    pub enabled: bool,
}

/// 日程响应
#[derive(Debug, Serialize)]
pub struct ScheduleResponse {
    pub events: Vec<CalendarEvent>,
}

/// 创建/更新事件请求
#[derive(Debug, Deserialize)]
pub struct SaveEventRequest {
    pub id: Option<String>,  // 可选，用于幂等创建
    pub name: String,
    pub start_time: String,
    pub end_time: String,
    pub color: Option<String>,
    pub weekdays: Option<Vec<u32>>,
    pub enabled: Option<bool>,
}

/// 预定义颜色
const SCHEDULE_COLORS: &[&str] = &[
    "#4CAF50", // 绿色
    "#2196F3", // 蓝色
    "#FF9800", // 橙色
    "#9C27B0", // 紫色
    "#F44336", // 红色
    "#00BCD4", // 青色
    "#795548", // 棕色
    "#607D8B", // 蓝灰
];

/// 导入脚本响应
#[derive(Debug, Serialize)]
pub struct ImportScriptResponse {
    pub success: bool,
    pub message: String,
    /// 是否存在冲突
    pub conflict: bool,
    /// 冲突时的现有内容
    pub existing_content: Option<String>,
    /// 建议的新文件名（用于 rename）
    pub suggested_name: Option<String>,
}

/// 启动 Web 服务器
pub async fn run_web_server(state: Arc<WebState>, port: u16) {
    let app = Router::new()
        .route("/", get(serve_index))
        .route("/vue.min.js", get(serve_vue))
        .route("/api/status", get(get_status))
        .route("/api/rules", get(get_rules))
        .route("/api/state", get(get_state))
        .route("/api/reload", get(reload_rules))
        .route("/api/scripts", get(get_scripts))
        .route("/api/scripts/export", get(export_all_scripts))
        .route("/api/scripts/import", axum::routing::post(import_script))
        .route("/api/script/{name}", get(get_script).put(save_script).delete(delete_script))
        .route("/api/script/{name}/export", get(export_script))
        .route("/api/script/{name}/rename", axum::routing::post(rename_script))
        .route("/api/events", get(get_events).post(create_event))
        .route("/api/event/{id}", get(get_event).put(update_event).delete(delete_event_by_id))
        .route("/api/script/{name}/simulate", axum::routing::post(simulate_script_handler))
        .route("/api/config", get(get_config))
        .route("/api/config/env", axum::routing::post(set_env).delete(delete_env))
        .with_state(state);

    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    println!("🌐 Web UI: http://127.0.0.1:{}", port);

    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
    axum::serve(listener, app).await.unwrap();
}

/// 首页
async fn serve_index() -> impl IntoResponse {
    Html(include_str!("../ui/web_ui.html"))
}

/// Vue.js
async fn serve_vue() -> impl IntoResponse {
    (
        [(axum::http::header::CONTENT_TYPE, "application/javascript")],
        include_str!("../ui/vue.min.js"),
    )
}

/// 获取状态
async fn get_status(State(state): State<Arc<WebState>>) -> Json<StatusResponse> {
    let scheduler = state.scheduler.lock().unwrap();
    let rules_count = scheduler.rules_count();

    Json(StatusResponse {
        current_time: Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
        rules_count,
    })
}

/// 获取规则列表
async fn get_rules(State(state): State<Arc<WebState>>) -> Json<RulesResponse> {
    let scheduler = state.scheduler.lock().unwrap();
    let rules = scheduler
        .get_rules()
        .iter()
        .map(|r| RuleInfo {
            name: r.name.clone(),
            display_name: r.display_name.clone(),
            description: r.description.clone(),
            time_range: r.trigger.time_range.clone(),
            interval_minutes: r.trigger.interval_minutes,
            events: r.trigger.events.clone(),
            weekdays: r.trigger.weekdays.clone(),
            enabled: r.trigger.enabled,
        })
        .collect();

    Json(RulesResponse { rules })
}

/// 获取事件存储路径
fn get_events_file() -> std::path::PathBuf {
    let home = std::env::var("USERPROFILE")
        .or_else(|_| std::env::var("HOME"))
        .unwrap_or_else(|_| ".".to_string());
    std::path::PathBuf::from(home).join(".yo").join("events.json")
}

/// 加载所有事件
fn load_events() -> Vec<CalendarEvent> {
    let path = get_events_file();
    if !path.exists() {
        return Vec::new();
    }
    fs::read_to_string(&path)
        .ok()
        .and_then(|content| serde_json::from_str(&content).ok())
        .unwrap_or_default()
}

/// 保存所有事件
fn save_events(events: &[CalendarEvent]) -> Result<(), String> {
    let path = get_events_file();
    let content = serde_json::to_string_pretty(events).map_err(|e| e.to_string())?;
    fs::write(&path, content).map_err(|e| e.to_string())
}

/// 获取所有事件
async fn get_events() -> Json<ScheduleResponse> {
    let events = load_events();
    Json(ScheduleResponse { events })
}

/// 获取单个事件
async fn get_event(Path(id): Path<String>) -> Result<Json<CalendarEvent>, StatusCode> {
    let events = load_events();
    events.into_iter()
        .find(|e| e.id == id)
        .map(Json)
        .ok_or(StatusCode::NOT_FOUND)
}

/// 创建事件
async fn create_event(Json(payload): Json<SaveEventRequest>) -> Json<ResultResponse> {
    let mut events = load_events();

    // 使用提供的 ID 或生成新 ID
    let id = payload.id.unwrap_or_else(|| format!("evt_{}", chrono::Utc::now().timestamp_millis()));

    // 幂等检查：如果 ID 已存在，跳过创建
    if events.iter().any(|e| e.id == id) {
        return Json(ResultResponse {
            success: true,
            message: format!("Event {} already exists", id),
        });
    }

    let color = payload.color.unwrap_or_else(|| {
        SCHEDULE_COLORS[events.len() % SCHEDULE_COLORS.len()].to_string()
    });

    events.push(CalendarEvent {
        id: id.clone(),
        name: payload.name,
        start_time: payload.start_time,
        end_time: payload.end_time,
        color,
        weekdays: payload.weekdays,
        enabled: payload.enabled.unwrap_or(true),
    });

    match save_events(&events) {
        Ok(_) => Json(ResultResponse {
            success: true,
            message: format!("Created event {}", id),
        }),
        Err(e) => Json(ResultResponse {
            success: false,
            message: e,
        }),
    }
}

/// 更新事件
async fn update_event(
    Path(id): Path<String>,
    Json(payload): Json<SaveEventRequest>,
) -> Json<ResultResponse> {
    let mut events = load_events();

    if let Some(event) = events.iter_mut().find(|e| e.id == id) {
        event.name = payload.name;
        event.start_time = payload.start_time;
        event.end_time = payload.end_time;
        if let Some(color) = payload.color {
            event.color = color;
        }
        event.weekdays = payload.weekdays;
        if let Some(enabled) = payload.enabled {
            event.enabled = enabled;
        }

        match save_events(&events) {
            Ok(_) => Json(ResultResponse {
                success: true,
                message: format!("Updated event {}", id),
            }),
            Err(e) => Json(ResultResponse {
                success: false,
                message: e,
            }),
        }
    } else {
        Json(ResultResponse {
            success: false,
            message: format!("Event {} not found", id),
        })
    }
}

/// 删除事件
async fn delete_event_by_id(Path(id): Path<String>) -> Json<ResultResponse> {
    let mut events = load_events();
    let len_before = events.len();
    events.retain(|e| e.id != id);

    if events.len() < len_before {
        match save_events(&events) {
            Ok(_) => Json(ResultResponse {
                success: true,
                message: format!("Deleted event {}", id),
            }),
            Err(e) => Json(ResultResponse {
                success: false,
                message: e,
            }),
        }
    } else {
        Json(ResultResponse {
            success: false,
            message: format!("Event {} not found", id),
        })
    }
}

/// 模拟事件响应
#[derive(Debug, Serialize)]
pub struct SimulatedEventResponse {
    pub time: String,
    pub text: String,
}

/// 模拟脚本响应
#[derive(Debug, Serialize)]
pub struct SimulateResponse {
    pub success: bool,
    pub events: Vec<SimulatedEventResponse>,
    pub message: String,
}

/// 模拟脚本执行，生成事件
async fn simulate_script_handler(
    State(state): State<Arc<WebState>>,
    Path(name): Path<String>,
) -> Json<SimulateResponse> {
    let scheduler = state.scheduler.lock().unwrap();
    let rules = scheduler.get_rules();

    // 找到对应的规则
    let rule = rules.iter().find(|r| r.name == name);
    if rule.is_none() {
        return Json(SimulateResponse {
            success: false,
            events: vec![],
            message: format!("Script {} not found", name),
        });
    }

    let rule = rule.unwrap();
    let interval = rule.trigger.interval_minutes.unwrap_or(1);
    let time_range = rule.trigger.time_range.clone();

    // 运行模拟
    let simulated = simulate_script(&rule.ast, time_range, interval);

    // 转换为响应格式
    let events: Vec<SimulatedEventResponse> = simulated
        .into_iter()
        .map(|e| SimulatedEventResponse {
            time: e.time,
            text: e.text,
        })
        .collect();

    let count = events.len();
    Json(SimulateResponse {
        success: true,
        events,
        message: format!("Simulated {} events", count),
    })
}

/// 获取全局状态
async fn get_state(State(state): State<Arc<WebState>>) -> Json<StateResponse> {
    let scheduler = state.scheduler.lock().unwrap();
    let global_state = scheduler.get_state();
    let gs = global_state.lock().unwrap();

    // 转换 script_states 到 JSON
    let scripts: HashMap<String, serde_json::Value> = gs.script_states
        .iter()
        .map(|(name, map)| (name.clone(), rhai_map_to_json(map)))
        .collect();

    Json(StateResponse { scripts })
}

/// Rhai Map 转 JSON Value
fn rhai_map_to_json(map: &rhai::Map) -> serde_json::Value {
    let obj: serde_json::Map<String, serde_json::Value> = map
        .iter()
        .map(|(k, v)| (k.to_string(), dynamic_to_json(v)))
        .collect();
    serde_json::Value::Object(obj)
}

/// Rhai Dynamic 转 JSON Value
fn dynamic_to_json(value: &rhai::Dynamic) -> serde_json::Value {
    if value.is_unit() {
        serde_json::Value::Null
    } else if let Ok(b) = value.as_bool() {
        serde_json::Value::Bool(b)
    } else if let Ok(i) = value.as_int() {
        serde_json::Value::Number(i.into())
    } else if let Ok(f) = value.as_float() {
        serde_json::Number::from_f64(f)
            .map(serde_json::Value::Number)
            .unwrap_or(serde_json::Value::Null)
    } else if let Ok(s) = value.clone().into_string() {
        serde_json::Value::String(s)
    } else if let Ok(arr) = value.clone().into_array() {
        let vec: Vec<serde_json::Value> = arr.iter().map(dynamic_to_json).collect();
        serde_json::Value::Array(vec)
    } else if let Some(map) = value.clone().try_cast::<rhai::Map>() {
        rhai_map_to_json(&map)
    } else {
        serde_json::Value::Null
    }
}

/// 重新加载规则
async fn reload_rules(State(state): State<Arc<WebState>>) -> Json<StatusResponse> {
    let mut scheduler = state.scheduler.lock().unwrap();
    let _ = scheduler.reload();
    let rules_count = scheduler.rules_count();

    Json(StatusResponse {
        current_time: Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
        rules_count,
    })
}

/// 获取脚本列表
async fn get_scripts() -> Json<ScriptsResponse> {
    let rules_dir = RhaiEngine::get_rules_dir();
    let mut scripts = Vec::new();

    if let Ok(entries) = fs::read_dir(&rules_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().map(|e| e == "rhai").unwrap_or(false) {
                if let Some(filename) = path.file_name().and_then(|s| s.to_str()) {
                    let name = path
                        .file_stem()
                        .and_then(|s| s.to_str())
                        .unwrap_or("unknown")
                        .to_string();

                    // 从脚本内容提取 display_name 和 description
                    let (display_name, description) = fs::read_to_string(&path)
                        .ok()
                        .map(|content| extract_script_metadata(&content))
                        .unwrap_or((None, None));

                    scripts.push(ScriptInfo {
                        name,
                        filename: filename.to_string(),
                        display_name,
                        description,
                    });
                }
            }
        }
    }

    scripts.sort_by(|a, b| a.filename.cmp(&b.filename));
    Json(ScriptsResponse { scripts })
}

/// 从脚本内容提取 name 和 description 变量
fn extract_script_metadata(content: &str) -> (Option<String>, Option<String>) {
    let mut display_name = None;
    let mut description = None;

    for line in content.lines() {
        let trimmed = line.trim();

        // 提取 let name = "...";
        if trimmed.starts_with("let name") {
            if let Some(start) = trimmed.find('"') {
                if let Some(end) = trimmed[start + 1..].find('"') {
                    display_name = Some(trimmed[start + 1..start + 1 + end].to_string());
                }
            }
        }

        // 提取 let description = "...";
        if trimmed.starts_with("let description") {
            if let Some(start) = trimmed.find('"') {
                if let Some(end) = trimmed[start + 1..].find('"') {
                    description = Some(trimmed[start + 1..start + 1 + end].to_string());
                }
            }
        }

        // 如果两个都找到了，提前退出
        if display_name.is_some() && description.is_some() {
            break;
        }
    }

    (display_name, description)
}

/// 获取脚本内容
async fn get_script(Path(name): Path<String>) -> Result<Json<ScriptContentResponse>, StatusCode> {
    let rules_dir = RhaiEngine::get_rules_dir();
    let path = rules_dir.join(format!("{}.rhai", name));

    match fs::read_to_string(&path) {
        Ok(content) => Ok(Json(ScriptContentResponse { name, content })),
        Err(_) => Err(StatusCode::NOT_FOUND),
    }
}

/// 保存脚本
async fn save_script(
    Path(name): Path<String>,
    Json(payload): Json<SaveScriptRequest>,
) -> Json<ResultResponse> {
    let rules_dir = RhaiEngine::get_rules_dir();
    let path = rules_dir.join(format!("{}.rhai", name));

    match fs::write(&path, &payload.content) {
        Ok(_) => Json(ResultResponse {
            success: true,
            message: format!("Saved {}.rhai", name),
        }),
        Err(e) => Json(ResultResponse {
            success: false,
            message: format!("Failed to save: {}", e),
        }),
    }
}

/// 删除脚本
async fn delete_script(Path(name): Path<String>) -> Json<ResultResponse> {
    let rules_dir = RhaiEngine::get_rules_dir();
    let path = rules_dir.join(format!("{}.rhai", name));

    if !path.exists() {
        return Json(ResultResponse {
            success: false,
            message: format!("Script {}.rhai not found", name),
        });
    }

    match fs::remove_file(&path) {
        Ok(_) => Json(ResultResponse {
            success: true,
            message: format!("Deleted {}.rhai", name),
        }),
        Err(e) => Json(ResultResponse {
            success: false,
            message: format!("Failed to delete: {}", e),
        }),
    }
}

/// 获取配置
async fn get_config() -> Json<ConfigResponse> {
    let config = GlobalConfig::load();
    Json(ConfigResponse {
        env: config.env,
    })
}

/// 设置环境变量
async fn set_env(Json(payload): Json<SetEnvRequest>) -> Json<ResultResponse> {
    let mut config = GlobalConfig::load();
    config.set(payload.key.clone(), payload.value);

    match config.save() {
        Ok(_) => Json(ResultResponse {
            success: true,
            message: format!("Set {}", payload.key),
        }),
        Err(e) => Json(ResultResponse {
            success: false,
            message: e,
        }),
    }
}

/// 删除环境变量
async fn delete_env(Json(payload): Json<DeleteEnvRequest>) -> Json<ResultResponse> {
    let mut config = GlobalConfig::load();

    if config.remove(&payload.key).is_none() {
        return Json(ResultResponse {
            success: false,
            message: format!("Key {} not found", payload.key),
        });
    }

    match config.save() {
        Ok(_) => Json(ResultResponse {
            success: true,
            message: format!("Deleted {}", payload.key),
        }),
        Err(e) => Json(ResultResponse {
            success: false,
            message: e,
        }),
    }
}

/// 导出单个脚本
async fn export_script(Path(name): Path<String>) -> Result<Json<ExportScriptResponse>, StatusCode> {
    let rules_dir = RhaiEngine::get_rules_dir();
    let path = rules_dir.join(format!("{}.rhai", name));

    match fs::read_to_string(&path) {
        Ok(content) => Ok(Json(ExportScriptResponse {
            filename: format!("{}.rhai", name),
            content,
        })),
        Err(_) => Err(StatusCode::NOT_FOUND),
    }
}

/// 导出所有脚本
async fn export_all_scripts() -> Json<ExportAllResponse> {
    let rules_dir = RhaiEngine::get_rules_dir();
    let mut scripts = Vec::new();

    if let Ok(entries) = fs::read_dir(&rules_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().map(|e| e == "rhai").unwrap_or(false) {
                if let (Some(filename), Ok(content)) = (
                    path.file_name().and_then(|s| s.to_str()),
                    fs::read_to_string(&path),
                ) {
                    scripts.push(ExportScriptResponse {
                        filename: filename.to_string(),
                        content,
                    });
                }
            }
        }
    }

    scripts.sort_by(|a, b| a.filename.cmp(&b.filename));
    Json(ExportAllResponse { scripts })
}

/// 导入脚本
async fn import_script(Json(payload): Json<ImportScriptRequest>) -> Json<ImportScriptResponse> {
    let rules_dir = RhaiEngine::get_rules_dir();

    // 确保文件名以 .rhai 结尾
    let filename = if payload.filename.ends_with(".rhai") {
        payload.filename.clone()
    } else {
        format!("{}.rhai", payload.filename)
    };

    let path = rules_dir.join(&filename);
    let action = payload.conflict_action.as_deref().unwrap_or("check");

    // 检查是否存在冲突
    if path.exists() {
        match action {
            "check" => {
                // 仅检查冲突，返回现有内容供对比
                let existing = fs::read_to_string(&path).unwrap_or_default();
                let suggested = generate_unique_filename(&rules_dir, &filename);
                return Json(ImportScriptResponse {
                    success: false,
                    message: format!("Script {} already exists", filename),
                    conflict: true,
                    existing_content: Some(existing),
                    suggested_name: Some(suggested),
                });
            }
            "overwrite" => {
                // 覆盖现有文件
                match fs::write(&path, &payload.content) {
                    Ok(_) => return Json(ImportScriptResponse {
                        success: true,
                        message: format!("Overwritten {}", filename),
                        conflict: false,
                        existing_content: None,
                        suggested_name: None,
                    }),
                    Err(e) => return Json(ImportScriptResponse {
                        success: false,
                        message: format!("Failed to write: {}", e),
                        conflict: false,
                        existing_content: None,
                        suggested_name: None,
                    }),
                }
            }
            "rename" => {
                // 使用新文件名保存
                let new_filename = generate_unique_filename(&rules_dir, &filename);
                let new_path = rules_dir.join(&new_filename);
                match fs::write(&new_path, &payload.content) {
                    Ok(_) => return Json(ImportScriptResponse {
                        success: true,
                        message: format!("Saved as {}", new_filename),
                        conflict: false,
                        existing_content: None,
                        suggested_name: Some(new_filename),
                    }),
                    Err(e) => return Json(ImportScriptResponse {
                        success: false,
                        message: format!("Failed to write: {}", e),
                        conflict: false,
                        existing_content: None,
                        suggested_name: None,
                    }),
                }
            }
            "skip" => {
                return Json(ImportScriptResponse {
                    success: true,
                    message: format!("Skipped {}", filename),
                    conflict: false,
                    existing_content: None,
                    suggested_name: None,
                });
            }
            _ => {}
        }
    }

    // 无冲突，直接保存
    match fs::write(&path, &payload.content) {
        Ok(_) => Json(ImportScriptResponse {
            success: true,
            message: format!("Imported {}", filename),
            conflict: false,
            existing_content: None,
            suggested_name: None,
        }),
        Err(e) => Json(ImportScriptResponse {
            success: false,
            message: format!("Failed to write: {}", e),
            conflict: false,
            existing_content: None,
            suggested_name: None,
        }),
    }
}

/// 重命名脚本
async fn rename_script(
    Path(old_name): Path<String>,
    Json(payload): Json<RenameScriptRequest>,
) -> Json<ResultResponse> {
    let rules_dir = RhaiEngine::get_rules_dir();
    let old_path = rules_dir.join(format!("{}.rhai", old_name));
    let new_name = payload.new_name.trim().trim_end_matches(".rhai");
    let new_path = rules_dir.join(format!("{}.rhai", new_name));

    if !old_path.exists() {
        return Json(ResultResponse {
            success: false,
            message: format!("Script {}.rhai not found", old_name),
        });
    }

    if new_path.exists() {
        return Json(ResultResponse {
            success: false,
            message: format!("Script {}.rhai already exists", new_name),
        });
    }

    match fs::rename(&old_path, &new_path) {
        Ok(_) => {
            // 同时重命名状态文件
            let state_dir = rules_dir.parent().unwrap().join("state");
            let old_state = state_dir.join(format!("{}.json", old_name));
            let new_state = state_dir.join(format!("{}.json", new_name));
            if old_state.exists() {
                let _ = fs::rename(&old_state, &new_state);
            }

            Json(ResultResponse {
                success: true,
                message: format!("Renamed to {}.rhai", new_name),
            })
        }
        Err(e) => Json(ResultResponse {
            success: false,
            message: format!("Failed to rename: {}", e),
        }),
    }
}

/// 生成唯一文件名
fn generate_unique_filename(dir: &std::path::Path, filename: &str) -> String {
    let stem = filename.trim_end_matches(".rhai");
    let mut counter = 1;
    loop {
        let new_name = format!("{}_{}.rhai", stem, counter);
        if !dir.join(&new_name).exists() {
            return new_name;
        }
        counter += 1;
    }
}
