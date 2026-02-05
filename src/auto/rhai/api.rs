//! Rhai API 注册

use super::effects::SideEffectExecutor;
use super::engine::generate_events;
use super::types::{calculate_end_time, get_home_dir, CalendarEvent, GlobalState};
use crate::auto::config::GlobalConfig;
use crate::auto::weather::QWeatherClient;
use colored::Colorize;
use rhai::Engine;
use std::sync::{Arc, Mutex};

/// 获取事件存储路径
fn get_events_file() -> std::path::PathBuf {
    std::path::PathBuf::from(get_home_dir()).join(".yo").join("events.json")
}

/// 生成确定性事件 ID
fn generate_event_id(script_name: &str, time: &str, text: &str) -> String {
    let mut hash: i32 = 0;
    for c in text.chars() {
        hash = hash.wrapping_mul(31).wrapping_add(c as i32);
    }
    format!("{}_{}_{:x}",
        script_name,
        time.replace(':', ""),
        hash.unsigned_abs()
    )
}

/// 注册所有 API 到引擎
pub fn register_all(engine: &mut Engine, state: Arc<Mutex<GlobalState>>, config: GlobalConfig) {
    let executor = Arc::new(SideEffectExecutor::new(state.clone()));
    register_time_apis(engine, executor.clone());
    register_action_apis(engine, state, executor, config.clone());
    register_env_apis(engine, config);
}

fn register_time_apis(engine: &mut Engine, executor: Arc<SideEffectExecutor>) {
    // 时间相关 API - 通过 executor 支持运行模式
    let e = executor.clone();
    engine.register_fn("hour", move || e.hour());

    let e = executor.clone();
    engine.register_fn("minute", move || e.minute());

    let e = executor.clone();
    engine.register_fn("second", move || e.second());

    let e = executor.clone();
    engine.register_fn("weekday", move || e.weekday());

    let e = executor.clone();
    engine.register_fn("is_weekend", move || e.is_weekend());

    let e = executor.clone();
    engine.register_fn("is_workday", move || e.is_workday());

    let e = executor.clone();
    engine.register_fn("time_str", move || e.time_str());

    let e = executor.clone();
    engine.register_fn("date_str", move || e.date_str());

    let e = executor.clone();
    engine.register_fn("in_time_range", move |start: &str, end: &str| -> bool {
        e.in_time_range(start, end)
    });

    // weekday_name - 星期几中文名
    let e = executor.clone();
    engine.register_fn("weekday_name", move || -> String {
        e.weekday_name()
    });

    // days_until_spring_festival - 距离春节天数
    let e = executor.clone();
    engine.register_fn("days_until_spring_festival", move || -> i64 {
        e.days_until_spring_festival()
    });

    // get_today_festival - 今日节日
    let e = executor.clone();
    engine.register_fn("get_today_festival", move || -> String {
        e.get_today_festival()
    });

    // get_today_solar_term - 今日节气
    let e = executor.clone();
    engine.register_fn("get_today_solar_term", move || -> String {
        e.get_today_solar_term()
    });

    // get_today_special - 今日节日或节气
    let e = executor.clone();
    engine.register_fn("get_today_special", move || -> String {
        e.get_today_special()
    });
}

