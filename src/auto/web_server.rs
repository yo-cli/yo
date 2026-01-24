use axum::{
    extract::State,
    http::StatusCode,
    response::{Html, IntoResponse, Json},
    routing::{get, post},
    Router,
};
use chrono::{DateTime, Duration, Local, NaiveTime, Timelike};
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::sync::RwLock;

use super::config::Task;
use super::shared_state::{OperationLog, SharedState, ShutdownWarning, TaskExecution};

/// 应用状态（共享）
pub type AppState = Arc<RwLock<SharedState>>;

/// 状态响应
#[derive(Debug, Serialize)]
pub struct StatusResponse {
    pub paused: bool,
    pub pause_until: Option<DateTime<Local>>,
    pub remaining_seconds: Option<i64>,
    pub current_time: DateTime<Local>,
    pub pause_count: u32,
    pub max_pauses: u32,
    pub in_lockscreen_window: bool,
    pub shutdown_warning: ShutdownWarning,
}

/// 暂停请求
#[derive(Debug, Deserialize)]
pub struct PauseRequest {
    pub minutes: u32,
}

/// 成功响应
#[derive(Debug, Serialize)]
pub struct SuccessResponse {
    pub success: bool,
    pub message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pause_count: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub shutdown_triggered: Option<bool>,
}

/// 任务列表响应
#[derive(Debug, Serialize)]
pub struct TasksResponse {
    pub tasks: Vec<Task>,
}

/// 历史记录响应
#[derive(Debug, Serialize)]
pub struct HistoryResponse {
    pub history: Vec<TaskExecution>,
}

/// 日志响应
#[derive(Debug, Serialize)]
pub struct LogsResponse {
    pub logs: Vec<OperationLog>,
}

/// 即将执行的任务
#[derive(Debug, Serialize, Clone)]
pub struct UpcomingTask {
    pub task_name: String,
    pub task_type: String,
    pub execute_at: DateTime<Local>,
    pub seconds_until: i64,
    /// 是否会触发关机
    #[serde(skip_serializing_if = "Option::is_none")]
    pub will_trigger_shutdown: Option<bool>,
    /// 关机警告消息
    #[serde(skip_serializing_if = "Option::is_none")]
    pub shutdown_warning_message: Option<String>,
}

/// 即将执行的任务列表响应
#[derive(Debug, Serialize)]
pub struct UpcomingTasksResponse {
    pub upcoming: Vec<UpcomingTask>,
    /// 全局关机警告
    pub shutdown_warning: ShutdownWarning,
}

/// 启动 Web 服务器
pub async fn run_web_server(state: AppState, port: u16) {
    let app = Router::new()
        .route("/", get(serve_index))
        .route("/api/status", get(get_status))
        .route("/api/pause", post(post_pause))
        .route("/api/resume", post(post_resume))
        .route("/api/tasks", get(get_tasks))
        .route("/api/history", get(get_history))
        .route("/api/logs", get(get_logs))
        .route("/api/upcoming", get(get_upcoming))
        .with_state(state);

    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    println!("🌐 Web UI running at http://127.0.0.1:{}", port);

    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
    axum::serve(listener, app).await.unwrap();
}

/// 提供首页 HTML
async fn serve_index() -> impl IntoResponse {
    Html(include_str!("web_ui.html"))
}

/// 获取状态
async fn get_status(State(state): State<AppState>) -> Json<StatusResponse> {
    let state = state.read().await;
    Json(StatusResponse {
        paused: state.pause_state.paused,
        pause_until: state.pause_state.pause_until,
        remaining_seconds: state.pause_state.remaining_seconds(),
        current_time: Local::now(),
        pause_count: state.pause_state.get_pause_count(),
        max_pauses: 2, // 最大暂停次数
        in_lockscreen_window: state.in_lockscreen_window,
        shutdown_warning: state.shutdown_warning.clone(),
    })
}

