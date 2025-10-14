// 快速 TTS 测试程序
use std::path::PathBuf;

// 复制必要的代码
mod tts {
    use colored::Colorize;
    use reqwest::blocking::Client;
    use serde::{Deserialize, Serialize};
    use std::fs;
    use std::path::PathBuf;
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
        data: Option<String>,
    }

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

        pub fn synthesize_to_file(
            &self,
            text: &str,
            speaker: &str,
            output_path: &PathBuf,
        ) -> Result<(), TtsError> {
            println!("{}", format!("🔊 Synthesizing: \"{}\"", text).blue().bold());

            let client = Client::new();
            let reqid = format!("{}", chrono::Local::now().timestamp_nanos());

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

            let auth_header = format!("Bearer;{}", self.api_key);
            let response = client
                .post(&self.api_url)
                .header("Authorization", &auth_header)
                .header("Content-Type", "application/json")
                .json(&request)
                .send()
                .map_err(|e| TtsError::RequestFailed(format!("{}", e)))?;

            if !response.status().is_success() {
                let status = response.status();
                let body = response.text().unwrap_or_else(|_| "Unknown error".to_string());
                return Err(TtsError::ApiError(format!("HTTP {}: {}", status, body)));
            }

            let response_json: TtsResponse = response
                .json()
                .map_err(|e| TtsError::RequestFailed(format!("Failed to parse JSON: {}", e)))?;

            if response_json.code != 3000 {
                return Err(TtsError::ApiError(format!(
                    "API error code {}: {}",
                    response_json.code, response_json.message
                )));
            }

            let audio_base64 = response_json
                .data
                .as_ref()
                .ok_or_else(|| TtsError::ApiError("No audio data in response".to_string()))?;

            let audio_data = base64::decode(audio_base64)
                .map_err(|e| TtsError::ApiError(format!("Failed to decode base64: {}", e)))?;

            fs::write(output_path, audio_data)
                .map_err(|e| TtsError::SaveAudioFailed(format!("{}", e)))?;

            println!("{}", "✓ Audio saved".green());
            Ok(())
        }

        pub fn synthesize_and_play(&self, text: &str, speaker: &str) -> Result<(), TtsError> {
            let voice_dir = PathBuf::from("voice");
            if !voice_dir.exists() {
                let _ = fs::create_dir(&voice_dir);
            }

            let permanent_file = voice_dir.join("last_tts.mp3");
            self.synthesize_to_file(text, speaker, &permanent_file)?;
            Self::play_audio(&permanent_file)?;
            Ok(())
        }

        fn play_audio(file_path: &PathBuf) -> Result<(), TtsError> {
            use std::fs::File;
            use std::io::BufReader;

            println!("{}", "🔊 Playing...".green().bold());

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

            println!("{}", "✓ Playback completed".green().bold());
            Ok(())
        }
    }
}

fn main() {
    let api_key = "7353882085|96Uy19kkSZEIrtxY8ospvBXP-AbdVOIp".to_string();
    let text = "内嵌播放器测试成功";
    let voice = "zh_female_shuangkuaisisi_moon_bigtts";

    println!("=== TTS Quick Test ===\n");

    let client = tts::VolcengineTtsClient::new(api_key);
    match client.synthesize_and_play(text, voice) {
        Ok(_) => println!("\n✅ Test passed!"),
        Err(e) => eprintln!("\n❌ Test failed: {}", e),
    }
}
