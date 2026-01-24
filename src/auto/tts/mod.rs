//! TTS 语音合成模块

mod client;
mod error;
mod player;
mod types;

pub use client::VolcengineTtsClient;
pub use player::play_audio;
