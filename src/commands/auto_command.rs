use crate::auto::scheduler::TaskScheduler;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum AutoError {
    #[error("Scheduler initialization failed: {0}")]
    SchedulerInitFailed(String),
    #[error("Scheduler runtime error: {0}")]
    SchedulerRuntimeError(String),
}

pub struct AutoCommand;

impl AutoCommand {
    /// 执行 auto 命令
    pub fn execute() -> Result<(), AutoError> {
        let mut scheduler = TaskScheduler::new()
            .map_err(|e| AutoError::SchedulerInitFailed(format!("{}", e)))?;

        scheduler
            .run()
            .map_err(|e| AutoError::SchedulerRuntimeError(format!("{}", e)))
    }
}
