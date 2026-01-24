use crate::auto::tts::VolcengineTtsClient;
use colored::Colorize;
use inquire::{Select, Text};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum VeError {
    #[error("TTS error: {0}")]
    TtsError(String),
    #[error("Configuration error: {0}")]
    ConfigError(String),
    #[error("Input error: {0}")]
    InputError(String),
}

pub struct VeCommand;

impl VeCommand {
    /// 执行火山引擎 TTS 测试命令
    pub fn execute() -> Result<(), VeError> {
        println!("{}", "=== Volcengine TTS Test ===".cyan().bold());
        println!("{}", "Interactive TTS synthesis and playback".blue());
        println!();

        // 提示用户输入 API Key
        println!(
            "{}",
            "ℹ Please enter your Volcengine API key (format: appid|token)".blue()
        );

        let api_key = Text::new("API Key:")
            .prompt()
            .map_err(|e| VeError::InputError(format!("Failed to read API key: {}", e)))?;

        if api_key.trim().is_empty() {
            return Err(VeError::InputError("API key cannot be empty".to_string()));
        }

        println!();

        // 选择声音类型
        let voices = vec![
            ("zh_female_wanwanxiaohe_moon_bigtts", "湾湾小何（女声）"),
            (
                "zh_male_beijingxiaoye_emo_v2_mars_bigtts",
                "北京小爷（男声）",
            ),
            ("zh_female_qingxin", "清新女声"),
            ("zh_male_qn_qingse", "青涩男声"),
        ];

        let voice_options: Vec<String> = voices
            .iter()
            .map(|(_, desc)| desc.to_string())
            .collect();

        println!("{}", "🎤 Select voice type:".cyan().bold());
        let selected_index = Select::new("Voice:", voice_options)
            .prompt()
            .map_err(|e| VeError::InputError(format!("Failed to select voice: {}", e)))?;

        let selected_voice = voices
            .iter()
            .find(|(_, desc)| *desc == selected_index.as_str())
            .map(|(id, _)| *id)
            .ok_or_else(|| VeError::ConfigError("Invalid voice selection".to_string()))?;

        println!(
            "{}",
            format!("  ✓ Selected voice: {}", selected_voice)
                .green()
                .bold()
        );
        println!();

        // 输入要合成的文本
        println!("{}", "✍️  Enter text to synthesize:".cyan().bold());
        let text = Text::new("Text:")
            .with_default("你好，我是语音助手，这是一个测试。")
            .prompt()
            .map_err(|e| VeError::InputError(format!("Failed to read text: {}", e)))?;

        if text.trim().is_empty() {
            return Err(VeError::InputError("Text cannot be empty".to_string()));
        }

        println!();
        println!("{}", "🚀 Starting TTS synthesis...".cyan().bold());
        println!();

        // 创建 TTS 客户端并执行合成
        let client = VolcengineTtsClient::new(api_key);
        client
            .synthesize_and_play(&text, selected_voice)
            .map_err(|e| VeError::TtsError(format!("{}", e)))?;

        println!();
        println!("{}", "✅ TTS test completed successfully!".green().bold());
        println!(
            "{}",
            "ℹ Audio saved to: ~/.yo/voice/last_tts.mp3".blue()
        );

        Ok(())
    }
}
