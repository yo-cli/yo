//! 火山引擎 TTS 同步客户端

use super::error::TtsError;
use super::player::{get_voice_dir, play_audio, play_hourly_chime};
use super::types::{TtsRequest, TtsResponse};
use colored::Colorize;
use reqwest::blocking::Client;
use sha2::{Digest, Sha256};
use std::fs;
use std::path::PathBuf;

pub struct VolcengineTtsClient {
    api_key: String,
    api_url: String,
    resource_id: String,
}

impl VolcengineTtsClient {
    /// 创建客户端，api_key 格式: "appid|access_token"
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

    /// 合成语音并保存到文件
    pub fn synthesize_to_file(&self, text: &str, speaker: &str, output: &PathBuf) -> Result<(), TtsError> {
        println!("{}", format!("  🔊 Synthesizing: \"{}\"", text).blue());

        let request = TtsRequest::new(&self.resource_id, speaker, text);
        let client = Client::new();

        let response = client
            .post(&self.api_url)
            .header("Authorization", format!("Bearer;{}", self.api_key))
            .header("Content-Type", "application/json")
            .json(&request)
            .send()
            .map_err(|e| TtsError::RequestFailed(e.to_string()))?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().unwrap_or_default();
            return Err(TtsError::ApiError(format!("HTTP {}: {}", status, body)));
        }

        let resp: TtsResponse = response.json()
            .map_err(|e| TtsError::RequestFailed(format!("Parse error: {}", e)))?;

        if resp.code != 3000 {
            return Err(TtsError::ApiError(format!("Code {}: {}", resp.code, resp.message)));
        }

        let audio_b64 = resp.data.ok_or_else(|| TtsError::ApiError("No audio data".into()))?;

        use base64::Engine;
        let audio = base64::engine::general_purpose::STANDARD.decode(&audio_b64)
            .map_err(|e| TtsError::ApiError(format!("Base64 decode: {}", e)))?;

        fs::write(output, audio).map_err(|e| TtsError::SaveAudioFailed(e.to_string()))?;
        println!("{}", format!("  ✓ Saved: {}", output.display()).green());
        Ok(())
    }

    /// 合成并播放（带缓存）
    pub fn synthesize_and_play(&self, text: &str, speaker: &str) -> Result<(), TtsError> {
        let cache_dir = get_voice_dir()?.join("cache");
        fs::create_dir_all(&cache_dir)
            .map_err(|e| TtsError::SaveAudioFailed(e.to_string()))?;

        let cache_key = Self::cache_key(text, speaker);
        let cache_file = cache_dir.join(format!("{}.mp3", cache_key));

        if cache_file.exists() {
            println!("{}", format!("  ✓ Cache hit ({})", cache_key).green());
            return play_audio(&cache_file);
        }

        println!("{}", "  ⚡ Cache miss, synthesizing...".yellow());
        self.synthesize_to_file(text, speaker, &cache_file)?;
        play_audio(&cache_file)
    }

    /// 预生成缓存（不播放）
    pub fn prefetch(&self, text: &str, speaker: &str) -> Result<bool, TtsError> {
        let cache_dir = get_voice_dir()?.join("cache");
        fs::create_dir_all(&cache_dir)
            .map_err(|e| TtsError::SaveAudioFailed(e.to_string()))?;

        let cache_key = Self::cache_key(text, speaker);
        let cache_file = cache_dir.join(format!("{}.mp3", cache_key));

        if cache_file.exists() {
            return Ok(false); // 已缓存
        }

        self.synthesize_to_file(text, speaker, &cache_file)?;
        Ok(true) // 新生成
    }

    /// 整点报时
    pub fn hourly_chime(&self, hour: u32) -> Result<(), TtsError> {
        println!("{}", format!("🕐 Chime for {} o'clock", hour).cyan());
        play_hourly_chime()?;
        println!("{}", "✓ Chime completed".green());
        Ok(())
    }

    fn cache_key(text: &str, speaker: &str) -> String {
        let mut hasher = Sha256::new();
        hasher.update(text.as_bytes());
        hasher.update(speaker.as_bytes());
        format!("{:x}", hasher.finalize()).chars().take(32).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore]
    fn test_tts_synthesis() {
        let client = VolcengineTtsClient::new("your-api-key".to_string());
        let result = client.synthesize_and_play("你好", "zh_female_wanwanxiaohe_moon_bigtts");
        assert!(result.is_ok());
    }
}
