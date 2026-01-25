use crate::auto::rhai::{set_global_scheduler, RhaiScheduler};
use crate::auto::screen::LockscreenMonitor;
use crate::auto::startup::{AutostartError, AutostartManager};
use crate::auto::state::{InstanceLock, LockError};
use crate::auto::web::{run_web_server, WebState};
use chrono::{Local, Timelike};
use colored::Colorize;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum AutoError {
    #[error("Scheduler initialization failed: {0}")]
    SchedulerInitFailed(String),
    #[error("Scheduler runtime error: {0}")]
    SchedulerRuntimeError(String),
    #[error("Another instance is already running (PID: {0})")]
    AlreadyRunning(u32),
    #[error("Failed to acquire instance lock: {0}")]
    LockError(String),
    #[error("Autostart error: {0}")]
    AutostartError(String),
}

impl From<LockError> for AutoError {
    fn from(e: LockError) -> Self {
        match e {
            LockError::AlreadyRunning(pid) => AutoError::AlreadyRunning(pid),
            other => AutoError::LockError(other.to_string()),
        }
    }
}

impl From<AutostartError> for AutoError {
    fn from(e: AutostartError) -> Self {
        AutoError::AutostartError(e.to_string())
    }
}

pub struct AutoCommand;

impl AutoCommand {
    /// 执行 auto 命令
    pub fn execute() -> Result<(), AutoError> {
        let lock = InstanceLock::new()?;
        lock.try_acquire()?;

        println!(
            "{}",
            format!("✓ Instance lock acquired (PID: {})", std::process::id()).green()
        );

        let scheduler =
            RhaiScheduler::new().map_err(|e| AutoError::SchedulerInitFailed(e))?;

        let scheduler_arc = Arc::new(Mutex::new(scheduler));
        set_global_scheduler(scheduler_arc.clone());

        if let Err(e) = LockscreenMonitor::start_global_monitor() {
            println!(
                "{}",
                format!("⚠ Failed to start lockscreen monitor: {}", e).yellow()
            );
        }

        let result = scheduler_arc
            .lock()
            .unwrap()
            .run()
            .map_err(|e| AutoError::SchedulerRuntimeError(e));
        result
    }

    /// 执行 auto 命令（带 Web UI）
    pub fn execute_with_web(port: u16) -> Result<(), AutoError> {
        let lock = InstanceLock::new()?;
        lock.try_acquire()?;

        println!(
            "{}",
            format!("✓ Instance lock acquired (PID: {})", std::process::id()).green()
        );

        let rt = tokio::runtime::Runtime::new()
            .map_err(|e| AutoError::SchedulerInitFailed(format!("Runtime error: {}", e)))?;

        rt.block_on(async {
            let scheduler =
                RhaiScheduler::new().map_err(|e| AutoError::SchedulerInitFailed(e))?;

            let scheduler_arc = Arc::new(Mutex::new(scheduler));
            set_global_scheduler(scheduler_arc.clone());

            if let Err(e) = LockscreenMonitor::start_global_monitor() {
                println!(
                    "{}",
                    format!("⚠ Failed to start lockscreen monitor: {}", e).yellow()
                );
            }

            // 打印 banner
            {
                let s = scheduler_arc.lock().unwrap();
                Self::print_banner(&s);
            }

            // 启动 Web 服务器
            let web_state = Arc::new(WebState::new(scheduler_arc.clone()));
            let web_task = tokio::spawn(async move {
                run_web_server(web_state, port).await;
            });

            // 启动调度器循环（不持有锁）
            let scheduler_clone = scheduler_arc.clone();
            let scheduler_task = tokio::task::spawn_blocking(move || {
                Self::run_scheduler_loop(scheduler_clone)
            });

            tokio::select! {
                _ = web_task => {},
                result = scheduler_task => {
                    if let Ok(Err(e)) = result {
                        return Err(e);
                    }
                },
            }

            Ok(())
        })
    }

    /// 调度器循环（不持续持有锁）
    fn run_scheduler_loop(scheduler: Arc<Mutex<RhaiScheduler>>) -> Result<(), AutoError> {
        println!("{}", format!("🚀 Started at {}", Local::now().format("%Y-%m-%d %H:%M:%S")).green().bold());
        println!("{}", "💡 Press Ctrl+C to stop".yellow());
        println!();

        // 启动时调用所有规则的 on_mount
        {
            let s = scheduler.lock().unwrap();
            s.call_on_mount_all();
        }

        loop {
            // 只在执行时获取锁，执行完立即释放
            {
                let mut s = scheduler.lock().unwrap();
                s.on_tick();
            }

            let now = Local::now();

            // 整点时重新加载
            if now.minute() == 0 && now.second() < 30 {
                let mut s = scheduler.lock().unwrap();
                let _ = s.reload();
            }

            // 睡眠时不持有锁
            std::thread::sleep(Duration::from_secs((60 - now.second()) as u64));
        }
    }

    fn print_banner(scheduler: &RhaiScheduler) {
        println!("\n{}", "╔════════════════════════════════════════════╗".cyan().bold());
        println!("{} {} {}", "║".cyan().bold(), "  🤖 Yo Rhai Scheduler".cyan().bold(), "               ║".cyan().bold());
        println!("{}", "╠════════════════════════════════════════════╣".cyan().bold());
        for rule in scheduler.get_rules() {
            let range = rule.trigger.time_range.as_ref()
                .map(|(s, e)| format!("{}-{}", s, e))
                .unwrap_or_else(|| "always".into());
            println!("{} {} {}", "║".cyan().bold(),
                format!("  📜 {} [{}]", rule.name, range).white(), "║".cyan().bold());
        }
        println!("{}\n", "╚════════════════════════════════════════════╝".cyan().bold());
    }

    /// 安装自启动
    pub fn autostart_install() -> Result<(), AutoError> {
        println!("{}", "Installing autostart...".blue());

        let git_bash_path = AutostartManager::detect_git_bash()?;
        println!(
            "{}",
            format!("✓ Git Bash detected: {}", git_bash_path.display()).green()
        );

        let config = AutostartManager::install()?;

        println!("{}", "✓ Autostart installed:".green());
        println!("  Script: {}", config.script_path.display());
        println!("  Location: {}", config.startup_folder.display());
        println!();

        Ok(())
    }

    /// 移除自启动
    pub fn autostart_remove() -> Result<(), AutoError> {
        println!("{}", "Removing autostart...".blue());
        AutostartManager::remove()?;
        println!("{}", "✓ Autostart removed".green());
        Ok(())
    }

    /// 显示自启动状态
    pub fn autostart_status() -> Result<(), AutoError> {
        let status = AutostartManager::status()?;

        if status.enabled {
            println!("{}", "Autostart: enabled".green().bold());
            if let Some(script_path) = status.script_path {
                println!("  Script: {}", script_path.display());
            }
            if let Some(git_bash_path) = status.git_bash_path {
                println!("  Git Bash: {}", git_bash_path.display());
            }
        } else {
            println!("{}", "Autostart: disabled".yellow().bold());
            println!("  Use 'yo run auto --autostart' to enable");
        }

        Ok(())
    }
}
