//! Rhai API 注册

use super::types::GlobalState;
use crate::auto::config::GlobalConfig;
use crate::auto::screen::is_screen_locked;
use crate::auto::tts::VolcengineTtsClient;
use chrono::{Datelike, Local, NaiveTime, Timelike};
use colored::Colorize;
use rhai::{Engine, Scope};
use std::process::Command;
use std::sync::{Arc, Mutex};

/// 模拟收集的事件
#[derive(Debug, Clone)]
pub struct SimulatedEvent {
    pub time: String,
    pub text: String,
}

/// 模拟上下文
#[derive(Default)]
pub struct SimulationContext {
    pub hour: i64,
    pub minute: i64,
    pub events: Vec<SimulatedEvent>,
}

/// 日历事件（用于生成）
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

/// 获取事件存储路径
fn get_events_file() -> std::path::PathBuf {
    let home = std::env::var("USERPROFILE")
        .or_else(|_| std::env::var("HOME"))
        .unwrap_or_else(|_| ".".to_string());
    std::path::PathBuf::from(home).join(".yo").join("events.json")
}

/// 生成确定性事件 ID
fn generate_event_id(script_name: &str, time: &str, text: &str) -> String {
    let mut hash: i32 = 0;
    for c in text.chars() {
        hash = hash.wrapping_mul(31).wrapping_add(c as i32);
    }
    format!("{}_{}_{}",
        script_name,
        time.replace(':', ""),
        format!("{:x}", hash.unsigned_abs())
    )
}

