use crate::auto::autostart::{AutostartManager, AutostartError};
use crate::auto::instance_lock::{InstanceLock, LockError};
use crate::auto::scheduler::TaskScheduler;
use crate::auto::scheduler_async::TaskSchedulerAsync;
use crate::auto::shared_state::SharedState;
use crate::auto::web_server::run_web_server;
use colored::Colorize;
use std::sync::Arc;
use thiserror::Error;
use tokio::sync::RwLock;

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
    /// 执行 auto 命令（同步版本 - 不带 Web UI）
    pub fn execute() -> Result<(), AutoError> {
        let mut scheduler = TaskScheduler::new()
            .map_err(|e| AutoError::SchedulerInitFailed(format!("{}", e)))?;

        scheduler
            .run()
            .map_err(|e| AutoError::SchedulerRuntimeError(format!("{}", e)))
    }

    /// 执行 auto 命令（异步版本 - 带 Web UI）
    pub fn execute_with_web(port: u16) -> Result<(), AutoError> {
        // 尝试获取单例锁
        let lock = InstanceLock::new()?;
        lock.try_acquire()?;

        println!("{}", format!("✓ Instance lock acquired (PID: {})", std::process::id()).green());

        // 创建 tokio 运行时
        let rt = tokio::runtime::Runtime::new()
            .map_err(|e| AutoError::SchedulerInitFailed(format!("Failed to create runtime: {}", e)))?;

        rt.block_on(async {
            // 加载共享状态
            let state = Arc::new(RwLock::new(SharedState::load().await));

            // 创建异步调度器
            let mut scheduler = TaskSchedulerAsync::new(state.clone())
                .await
                .map_err(|e| AutoError::SchedulerInitFailed(format!("{}", e)))?;

            // 启动 Web 服务器任务
            let web_state = state.clone();
            let web_task = tokio::spawn(async move {
                run_web_server(web_state, port).await;
            });

            // 启动调度器任务
            let scheduler_task = tokio::spawn(async move {
                if let Err(e) = scheduler.run().await {
                    eprintln!("Scheduler error: {}", e);
                }
            });

            // 等待任务（实际上会一直运行）
            tokio::select! {
                _ = web_task => {},
                _ = scheduler_task => {},
            }

            Ok(())
        })
    }

    /// 安装自启动
    pub fn autostart_install(port: u16) -> Result<(), AutoError> {
        println!("{}", "Installing autostart...".blue());

        // 检测 Git Bash
        let git_bash_path = AutostartManager::detect_git_bash()?;
        println!("{}", format!("✓ Git Bash detected: {}", git_bash_path.display()).green());

        // 安装自启动脚本
        let config = AutostartManager::install(port)?;

        println!("{}", "✓ Autostart installed:".green());
        println!("  Script: {}", config.script_path.display());
        println!("  Location: {}", config.startup_folder.display());
        println!("  Port: {}", config.port);
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
            if let Some(port) = status.port {
                println!("  Port: {}", port);
            }
            if let Some(git_bash_path) = status.git_bash_path {
                println!("  Git Bash: {}", git_bash_path.display());
            }
        } else {
            println!("{}", "Autostart: disabled".yellow().bold());
            println!("  Use 'yo run auto --web --autostart' to enable");
        }

        Ok(())
    }
}
