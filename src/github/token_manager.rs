use crate::common::crypto_utils::CryptoUtils;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum TokenError {
    #[error("Token file does not exist for user {0}")]
    TokenNotFound(String),
    #[error("Failed to open token file: {0}")]
    FileOpenError(String),
    #[error("Token file is empty or invalid")]
    EmptyToken,
    #[error("Failed to decrypt token: {0}")]
    DecryptionError(String),
    #[error("Failed to encrypt token: {0}")]
    EncryptionError(String),
    #[error("Failed to create directory structure: {0}")]
    DirectoryError(String),
    #[error("Failed to create token file: {0}")]
    FileCreateError(String),
    #[error("Failed to delete token file: {0}")]
    DeleteError(String),
    #[error("HOME environment variable not set")]
    HomeNotSet,
    #[error("IO error: {0}")]
    IoError(#[from] io::Error),
}

pub struct GitHubTokenManager;

impl GitHubTokenManager {
    /// 获取 token 文件路径
    fn get_token_path(username: &str) -> Result<PathBuf, TokenError> {
        let home = std::env::var("HOME").map_err(|_| TokenError::HomeNotSet)?;
        Ok(PathBuf::from(home)
            .join(".yo")
            .join("github")
            .join(username)
            .join("token"))
    }

    /// 确保目录结构存在
    fn ensure_directory_structure(username: &str) -> Result<(), TokenError> {
        let home = std::env::var("HOME").map_err(|_| TokenError::HomeNotSet)?;
        let base_path = PathBuf::from(home);

        let yo_dir = base_path.join(".yo");
        let github_dir = yo_dir.join("github");
        let user_dir = github_dir.join(username);

        // 创建 .yo 目录并设置权限为 700
        if !yo_dir.exists() {
            fs::create_dir(&yo_dir)
                .map_err(|e| TokenError::DirectoryError(format!("Failed to create .yo: {}", e)))?;
            Self::set_secure_permissions(&yo_dir, true)?;
        }

        // 创建 .yo/github 目录
        if !github_dir.exists() {
            fs::create_dir(&github_dir).map_err(|e| {
                TokenError::DirectoryError(format!("Failed to create github dir: {}", e))
            })?;
            Self::set_secure_permissions(&github_dir, true)?;
        }

        // 创建 .yo/github/username 目录
        if !user_dir.exists() {
            fs::create_dir(&user_dir).map_err(|e| {
                TokenError::DirectoryError(format!("Failed to create user dir: {}", e))
            })?;
            Self::set_secure_permissions(&user_dir, true)?;
        }

        Ok(())
    }

    /// 设置安全权限 (Unix only)
    #[cfg(unix)]
    fn set_secure_permissions(path: &Path, is_directory: bool) -> Result<(), TokenError> {
        use std::os::unix::fs::PermissionsExt;
        let mode = if is_directory { 0o700 } else { 0o600 };
        let permissions = fs::Permissions::from_mode(mode);
        fs::set_permissions(path, permissions)
            .map_err(|e| TokenError::DirectoryError(format!("Failed to set permissions: {}", e)))
    }

    #[cfg(not(unix))]
    fn set_secure_permissions(_path: &Path, _is_directory: bool) -> Result<(), TokenError> {
        // Windows 上不设置权限
        Ok(())
    }

    /// 检查 token 是否存在
    pub fn has_token(username: &str) -> bool {
        Self::get_token_path(username)
            .map(|path| path.exists() && path.is_file())
            .unwrap_or(false)
    }

    /// 获取 token
    pub fn get_token(username: &str) -> Result<String, TokenError> {
        let token_path = Self::get_token_path(username)?;

        if !token_path.exists() {
            return Err(TokenError::TokenNotFound(username.to_string()));
        }

        let encrypted_token = fs::read_to_string(&token_path)
            .map_err(|e| TokenError::FileOpenError(format!("{}", e)))?;

        let encrypted_token = encrypted_token.trim();
        if encrypted_token.is_empty() {
            return Err(TokenError::EmptyToken);
        }

        // 解密 token
        CryptoUtils::decrypt(encrypted_token)
            .map_err(|e| TokenError::DecryptionError(format!("{}", e)))
    }

    /// 保存 token
    pub fn save_token(username: &str, token: &str) -> Result<(), TokenError> {
        // 确保目录结构存在
        Self::ensure_directory_structure(username)?;

        let token_path = Self::get_token_path(username)?;

        // 加密 token
        let encrypted_token = CryptoUtils::encrypt(token)
            .map_err(|e| TokenError::EncryptionError(format!("{}", e)))?;

        // 写入文件
        let mut file = fs::File::create(&token_path)
            .map_err(|e| TokenError::FileCreateError(format!("{}", e)))?;
        writeln!(file, "{}", encrypted_token)?;

        // 设置安全权限
        Self::set_secure_permissions(&token_path, false)?;

        Ok(())
    }

    /// Delete the saved token
    pub fn delete_token(username: &str) -> Result<(), TokenError> {
        let token_path = Self::get_token_path(username)?;
        if token_path.exists() {
            fs::remove_file(&token_path)
                .map_err(|e| TokenError::DeleteError(format!("{}", e)))?;
        }
        Ok(())
    }
}
