use crate::auto::config::Task;
use colored::Colorize;
use std::process::Command;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ExecutorError {
    #[error("Command execution failed: {0}")]
    CommandFailed(String),
    #[error("Unsupported task type: {0}")]
    UnsupportedTaskType(String),
    #[error("Lock screen command not available on this platform")]
    LockScreenNotSupported,
}

pub struct TaskExecutor;

impl TaskExecutor {
    /// 执行任务
    pub fn execute_task(task: &Task) -> Result<(), ExecutorError> {
        println!(
            "{}",
            format!("🚀 Executing task: {}", task.name).cyan().bold()
        );

        match task.task_type.as_str() {
            "lockscreen" | "lockscreen_repeated" => Self::execute_lockscreen(),
            "command" => {
                if let Some(ref cmd) = task.command {
                    Self::execute_command(cmd)
                } else {
                    Err(ExecutorError::CommandFailed(
                        "No command specified".to_string(),
                    ))
                }
            }
            _ => Err(ExecutorError::UnsupportedTaskType(task.task_type.clone())),
        }
    }

    /// 执行锁屏命令
    fn execute_lockscreen() -> Result<(), ExecutorError> {
        #[cfg(target_os = "linux")]
        {
            // 尝试多种 Linux 锁屏方法
            let commands = vec![
                // GNOME / Ubuntu
                "gnome-screensaver-command -l",
                // KDE
                "qdbus org.freedesktop.ScreenSaver /ScreenSaver Lock",
                // Alternative KDE
                "loginctl lock-session",
                // Xscreensaver
                "xscreensaver-command -lock",
                // i3lock
                "i3lock -c 000000",
            ];

            for cmd in commands {
                let result = Command::new("sh").args(&["-c", cmd]).status();

                if let Ok(status) = result {
                    if status.success() {
                        println!("{}", "✓ Screen locked successfully".green().bold());
                        return Ok(());
                    }
                }
            }

            Err(ExecutorError::LockScreenNotSupported)
        }

        #[cfg(target_os = "macos")]
        {
            // macOS 锁屏
            let status = Command::new("pmset")
                .args(&["displaysleepnow"])
                .status()
                .map_err(|e| ExecutorError::CommandFailed(format!("{}", e)))?;

            if status.success() {
                println!("{}", "✓ Screen locked successfully".green().bold());
                Ok(())
            } else {
                Err(ExecutorError::CommandFailed(
                    "Failed to lock screen".to_string(),
                ))
            }
        }

        #[cfg(target_os = "windows")]
        {
            // Windows 锁屏
            let status = Command::new("rundll32.exe")
                .args(&["user32.dll,LockWorkStation"])
                .status()
                .map_err(|e| ExecutorError::CommandFailed(format!("{}", e)))?;

            if status.success() {
                println!("{}", "✓ Screen locked successfully".green().bold());
                Ok(())
            } else {
                Err(ExecutorError::CommandFailed(
                    "Failed to lock screen".to_string(),
                ))
            }
        }

        #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
        {
            Err(ExecutorError::LockScreenNotSupported)
        }
    }

    /// 执行自定义命令
    fn execute_command(command: &str) -> Result<(), ExecutorError> {
        println!(
            "{}",
            format!("  Command: {}", command).blue().bold()
        );

        let status = if cfg!(target_os = "windows") {
            Command::new("cmd").args(&["/C", command]).status()
        } else {
            Command::new("sh").args(&["-c", command]).status()
        };

        match status {
            Ok(status) if status.success() => {
                println!("{}", "✓ Command executed successfully".green().bold());
                Ok(())
            }
            Ok(_) => Err(ExecutorError::CommandFailed(
                "Command failed with non-zero exit code".to_string(),
            )),
            Err(e) => Err(ExecutorError::CommandFailed(format!("{}", e))),
        }
    }
}
