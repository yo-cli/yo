use serde::{Deserialize, Serialize};
use std::fs;
use std::io::Write;
use std::path::PathBuf;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("Failed to create config directory: {0}")]
    DirectoryError(String),
    #[error("Failed to read config file: {0}")]
    ReadError(String),
    #[error("Failed to write config file: {0}")]
    WriteError(String),
    #[error("Failed to parse config: {0}")]
    ParseError(String),
    #[error("HOME environment variable not set")]
    HomeNotSet,
    #[error("IO error: {0}")]
    IoError(#[from] std::io::Error),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Task {
    /// 任务名称
    pub name: String,
    /// 任务类型 (lockscreen_repeated, command, tts_command 等)
    pub task_type: String,
    /// 开始时间 (HH:MM 格式)
    pub start_time: String,
    /// 结束时间 (HH:MM 格式)
    pub end_time: String,
    /// 间隔分钟数
    pub interval_minutes: u32,
    /// 是否启用
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    /// 任务命令或参数
    #[serde(default)]
    pub command: Option<String>,
    /// 任务描述
    #[serde(default)]
    pub description: Option<String>,
    /// TTS 文本内容
    #[serde(default)]
    pub tts_text: Option<String>,
    /// TTS 语音模型/音色
    #[serde(default)]
    pub tts_voice: Option<String>,
    /// TTS API Key
    #[serde(default)]
    pub tts_api_key: Option<String>,
}

fn default_enabled() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AutoConfig {
    pub tasks: Vec<Task>,
}

impl Default for AutoConfig {
    fn default() -> Self {
        Self {
            tasks: vec![Task {
                name: "night_lockscreen".to_string(),
                task_type: "lockscreen_repeated".to_string(),
                start_time: "22:00".to_string(),
                end_time: "06:00".to_string(),
                interval_minutes: 5,
                enabled: true,
                command: None,
                description: Some("Lock screen every 5 minutes from 22:00 to 06:00".to_string()),
                tts_text: None,
                tts_voice: None,
                tts_api_key: None,
            }],
        }
    }
}

pub struct ConfigManager;

impl ConfigManager {
    /// 获取配置文件路径
    fn get_config_path() -> Result<PathBuf, ConfigError> {
        let home = std::env::var("HOME").map_err(|_| ConfigError::HomeNotSet)?;
        Ok(PathBuf::from(home).join(".yo").join("auto_config.json"))
    }

    /// 确保配置目录存在
    fn ensure_config_directory() -> Result<(), ConfigError> {
        let home = std::env::var("HOME").map_err(|_| ConfigError::HomeNotSet)?;
        let yo_dir = PathBuf::from(home).join(".yo");

        if !yo_dir.exists() {
            fs::create_dir_all(&yo_dir)
                .map_err(|e| ConfigError::DirectoryError(format!("{}", e)))?;

            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let permissions = fs::Permissions::from_mode(0o700);
                fs::set_permissions(&yo_dir, permissions)
                    .map_err(|e| ConfigError::DirectoryError(format!("{}", e)))?;
            }
        }

        Ok(())
    }

    /// 加载配置
    pub fn load_config() -> Result<AutoConfig, ConfigError> {
        Self::ensure_config_directory()?;
        let config_path = Self::get_config_path()?;

        if !config_path.exists() {
            // 如果配置文件不存在，创建默认配置
            let default_config = AutoConfig::default();
            Self::save_config(&default_config)?;
            return Ok(default_config);
        }

        let content = fs::read_to_string(&config_path)
            .map_err(|e| ConfigError::ReadError(format!("{}", e)))?;

        serde_json::from_str(&content)
            .map_err(|e| ConfigError::ParseError(format!("{}", e)))
    }

    /// 保存配置
    pub fn save_config(config: &AutoConfig) -> Result<(), ConfigError> {
        Self::ensure_config_directory()?;
        let config_path = Self::get_config_path()?;

        let json = serde_json::to_string_pretty(config)
            .map_err(|e| ConfigError::WriteError(format!("{}", e)))?;

        let mut file = fs::File::create(&config_path)
            .map_err(|e| ConfigError::WriteError(format!("{}", e)))?;

        file.write_all(json.as_bytes())
            .map_err(|e| ConfigError::WriteError(format!("{}", e)))?;

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let permissions = fs::Permissions::from_mode(0o600);
            fs::set_permissions(&config_path, permissions)
                .map_err(|e| ConfigError::WriteError(format!("{}", e)))?;
        }

        Ok(())
    }

    /// 获取配置文件路径（用于显示）
    pub fn get_config_path_str() -> Result<String, ConfigError> {
        Ok(Self::get_config_path()?.to_string_lossy().to_string())
    }
}
