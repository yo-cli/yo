//! 音频播放 - 空实现 (无 audio feature 时使用)
//!
//! 所有函数静默成功，不影响业务逻辑

use super::error::TtsError;
use std::fs;
use std::path::PathBuf;

/// 播放音频文件 (空实现，静默成功)
pub fn play_audio(_file_path: &PathBuf) -> Result<(), TtsError> {
    Ok(())
}

/// 获取音频目录 (~/.yo/voice/)
pub fn get_voice_dir() -> Result<PathBuf, TtsError> {
    let home = std::env::var("USERPROFILE")
        .or_else(|_| std::env::var("HOME"))
        .map_err(|_| TtsError::PlayAudioFailed("Cannot find home directory".to_string()))?;

    let voice_dir = PathBuf::from(home).join(".yo").join("voice");
    fs::create_dir_all(&voice_dir)
        .map_err(|e| TtsError::PlayAudioFailed(format!("Failed to create voice dir: {}", e)))?;

    Ok(voice_dir)
}

/// 播放整点报时 (空实现，静默成功)
pub fn play_hourly_chime() -> Result<(), TtsError> {
    Ok(())
}
