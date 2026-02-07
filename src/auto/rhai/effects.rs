//! 副作用执行器 - 统一管理所有有副作用的操作
//! 根据运行模式决定真实执行还是模拟执行

use super::types::{CollectedEvent, EventType, GlobalState, RunMode, parse_time_to_minutes};
use crate::auto::tts::VolcengineTtsClient;
use chrono::{Datelike, Timelike};
use colored::Colorize;
use std::process::Command;
use std::sync::{Arc, Mutex};

/// 副作用执行器
pub struct SideEffectExecutor {
    state: Arc<Mutex<GlobalState>>,
}

impl SideEffectExecutor {
    pub fn new(state: Arc<Mutex<GlobalState>>) -> Self {
        Self { state }
    }

    /// 获取当前运行模式
    fn run_mode(&self) -> RunMode {
        self.state.lock().unwrap().exec_ctx.mode.clone()
    }

    /// 是否为真实执行模式
    fn is_real(&self) -> bool {
        self.run_mode().is_real()
    }

    /// 语音播报
    pub fn speak(&self, text: &str) {
        self.speak_internal(text, 150);
    }

    /// 语音播报（带自定义停顿）
    pub fn speak_with_pause(&self, text: &str, pause_ms: i64) {
        self.speak_internal(text, pause_ms);
    }

    /// 内部语音播报实现
    fn speak_internal(&self, text: &str, pause_ms: i64) {
        let (api_key, voice, mode) = {
            let st = self.state.lock().unwrap();
            (
                st.tts_api_key.clone(),
                st.tts_voice.clone(),
                st.exec_ctx.mode.clone(),
            )
        };

        match mode {
            RunMode::Real => {
                // 真实模式：生成缓存并播放
                if let (Some(key), Some(v)) = (api_key, voice) {
                    println!("{}", format!("🔊 Speaking: \"{}\"", text).blue());
                    let client = VolcengineTtsClient::new(key);
                    if let Err(e) = client.synthesize_and_play(text, &v) {
                        println!("{}", format!("  ⚠ TTS failed: {}", e).yellow());
                    }
                    if pause_ms > 0 {
                        std::thread::sleep(std::time::Duration::from_millis(pause_ms as u64));
                    }
                } else {
                    println!("{}", "⚠ TTS not configured".yellow());
                }
            }
            RunMode::CacheTts { .. } => {
                // 缓存模式：只生成缓存，不播放
                if let (Some(key), Some(v)) = (api_key, voice) {
                    println!("{}", format!("🔊 Caching: \"{}\"", text).dimmed());
                    let client = VolcengineTtsClient::new(key);
                    if let Err(e) = client.prefetch(text, &v) {
                        println!("{}", format!("  ⚠ Prefetch failed: {}", e).yellow());
                    }
                }
            }
            RunMode::GenerateEvents { hour, minute } => {
                // 事件生成模式：只收集文本
                let time = format!("{:02}:{:02}", hour, minute);
                let mut st = self.state.lock().unwrap();
                st.exec_ctx.collected_events.push(CollectedEvent {
                    time,
                    text: text.to_string(),
                    event_type: EventType::Speak,
                });
            }
        }
    }

    /// 锁定屏幕
    pub fn lock_screen(&self) {
        if !self.is_real() {
            return;
        }
        println!("{}", "🔒 Locking screen...".cyan());
        #[cfg(target_os = "windows")]
        {
            let _ = Command::new("rundll32.exe")
                .args(["user32.dll,LockWorkStation"])
                .status();
        }
        #[cfg(target_os = "linux")]
        {
            let _ = Command::new("loginctl").arg("lock-session").status();
        }
        #[cfg(target_os = "macos")]
        {
            let _ = Command::new("pmset").arg("displaysleepnow").status();
        }
    }

    /// 进入睡眠模式
    pub fn enter_sleep(&self) {
        if !self.is_real() {
            return;
        }
        println!("{}", "😴 Entering sleep mode...".cyan());
        #[cfg(target_os = "windows")]
        {
            let _ = Command::new("rundll32.exe")
                .args(["powrprof.dll,SetSuspendState", "0,1,0"])
                .status();
        }
        #[cfg(target_os = "linux")]
        {
            let _ = Command::new("systemctl").arg("suspend").status();
        }
        #[cfg(target_os = "macos")]
        {
            let _ = Command::new("pmset").arg("sleepnow").status();
        }
    }

    /// 关机
    pub fn shutdown(&self, delay: i64) {
        if !self.is_real() {
            return;
        }
        println!(
            "{}",
            format!("⚠ Shutdown scheduled in {} seconds", delay)
                .red()
                .bold()
        );
        #[cfg(target_os = "windows")]
        {
            let _ = Command::new("shutdown")
                .args(["/s", "/t", &delay.to_string()])
                .status();
        }
        #[cfg(target_os = "linux")]
        {
            let _ = Command::new("shutdown")
                .args(["-h", &format!("+{}", delay / 60)])
                .status();
        }
        #[cfg(target_os = "macos")]
        {
            let _ = Command::new("shutdown")
                .args(["-h", &format!("+{}", delay / 60)])
                .status();
        }
    }