/// 暂停
async fn post_pause(
    State(state): State<AppState>,
    Json(req): Json<PauseRequest>,
) -> Result<Json<SuccessResponse>, StatusCode> {
    if req.minutes == 0 || req.minutes > 1440 {
        return Err(StatusCode::BAD_REQUEST);
    }

    let mut state = state.write().await;
    let (pause_count, exceeded) = state.pause(req.minutes).await;

    let message = if exceeded {
        format!(
            "Paused for {} minutes. WARNING: Pause limit exceeded ({}/2), shutdown will be triggered!",
            req.minutes, pause_count
        )
    } else {
        format!("Paused for {} minutes (count: {}/2)", req.minutes, pause_count)
    };

    Ok(Json(SuccessResponse {
        success: true,
        message: Some(message),
        pause_count: Some(pause_count),
        shutdown_triggered: Some(exceeded),
    }))
}

/// 恢复
async fn post_resume(State(state): State<AppState>) -> Json<SuccessResponse> {
    let mut state = state.write().await;
    state.resume(false).await; // false 表示手动恢复

    Json(SuccessResponse {
        success: true,
        message: Some("Resumed".to_string()),
        pause_count: None,
        shutdown_triggered: None,
    })
}

/// 获取任务列表
async fn get_tasks(State(state): State<AppState>) -> Json<TasksResponse> {
    let state = state.read().await;
    Json(TasksResponse {
        tasks: state.config.tasks.clone(),
    })
}

/// 获取任务历史
async fn get_history(State(state): State<AppState>) -> Json<HistoryResponse> {
    let state = state.read().await;
    let history: Vec<_> = state.task_history.iter().cloned().collect();
    Json(HistoryResponse { history })
}

/// 获取操作日志
async fn get_logs(State(state): State<AppState>) -> Json<LogsResponse> {
    let state = state.read().await;
    let logs: Vec<_> = state.operation_logs.iter().cloned().collect();
    Json(LogsResponse { logs })
}

/// 获取即将执行的任务
async fn get_upcoming(State(state): State<AppState>) -> Json<UpcomingTasksResponse> {
    let state = state.read().await;
    let now = Local::now();
    let mut upcoming_tasks = Vec::new();

    // 检查是否有关机警告
    let shutdown_warning = state.shutdown_warning.clone();
    let has_shutdown_warning = shutdown_warning.pending;

    // 遍历所有启用的任务
    for task in &state.config.tasks {
        if !task.enabled {
            continue;
        }

        // 计算该任务的下次执行时间列表
        if let Ok(next_times) = calculate_next_execution_times(task, &now, 20) {
            for next_time in next_times {
                let seconds_until = (next_time - now).num_seconds();
                if seconds_until > 0 {
                    // 检查这个任务是否会触发关机
                    let (will_trigger, warning_msg) = if has_shutdown_warning
                        && (task.task_type == "lockscreen_repeated" || task.task_type == "lockscreen")
                    {
                        let msg = match shutdown_warning.reason.as_deref() {
                            Some("unlock_exceeded") => Some(format!(
                                "⚠️ 解锁次数已达上限 ({}/{}), 此任务将触发关机!",
                                shutdown_warning.current_count, shutdown_warning.max_count
                            )),
                            Some("pause_exceeded") => Some(format!(
                                "⚠️ 暂停次数已超限 ({}/{}), 此任务将触发关机!",
                                shutdown_warning.current_count, shutdown_warning.max_count
                            )),
                            _ => Some("⚠️ 此任务将触发关机!".to_string()),
                        };
                        (Some(true), msg)
                    } else {
                        (None, None)
                    };

                    upcoming_tasks.push(UpcomingTask {
                        task_name: task.name.clone(),
                        task_type: task.task_type.clone(),
                        execute_at: next_time,
                        seconds_until,
                        will_trigger_shutdown: will_trigger,
                        shutdown_warning_message: warning_msg,
                    });
                }
            }
        }
    }

    // 按执行时间排序
    upcoming_tasks.sort_by_key(|t| t.execute_at);

    // 只返回前 8 个
    upcoming_tasks.truncate(8);

    Json(UpcomingTasksResponse {
        upcoming: upcoming_tasks,
        shutdown_warning,
    })
}

