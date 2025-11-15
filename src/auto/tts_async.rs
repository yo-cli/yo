use colored::Colorize;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
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
            format!("  🔊 Synthesizing speech: \"{}\"", text).blue().bold()
        );
        println!(
            "{}",
            format!("  🎤 Voice: {}", speaker).blue().bold()
        );

        // 生成唯一的 reqid
        let reqid = format!("{}", chrono::Local::now().timestamp_nanos_opt().unwrap_or(0));

        let request = TtsRequest {
            app: AppConfig {
                appid: self.resource_id.clone(),
                token: "access_token".to_string(),
                cluster: "volcano_tts".to_string(),
            },
            user: UserConfig {
                uid: "yo_tts_user".to_string(),
            },
            audio: AudioConfig {
                voice_type: speaker.to_string(),
                encoding: "mp3".to_string(),
                speed_ratio: 1.0,
                rate: 24000,
            },
            request: RequestConfig {
                reqid,
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

        // 检查响应状态
        if !response.status().is_success() {
            let status = response.status();
            let body = response
                .text()
                .await
                .unwrap_or_else(|_| "Unknown error".to_string());
            return Err(TtsError::ApiError(format!(
                "HTTP {}: {}",
                status, body
            )));
        }

        // 解析 JSON 响应（V1 API 返回单个 JSON 对象）
        let response_json: TtsResponse = response
            .json()
            .await
            .map_err(|e| TtsError::RequestFailed(format!("Failed to parse JSON: {}", e)))?;

        // 检查返回码（V1 API 成功码是 3000）
        if response_json.code != 3000 {
            return Err(TtsError::ApiError(format!(
                "API error code {}: {}",
                response_json.code, response_json.message
            )));
        }

        println!("{}", "  ✓ TTS synthesis completed".green());

        // 提取并解码 base64 音频数据（V1 API data 字段直接是 base64 字符串）
        let audio_base64 = response_json
            .data
            .as_ref()
            .ok_or_else(|| TtsError::ApiError("No audio data in response".to_string()))?;

        use base64::Engine as _;
        let audio_data = base64::engine::general_purpose::STANDARD
            .decode(audio_base64)
            .map_err(|e| TtsError::ApiError(format!("Failed to decode base64: {}", e)))?;

        // 保存音频文件
        fs::write(output_path, audio_data)
            .map_err(|e| TtsError::SaveAudioFailed(format!("{}", e)))?;

        println!(
            "{}",
            format!("  ✓ Audio saved to: {}", output_path.display())
                .green()
                .bold()
        );

        Ok(())
    }

    /// 合成语音并播放（异步，带缓存）
    pub async fn synthesize_and_play(&self, text: &str, speaker: &str) -> Result<(), TtsError> {
        let voice_dir = Self::get_voice_dir();
        let cache_dir = voice_dir.join("cache");

        // 确保缓存目录存在
        fs::create_dir_all(&cache_dir)
            .map_err(|e| TtsError::SaveAudioFailed(format!("Failed to create cache directory: {}", e)))?;

        // 生成缓存键（基于文本和语音模型）
        let cache_key = Self::generate_cache_key(text, speaker);
        let cache_file = cache_dir.join(format!("{}.mp3", cache_key));

        // 检查缓存是否存在
        if cache_file.exists() {
            println!(
                "{}",
                format!("  ✓ Using cached audio ({})", cache_key)
                    .green()
                    .bold()
            );
            Self::play_audio(&cache_file)?;
            return Ok(());
        }

        // 缓存未命中，调用 API 合成
        println!(
            "{}",
            "  ⚡ Cache miss, synthesizing new audio..."
                .yellow()
                .bold()
        );
        self.synthesize_to_file(text, speaker, &cache_file).await?;

        // 播放缓存的文件
        Self::play_audio(&cache_file)?;

        Ok(())
    }

    /// 生成缓存键（基于文本和语音模型的 SHA256 哈希）
    fn generate_cache_key(text: &str, speaker: &str) -> String {
        let mut hasher = Sha256::new();
        hasher.update(text.as_bytes());
        hasher.update(speaker.as_bytes());
        let result = hasher.finalize();
        // 取前 16 个字节转为十六进制字符串
        format!("{:x}", result).chars().take(32).collect()
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

    /// 播放音频文件 - 使用 rodio 内置播放器（所有平台通用）
    fn play_audio(file_path: &PathBuf) -> Result<(), TtsError> {
        println!(
            "{}",
            format!("  🔊 Playing audio: {}", file_path.display())
                .green()
                .bold()
        );

        // 验证文件存在
        if !file_path.exists() {
            return Err(TtsError::PlayAudioFailed(format!(
                "Audio file not found: {}",
                file_path.display()
            )));
        }

        println!("{}", format!("  📁 Audio file path: {}", file_path.display()).blue());

        // 使用 rodio 播放音频（所有平台统一）
        use std::fs::File;
        use std::io::BufReader;

        let file = File::open(file_path)
            .map_err(|e| TtsError::PlayAudioFailed(format!("Failed to open file: {}", e)))?;
        let source = BufReader::new(file);

        println!("{}", "  🎵 Initializing audio output...".blue());

        let (_stream, stream_handle) = rodio::OutputStream::try_default()
            .map_err(|e| TtsError::PlayAudioFailed(format!("Failed to get audio output: {}", e)))?;

        let sink = rodio::Sink::try_new(&stream_handle)
            .map_err(|e| TtsError::PlayAudioFailed(format!("Failed to create audio sink: {}", e)))?;

        println!("{}", "  🎵 Decoding audio...".blue());

        let decoder = rodio::Decoder::new(source)
            .map_err(|e| TtsError::PlayAudioFailed(format!("Failed to decode audio: {}", e)))?;

        println!("{}", "  ▶️  Playing...".green().bold());

        sink.append(decoder);
        sink.sleep_until_end();

        println!("{}", "  ✓ Audio playback completed".green().bold());
        Ok(())
    }

    fn get_voice_dir() -> PathBuf {
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
        PathBuf::from(home).join(".yo").join("voice")
    }
}
