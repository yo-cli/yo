//! 全局配置管理
//!
//! 类似 GitHub 环境变量的全局配置系统
//! 配置存储在 ~/.yo/config.json

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

/// 全局配置
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct GlobalConfig {
    /// 环境变量
    #[serde(default)]
    pub env: HashMap<String, String>,
}

impl GlobalConfig {
    /// 获取配置文件路径
    pub fn config_path() -> PathBuf {
        let home = std::env::var("USERPROFILE")
            .or_else(|_| std::env::var("HOME"))
            .unwrap_or_else(|_| ".".to_string());
        PathBuf::from(home).join(".yo").join("config.json")
    }

    /// 加载配置
    pub fn load() -> Self {
        let path = Self::config_path();
        if path.exists() {
            match fs::read_to_string(&path) {
                Ok(content) => {
                    serde_json::from_str(&content).unwrap_or_default()
                }
                Err(_) => Self::default(),
            }
        } else {
            Self::default()
        }
    }

    /// 保存配置
    pub fn save(&self) -> Result<(), String> {
        let path = Self::config_path();

        // 确保目录存在
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .map_err(|e| format!("Failed to create config directory: {}", e))?;
        }

        let content = serde_json::to_string_pretty(self)
            .map_err(|e| format!("Failed to serialize config: {}", e))?;

        fs::write(&path, content)
            .map_err(|e| format!("Failed to write config: {}", e))?;

        Ok(())
    }

    /// 获取环境变量
    pub fn get(&self, key: &str) -> Option<&String> {
        self.env.get(key)
    }

    /// 设置环境变量
    pub fn set(&mut self, key: String, value: String) {
        self.env.insert(key, value);
    }

    /// 删除环境变量
    pub fn remove(&mut self, key: &str) -> Option<String> {
        self.env.remove(key)
    }

    /// 获取所有环境变量
    pub fn get_all(&self) -> &HashMap<String, String> {
        &self.env
    }
}

/// 预定义的配置键
pub mod keys {
    pub const TTS_API_KEY: &str = "TTS_API_KEY";
    pub const TTS_VOICE: &str = "TTS_VOICE";
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_config_default() {
        let config = GlobalConfig::default();
        assert!(config.env.is_empty());
    }

    #[test]
    fn test_config_set_get() {
        let mut config = GlobalConfig::default();
        config.set("TEST_KEY".to_string(), "test_value".to_string());
        assert_eq!(config.get("TEST_KEY"), Some(&"test_value".to_string()));
    }
}
