//! TTS 语音合成模块

mod client;
mod error;
mod types;

// 条件编译：选择真实实现或空实现
#[cfg(feature = "audio")]
mod player;
#[cfg(not(feature = "audio"))]
#[path = "player_stub.rs"]
mod player;

pub use client::VolcengineTtsClient;
pub use player::play_audio;
