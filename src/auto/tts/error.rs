//! TTS 错误类型

use thiserror::Error;

#[derive(Debug, Error)]
pub enum TtsError {
    #[error("HTTP request failed: {0}")]
    RequestFailed(String),
    #[error("Failed to save audio: {0}")]
    SaveAudioFailed(String),
    #[error("Failed to play audio: {0}")]
    PlayAudioFailed(String),
    #[error("API error: {0}")]
    ApiError(String),
}
