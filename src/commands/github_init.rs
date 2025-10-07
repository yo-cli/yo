use crate::github::api_client::GitHubAPIClient;
use crate::github::ssh_key_manager::SSHKeyManager;
use crate::github::token_manager::GitHubTokenManager;
use chrono::Local;
use colored::Colorize;
use hostname;
use regex::Regex;
use std::io::{self, Write};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum InitError {
    #[error("Invalid repository format. Expected: @username/repo")]
    InvalidFormat,
    #[error("Token cannot be empty")]
    EmptyToken,
    #[error("Failed to retrieve saved token: {0}")]
    TokenRetrievalFailed(String),
    #[error("Failed to save token: {0}")]
    TokenSaveFailed(String),
    #[error("Token verification failed: {0}")]
    TokenVerificationFailed(String),
    #[error("Repository access check failed: {0}")]
    RepositoryAccessFailed(String),
    #[error("Insufficient permissions. You need write access to add deploy keys.")]
    InsufficientPermissions,
    #[error("SSH key generation failed: {0}")]
    SSHKeyGenerationFailed(String),
    #[error("Failed to add deploy key: {0}")]
    DeployKeyFailed(String),
    #[error("Failed to update SSH config: {0}")]
    SSHConfigFailed(String),
    #[error("Failed to get hostname")]
    HostnameFailed,
}

#[derive(Debug, Clone)]
struct RepoInfo {
    username: String,
    repository: String,
}

pub struct GitHubInitCommand;

impl GitHubInitCommand {
    /// 解析仓库规范
    fn parse_repo_spec(repo_spec: &str) -> Result<RepoInfo, InitError> {
        let re = Regex::new(r"^@([a-zA-Z0-9][a-zA-Z0-9\-]*[a-zA-Z0-9]|[a-zA-Z0-9])/([a-zA-Z0-9][a-zA-Z0-9\-_.]*[a-zA-Z0-9]|[a-zA-Z0-9])$")
            .unwrap();

        if let Some(captures) = re.captures(repo_spec) {
            Ok(RepoInfo {
                username: captures[1].to_string(),
                repository: captures[2].to_string(),
            })
        } else {
            Err(InitError::InvalidFormat)
        }
    }

    /// 提示用户输入 token
    fn prompt_for_token(username: &str) -> Result<String, InitError> {
        println!("{}", format!("⚠ Token for @{} not found.", username).yellow().bold());
        println!("{}", "ℹ Please enter your GitHub Personal Access Token:".blue().bold());
        println!("{}", "ℹ (Token should have 'repo' scope for private repositories or 'public_repo' for public repositories)".blue().bold());
        print!("{}", "Token: ".cyan().bold());
        io::stdout().flush().ok();

        let mut token = String::new();
        io::stdin().read_line(&mut token).ok();
        let token = token.trim().to_string();

        if token.is_empty() {
            return Err(InitError::EmptyToken);
        }

        // 基本的 token 格式验证
        if !token.starts_with("ghp_") && !token.starts_with("github_pat_") {
            println!("{}", "⚠ Warning: Token doesn't match expected GitHub token format".yellow().bold());
        }

        Ok(token)
    }

    /// 生成 Deploy Key 标题
    fn generate_deploy_key_title() -> Result<String, InitError> {
        let hostname = hostname::get()
            .map_err(|_| InitError::HostnameFailed)?
            .to_string_lossy()
            .to_string();

        let now = Local::now();
        let title = format!(
            "yo-{}-{:04}-{:02}-{:02}",
            hostname,
            now.format("%Y").to_string().parse::<i32>().unwrap_or(2024),
            now.format("%m").to_string().parse::<u32>().unwrap_or(1),
            now.format("%d").to_string().parse::<u32>().unwrap_or(1)
        );

        Ok(title)
    }

