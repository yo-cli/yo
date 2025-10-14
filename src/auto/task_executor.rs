use crate::auto::config::Task;
use crate::auto::tts::VolcengineTtsClient;
use chrono::Timelike;
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
    #[error("TTS error: {0}")]
    TtsError(String),
    #[error("Missing required parameter: {0}")]
    MissingParameter(String),
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
            "tts_command" => Self::execute_tts(task),
            "hourly_chime" => Self::execute_hourly_chime(task),
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

    /// 执行 TTS 命令
    fn execute_tts(task: &Task) -> Result<(), ExecutorError> {
        // 验证必需参数
        let text = task
            .tts_text
            .as_ref()
            .ok_or_else(|| ExecutorError::MissingParameter("tts_text".to_string()))?;

        let voice = task
            .tts_voice
            .as_ref()
            .ok_or_else(|| ExecutorError::MissingParameter("tts_voice".to_string()))?;

        let api_key = task
            .tts_api_key
            .as_ref()
            .ok_or_else(|| ExecutorError::MissingParameter("tts_api_key".to_string()))?;

        println!(
            "{}",
            format!("  🔊 TTS Text: \"{}\"", text).blue().bold()
        );
        println!(
            "{}",
            format!("  🎤 Voice: {}", voice).blue().bold()
        );

        // 创建 TTS 客户端并执行
        let client = VolcengineTtsClient::new(api_key.clone());
        client
            .synthesize_and_play(text, voice)
            .map_err(|e| ExecutorError::TtsError(format!("{}", e)))?;

        println!("{}", "✓ TTS executed successfully".green().bold());
        Ok(())
    }

    /// 执行整点报时
    fn execute_hourly_chime(task: &Task) -> Result<(), ExecutorError> {
        // 获取当前小时
        let now = chrono::Local::now();
        let hour = now.hour();

        println!(
            "{}",
            format!("🕐 Hourly chime: {} o'clock", hour).cyan().bold()
        );

        // 获取 API Key
        let api_key = task
            .tts_api_key
            .as_ref()
            .ok_or_else(|| ExecutorError::MissingParameter("tts_api_key".to_string()))?;

        // 创建 TTS 客户端并执行整点报时
        let client = VolcengineTtsClient::new(api_key.clone());
        client
            .hourly_chime(hour)
            .map_err(|e| ExecutorError::TtsError(format!("{}", e)))?;

        Ok(())
    }
}
