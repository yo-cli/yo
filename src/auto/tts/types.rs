//! TTS 请求/响应类型

use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize)]
pub struct AppConfig {
    pub appid: String,
    pub token: String,
    pub cluster: String,
}

#[derive(Debug, Serialize)]
pub struct UserConfig {
    pub uid: String,
}

#[derive(Debug, Serialize)]
pub struct AudioConfig {
    pub voice_type: String,
    pub encoding: String,
    pub speed_ratio: f32,
    pub rate: u32,
}

#[derive(Debug, Serialize)]
pub struct RequestConfig {
    pub reqid: String,
    pub text: String,
    pub operation: String,
}

#[derive(Debug, Serialize)]
pub struct TtsRequest {
    pub app: AppConfig,
    pub user: UserConfig,
    pub audio: AudioConfig,
    pub request: RequestConfig,
}

#[derive(Debug, Deserialize)]
pub struct TtsResponse {
    pub code: i32,
    pub message: String,
    #[serde(default)]
    pub data: Option<String>,
}

impl TtsRequest {
    pub fn new(appid: &str, speaker: &str, text: &str) -> Self {
        let reqid = format!("{}", chrono::Local::now().timestamp_nanos_opt().unwrap_or(0));
        Self {
            app: AppConfig {
                appid: appid.to_string(),
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
        }
    }
}
