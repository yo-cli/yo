use colored::Colorize;
use reqwest::blocking::Client;
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
struct TtsResponse {
    code: i32,
    message: String,
    #[serde(default)]
    data: Option<String>, // V1 API returns base64 string directly in data field
}

/// 火山引擎 TTS 客户端
pub struct VolcengineTtsClient {
    api_key: String,
    api_url: String,
    resource_id: String,
}

impl VolcengineTtsClient {
    /// 创建新的 TTS 客户端（使用 V1 HTTP API）
    /// api_key 格式: "appid|access_token"
    pub fn new(api_key: String) -> Self {
        let parts: Vec<&str> = api_key.split('|').collect();
        let (appid, token) = if parts.len() == 2 {
            (parts[0].to_string(), parts[1].to_string())
        } else {
            // 兼容旧格式
            (api_key.clone(), api_key.clone())
        };

        Self {
            api_key: token,
            api_url: "https://openspeech.bytedance.com/api/v1/tts".to_string(),
            resource_id: appid,
        }
    }

    /// 合成语音并保存到文件
    pub fn synthesize_to_file(
        &self,
        text: &str,
        speaker: &str,
        output_path: &PathBuf,
    ) -> Result<(), TtsError> {
        println!(
            "{}",
            format!("  🔊 Synthesizing speech: \"{}\"", text)
                .blue()
                .bold()
        );
        println!(
            "{}",
            format!("  🎤 Voice: {}", speaker).blue().bold()
        );

        let client = Client::new();

        // 生成唯一的 reqid
        let reqid = format!("{}", chrono::Local::now().timestamp_nanos_opt().unwrap_or(0));

        // 构建请求参数（V1 API 格式）
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

        // 发送请求（使用 Bearer Token 认证）
        let auth_header = format!("Bearer;{}", self.api_key);
        let response = client
            .post(&self.api_url)
            .header("Authorization", &auth_header)
            .header("Content-Type", "application/json")
            .json(&request)
            .send()
            .map_err(|e| TtsError::RequestFailed(format!("{}", e)))?;

        // 检查响应状态
        if !response.status().is_success() {
            let status = response.status();
            let body = response
                .text()
                .unwrap_or_else(|_| "Unknown error".to_string());
            return Err(TtsError::ApiError(format!(
                "HTTP {}: {}",
                status, body
            )));
        }

        // 解析 JSON 响应（V1 API 返回单个 JSON 对象）
        let response_json: TtsResponse = response
            .json()
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

    /// 合成语音并播放
    pub fn synthesize_and_play(&self, text: &str, speaker: &str) -> Result<(), TtsError> {
        // 保存到用户配置目录
        let voice_dir = Self::get_voice_dir()?;
        let permanent_file = voice_dir.join("last_tts.mp3");

        // 合成语音到文件
        self.synthesize_to_file(text, speaker, &permanent_file)?;

        // 播放音频
        Self::play_audio(&permanent_file)?;

        // 不删除文件，保留用于手动测试

        Ok(())
    }

    /// 播放音频文件 - 使用内嵌播放器（所有平台通用）
    pub fn play_audio(file_path: &PathBuf) -> Result<(), TtsError> {
        println!(
            "{}",
            format!("  🔊 Playing audio: {}", file_path.display())
                .green()
                .bold()
        );

        // 检测是否在 WSL 环境中
        let is_wsl = std::path::Path::new("/proc/version").exists()
            && std::fs::read_to_string("/proc/version")
                .map(|s| s.to_lowercase().contains("microsoft") || s.to_lowercase().contains("wsl"))
                .unwrap_or(false);

        // 在 WSL 或 Windows 中使用 Windows 命令播放
        #[cfg(target_os = "windows")]
        {
            use std::process::Command;
            let path_str = file_path.to_string_lossy().to_string();
            Command::new("cmd")
                .args(&["/C", "start", "", &path_str])
                .spawn()
                .map_err(|e| TtsError::PlayAudioFailed(format!("{}", e)))?;

            println!("{}", "  ✓ Audio playback started".green().bold());
            return Ok(());
        }

        #[cfg(not(target_os = "windows"))]
        {
            // 在 WSL 中使用 Windows 命令播放
            if is_wsl {
                use std::process::Command;
                let path_str = file_path.to_string_lossy().to_string();
                Command::new("cmd.exe")
                    .args(&["/C", "start", "", &path_str])
                    .spawn()
                    .map_err(|e| TtsError::PlayAudioFailed(format!("{}", e)))?;

                println!("{}", "  ✓ Audio playback started".green().bold());
                return Ok(());
            }

            // 真实的 Linux 环境使用 rodio
            use std::fs::File;
            use std::io::BufReader;

            let file = File::open(file_path)
                .map_err(|e| TtsError::PlayAudioFailed(format!("Failed to open file: {}", e)))?;
            let source = BufReader::new(file);

            let (_stream, stream_handle) = rodio::OutputStream::try_default()
                .map_err(|e| TtsError::PlayAudioFailed(format!("Failed to get audio output: {}", e)))?;

            let sink = rodio::Sink::try_new(&stream_handle)
                .map_err(|e| TtsError::PlayAudioFailed(format!("Failed to create audio sink: {}", e)))?;

            let decoder = rodio::Decoder::new(source)
                .map_err(|e| TtsError::PlayAudioFailed(format!("Failed to decode audio: {}", e)))?;

            sink.append(decoder);
            sink.sleep_until_end();

            println!("{}", "  ✓ Audio playback completed".green().bold());
            Ok(())
        }
    }

    /// 整点报时：播放时钟报时声音
    pub fn hourly_chime(&self, hour: u32) -> Result<(), TtsError> {
        println!(
            "{}",
            format!("🕐 Hourly chime for {} o'clock", hour)
                .cyan()
                .bold()
        );

        // 播放时钟报时声音（从配置目录）
        let voice_dir = Self::get_voice_dir()?;
        let chime_file = voice_dir.join("clock").join("Hour_Chime_from_Clock.mp3");

        if chime_file.exists() {
            println!("{}", "  🔔 Playing hour chime...".cyan());
            Self::play_audio(&chime_file)?;
        } else {
            return Err(TtsError::PlayAudioFailed(format!(
                "Hour chime file not found: {}\nPlease place the audio file at this location.",
                chime_file.display()
            )));
        }

        println!("{}", "✓ Hourly chime completed".green().bold());
        Ok(())
    }

    /// 获取音频文件目录（~/.yo/voice/）
    fn get_voice_dir() -> Result<PathBuf, TtsError> {
        let home = std::env::var("USERPROFILE")
            .or_else(|_| std::env::var("HOME"))
            .map_err(|_| TtsError::PlayAudioFailed("Cannot find home directory".to_string()))?;

        let voice_dir = PathBuf::from(home).join(".yo").join("voice");

        // 确保目录存在
        if !voice_dir.exists() {
            fs::create_dir_all(&voice_dir)
                .map_err(|e| TtsError::PlayAudioFailed(format!("Failed to create voice directory: {}", e)))?;
        }

        // 确保 clock 子目录存在
        let clock_dir = voice_dir.join("clock");
        if !clock_dir.exists() {
            fs::create_dir_all(&clock_dir)
                .map_err(|e| TtsError::PlayAudioFailed(format!("Failed to create clock directory: {}", e)))?;
        }

        // 如果时钟报时音频文件不存在，从嵌入的数据中提取（静默提取）
        let chime_file = clock_dir.join("Hour_Chime_from_Clock.mp3");
        if !chime_file.exists() {
            fs::write(&chime_file, HOUR_CHIME_AUDIO)
                .map_err(|e| TtsError::PlayAudioFailed(format!("Failed to extract hour chime audio: {}", e)))?;
        }

        Ok(voice_dir)
    }
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore] // 需要真实的 API Key 才能运行
    fn test_tts_synthesis() {
        let client = VolcengineTtsClient::new("your-api-key".to_string());
        let result = client.synthesize_and_play("你好，我是小智", "zh_female_wanwanxiaohe_moon_bigtts");
        assert!(result.is_ok());
    }
}
