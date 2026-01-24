//! Rhai 规则调度器

use super::engine::RhaiEngine;
use super::index::TimeIndex;
use super::types::{Rule, Trigger};
use chrono::{Datelike, Local, NaiveTime, Timelike};
use colored::Colorize;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

pub struct RhaiScheduler {
    engine: Arc<RhaiEngine>,
    rules: Vec<Rule>,
    index: TimeIndex,
    last_tick_minute: Option<u32>,
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

        Ok(Self { engine, rules, index, last_tick_minute: None })
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

    pub fn on_tick(&mut self) {
        let now = Local::now();
        let (hour, minute) = (now.hour(), now.minute());

        if self.last_tick_minute == Some(minute) { return; }
        self.last_tick_minute = Some(minute);

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

    pub fn on_unlock(&self) {
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

    pub fn on_lock(&self) {
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