fn register_action_apis(
    engine: &mut Engine,
    state: Arc<Mutex<GlobalState>>,
    executor: Arc<SideEffectExecutor>,
    config: GlobalConfig,
) {
    // generate_script_events - 生成脚本的日历事件
    let s = state.clone();
    let cfg = config.clone();
    engine.register_fn("generate_script_events", move |script_name: &str| {
        let current = {
            let gs = s.lock().unwrap();
            gs.current_script.clone()
        };

        if let Some(script) = current {
            // 使用传入的脚本名（用于 ID 生成）
            let name = if script_name.is_empty() { script.name.clone() } else { script_name.to_string() };

            // 使用统一的事件生成机制
            let events = generate_events(
                &script.ast,
                script.time_range.clone(),
                script.interval_minutes,
                cfg.clone(),
            );

            if events.is_empty() {
                println!("{}", format!("📅 [{}] No events to generate", name).yellow());
                return;
            }

            // 加载现有事件
            let events_file = get_events_file();
            let mut calendar_events: Vec<CalendarEvent> = if events_file.exists() {
                std::fs::read_to_string(&events_file)
                    .ok()
                    .and_then(|c| serde_json::from_str(&c).ok())
                    .unwrap_or_default()
            } else {
                Vec::new()
            };

            let existing_ids: std::collections::HashSet<String> = calendar_events.iter().map(|e| e.id.clone()).collect();

            // 预定义颜色
            let colors = ["#4CAF50", "#2196F3", "#FF9800", "#9C27B0", "#F44336", "#00BCD4", "#795548", "#607D8B"];
            let color = colors[calendar_events.len() % colors.len()];

            // 获取脚本的星期限制
            let script_weekdays = script.weekdays.clone();

            let mut created = 0;
            for evt in events {
                // 生成确定性 ID
                let id = generate_event_id(&name, &evt.time, &evt.text);

                // 幂等检查
                if existing_ids.contains(&id) {
                    continue;
                }

                // 计算结束时间
                let end_time = calculate_end_time(&evt.time, 5);

                calendar_events.push(CalendarEvent {
                    id,
                    name: evt.text,
                    start_time: evt.time,
                    end_time,
                    color: color.to_string(),
                    weekdays: script_weekdays.clone(),
                    enabled: true,
                });
                created += 1;
            }

            // 保存
            if created > 0 {
                if let Ok(content) = serde_json::to_string_pretty(&calendar_events) {
                    let _ = std::fs::write(&events_file, content);
                }
                println!("{}", format!("📅 [{}] Generated {} events", name, created).green());
            } else {
                println!("{}", format!("📅 [{}] All events already exist", name).yellow());
            }
        } else {
            println!("{}", "⚠ generate_script_events: no current script context".yellow());
        }
    });

    // speak(text) - 通过 executor 支持运行模式
    let e = executor.clone();
    engine.register_fn("speak", move |text: &str| {
        e.speak(text);
    });

    // speak(text, pause_ms) - 自定义停顿时间
    let e = executor.clone();
    engine.register_fn("speak", move |text: &str, pause_ms: i64| {
        e.speak_with_pause(text, pause_ms);
    });

    // screen_locked - 检查屏幕是否已锁定（非真实模式返回 false）
    let e = executor.clone();
    engine.register_fn("screen_locked", move || -> bool {
        e.screen_locked()
    });

    // lock_screen - 通过 executor 支持运行模式
    let e = executor.clone();
    engine.register_fn("lock_screen", move || {
        e.lock_screen();
    });

    // enter_sleep - 通过 executor 支持运行模式
    let e = executor.clone();
    engine.register_fn("enter_sleep", move || {
        e.enter_sleep();
    });

    // shutdown - 通过 executor 支持运行模式
    let e = executor.clone();
    engine.register_fn("shutdown", move |delay: i64| {
        e.shutdown(delay);
    });

    // chime - 通过 executor 支持运行模式
    let e = executor.clone();
    engine.register_fn("chime", move |hour: i64| {
        e.chime(hour);
    });

    // log - 通过 executor 支持运行模式
    let e = executor.clone();
    engine.register_fn("log", move |msg: &str| {
        e.log(msg);
    });

    // configure_tts
    let s = state;
    engine.register_fn("configure_tts", move |api_key: &str, voice: &str| {
        let mut st = s.lock().unwrap();
        let is_real = st.exec_ctx.mode.is_real();
        st.tts_api_key = Some(api_key.to_string());
        st.tts_voice = Some(voice.to_string());
        if is_real {
            println!("{}", format!("🔊 TTS configured: voice={}", voice).cyan());
        }
    });
}

fn register_env_apis(engine: &mut Engine, config: GlobalConfig) {
    let config = Arc::new(config);

    // get_env - 获取环境变量
    let c = config.clone();
    engine.register_fn("get_env", move |name: &str| -> String {
        c.get(name).cloned().unwrap_or_default()
    });

    // has_env - 检查环境变量是否存在
    let c = config.clone();
    engine.register_fn("has_env", move |name: &str| -> bool {
        c.get(name).is_some()
    });

    // get_weather - 获取天气信息
    let c = config.clone();
    engine.register_fn("get_weather", move |location: &str| -> rhai::Map {
        let mut result = rhai::Map::new();

        let credential_id = match c.get("QWEATHER_CREDENTIAL_ID") {
            Some(id) => id.clone(),
            None => {
                println!("{}", "⚠ 未配置 QWEATHER_CREDENTIAL_ID".yellow());
                result.insert("error".into(), "未配置 QWEATHER_CREDENTIAL_ID".into());
                return result;
            }
        };

        let project_id = match c.get("QWEATHER_PROJECT_ID") {
            Some(id) => id.clone(),
            None => {
                println!("{}", "⚠ 未配置 QWEATHER_PROJECT_ID".yellow());
                result.insert("error".into(), "未配置 QWEATHER_PROJECT_ID".into());
                return result;
            }
        };

        let private_key = match c.get("QWEATHER_API_KEY") {
            Some(key) => key.clone(),
            None => {
                println!("{}", "⚠ 未配置 QWEATHER_API_KEY (私钥)".yellow());
                result.insert("error".into(), "未配置 QWEATHER_API_KEY".into());
                return result;
            }
        };

        let client = QWeatherClient::new(credential_id, project_id, private_key);
        match client.get_weather(location) {
            Ok(info) => {
                result.insert("weather".into(), info.weather.into());
                result.insert("temp".into(), (info.temp as i64).into());
                result.insert("feels_like".into(), (info.feels_like as i64).into());
                result.insert("humidity".into(), (info.humidity as i64).into());
                result.insert("wind_dir".into(), info.wind_dir.into());
                result.insert("wind_scale".into(), info.wind_scale.into());
            }
            Err(e) => {
                println!("{}", format!("⚠ 获取天气失败: {}", e).yellow());
                result.insert("error".into(), e.into());
            }
        }

        result
    });
}
