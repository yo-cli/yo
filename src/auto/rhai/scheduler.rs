//! Rhai 规则调度器

use super::engine::RhaiEngine;
use super::index::TimeIndex;
use super::types::{parse_time_to_minutes, Rule, Trigger};
use chrono::{Datelike, Local, Timelike};
use colored::Colorize;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

/// 待执行的规则调用（在释放 scheduler 锁之后执行）
pub struct PendingExecutions {
    engine: Arc<RhaiEngine>,
    calls: Vec<(Rule, &'static str)>,
}

impl PendingExecutions {
    fn new(engine: Arc<RhaiEngine>) -> Self {
        Self { engine, calls: Vec::new() }
    }

    fn push(&mut self, rule: Rule, fn_name: &'static str) {
        self.calls.push((rule, fn_name));
    }

    /// 执行所有待处理的规则调用（调用前必须已释放 scheduler 锁）
    pub fn execute(self) {
        for (rule, fn_name) in self.calls {
            let result = match fn_name {
                "on_tick" => self.engine.call_on_tick(&rule),
                "on_unlock" => self.engine.call_on_unlock(&rule),
                "on_lock" => self.engine.call_on_lock(&rule),
                "on_destroy" => self.engine.call_on_destroy(&rule),
                "on_mount" => self.engine.call_on_mount(&rule),
                _ => Ok(()),
            };
            if let Err(e) = result {
                if !e.contains("not found") {
                    println!("{}", format!("  ⚠ [{}] {} error: {}", rule.name, fn_name, e).yellow());
                }
            }
        }
    }
}

pub struct RhaiScheduler {
    engine: Arc<RhaiEngine>,
    rules: Vec<Rule>,
    index: TimeIndex,
    last_tick_minute: Option<u32>,
    /// 已模拟的规则（按日期+规则名记录，每天重置）
    simulated_rules: std::collections::HashSet<String>,
    simulate_date: Option<u32>,
}

impl RhaiScheduler {
    pub fn new() -> Result<Self, String> {
        let engine = Arc::new(RhaiEngine::new(true));
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

        Ok(Self {
            engine, rules, index,
            last_tick_minute: None,
            simulated_rules: std::collections::HashSet::new(),
            simulate_date: None,
        })
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

    /// 检查时间范围转换，收集 on_destroy 调用到 pending
    fn collect_time_range_transitions(&self, pending: &mut PendingExecutions) {
        let state = self.engine.get_state();

        for rule in &self.rules {
            let currently_in_range = Self::check_in_time_range(rule);

            let was_in_range = {
                let gs = state.lock().unwrap();
                gs.script_in_range.get(&rule.name).copied().unwrap_or(false)
            };

            // 从范围内 -> 范围外，收集 on_destroy
            if was_in_range && !currently_in_range {
                println!("{}", format!("🔚 [{}] Leaving time range, calling on_destroy", rule.name).yellow());
                pending.push(rule.clone(), "on_destroy");
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

        // 清除已模拟记录，让修改后的规则可以重新预构建
        self.simulated_rules.clear();

        // 初始化新规则的时间范围状态
        {
            let state = self.engine.get_state();
            let mut gs = state.lock().unwrap();
            for rule in &self.rules {
                if !gs.script_in_range.contains_key(&rule.name) {
                    let in_range = Self::check_in_time_range(rule);
                    gs.script_in_range.insert(rule.name.clone(), in_range);
                }
            }
        }

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

    /// 收集 tick 待执行的规则（持有锁时调用），返回后释放锁再执行
    pub fn prepare_tick(&mut self) -> PendingExecutions {
        let mut pending = PendingExecutions::new(self.engine.clone());

        let now = Local::now();
        let (hour, minute) = (now.hour(), now.minute());

        if self.last_tick_minute == Some(minute) { return pending; }
        self.last_tick_minute = Some(minute);

        // 收集时间范围转换
        self.collect_time_range_transitions(&mut pending);

        // 预模拟即将激活的规则
        self.simulate_upcoming_rules();

        let indices = match self.index.tick_rules.get(&hour) {
            Some(i) => i.clone(),
            None => return pending,
        };

        for idx in indices {
            if let Some(rule) = self.rules.get(idx) {
                if self.should_execute(rule) {
                    println!("{}", format!("⏰ [{}] Executing tick", rule.name).yellow());
                    pending.push(rule.clone(), "on_tick");
                }
            }
        }

        pending
    }

    /// 收集 unlock 待执行的规则
    pub fn prepare_unlock(&self) -> PendingExecutions {
        let mut pending = PendingExecutions::new(self.engine.clone());
        for idx in &self.index.unlock_rules {
            if let Some(rule) = self.rules.get(*idx) {
                if self.should_execute_event(rule) {
                    println!("{}", format!("🔓 [{}] Processing unlock", rule.name).yellow());
                    pending.push(rule.clone(), "on_unlock");
                }
            }
        }
        pending
    }

    /// 收集 lock 待执行的规则
    pub fn prepare_lock(&self) -> PendingExecutions {
        let mut pending = PendingExecutions::new(self.engine.clone());
        for idx in &self.index.lock_rules {
            if let Some(rule) = self.rules.get(*idx) {
                if self.should_execute_event(rule) {
                    println!("{}", format!("🔒 [{}] Processing lock", rule.name).yellow());
                    pending.push(rule.clone(), "on_lock");
                }
            }
        }
        pending
    }

    /// TTS 缓存预热：提前 1 分钟模拟执行即将激活的规则
    /// 使用独立 Engine 实例，让 speak 提前生成 TTS 缓存
    fn simulate_upcoming_rules(&mut self) {
        let now = Local::now();
        let today = now.day();

        // 日期变更，重置记录
        if self.simulate_date != Some(today) {
            self.simulated_rules.clear();
            self.simulate_date = Some(today);
        }

        // 计算 1 分钟后的时间
        let future = now + chrono::Duration::minutes(1);
        let future_hour = future.hour();
        let future_minute = future.minute();
        let future_mins = future_hour * 60 + future_minute;

        for rule in &self.rules {
            if !rule.trigger.enabled { continue; }

            // 检查是否已模拟
            if self.simulated_rules.contains(&rule.name) { continue; }

            // 检查星期限制
            if let Some(ref weekdays) = rule.trigger.weekdays {
                let today = future.weekday().num_days_from_monday() + 1;
                if !weekdays.contains(&today) { continue; }
            }

            // 检查规则是否有时间范围
            if let Some((ref start, _)) = rule.trigger.time_range {
                let start_mins = parse_time_to_minutes(start) as u32;

                // 如果 1 分钟后刚好是规则开始时间
                if future_mins == start_mins {
                    println!("{}", format!("🔮 Simulating [{}] (starts at {})", rule.name, start).cyan());

                    // 创建独立 Engine 实例，避免与主 Engine 状态竞争
                    let sim_engine = Arc::new(RhaiEngine::new(false));
                    let rule_clone = rule.clone();
                    let rule_name = rule.name.clone();
                    let sim_hour = future_hour;
                    let sim_minute = future_minute;

                    std::thread::spawn(move || {
                        if let Err(e) = sim_engine.prebuild_tts(&rule_clone, sim_hour, sim_minute) {
                            println!("{}", format!("  ⚠ Prebuild error: {}", e).yellow());
                        }
                    });

                    self.simulated_rules.insert(rule_name);
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
            // 避免除零：interval 为 0 时视为每分钟执行
            let interval = if interval == 0 { 1 } else { interval };
            if !Self::minutes_since_start(trigger).is_multiple_of(interval) { return false; }
        }

        true
    }

    fn in_time_range(start: &str, end: &str) -> bool {
        let now = Local::now();
        let now_mins = (now.hour() * 60 + now.minute()) as i64;
        let s = parse_time_to_minutes(start);
        let e = parse_time_to_minutes(end);

        if s > e {
            // 跨午夜
            now_mins >= s || now_mins < e
        } else {
            now_mins >= s && now_mins < e
        }
    }

    fn minutes_since_start(trigger: &Trigger) -> u32 {
        let now = Local::now();
        let now_mins = (now.hour() * 60 + now.minute()) as i64;

        if let Some((ref start, _)) = trigger.time_range {
            let start_mins = parse_time_to_minutes(start);
            return if now_mins >= start_mins {
                (now_mins - start_mins) as u32
            } else {
                (24 * 60 - start_mins + now_mins) as u32
            };
        }
        now.minute()
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
            let pending = self.prepare_tick();
            pending.execute();
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
    let pending = {
        let global = GLOBAL_SCHEDULER.lock().unwrap();
        if let Some(ref s) = *global {
            s.lock().unwrap().prepare_unlock()
        } else {
            return;
        }
    };
    // 锁已释放，执行不会阻塞 Web UI
    pending.execute();
}

pub fn trigger_lock_event() {
    let pending = {
        let global = GLOBAL_SCHEDULER.lock().unwrap();
        if let Some(ref s) = *global {
            s.lock().unwrap().prepare_lock()
        } else {
            return;
        }
    };
    // 锁已释放，执行不会阻塞 Web UI
    pending.execute();
}
