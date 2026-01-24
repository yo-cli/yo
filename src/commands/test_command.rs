use crate::auto::tts::play_audio;
use chrono::Timelike;
use colored::Colorize;
use std::path::PathBuf;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum TestError {
    #[error("TTS error: {0}")]
    TtsError(String),
    #[error("Configuration error: {0}")]
    ConfigError(String),
}

pub struct TestCommand;

impl TestCommand {
    /// 执行测试命令 - 播放整点钟声
    pub fn execute() -> Result<(), TestError> {
        println!("{}", "=== Yo Test Mode ===".cyan().bold());
        println!("{}", "Testing hourly chime playback...".blue());
        println!();

        // 获取当前时间
        let now = chrono::Local::now();
        let hour = now.hour();

        println!(
            "{}",
            format!("🕐 Current time: {} o'clock", hour)
                .cyan()
                .bold()
        );
        println!();

        // 直接播放钟声文件（不需要 API Key）
        let voice_dir = Self::get_voice_dir()?;
        let chime_file = voice_dir.join("clock").join("Hour_Chime_from_Clock.mp3");

        println!(
            "{}",
            format!("📁 Chime file: {}", chime_file.display())
                .blue()
                .bold()
        );

        if !chime_file.exists() {
            return Err(TestError::ConfigError(format!(
                "Hour chime file not found: {}\n\
                Please ensure the audio file exists at this location.\n\
                The file should be automatically extracted on first run.",
                chime_file.display()
            )));
        }

        println!("{}", "🔔 Playing hour chime...".green().bold());
        println!();

        // 播放音频
        play_audio(&chime_file)
            .map_err(|e| TestError::TtsError(format!("{}", e)))?;

        println!();
        println!("{}", "✅ Test completed successfully!".green().bold());
        println!("{}", "You should hear the chime sound now.".blue());

        Ok(())
    }

    /// 获取音频文件目录（~/.yo/voice/）
    fn get_voice_dir() -> Result<PathBuf, TestError> {
        let home = std::env::var("USERPROFILE")
            .or_else(|_| std::env::var("HOME"))
            .map_err(|_| TestError::ConfigError("Cannot find home directory".to_string()))?;

        let voice_dir = PathBuf::from(home).join(".yo").join("voice");

        // 确保目录存在
        if !voice_dir.exists() {
            std::fs::create_dir_all(&voice_dir).map_err(|e| {
                TestError::ConfigError(format!("Failed to create voice directory: {}", e))
            })?;
        }

        // 确保 clock 子目录存在
        let clock_dir = voice_dir.join("clock");
        if !clock_dir.exists() {
            std::fs::create_dir_all(&clock_dir).map_err(|e| {
                TestError::ConfigError(format!("Failed to create clock directory: {}", e))
            })?;
        }

        // 如果时钟报时音频文件不存在，从嵌入的数据中提取
        let chime_file = clock_dir.join("Hour_Chime_from_Clock.mp3");
        if !chime_file.exists() {
            println!(
                "{}",
                "📦 Extracting embedded hour chime audio...".yellow()
            );
            const HOUR_CHIME_AUDIO: &[u8] =
                include_bytes!("../../voice/clock/Hour_Chime_from_Clock.mp3");
            std::fs::write(&chime_file, HOUR_CHIME_AUDIO).map_err(|e| {
                TestError::ConfigError(format!("Failed to extract hour chime audio: {}", e))
            })?;
            println!(
                "{}",
                format!("✓ Extracted to: {}", chime_file.display())
                    .green()
                    .bold()
            );
        }

        Ok(voice_dir)
    }
}
