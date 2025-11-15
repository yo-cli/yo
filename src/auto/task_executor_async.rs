use crate::auto::config::Task;
use crate::auto::tts_async::VolcengineTtsClient;
use chrono::Timelike;
use colored::Colorize;
use std::sync::Arc;
use thiserror::Error;
use tokio::process::Command;
use tokio::sync::RwLock;

use super::shared_state::{SharedState, TaskExecution};

#[derive(Debug, Error)]
pub enum ExecutorError {
    #[error("Command execution failed: {0}")]
    CommandFailed(String),
    #[error("Unsupported task type: {0}")]
    UnsupportedTaskType(String),
    #[error("Lock screen command not available on this platform")]
    #[allow(dead_code)]
    LockScreenNotSupported,
    #[error("TTS error: {0}")]
    TtsError(String),
    #[error("Missing required parameter: {0}")]
    MissingParameter(String),
}

pub struct TaskExecutorAsync;

impl TaskExecutorAsync {
    /// 执行任务（异步）
    pub async fn execute_task(
        task: &Task,
        state: Arc<RwLock<SharedState>>,
    ) -> Result<(), ExecutorError> {
        println!(
            "{}",
            format!("🚀 Executing task: {}", task.name).cyan().bold()
        );

        let result = match task.task_type.as_str() {
            "lockscreen" | "lockscreen_repeated" => Self::execute_lockscreen().await,
            "adaptive_lockscreen" => Self::execute_adaptive_lockscreen(task).await,
            "command" => {
                if let Some(ref cmd) = task.command {
                    Self::execute_command(cmd).await
                } else {
                    Err(ExecutorError::CommandFailed(
                        "No command specified".to_string(),
                    ))
                }
            }
            "tts_command" => Self::execute_tts(task).await,
            "hourly_chime" => Self::execute_hourly_chime(task).await,
            _ => Err(ExecutorError::UnsupportedTaskType(task.task_type.clone())),
        };

        // 记录执行结果到历史
        let mut state = state.write().await;
        state
            .add_history(TaskExecution {
                task_name: task.name.clone(),
                executed_at: chrono::Local::now(),
                task_type: task.task_type.clone(),
                success: result.is_ok(),
                message: result.as_ref().err().map(|e| e.to_string()),
            })
            .await;

        // 如果成功，记录操作日志
        if result.is_ok() {
            state
                .log_operation(
                    "task_executed",
                    &format!("Task '{}' executed successfully", task.name),
                )
                .await;
        } else {
            state
                .log_operation(
                    "task_failed",
                    &format!(
                        "Task '{}' failed: {}",
                        task.name,
                        result.as_ref().err().unwrap()
                    ),
                )
                .await;
        }

        result
    }

