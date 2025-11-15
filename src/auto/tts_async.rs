use colored::Colorize;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;
use thiserror::Error;

// 嵌入音频文件到二进制
const HOUR_CHIME_AUDIO: &[u8] = include_bytes!("../../voice/clock/Hour_Chime_from_Clock.mp3");

#[derive(Debug, Error)]
pub enum TtsError {
    #[error("HTTP request failed: {0}")]
    RequestFailed(String),
    #[error("Failed to save audio: {0}")]
    SaveAudioFailed(String),
    #[error("Failed to play audio: {0}")]
    PlayAudioFailed(String),
    #[error("API error: {0}")]
    #[allow(dead_code)]
    ApiError(String),
}

#[derive(Debug, Serialize)]
struct AppConfig {
    appid: String,
    token: String,
    cluster: String,
}

#[derive(Debug, Serialize)]
struct UserConfig {
    uid: String,
}

#[derive(Debug, Serialize)]
struct AudioConfig {
    voice_type: String,
    encoding: String,
    speed_ratio: f32,
    rate: u32,
}

#[derive(Debug, Serialize)]
struct RequestConfig {
    reqid: String,
    text: String,
    operation: String,
}

#[derive(Debug, Serialize)]
struct TtsRequest {
    app: AppConfig,
    user: UserConfig,
    audio: AudioConfig,
    request: RequestConfig,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct TtsResponse {
    code: i32,
    message: String,
    #[serde(default)]
    data: Option<String>,
}

/// 火山引擎 TTS 客户端（异步版本）
pub struct VolcengineTtsClient {
    api_key: String,
    api_url: String,
    resource_id: String,
}

impl VolcengineTtsClient {
    pub fn new(api_key: String) -> Self {
        let parts: Vec<&str> = api_key.split('|').collect();
        let (appid, token) = if parts.len() == 2 {
            (parts[0].to_string(), parts[1].to_string())
        } else {
            (api_key.clone(), api_key.clone())
        };

        Self {
            api_key: token,
            api_url: "https://openspeech.bytedance.com/api/v1/tts".to_string(),
            resource_id: appid,
        }
    }

    /// 合成语音并保存到文件（异步）
    pub async fn synthesize_to_file(
        &self,
        text: &str,
        speaker: &str,
        output_path: &PathBuf,
    ) -> Result<(), TtsError> {
        println!(
            "{}",
            format!("🔊 Synthesizing speech: \"{}\"", text).blue().bold()
        );

        let request = TtsRequest {
            app: AppConfig {
                appid: self.resource_id.clone(),
                token: self.api_key.clone(),
                cluster: "volcano_tts".to_string(),
            },
            user: UserConfig {
                uid: "test_user".to_string(),
            },
            audio: AudioConfig {
                voice_type: speaker.to_string(),
                encoding: "mp3".to_string(),
                speed_ratio: 1.0,
                rate: 24000,
            },
            request: RequestConfig {
                reqid: format!("req_{}", chrono::Local::now().timestamp()),
                text: text.to_string(),
                operation: "query".to_string(),
            },
        };

        let client = Client::new();
        let response = client
            .post(&self.api_url)
            .header("Authorization", format!("Bearer;{}", self.api_key))
            .header("Content-Type", "application/json")
            .json(&request)
            .send()
            .await
            .map_err(|e| TtsError::RequestFailed(format!("{}", e)))?;

        let audio_bytes = response
            .bytes()
            .await
            .map_err(|e| TtsError::RequestFailed(format!("{}", e)))?;

        // 保存音频文件
        fs::write(output_path, audio_bytes)
            .map_err(|e| TtsError::SaveAudioFailed(format!("{}", e)))?;

        println!("{}", "✓ Speech synthesized successfully".green().bold());
        Ok(())
    }

    /// 合成语音并播放（异步）
    pub async fn synthesize_and_play(&self, text: &str, speaker: &str) -> Result<(), TtsError> {
        let voice_dir = Self::get_voice_dir();
        fs::create_dir_all(&voice_dir)
            .map_err(|e| TtsError::SaveAudioFailed(format!("{}", e)))?;

        let output_path = voice_dir.join("last_tts.mp3");
        self.synthesize_to_file(text, speaker, &output_path).await?;

        Self::play_audio(&output_path)?;
        Ok(())
    }

    /// 整点报时（异步）
    pub async fn hourly_chime(&self, _hour: u32) -> Result<(), TtsError> {
        let voice_dir = Self::get_voice_dir();
        fs::create_dir_all(&voice_dir)
            .map_err(|e| TtsError::SaveAudioFailed(format!("{}", e)))?;

        let chime_path = voice_dir.join("hour_chime.mp3");
        fs::write(&chime_path, HOUR_CHIME_AUDIO)
            .map_err(|e| TtsError::SaveAudioFailed(format!("{}", e)))?;

        Self::play_audio(&chime_path)?;
        Ok(())
    }

    /// 播放音频文件
    fn play_audio(audio_path: &PathBuf) -> Result<(), TtsError> {
        println!(
            "{}",
            format!("🔊 Playing audio: {}", audio_path.display())
                .blue()
                .bold()
        );

        #[cfg(target_os = "windows")]
        {
            use std::process::Command;
            let path_str = audio_path.to_string_lossy().to_string();
            Command::new("cmd")
                .args(&["/C", "start", "", &path_str])
                .spawn()
                .map_err(|e| TtsError::PlayAudioFailed(format!("{}", e)))?;
        }

        #[cfg(not(target_os = "windows"))]
        {
            // 检测是否在 WSL 环境中
            let is_wsl = std::path::Path::new("/proc/version").exists()
                && std::fs::read_to_string("/proc/version")
                    .map(|s| s.to_lowercase().contains("microsoft") || s.to_lowercase().contains("wsl"))
                    .unwrap_or(false);

            // 在 WSL 中使用 Windows 命令播放
            if is_wsl {
                use std::process::Command;
                let path_str = audio_path.to_string_lossy().to_string();
                Command::new("cmd.exe")
                    .args(&["/C", "start", "", &path_str])
                    .spawn()
                    .map_err(|e| TtsError::PlayAudioFailed(format!("{}", e)))?;
            } else {
                // 真实的 Linux 环境使用 rodio
                use rodio::{Decoder, OutputStream, Sink};
                use std::fs::File;
                use std::io::BufReader;

                let (_stream, stream_handle) = OutputStream::try_default()
                    .map_err(|e| TtsError::PlayAudioFailed(format!("{}", e)))?;
                let sink = Sink::try_new(&stream_handle)
                    .map_err(|e| TtsError::PlayAudioFailed(format!("{}", e)))?;

                let file = File::open(audio_path)
                    .map_err(|e| TtsError::PlayAudioFailed(format!("{}", e)))?;
                let source = Decoder::new(BufReader::new(file))
                    .map_err(|e| TtsError::PlayAudioFailed(format!("{}", e)))?;

                sink.append(source);
                sink.sleep_until_end();
            }
        }

        println!("{}", "✓ Audio playback completed".green().bold());
        Ok(())
    }

    fn get_voice_dir() -> PathBuf {
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
        PathBuf::from(home).join(".yo").join("voice")
    }
}
