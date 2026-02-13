use crate::github::api_client::GitHubAPIClient;
use crate::github::ssh_key_manager::SSHKeyManager;
use crate::github::token_manager::GitHubTokenManager;
use chrono::Local;
use colored::Colorize;
use hostname;
use inquire::Select;
use regex::Regex;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::PathBuf;
use std::process::Command;
use thiserror::Error;

/// 初始化模式
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum InitMode {
    /// 交互式选择模式
    Interactive,
    /// SSH Deploy Key 模式
    Ssh,
    /// HTTPS Token 模式
    Https,
}

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
    #[error("Failed to configure git credentials: {0}")]
    GitCredentialFailed(String),
    #[error("User cancelled operation")]
    UserCancelled,
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
        println!(
            "{}",
            format!("⚠ Token for @{} not found.", username)
                .yellow()
                .bold()
        );
        println!(
            "{}",
            "ℹ Please enter your GitHub Personal Access Token:".blue().bold()
        );
        println!();
        println!("{}", "  Supported token types:".blue());
        println!(
            "{}",
            "  • Classic token (ghp_...): needs 'repo' scope".blue()
        );
        println!(
            "{}",
            "  • Fine-grained token (github_pat_...): needs:".blue()
        );
        println!(
            "{}",
            "    - Repository permissions > Administration: Read and write".blue()
        );
        println!(
            "{}",
            "    - Account permissions > Profile: Read-only".blue()
        );
        println!();
        print!("{}", "Token: ".cyan().bold());
        io::stdout().flush().ok();

        let mut token = String::new();
        io::stdin().read_line(&mut token).ok();
        let token = token.trim().to_string();

        if token.is_empty() {
            return Err(InitError::EmptyToken);
        }

        // 基本的 token 格式验证
        if token.starts_with("ghp_") {
            println!(
                "{}",
                "ℹ Detected: Classic personal access token".blue().bold()
            );
        } else if token.starts_with("github_pat_") {
            println!(
                "{}",
                "ℹ Detected: Fine-grained personal access token".blue().bold()
            );
        } else {
            println!(
                "{}",
                "⚠ Warning: Token format not recognized (expected ghp_... or github_pat_...)"
                    .yellow()
                    .bold()
            );
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
    pub fn execute(repo_spec: &str, mode: InitMode) -> Result<(), InitError> {
        // 解析仓库规范
        let repo_info = Self::parse_repo_spec(repo_spec)?;

        // 如果是交互式模式，让用户选择
        let mode = if mode == InitMode::Interactive {
            Self::prompt_mode_selection()?
        } else {
            mode
        };

        match mode {
            InitMode::Ssh => Self::execute_ssh(repo_info),
            InitMode::Https => Self::execute_https(repo_info),
            InitMode::Interactive => unreachable!(),
        }
    }

    /// 交互式选择模式
    fn prompt_mode_selection() -> Result<InitMode, InitError> {
        let options = vec![
            "HTTPS + Token (recommended for CI/CD, simpler setup)",
            "SSH Deploy Key (traditional, per-repository isolation)",
        ];

        let selection = Select::new("Select authentication mode:", options)
            .with_help_message("HTTPS is simpler; SSH provides better isolation")
            .prompt()
            .map_err(|_| InitError::UserCancelled)?;

        if selection.starts_with("HTTPS") {
            Ok(InitMode::Https)
        } else {
            Ok(InitMode::Ssh)
        }
    }

    /// 执行 SSH Deploy Key 模式
    fn execute_ssh(repo_info: RepoInfo) -> Result<(), InitError> {

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
            format!("✓ Token verified for user: {}", user_info.get_login())
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

        // 生成 SSH 密钥对（如果已存在则复用）
        let existing = SSHKeyManager::get_existing_key_pair(&repo_info.username, &repo_info.repository)
            .map_err(|e| InitError::SSHKeyGenerationFailed(format!("{}", e)))?;

        let reusing = existing.is_some();

        let key_pair = if let Some(kp) = existing {
            println!(
                "{}",
                "ℹ Existing SSH key pair found, reusing...".yellow().bold()
            );
            kp
        } else {
            println!("{}", "ℹ Generating SSH key pair...".blue().bold());
            SSHKeyManager::generate_key_pair(&repo_info.username, &repo_info.repository)
                .map_err(|e| InitError::SSHKeyGenerationFailed(format!("{}", e)))?
        };

        println!(
            "{}",
            if reusing {
                "✓ SSH key pair reused:".green().bold()
            } else {
                "✓ SSH key pair generated:".green().bold()
            }
        );
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
        match api_client.add_deploy_key(
            &repo_info.username,
            &repo_info.repository,
            &deploy_key_title,
            &key_pair.public_key_content,
            false,
        ) {
            Ok(()) => {
                println!(
                    "{}",
                    "✓ Deploy key added to GitHub repository".green().bold()
                );
            }
            Err(ref e) if reusing && (format!("{}", e).contains("already exists") || format!("{}", e).contains("422")) => {
                println!(
                    "{}",
                    "✓ Deploy key already exists on GitHub, skipping".yellow().bold()
                );
            }
            Err(e) => {
                return Err(InitError::DeployKeyFailed(format!("{}", e)));
            }
        }

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

    /// 执行 HTTPS Token 模式
    fn execute_https(repo_info: RepoInfo) -> Result<(), InitError> {
        println!(
            "{}",
            format!(
                "ℹ Initializing GitHub HTTPS setup for {}/{}",
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
            let token = Self::prompt_for_token_https(&repo_info.username)?;

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
        let api_client = GitHubAPIClient::new(token.clone())
            .map_err(|e| InitError::TokenVerificationFailed(format!("{}", e)))?;

        let user_info = api_client
            .verify_token()
            .map_err(|e| InitError::TokenVerificationFailed(format!("{}", e)))?;

        println!(
            "{}",
            format!("✓ Token verified for user: {}", user_info.get_login())
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

        // 配置 git credential helper
        println!("{}", "ℹ Configuring git credentials...".blue().bold());
        Self::configure_git_credentials(&token, &repo_info.username, &repo_info.repository)?;

        println!("{}", "✓ Git credentials configured".green().bold());

        // 生成克隆命令
        let clone_url = format!(
            "https://github.com/{}/{}.git",
            repo_info.username, repo_info.repository
        );

        // 成功消息
        println!();
        println!(
            "{}",
            "✓ GitHub HTTPS setup completed successfully!".green().bold()
        );
        println!();
        println!(
            "{} {}",
            "Clone with:".cyan().bold(),
            format!("git clone {}", clone_url).cyan().bold()
        );
        println!();
        println!(
            "{}",
            format!(
                "ℹ Credential stored for {}/{} in ~/.git-credentials",
                repo_info.username, repo_info.repository
            ).blue()
        );
        println!(
            "{}",
            "ℹ Each repo uses its own token (credential.useHttpPath=true)".blue()
        );
        println!(
            "{}",
            "ℹ Run 'yo-git @owner/another-repo --https' to configure more repos".blue()
        );
        println!();

        Ok(())
    }

    /// 提示用户输入 token (HTTPS 模式)
    fn prompt_for_token_https(username: &str) -> Result<String, InitError> {
        println!(
            "{}",
            format!("⚠ Token for @{} not found.", username)
                .yellow()
                .bold()
        );
        println!(
            "{}",
            "ℹ Please enter your GitHub Personal Access Token:".blue().bold()
        );
        println!();
        println!("{}", "  Supported token types:".blue());
        println!(
            "{}",
            "  • Classic token (ghp_...): needs 'repo' scope".blue()
        );
        println!(
            "{}",
            "  • Fine-grained token (github_pat_...): needs:".blue()
        );
        println!(
            "{}",
            "    - Repository permissions > Contents: Read-only (for pull/fetch)".blue()
        );
        println!(
            "{}",
            "    - Repository permissions > Contents: Read and write (for push)".blue()
        );
        println!(
            "{}",
            "    - Account permissions > Profile: Read-only".blue()
        );
        println!();
        print!("{}", "Token: ".cyan().bold());
        io::stdout().flush().ok();

        let mut token = String::new();
        io::stdin().read_line(&mut token).ok();
        let token = token.trim().to_string();

        if token.is_empty() {
            return Err(InitError::EmptyToken);
        }

        // 基本的 token 格式验证
        if token.starts_with("ghp_") {
            println!(
                "{}",
                "ℹ Detected: Classic personal access token".blue().bold()
            );
        } else if token.starts_with("github_pat_") {
            println!(
                "{}",
                "ℹ Detected: Fine-grained personal access token".blue().bold()
            );
        } else {
            println!(
                "{}",
                "⚠ Warning: Token format not recognized (expected ghp_... or github_pat_...)"
                    .yellow()
                    .bold()
            );
        }

        Ok(token)
    }

    /// 配置 git credentials (按仓库路径匹配)
    fn configure_git_credentials(token: &str, username: &str, repository: &str) -> Result<(), InitError> {
        // 1. 确保 credential.helper 设置为 store
        let output = Command::new("git")
            .args(["config", "--global", "credential.helper", "store"])
            .output()
            .map_err(|e| InitError::GitCredentialFailed(format!("Failed to run git config: {}", e)))?;

        if !output.status.success() {
            return Err(InitError::GitCredentialFailed(
                "Failed to set credential.helper".to_string(),
            ));
        }

        // 2. 开启 useHttpPath，让 git 按完整路径匹配 credential
        let output = Command::new("git")
            .args(["config", "--global", "credential.useHttpPath", "true"])
            .output()
            .map_err(|e| InitError::GitCredentialFailed(format!("Failed to run git config: {}", e)))?;

        if !output.status.success() {
            return Err(InitError::GitCredentialFailed(
                "Failed to set credential.useHttpPath".to_string(),
            ));
        }

        // 3. 写入 ~/.git-credentials (按仓库路径)
        let credentials_path = Self::get_git_credentials_path()?;
        let repo_path = format!("{}/{}.git", username, repository);
        let credential_line = format!("https://x-access-token:{}@github.com/{}\n", token, repo_path);
        let match_pattern = format!("@github.com/{}", repo_path);

        // 检查文件是否已存在
        if credentials_path.exists() {
            let existing = fs::read_to_string(&credentials_path).unwrap_or_default();

            // 检查是否已有这个仓库的 credential
            if existing.contains(&match_pattern) {
                // 替换已有的这个仓库的 credential
                let new_content: String = existing
                    .lines()
                    .filter(|line| !line.contains(&match_pattern))
                    .collect::<Vec<_>>()
                    .join("\n");
                let mut new_content = if new_content.is_empty() {
                    credential_line
                } else {
                    format!("{}\n{}", new_content, credential_line)
                };
                if !new_content.ends_with('\n') {
                    new_content.push('\n');
                }
                fs::write(&credentials_path, new_content)
                    .map_err(|e| InitError::GitCredentialFailed(format!("Failed to write credentials: {}", e)))?;
            } else {
                // 追加新的 credential
                let mut file = OpenOptions::new()
                    .append(true)
                    .open(&credentials_path)
                    .map_err(|e| InitError::GitCredentialFailed(format!("Failed to open credentials file: {}", e)))?;
                file.write_all(credential_line.as_bytes())
                    .map_err(|e| InitError::GitCredentialFailed(format!("Failed to write credentials: {}", e)))?;
            }
        } else {
            // 创建新文件
            if let Some(parent) = credentials_path.parent() {
                fs::create_dir_all(parent)
                    .map_err(|e| InitError::GitCredentialFailed(format!("Failed to create directory: {}", e)))?;
            }
            fs::write(&credentials_path, credential_line)
                .map_err(|e| InitError::GitCredentialFailed(format!("Failed to write credentials: {}", e)))?;
        }

        // 4. 设置文件权限 (Unix only)
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let permissions = std::fs::Permissions::from_mode(0o600);
            fs::set_permissions(&credentials_path, permissions)
                .map_err(|e| InitError::GitCredentialFailed(format!("Failed to set permissions: {}", e)))?;
        }

        Ok(())
    }

    /// 获取 ~/.git-credentials 路径
    fn get_git_credentials_path() -> Result<PathBuf, InitError> {
        let home = dirs_next::home_dir()
            .or_else(|| std::env::var("HOME").ok().map(PathBuf::from))
            .or_else(|| std::env::var("USERPROFILE").ok().map(PathBuf::from))
            .ok_or_else(|| InitError::GitCredentialFailed("Cannot determine home directory".to_string()))?;

        Ok(home.join(".git-credentials"))
    }
}
