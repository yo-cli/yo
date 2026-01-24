//! Rhai Web 服务器

use super::types::WebState;
use crate::auto::config::GlobalConfig;
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

/// 启动 Web 服务器
pub async fn run_web_server(state: Arc<WebState>, port: u16) {
    let app = Router::new()
        .route("/", get(serve_index))
        .route("/api/status", get(get_status))
        .route("/api/rules", get(get_rules))
        .route("/api/reload", get(reload_rules))
        .route("/api/scripts", get(get_scripts))
        .route("/api/script/{name}", get(get_script).put(save_script).delete(delete_script))
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
