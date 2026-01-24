//! 自启动类型定义

use std::path::PathBuf;

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct AutostartConfig {
    pub git_bash_path: PathBuf,
    pub startup_folder: PathBuf,
    pub script_path: PathBuf,
}

#[derive(Debug, Clone)]
pub struct AutostartStatus {
    pub enabled: bool,
    pub script_path: Option<PathBuf>,
    pub git_bash_path: Option<PathBuf>,
}