    /// 执行 init 命令
    pub fn execute(repo_spec: &str) -> Result<(), InitError> {
        // 解析仓库规范
        let repo_info = Self::parse_repo_spec(repo_spec)?;

        println!(
            "{}",
            format!(
                "ℹ Initializing GitHub SSH setup for {}/{}",
                repo_info.username, repo_info.repository
            )
            .blue()
            .bold()
        );

        // 检查 token 是否存在,如果不存在则提示输入
        let token = if GitHubTokenManager::has_token(&repo_info.username) {
            println!(
                "{}",
                format!("ℹ Using saved token for @{}", repo_info.username)
                    .blue()
                    .bold()
            );
            GitHubTokenManager::get_token(&repo_info.username)
                .map_err(|e| InitError::TokenRetrievalFailed(format!("{}", e)))?
        } else {
            let token = Self::prompt_for_token(&repo_info.username)?;

            // 保存 token
            GitHubTokenManager::save_token(&repo_info.username, &token)
                .map_err(|e| InitError::TokenSaveFailed(format!("{}", e)))?;

            println!(
                "{}",
                format!(
                    "✓ Token saved to ~/.yo/github/{}/token",
                    repo_info.username
                )
                .green()
                .bold()
            );

            token
        };

        // 验证 token 并获取用户信息
        println!("{}", "ℹ Verifying GitHub token...".blue().bold());
        let api_client = GitHubAPIClient::new(token)
            .map_err(|e| InitError::TokenVerificationFailed(format!("{}", e)))?;

        let user_info = api_client
            .verify_token()
            .map_err(|e| InitError::TokenVerificationFailed(format!("{}", e)))?;

        println!(
            "{}",
            format!("✓ Token verified for user: {}", user_info.login)
                .green()
                .bold()
        );

        // 检查仓库访问权限
        println!(
            "{}",
            format!(
                "ℹ Checking repository access for {}/{}...",
                repo_info.username, repo_info.repository
            )
            .blue()
            .bold()
        );

        let repo_info_result = api_client
            .get_repository_info(&repo_info.username, &repo_info.repository)
            .map_err(|e| InitError::RepositoryAccessFailed(format!("{}", e)))?;

        println!(
            "{}",
            format!(
                "✓ Repository access confirmed (permissions: {})",
                repo_info_result.get_permission_level()
            )
            .green()
            .bold()
        );

        // 检查是否有足够的权限
        if repo_info_result.get_permission_level() == "read" {
            return Err(InitError::InsufficientPermissions);
        }

        // 生成 SSH 密钥对
        println!("{}", "ℹ Generating SSH key pair...".blue().bold());
        let key_pair = SSHKeyManager::generate_key_pair(&repo_info.username, &repo_info.repository)
            .map_err(|e| InitError::SSHKeyGenerationFailed(format!("{}", e)))?;

        println!("{}", "✓ SSH key pair generated:".green().bold());
        println!(
            "{}",
            format!("  Private key: {}", key_pair.private_key_path)
                .blue()
                .bold()
        );
        println!(
            "{}",
            format!("  Public key: {}", key_pair.public_key_path)
                .blue()
                .bold()
        );

        // 生成 deploy key 标题
        let deploy_key_title = Self::generate_deploy_key_title()?;

        // 添加 deploy key 到 GitHub
        println!(
            "{}",
            "ℹ Adding deploy key to GitHub repository...".blue().bold()
        );
        api_client
            .add_deploy_key(
                &repo_info.username,
                &repo_info.repository,
                &deploy_key_title,
                &key_pair.public_key_content,
                false,
            )
            .map_err(|e| InitError::DeployKeyFailed(format!("{}", e)))?;

        println!(
            "{}",
            "✓ Deploy key added to GitHub repository".green().bold()
        );

        // 更新 SSH 配置
        println!("{}", "ℹ Updating SSH configuration...".blue().bold());
        SSHKeyManager::update_ssh_config(
            &repo_info.username,
            &repo_info.repository,
            &key_pair.private_key_path,
        )
        .map_err(|e| InitError::SSHConfigFailed(format!("{}", e)))?;

        println!("{}", "✓ SSH configuration updated".green().bold());

        // 生成克隆命令
        let clone_command =
            SSHKeyManager::get_clone_command(&repo_info.username, &repo_info.repository);

        // 成功消息
        println!();
        println!(
            "{}",
            "✓ GitHub SSH setup completed successfully!".green().bold()
        );
        println!();
        println!(
            "{} {}",
            "Clone with:".cyan().bold(),
            clone_command.cyan().bold()
        );
        println!();

        Ok(())
    }
}
