//! Rhai 规则调度器

use super::engine::RhaiEngine;
use super::index::TimeIndex;
use super::types::{Rule, Trigger};
use chrono::{DateTime, Datelike, Local, NaiveTime, Timelike};
use colored::Colorize;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

pub struct RhaiScheduler {
    engine: Arc<RhaiEngine>,
    rules: Vec<Rule>,
    index: TimeIndex,
    last_tick_minute: Option<u32>,
    paused_until: Option<DateTime<Local>>,
}

impl RhaiScheduler {
    pub fn new() -> Result<Self, String> {
        let engine = Arc::new(RhaiEngine::new());
        println!("{}", "📂 Loading rules...".cyan().bold());
        let rules = engine.load_rules()?;
        let index = TimeIndex::build(&rules);

        let tick_count: usize = index.tick_rules.values().map(|v| v.len()).sum();
        println!("{}", format!("📊 Index: {} tick, {} unlock, {} lock",
            tick_count, index.unlock_rules.len(), index.lock_rules.len()).blue());

        // 初始化每个规则的时间范围状态
        {
            let state = engine.get_state();
            let mut gs = state.lock().unwrap();
            for rule in &rules {
                let in_range = Self::check_in_time_range(rule);
                gs.script_in_range.insert(rule.name.clone(), in_range);
            }
        }

        Ok(Self { engine, rules, index, last_tick_minute: None, paused_until: None })
    }

    /// 启动时调用所有规则的 on_mount
    pub fn call_on_mount_all(&self) {
        println!("{}", "🚀 Calling on_mount for all rules...".cyan());
        for rule in &self.rules {
            if let Err(e) = self.engine.call_on_mount(rule) {
                if !e.contains("not found") {
                    println!("{}", format!("  ⚠ [{}] on_mount error: {}", rule.name, e).yellow());
                }
            }
        }
    }

    /// 检查规则是否在时间范围内
    fn check_in_time_range(rule: &Rule) -> bool {
        if let Some((ref start, ref end)) = rule.trigger.time_range {
            Self::in_time_range(start, end)
        } else {
            true // 无时间范围限制，始终在范围内
        }
    }

    /// 检查时间范围转换，调用 on_destroy
    fn check_time_range_transitions(&self) {
        let state = self.engine.get_state();

        for rule in &self.rules {
            let currently_in_range = Self::check_in_time_range(rule);

            let was_in_range = {
                let gs = state.lock().unwrap();
                gs.script_in_range.get(&rule.name).copied().unwrap_or(false)
            };

            // 从范围内 -> 范围外，调用 on_destroy
            if was_in_range && !currently_in_range {
                println!("{}", format!("🔚 [{}] Leaving time range, calling on_destroy", rule.name).yellow());
                if let Err(e) = self.engine.call_on_destroy(rule) {
                    if !e.contains("not found") {
                        println!("{}", format!("  ⚠ on_destroy error: {}", e).yellow());
                    }
                }
            }

            // 更新状态
            {
                let mut gs = state.lock().unwrap();
                gs.script_in_range.insert(rule.name.clone(), currently_in_range);
            }
        }
    }

    pub fn reload(&mut self) -> Result<(), String> {
        println!("{}", "🔄 Reloading rules...".cyan());
        self.rules = self.engine.load_rules()?;
        self.index = TimeIndex::build(&self.rules);
        Ok(())
    }

    /// 获取规则数量
    pub fn rules_count(&self) -> usize {
        self.rules.len()
    }

    /// 获取规则列表
    pub fn get_rules(&self) -> &Vec<Rule> {
        &self.rules
    }

    /// 获取全局状态
    pub fn get_state(&self) -> Arc<Mutex<super::types::GlobalState>> {
        self.engine.get_state()
    }

    /// 暂停所有规则指定分钟数
    pub fn pause(&mut self, minutes: u32) {
        let until = Local::now() + chrono::Duration::minutes(minutes as i64);
        println!("{}", format!("⏸ Scheduler paused for {} minutes (until {})", minutes, until.format("%H:%M:%S")).yellow().bold());
        self.paused_until = Some(until);
    }

    /// 取消暂停
    pub fn resume(&mut self) {
        if self.paused_until.is_some() {
            println!("{}", "▶ Scheduler resumed".green().bold());
            self.paused_until = None;
        }
    }

    /// 是否处于暂停状态
    pub fn is_paused(&mut self) -> bool {
        if let Some(until) = self.paused_until {
            if Local::now() >= until {
                println!("{}", "▶ Pause expired, scheduler resumed".green().bold());
                self.paused_until = None;
                false
            } else {
                true
            }
        } else {
            false
        }
    }

    /// 暂停剩余秒数，None 表示未暂停
    pub fn pause_remaining_secs(&self) -> Option<i64> {
        self.paused_until.map(|until| {
            let remaining = (until - Local::now()).num_seconds();
            if remaining > 0 { remaining } else { 0 }
        })
    }

    pub fn on_tick(&mut self) {
        if self.is_paused() { return; }

        let now = Local::now();
        let (hour, minute) = (now.hour(), now.minute());

        if self.last_tick_minute == Some(minute) { return; }
        self.last_tick_minute = Some(minute);

        // 检查时间范围转换
        self.check_time_range_transitions();

        let indices = match self.index.tick_rules.get(&hour) {
            Some(i) => i.clone(),
            None => return,
        };

        for idx in indices {
            if let Some(rule) = self.rules.get(idx) {
                if self.should_execute(rule) {
                    println!("{}", format!("⏰ [{}] Executing tick", rule.name).yellow());
                    if let Err(e) = self.engine.call_on_tick(rule) {
                        println!("{}", format!("  ⚠ Error: {}", e).yellow());
                    }
                }
            }
        }
    }

