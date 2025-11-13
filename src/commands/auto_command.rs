use crate::auto::scheduler::TaskScheduler;
use crate::auto::scheduler_async::TaskSchedulerAsync;
use crate::auto::shared_state::SharedState;
use crate::auto::web_server::run_web_server;
use std::sync::Arc;
use thiserror::Error;
use tokio::sync::RwLock;

#[derive(Debug, Error)]
pub enum AutoError {
    #[error("Scheduler initialization failed: {0}")]
    SchedulerInitFailed(String),
    #[error("Scheduler runtime error: {0}")]
    SchedulerRuntimeError(String),
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
}