    /// 整点报时
    pub fn chime(&self, hour: i64) {
        let (api_key, voice, mode) = {
            let st = self.state.lock().unwrap();
            (
                st.tts_api_key.clone(),
                st.tts_voice.clone(),
                st.exec_ctx.mode.clone(),
            )
        };

        match mode {
            RunMode::Real => {
                if let (Some(key), Some(v)) = (api_key, voice) {
                    println!("{}", format!("🕐 Chime for {} o'clock", hour).cyan());
                    let client = VolcengineTtsClient::new(key);
                    // 先播放报时音
                    if let Err(e) = client.hourly_chime(hour as u32) {
                        println!("{}", format!("  ⚠ Chime failed: {}", e).yellow());
                    }
                    // 再播放语音
                    let text = format!("现在是{}点整", hour);
                    if let Err(e) = client.synthesize_and_play(&text, &v) {
                        println!("{}", format!("  ⚠ TTS failed: {}", e).yellow());
                    }
                }
            }
            RunMode::CacheTts { .. } => {
                // 缓存报时语音
                if let (Some(key), Some(v)) = (api_key, voice) {
                    let text = format!("现在是{}点整", hour);
                    println!("{}", format!("🕐 Caching chime: \"{}\"", text).dimmed());
                    let client = VolcengineTtsClient::new(key);
                    let _ = client.prefetch(&text, &v);
                }
            }
            RunMode::GenerateEvents { hour: sim_hour, minute: sim_minute } => {
                // 收集报时事件
                let time = format!("{:02}:{:02}", sim_hour, sim_minute);
                let mut st = self.state.lock().unwrap();
                st.exec_ctx.collected_events.push(CollectedEvent {
                    time,
                    text: format!("{}点报时", hour),
                    event_type: EventType::Chime,
                });
            }
        }
    }

    /// 日志
    pub fn log(&self, msg: &str) {
        match self.run_mode() {
            RunMode::Real => {
                println!("{}", format!("📝 {}", msg).white());
            }
            RunMode::CacheTts { .. } | RunMode::GenerateEvents { .. } => {
                // 非真实模式静默
            }
        }
    }

    /// 获取小时（非真实模式返回模拟时间）
    pub fn hour(&self) -> i64 {
        let mode = self.run_mode();
        match mode.sim_time() {
            Some((hour, _)) => hour as i64,
            None => chrono::Local::now().hour() as i64,
        }
    }

    /// 获取分钟（非真实模式返回模拟时间）
    pub fn minute(&self) -> i64 {
        let mode = self.run_mode();
        match mode.sim_time() {
            Some((_, minute)) => minute as i64,
            None => chrono::Local::now().minute() as i64,
        }
    }

    /// 获取秒（非真实模式返回 0）
    pub fn second(&self) -> i64 {
        if self.is_real() {
            chrono::Local::now().second() as i64
        } else {
            0
        }
    }

    /// 获取时间字符串（非真实模式返回模拟时间）
    pub fn time_str(&self) -> String {
        let mode = self.run_mode();
        match mode.sim_time() {
            Some((hour, minute)) => format!("{:02}:{:02}", hour, minute),
            None => chrono::Local::now().format("%H:%M").to_string(),
        }
    }

    /// 检查是否在时间范围内（非真实模式使用模拟时间）
    pub fn in_time_range(&self, start: &str, end: &str) -> bool {
        let mode = self.run_mode();
        let (hour, minute) = match mode.sim_time() {
            Some((h, m)) => (h, m),
            None => {
                let now = chrono::Local::now();
                (now.hour(), now.minute())
            }
        };

        let now_mins = (hour * 60 + minute) as i64;
        let s = parse_time_to_minutes(start);
        let e = parse_time_to_minutes(end);

        if s > e {
            // 跨午夜
            now_mins >= s || now_mins < e
        } else {
            now_mins >= s && now_mins < e
        }
    }

    /// 获取星期几（1=周一, 7=周日）
    /// 注：模拟模式下仍返回真实日期的星期，因为模拟只模拟时间不模拟日期
    pub fn weekday(&self) -> i64 {
        chrono::Local::now().weekday().num_days_from_monday() as i64 + 1
    }

    /// 是否为周末
    pub fn is_weekend(&self) -> bool {
        chrono::Local::now().weekday().num_days_from_monday() >= 5
    }

    /// 是否为工作日
    pub fn is_workday(&self) -> bool {
        chrono::Local::now().weekday().num_days_from_monday() < 5
    }

    /// 获取日期字符串
    pub fn date_str(&self) -> String {
        chrono::Local::now().format("%Y-%m-%d").to_string()
    }

    /// 获取星期几中文名
    pub fn weekday_name(&self) -> String {
        let wd = self.weekday() as u32;
        crate::auto::calendar::weekday_name(wd).to_string()
    }

    /// 检查屏幕是否已锁定（非 Real 模式返回 false）
    pub fn screen_locked(&self) -> bool {
        if !self.is_real() {
            false
        } else {
            crate::auto::screen::is_screen_locked()
        }
    }

    /// 距离春节天数
    pub fn days_until_spring_festival(&self) -> i64 {
        crate::auto::calendar::days_until_spring_festival()
    }

    /// 今日节日
    pub fn get_today_festival(&self) -> String {
        crate::auto::calendar::get_today_festival().unwrap_or_default()
    }

    /// 今日节气
    pub fn get_today_solar_term(&self) -> String {
        crate::auto::calendar::get_today_solar_term().unwrap_or_default()
    }

    /// 今日节日或节气
    pub fn get_today_special(&self) -> String {
        crate::auto::calendar::get_today_special().unwrap_or_default()
    }

    /// 修改密码为锁定密码
    pub fn change_password(&self) {
        if !self.is_real() {
            return;
        }
        if let Err(e) = crate::auto::password::PasswordManager::change_password() {
            println!("{}", format!("✗ change_password 失败: {}", e).red().bold());
        }
    }

    /// 恢复原密码
    pub fn restore_password(&self) {
        if !self.is_real() {
            return;
        }
        if let Err(e) = crate::auto::password::PasswordManager::restore_password() {
            println!("{}", format!("✗ restore_password 失败: {}", e).red().bold());
        }
    }

}
