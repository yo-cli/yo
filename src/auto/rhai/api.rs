//! Rhai API 注册

use super::types::GlobalState;
use crate::auto::config::GlobalConfig;
use crate::auto::screen::is_screen_locked;
use crate::auto::tts::VolcengineTtsClient;
use chrono::{Datelike, Local, NaiveTime, Timelike};
use colored::Colorize;
use rhai::Engine;
use std::process::Command;
use std::sync::{Arc, Mutex};

/// 注册所有 API 到引擎
pub fn register_all(engine: &mut Engine, state: Arc<Mutex<GlobalState>>, config: GlobalConfig) {
    register_time_apis(engine);
    register_counter_apis(engine, state.clone());
    register_flag_apis(engine, state.clone());
    register_action_apis(engine, state.clone());
    register_util_apis(engine, state);
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

fn register_counter_apis(engine: &mut Engine, state: Arc<Mutex<GlobalState>>) {
    let s = state.clone();
    engine.register_fn("inc_counter", move |name: &str| -> i64 {
        let mut st = s.lock().unwrap();
        let counter = st.counters.entry(name.to_string()).or_insert(0);
        *counter += 1;
        *counter
    });

    let s = state.clone();
    engine.register_fn("get_counter", move |name: &str| -> i64 {
        s.lock().unwrap().counters.get(name).copied().unwrap_or(0)
    });

    let s = state.clone();
    engine.register_fn("set_counter", move |name: &str, value: i64| {
        s.lock().unwrap().counters.insert(name.to_string(), value);
    });

    let s = state;
    engine.register_fn("reset_counter", move |name: &str| {
        s.lock().unwrap().counters.remove(name);
    });
}

fn register_flag_apis(engine: &mut Engine, state: Arc<Mutex<GlobalState>>) {
    let s = state.clone();
    engine.register_fn("set_flag", move |name: &str, value: bool| {
        s.lock().unwrap().flags.insert(name.to_string(), value);
    });

    let s = state;
    engine.register_fn("get_flag", move |name: &str| -> bool {
        s.lock().unwrap().flags.get(name).copied().unwrap_or(false)
    });
}

fn register_action_apis(engine: &mut Engine, state: Arc<Mutex<GlobalState>>) {
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

fn register_util_apis(engine: &mut Engine, state: Arc<Mutex<GlobalState>>) {
    engine.register_fn("reset_if_new_day", move |prefix: &str| -> bool {
        let today = Local::now().format("%Y-%m-%d").to_string();
        let last_day_key = format!("{}_last_day", prefix);
        let today_num = Local::now().ordinal() as i64;

        let mut s = state.lock().unwrap();
        let last_day = s.counters.get(&last_day_key).copied().unwrap_or(0);

        if last_day != today_num {
            s.counters.insert(last_day_key, today_num);
            s.counters.remove(&format!("{}_unlock", prefix));
            s.flags.remove(&format!("{}_shutdown", prefix));
            println!("{}", format!("🔄 Reset counters for {} (new day: {})", prefix, today).cyan());
            true
        } else {
            false
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
    engine.register_fn("has_env", move |name: &str| -> bool {
        config.get(name).is_some()
    });
}
