use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum SSHError {
    #[error("Command failed: {0}")]
    CommandFailed(String),
    #[error("Failed to create key directory: {0}")]
    DirectoryError(String),
    #[error("Failed to open file: {0}")]
    FileError(String),
    #[error("SSH keys already exist for {0}/{1}")]
    KeysExist(String, String),
    #[error("Public key file is empty")]
    EmptyPublicKey,
    #[error("HOME environment variable not set")]
    HomeNotSet,
    #[error("Permission error: {0}")]
    PermissionError(String),
    #[error("IO error: {0}")]
    IoError(#[from] std::io::Error),
}

#[derive(Debug, Clone)]
pub struct KeyPair {
    pub private_key_path: String,
    pub public_key_path: String,
    pub public_key_content: String,
}

pub struct SSHKeyManager;

impl SSHKeyManager {
    /// 获取密钥目录
    fn get_key_directory(username: &str) -> Result<PathBuf, SSHError> {
        let home = std::env::var("HOME").map_err(|_| SSHError::HomeNotSet)?;
        Ok(PathBuf::from(home)
            .join(".yo")
            .join("github")
            .join(username)
            .join("keys"))
    }

    /// 获取私钥路径
    fn get_private_key_path(username: &str, repo: &str) -> Result<PathBuf, SSHError> {
        Ok(Self::get_key_directory(username)?.join(repo))
    }

    /// 获取公钥路径
    fn get_public_key_path(username: &str, repo: &str) -> Result<PathBuf, SSHError> {
        Ok(Self::get_key_directory(username)?.join(format!("{}.pub", repo)))
    }

    /// 设置安全权限
    #[cfg(unix)]
    fn set_secure_permissions(path: &Path, is_directory: bool) -> Result<(), SSHError> {
        use std::os::unix::fs::PermissionsExt;
        let mode = if is_directory { 0o700 } else { 0o600 };
        let permissions = fs::Permissions::from_mode(mode);
        fs::set_permissions(path, permissions)
            .map_err(|e| SSHError::PermissionError(format!("{}", e)))
    }

    #[cfg(not(unix))]
    fn set_secure_permissions(_path: &Path, _is_directory: bool) -> Result<(), SSHError> {
        Ok(())
    }

    /// 确保密钥目录存在
    fn ensure_key_directory(username: &str) -> Result<(), SSHError> {
        let key_dir = Self::get_key_directory(username)?;

        if !key_dir.exists() {
            fs::create_dir_all(&key_dir)
                .map_err(|e| SSHError::DirectoryError(format!("{}", e)))?;
            Self::set_secure_permissions(&key_dir, true)?;
        }

        Ok(())
    }

    /// 执行命令
    fn execute_command(command: &str) -> Result<(), SSHError> {
        let status = if cfg!(target_os = "windows") {
            Command::new("cmd").args(&["/C", command]).status()
        } else {
            Command::new("sh").args(&["-c", command]).status()
        };

        match status {
            Ok(status) if status.success() => Ok(()),
            Ok(_) => Err(SSHError::CommandFailed(command.to_string())),
            Err(e) => Err(SSHError::CommandFailed(format!("{}: {}", command, e))),
        }
    }

    /// 读取公钥内容
    fn read_public_key(public_key_path: &Path) -> Result<String, SSHError> {
        let content = fs::read_to_string(public_key_path)
            .map_err(|e| SSHError::FileError(format!("{}", e)))?;

        let content = content.trim();
        if content.is_empty() {
            return Err(SSHError::EmptyPublicKey);
        }

        Ok(content.to_string())
    }

    /// 生成密钥对
    pub fn generate_key_pair(username: &str, repo: &str) -> Result<KeyPair, SSHError> {
        // 确保密钥目录存在
        Self::ensure_key_directory(username)?;

        let private_key_path = Self::get_private_key_path(username, repo)?;
        let public_key_path = Self::get_public_key_path(username, repo)?;

        // 检查密钥是否已存在
        if private_key_path.exists() || public_key_path.exists() {
            return Err(SSHError::KeysExist(
                username.to_string(),
                repo.to_string(),
            ));
        }

        // 生成 Ed25519 密钥对
        let command = format!(
            "ssh-keygen -t ed25519 -f \"{}\" -N \"\" -C \"yo-github-{}-{}\" > /dev/null 2>&1",
            private_key_path.display(),
            username,
            repo
        );

        Self::execute_command(&command)?;

        // 设置安全权限
        Self::set_secure_permissions(&private_key_path, false)?;

        // 读取公钥内容
        let public_key_content = Self::read_public_key(&public_key_path)?;

        Ok(KeyPair {
            private_key_path: private_key_path.to_string_lossy().to_string(),
            public_key_path: public_key_path.to_string_lossy().to_string(),
            public_key_content,
        })
    }

    /// 备份 SSH 配置
    fn backup_ssh_config() -> Result<(), SSHError> {
        let home = std::env::var("HOME").map_err(|_| SSHError::HomeNotSet)?;
        let ssh_config_path = PathBuf::from(&home).join(".ssh").join("config");
        let backup_path = PathBuf::from(&home).join(".ssh").join("config.yo.backup");

        if ssh_config_path.exists() && !backup_path.exists() {
            fs::copy(&ssh_config_path, &backup_path)?;
        }

        Ok(())
    }

    /// 追加到 SSH 配置
    fn append_to_ssh_config(config_entry: &str) -> Result<(), SSHError> {
        let home = std::env::var("HOME").map_err(|_| SSHError::HomeNotSet)?;
        let ssh_dir = PathBuf::from(&home).join(".ssh");
        let ssh_config_path = ssh_dir.join("config");

        // 确保 .ssh 目录存在
        if !ssh_dir.exists() {
            fs::create_dir(&ssh_dir)?;
            Self::set_secure_permissions(&ssh_dir, true)?;
        }

        // 备份现有配置
        Self::backup_ssh_config()?;

        // 追加配置
        let mut file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&ssh_config_path)?;

        writeln!(file, "\n{}\n", config_entry)?;

        // 设置权限
        Self::set_secure_permissions(&ssh_config_path, false)?;

        Ok(())
    }

    /// 更新 SSH 配置
    pub fn update_ssh_config(
        username: &str,
        repo: &str,
        private_key_path: &str,
    ) -> Result<(), SSHError> {
        let host_alias = format!("github.com.{}.{}", username, repo);

        let config_entry = format!(
            "Host {}\n    HostName github.com\n    User git\n    IdentityFile {}",
            host_alias, private_key_path
        );

        Self::append_to_ssh_config(&config_entry)
    }

    /// 获取克隆命令
    pub fn get_clone_command(username: &str, repo: &str) -> String {
        let host_alias = format!("github.com.{}.{}", username, repo);
        format!("git clone git@{}:{}/{}.git", host_alias, username, repo)
    }
}