    fn should_execute(&self, rule: &Rule) -> bool {
        let trigger = &rule.trigger;
        if !trigger.enabled { return false; }

        if let Some(ref weekdays) = trigger.weekdays {
            let today = Local::now().weekday().num_days_from_monday() + 1;
            if !weekdays.contains(&today) { return false; }
        }

        if let Some((ref start, ref end)) = trigger.time_range {
            if !Self::in_time_range(start, end) { return false; }
        }

        if let Some(interval) = trigger.interval_minutes {
            if Self::minutes_since_start(trigger) % interval != 0 { return false; }
        }

        true
    }

    fn in_time_range(start: &str, end: &str) -> bool {
        let now = Local::now().time();
        let s = NaiveTime::parse_from_str(start, "%H:%M").ok();
        let e = NaiveTime::parse_from_str(end, "%H:%M").ok();
        match (s, e) {
            (Some(st), Some(et)) if st > et => now >= st || now < et,
            (Some(st), Some(et)) => now >= st && now < et,
            _ => false,
        }
    }

    fn minutes_since_start(trigger: &Trigger) -> u32 {
        let now = Local::now().time();
        if let Some((ref start, _)) = trigger.time_range {
            if let Ok(st) = NaiveTime::parse_from_str(start, "%H:%M") {
                let now_mins = now.hour() * 60 + now.minute();
                let start_mins = st.hour() * 60 + st.minute();
                return if now_mins >= start_mins {
                    now_mins - start_mins
                } else {
                    24 * 60 - start_mins + now_mins
                };
            }
        }
        now.minute()
    }

    pub fn on_unlock(&mut self) {
        if self.is_paused() { return; }
        for idx in &self.index.unlock_rules {
            if let Some(rule) = self.rules.get(*idx) {
                if !self.should_execute_event(rule) { continue; }
                println!("{}", format!("🔓 [{}] Processing unlock", rule.name).yellow());
                if let Err(e) = self.engine.call_on_unlock(rule) {
                    println!("{}", format!("  ⚠ Error: {}", e).yellow());
                }
            }
        }
    }

    pub fn on_lock(&mut self) {
        if self.is_paused() { return; }
        for idx in &self.index.lock_rules {
            if let Some(rule) = self.rules.get(*idx) {
                if let Err(e) = self.engine.call_on_lock(rule) {
                    println!("{}", format!("  ⚠ Error: {}", e).yellow());
                }
            }
        }
    }

    fn should_execute_event(&self, rule: &Rule) -> bool {
        if let Some((ref s, ref e)) = rule.trigger.time_range {
            if !Self::in_time_range(s, e) { return false; }
        }
        if let Some(ref weekdays) = rule.trigger.weekdays {
            let today = Local::now().weekday().num_days_from_monday() + 1;
            if !weekdays.contains(&today) { return false; }
        }
        true
    }

    pub fn run(&mut self) -> Result<(), String> {
        self.print_banner();
        println!("{}", format!("🚀 Started at {}", Local::now().format("%Y-%m-%d %H:%M:%S")).green().bold());
        println!("{}", "💡 Press Ctrl+C to stop".yellow());
        println!();

        // 启动时调用所有规则的 on_mount
        self.call_on_mount_all();

        loop {
            self.on_tick();
            let now = Local::now();
            if now.minute() == 0 && now.second() < 30 {
                let _ = self.reload();
            }
            thread::sleep(Duration::from_secs((60 - now.second()) as u64));
        }
    }

    fn print_banner(&self) {
        println!("\n{}", "╔════════════════════════════════════════════╗".cyan().bold());
        println!("{} {} {}", "║".cyan().bold(), "  🤖 Yo Rhai Scheduler".cyan().bold(), "               ║".cyan().bold());
        println!("{}", "╠════════════════════════════════════════════╣".cyan().bold());
        for rule in &self.rules {
            let range = rule.trigger.time_range.as_ref()
                .map(|(s, e)| format!("{}-{}", s, e))
                .unwrap_or_else(|| "always".into());
            println!("{} {} {}", "║".cyan().bold(),
                format!("  📜 {} [{}]", rule.name, range).white(), "║".cyan().bold());
        }
        println!("{}\n", "╚════════════════════════════════════════════╝".cyan().bold());
    }
}

// Global scheduler for event callbacks
static GLOBAL_SCHEDULER: Mutex<Option<Arc<Mutex<RhaiScheduler>>>> = Mutex::new(None);

pub fn set_global_scheduler(scheduler: Arc<Mutex<RhaiScheduler>>) {
    *GLOBAL_SCHEDULER.lock().unwrap() = Some(scheduler);
}

pub fn trigger_unlock_event() {
    if let Some(ref s) = *GLOBAL_SCHEDULER.lock().unwrap() {
        s.lock().unwrap().on_unlock();
    }
}

pub fn trigger_lock_event() {
    if let Some(ref s) = *GLOBAL_SCHEDULER.lock().unwrap() {
        s.lock().unwrap().on_lock();
    }
}