/// 计算结束时间
fn calculate_end_time(start: &str, duration_mins: u32) -> String {
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

/// 注册所有 API 到引擎
pub fn register_all(engine: &mut Engine, state: Arc<Mutex<GlobalState>>, config: GlobalConfig) {
    register_time_apis(engine);
    register_action_apis(engine, state);
    register_env_apis(engine, config);
}

fn register_time_apis(engine: &mut Engine) {
    engine.register_fn("hour", || Local::now().hour() as i64);
    engine.register_fn("minute", || Local::now().minute() as i64);
    engine.register_fn("second", || Local::now().second() as i64);
    engine.register_fn("weekday", || {
        Local::now().weekday().num_days_from_monday() as i64 + 1
    });
    engine.register_fn("is_weekend", || {
        Local::now().weekday().num_days_from_monday() >= 5
    });
    engine.register_fn("is_workday", || {
        Local::now().weekday().num_days_from_monday() < 5
    });
    engine.register_fn("time_str", || Local::now().format("%H:%M").to_string());
    engine.register_fn("date_str", || Local::now().format("%Y-%m-%d").to_string());

    engine.register_fn("in_time_range", |start: &str, end: &str| -> bool {
        let now = Local::now().time();
        let start_time = NaiveTime::parse_from_str(start, "%H:%M").ok();
        let end_time = NaiveTime::parse_from_str(end, "%H:%M").ok();
        match (start_time, end_time) {
            (Some(s), Some(e)) if s > e => now >= s || now < e,
            (Some(s), Some(e)) => now >= s && now < e,
            _ => false,
        }
    });
}

fn register_action_apis(engine: &mut Engine, state: Arc<Mutex<GlobalState>>) {
    // generate_script_events - 生成脚本的日历事件
    let s = state.clone();
    engine.register_fn("generate_script_events", move |script_name: &str| {
        let current = {
            let gs = s.lock().unwrap();
            gs.current_script.clone()
        };

        if let Some(script) = current {
            // 使用传入的脚本名（用于 ID 生成）
            let name = if script_name.is_empty() { &script.name } else { script_name };

            // 运行模拟
            let simulated = simulate_script(&script.ast, script.time_range, script.interval_minutes);

            if simulated.is_empty() {
                println!("{}", format!("📅 [{}] No events to generate", name).yellow());
                return;
            }

            // 加载现有事件
            let events_file = get_events_file();
            let mut events: Vec<CalendarEvent> = if events_file.exists() {
                std::fs::read_to_string(&events_file)
                    .ok()
                    .and_then(|c| serde_json::from_str(&c).ok())
                    .unwrap_or_default()
            } else {
                Vec::new()
            };

            let existing_ids: std::collections::HashSet<String> = events.iter().map(|e| e.id.clone()).collect();

            // 预定义颜色
            let colors = ["#4CAF50", "#2196F3", "#FF9800", "#9C27B0", "#F44336", "#00BCD4", "#795548", "#607D8B"];
            let color = colors[events.len() % colors.len()];

            let mut created = 0;
            for evt in simulated {
                // 生成确定性 ID
                let id = generate_event_id(name, &evt.time, &evt.text);

                // 幂等检查
                if existing_ids.contains(&id) {
                    continue;
                }

                // 计算结束时间
                let end_time = calculate_end_time(&evt.time, 5);

                events.push(CalendarEvent {
                    id,
                    name: evt.text,
                    start_time: evt.time,
                    end_time,
                    color: color.to_string(),
                    weekdays: None,
                    enabled: true,
                });
                created += 1;
            }

            // 保存
            if created > 0 {
                if let Ok(content) = serde_json::to_string_pretty(&events) {
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

    // speak
    let s = state.clone();
    engine.register_fn("speak", move |text: &str| {
        let st = s.lock().unwrap();
        if let (Some(key), Some(voice)) = (&st.tts_api_key, &st.tts_voice) {
            println!("{}", format!("🔊 Speaking: \"{}\"", text).blue());
            let client = VolcengineTtsClient::new(key.clone());
            if let Err(e) = client.synthesize_and_play(text, voice) {
                println!("{}", format!("⚠ TTS failed: {}", e).yellow());
            }
        } else {
            println!("{}", "⚠ TTS not configured".yellow());
        }
    });

    // screen_locked - 检查屏幕是否已锁定
    engine.register_fn("screen_locked", || -> bool {
        is_screen_locked()
    });

    // lock_screen
    engine.register_fn("lock_screen", || {
        println!("{}", "🔒 Locking screen...".cyan());
        #[cfg(target_os = "windows")]
        { let _ = Command::new("rundll32.exe").args(["user32.dll,LockWorkStation"]).status(); }
        #[cfg(target_os = "linux")]
        { let _ = Command::new("loginctl").arg("lock-session").status(); }
        #[cfg(target_os = "macos")]
        { let _ = Command::new("pmset").arg("displaysleepnow").status(); }
    });

    // shutdown
    engine.register_fn("shutdown", |delay: i64| {
        println!("{}", format!("⚠️ Shutdown in {} seconds", delay).red().bold());
        #[cfg(target_os = "windows")]
        { let _ = Command::new("shutdown").args(["/s", "/t", &delay.to_string()]).spawn(); }
        #[cfg(target_os = "linux")]
        { let _ = Command::new("shutdown").args(["-h", &format!("+{}", delay / 60)]).spawn(); }
        #[cfg(target_os = "macos")]
        { let _ = Command::new("sudo").args(["shutdown", "-h", &format!("+{}", delay / 60)]).spawn(); }
    });

    // chime
    let s = state.clone();
    engine.register_fn("chime", move |hour: i64| {
        let st = s.lock().unwrap();
        if let Some(key) = &st.tts_api_key {
            println!("{}", format!("🕐 Chime: {} o'clock", hour).cyan());
            let client = VolcengineTtsClient::new(key.clone());
            if let Err(e) = client.hourly_chime(hour as u32) {
                println!("{}", format!("⚠ Chime failed: {}", e).yellow());
            }
        }
    });

    // log
    engine.register_fn("log", |msg: &str| {
        println!("{}", format!("📝 {}", msg).white());
    });

    // configure_tts
    let s = state;
    engine.register_fn("configure_tts", move |api_key: &str, voice: &str| {
        let mut st = s.lock().unwrap();
        st.tts_api_key = Some(api_key.to_string());
        st.tts_voice = Some(voice.to_string());
        println!("{}", format!("🔊 TTS configured: voice={}", voice).cyan());
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
    engine.register_fn("has_env", move |name: &str| -> bool {
        config.get(name).is_some()
    });
}

/// 创建模拟引擎，用于收集脚本会触发的事件
pub fn create_simulation_engine(ctx: Arc<Mutex<SimulationContext>>) -> Engine {
    let mut engine = Engine::new();

    // 模拟时间 API
    let c = ctx.clone();
    engine.register_fn("hour", move || -> i64 {
        c.lock().unwrap().hour
    });

    let c = ctx.clone();
    engine.register_fn("minute", move || -> i64 {
        c.lock().unwrap().minute
    });

    engine.register_fn("second", || -> i64 { 0 });

    engine.register_fn("weekday", || -> i64 {
        Local::now().weekday().num_days_from_monday() as i64 + 1
    });

    engine.register_fn("is_weekend", || -> bool {
        Local::now().weekday().num_days_from_monday() >= 5
    });

    engine.register_fn("is_workday", || -> bool {
        Local::now().weekday().num_days_from_monday() < 5
    });

    let c = ctx.clone();
    engine.register_fn("time_str", move || -> String {
        let ctx = c.lock().unwrap();
        format!("{:02}:{:02}", ctx.hour, ctx.minute)
    });

    engine.register_fn("date_str", || -> String {
        Local::now().format("%Y-%m-%d").to_string()
    });

    let c = ctx.clone();
    engine.register_fn("in_time_range", move |start: &str, end: &str| -> bool {
        let ctx = c.lock().unwrap();
        let now_mins = ctx.hour * 60 + ctx.minute;
        let parse = |s: &str| -> Option<i64> {
            let parts: Vec<&str> = s.split(':').collect();
            if parts.len() >= 2 {
                Some(parts[0].parse::<i64>().ok()? * 60 + parts[1].parse::<i64>().ok()?)
            } else {
                None
            }
        };
        match (parse(start), parse(end)) {
            (Some(s), Some(e)) if s > e => now_mins >= s || now_mins < e,
            (Some(s), Some(e)) => now_mins >= s && now_mins < e,
            _ => false,
        }
    });

    // 模拟 speak - 收集事件
    let c = ctx.clone();
    engine.register_fn("speak", move |text: &str| {
        let mut ctx = c.lock().unwrap();
        let time = format!("{:02}:{:02}", ctx.hour, ctx.minute);
        ctx.events.push(SimulatedEvent {
            time,
            text: text.to_string(),
        });
    });

    // 模拟 chime - 收集事件
    let c = ctx.clone();
    engine.register_fn("chime", move |hour: i64| {
        let mut ctx = c.lock().unwrap();
        let time = format!("{:02}:00", hour);
        ctx.events.push(SimulatedEvent {
            time,
            text: format!("{}点报时", hour),
        });
    });

    // 空操作 - 模拟时不执行
    engine.register_fn("screen_locked", || -> bool { false });
    engine.register_fn("lock_screen", || {});
    engine.register_fn("shutdown", |_delay: i64| {});
    engine.register_fn("log", |_msg: &str| {});
    engine.register_fn("configure_tts", |_key: &str, _voice: &str| {});
    engine.register_fn("get_env", |_name: &str| -> String { String::new() });
    engine.register_fn("has_env", |_name: &str| -> bool { false });

    engine
}

/// 模拟执行脚本并收集事件
pub fn simulate_script(
    ast: &rhai::AST,
    time_range: Option<(String, String)>,
    interval_minutes: u32,
) -> Vec<SimulatedEvent> {
    let ctx = Arc::new(Mutex::new(SimulationContext::default()));
    let engine = create_simulation_engine(ctx.clone());

    // 确定时间范围
    let (start_mins, end_mins) = if let Some((start, end)) = time_range {
        let parse = |s: &str| -> i64 {
            let parts: Vec<&str> = s.split(':').collect();
            if parts.len() >= 2 {
                parts[0].parse::<i64>().unwrap_or(0) * 60 + parts[1].parse::<i64>().unwrap_or(0)
            } else {
                0
            }
        };
        (parse(&start), parse(&end))
    } else {
        (0, 24 * 60)
    };

    // 处理跨午夜
    let ranges: Vec<(i64, i64)> = if end_mins <= start_mins {
        vec![(start_mins, 24 * 60), (0, end_mins)]
    } else {
        vec![(start_mins, end_mins)]
    };

    // 遍历时间范围
    for (range_start, range_end) in ranges {
        let mut current = range_start;
        while current < range_end {
            let hour = current / 60;
            let minute = current % 60;

            // 设置模拟时间
            {
                let mut c = ctx.lock().unwrap();
                c.hour = hour;
                c.minute = minute;
            }

            // 运行脚本获取变量
            let mut scope = Scope::new();
            let _ = engine.run_ast_with_scope(&mut scope, ast);

            // 调用 on_tick
            let _ = engine.call_fn::<()>(&mut scope, ast, "on_tick", ());

            current += interval_minutes as i64;
        }
    }

    // 返回收集的事件
    let c = ctx.lock().unwrap();
    c.events.clone()
}