/// 计算任务的下次执行时间（返回多个）
fn calculate_next_execution_times(
    task: &Task,
    now: &DateTime<Local>,
    max_count: usize,
) -> Result<Vec<DateTime<Local>>, Box<dyn std::error::Error>> {
    let mut result = Vec::new();

    // 特殊处理整点报时任务
    if task.task_type == "hourly_chime" {
        let mut check_time = now.clone();
        let search_until = *now + Duration::hours(48);

        while check_time < search_until && result.len() < max_count {
            // 计算下一个整点
            let next_hour = if check_time.minute() == 0 && check_time.second() == 0 {
                check_time + Duration::hours(1)
            } else {
                check_time
                    .date_naive()
                    .and_hms_opt((check_time.hour() + 1) % 24, 0, 0)
                    .unwrap()
                    .and_local_timezone(Local)
                    .unwrap()
            };

            // 如果是下一天的整点
            let next_exec = if next_hour <= check_time {
                (check_time.date_naive() + Duration::days(1))
                    .and_hms_opt(next_hour.hour(), 0, 0)
                    .unwrap()
                    .and_local_timezone(Local)
                    .unwrap()
            } else {
                next_hour
            };

            if next_exec > *now && !result.contains(&next_exec) {
                result.push(next_exec);
            }

            check_time = next_exec + Duration::seconds(1);
        }

        result.sort();
        return Ok(result);
    }

    let start_time = NaiveTime::parse_from_str(&task.start_time, "%H:%M")?;
    let end_time = NaiveTime::parse_from_str(&task.end_time, "%H:%M")?;
    let interval_minutes = task.interval_minutes as i64;

    // 从当前时间开始，向前查找可能的执行时间点
    let mut check_time = now.clone();

    // 最多查找未来 48 小时
    let search_until = *now + Duration::hours(48);

    while check_time < search_until && result.len() < max_count {
        let current_naive_time = check_time.time();

        // 检查是否在时间范围内
        let in_range = if start_time > end_time {
            // 跨午夜情况
            current_naive_time >= start_time || current_naive_time < end_time
        } else {
            current_naive_time >= start_time && current_naive_time < end_time
        };

        if in_range {
            // 计算当天该任务的开始时间
            let today_start = check_time
                .date_naive()
                .and_time(start_time)
                .and_local_timezone(Local)
                .unwrap();

            // 如果开始时间在未来
            let reference_start = if today_start > *now {
                today_start
            } else {
                // 如果跨午夜且当前时间在午夜后
                if start_time > end_time && current_naive_time < end_time {
                    // 使用昨天的开始时间
                    (check_time.date_naive() - Duration::days(1))
                        .and_time(start_time)
                        .and_local_timezone(Local)
                        .unwrap()
                } else {
                    today_start
                }
            };

            // 计算从开始时间到当前检查时间的分钟数
            let minutes_since_start = (check_time - reference_start).num_minutes();

            // 找到下一个执行点
            let next_interval = if minutes_since_start < 0 {
                0
            } else {
                ((minutes_since_start / interval_minutes) + 1) * interval_minutes
            };

            let next_exec = reference_start + Duration::minutes(next_interval);

            // 检查这个执行时间是否在有效范围内且在未来
            if next_exec > *now && next_exec < search_until {
                let next_exec_time = next_exec.time();
                let in_range_check = if start_time > end_time {
                    next_exec_time >= start_time || next_exec_time < end_time
                } else {
                    next_exec_time >= start_time && next_exec_time < end_time
                };

                if in_range_check && !result.contains(&next_exec) {
                    result.push(next_exec);
                }
            }
        }

        // 向前推进时间
        check_time = check_time + Duration::minutes(interval_minutes.min(30));
    }

    result.sort();
    Ok(result)
}