    /// 执行锁屏命令（异步）
    async fn execute_lockscreen() -> Result<(), ExecutorError> {
        #[cfg(target_os = "linux")]
        {
            let commands = vec![
                "gnome-screensaver-command -l",
                "qdbus org.freedesktop.ScreenSaver /ScreenSaver Lock",
                "loginctl lock-session",
                "xscreensaver-command -lock",
                "i3lock -c 000000",
            ];

            for cmd in commands {
                let result = Command::new("sh").args(&["-c", cmd]).status().await;

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
            let status = Command::new("pmset")
                .args(&["displaysleepnow"])
                .status()
                .await
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
            let status = Command::new("rundll32.exe")
                .args(&["user32.dll,LockWorkStation"])
                .status()
                .await
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

    /// 执行自定义命令（异步）
    async fn execute_command(command: &str) -> Result<(), ExecutorError> {
        println!("{}", format!("  Command: {}", command).blue().bold());

        let status = if cfg!(target_os = "windows") {
            Command::new("cmd").args(&["/C", command]).status().await
        } else {
            Command::new("sh").args(&["-c", command]).status().await
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

    /// 执行 TTS 命令（异步）
    async fn execute_tts(task: &Task) -> Result<(), ExecutorError> {
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

        println!("{}", format!("  🔊 TTS Text: \"{}\"", text).blue().bold());
        println!("{}", format!("  🎤 Voice: {}", voice).blue().bold());

        // 播放语音前检查锁屏状态
        if Self::is_screen_locked() {
            println!(
                "{}",
                "ℹ Screen is locked, skipping TTS playback".blue()
            );
            return Ok(());
        }

        let client = VolcengineTtsClient::new(api_key.clone());
        client
            .synthesize_and_play(text, voice)
            .await
            .map_err(|e| ExecutorError::TtsError(format!("{}", e)))?;

        println!("{}", "✓ TTS executed successfully".green().bold());
        Ok(())
    }

    /// 执行整点报时（异步）
    async fn execute_hourly_chime(task: &Task) -> Result<(), ExecutorError> {
        let now = chrono::Local::now();
        let hour = now.hour();

        println!(
            "{}",
            format!("🕐 Hourly chime: {} o'clock", hour).cyan().bold()
        );

        let api_key = task
            .tts_api_key
            .as_ref()
            .ok_or_else(|| ExecutorError::MissingParameter("tts_api_key".to_string()))?;

        // 播放语音前检查锁屏状态
        if Self::is_screen_locked() {
            println!(
                "{}",
                "ℹ Screen is locked, skipping hourly chime".blue()
            );
            return Ok(());
        }

        let client = VolcengineTtsClient::new(api_key.clone());
        client
            .hourly_chime(hour)
            .await
            .map_err(|e| ExecutorError::TtsError(format!("{}", e)))?;

        Ok(())
    }

    /// 执行自适应锁屏（异步）
    async fn execute_adaptive_lockscreen(task: &Task) -> Result<(), ExecutorError> {
        println!(
            "{}",
            format!("🔒 Executing adaptive lockscreen task: {}", task.name)
                .cyan()
                .bold()
        );

        // 检查屏幕是否已锁定
        if Self::is_screen_locked() {
            println!(
                "{}",
                "ℹ Screen is already locked, skipping TTS and lock".blue()
            );
            return Ok(());
        }

        // 播放 TTS 提示（如果配置了）
        if let (Some(text), Some(voice), Some(api_key)) =
            (&task.tts_text, &task.tts_voice, &task.tts_api_key)
        {
            println!(
                "{}",
                format!("🔊 Playing TTS reminder: \"{}\"", text)
                    .blue()
                    .bold()
            );

            let client = VolcengineTtsClient::new(api_key.clone());
            if let Err(e) = client.synthesize_and_play(text, voice).await {
                println!("{}", format!("⚠ TTS playback failed: {}", e).yellow());
            }
        }

        // 执行锁屏
        Self::execute_lockscreen().await?;

        Ok(())
    }

    /// 检查屏幕是否已锁定（Windows）
    /// 通过检查 LogonUI.exe 进程是否存在来判断
    #[cfg(target_os = "windows")]
    fn is_screen_locked() -> bool {
        use windows::Win32::System::Diagnostics::ToolHelp::{
            CreateToolhelp32Snapshot, Process32FirstW, Process32NextW,
            PROCESSENTRY32W, TH32CS_SNAPPROCESS,
        };
        use windows::Win32::Foundation::CloseHandle;

        unsafe {
            let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);

            if let Ok(snapshot) = snapshot {
                let mut process_entry = PROCESSENTRY32W {
                    dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
                    ..Default::default()
                };

                if Process32FirstW(snapshot, &mut process_entry).is_ok() {
                    loop {
                        // 将进程名从 UTF-16 转换为字符串
                        let process_name = String::from_utf16_lossy(&process_entry.szExeFile);
                        let process_name = process_name.trim_end_matches('\0');

                        // 检查是否是 LogonUI.exe（锁屏界面进程）
                        if process_name.eq_ignore_ascii_case("LogonUI.exe") {
                            let _ = CloseHandle(snapshot);
                            return true;
                        }

                        // 移动到下一个进程
                        if Process32NextW(snapshot, &mut process_entry).is_err() {
                            break;
                        }
                    }
                }

                let _ = CloseHandle(snapshot);
            }
        }

        false
    }

    #[cfg(not(target_os = "windows"))]
    fn is_screen_locked() -> bool {
        false
    }
}
