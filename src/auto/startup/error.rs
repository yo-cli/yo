//! 自启动错误类型

use thiserror::Error;

#[derive(Debug, Error)]
pub enum AutostartError {
    #[error("Git Bash not found. Please install Git for Windows.")]
    GitBashNotFound,
    #[error("Failed to get startup folder path: {0}")]
    StartupFolderError(String),
    #[error("Failed to create autostart script: {0}")]
    CreateScriptError(String),
    #[error("Failed to remove autostart script: {0}")]
    RemoveScriptError(String),
    #[cfg(not(windows))]
    #[error("This feature is only supported on Windows")]
    NotWindows,
}
