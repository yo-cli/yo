use super::docker_manager::S5DockerManager;
use super::network_utils::S5NetworkUtils;
use colored::Colorize;
use std::io::{self, Write};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ProxyError {
    #[error("Docker setup failed: {0}")]
    DockerSetupFailed(String),
    #[error("Configuration setup failed: {0}")]
    ConfigurationFailed(String),
    #[error("Container cleanup failed: {0}")]
    CleanupFailed(String),
    #[error("Image pull failed: {0}")]
    ImagePullFailed(String),
    #[error("Container start failed: {0}")]
    ContainerStartFailed(String),
    #[error("WSL environment not supported")]
    WSLNotSupported,
    #[error("No available ports found in range 30000-40000")]
    NoAvailablePorts,
    #[error("Port {0} is already in use")]
    PortInUse(u16),
    #[error("Port must be between 30000 and 40000")]
    InvalidPortRange,
}

#[derive(Debug, Clone)]
pub struct ProxyConfig {
    pub password: String,
    pub port: u16,
    pub public_ip: String,
}

pub struct S5ProxyManager;

impl S5ProxyManager {
    /// 运行 SOCKS5 代理
    pub fn run_socks5_proxy(interactive: bool) -> Result<(), ProxyError> {
        // 检查 WSL 环境
        if S5NetworkUtils::is_wsl() {
            println!("{}", "✗ WSL environment detected.".red().bold());
            println!("{}", "⚠ This tool is designed for native Linux environments (Ubuntu, Debian, etc.)".yellow().bold());
            println!("{}", "ℹ For WSL users, please use Docker Desktop for Windows instead:".blue().bold());
            println!("{}", "ℹ https://www.docker.com/products/docker-desktop/".blue().bold());
            return Err(ProxyError::WSLNotSupported);
        }

        if interactive {
            println!("{}", "Setting up SOCKS5 + HTTP proxy with GOST (Interactive Mode)...".cyan().bold());
        } else {
            println!("{}", "Setting up SOCKS5 + HTTP proxy with GOST (Automatic Mode)...".cyan().bold());
        }

        // 确保 Docker 可用
        S5DockerManager::ensure_docker_available(interactive)
            .map_err(|e| ProxyError::DockerSetupFailed(format!("{}", e)))?;

        // 设置代理配置
        let config = Self::setup_proxy_configuration(interactive)?;

        // 清理现有容器
        S5DockerManager::cleanup_existing_container("gost-s5")
            .map_err(|e| ProxyError::CleanupFailed(format!("{}", e)))?;

        // 拉取 GOST 镜像
        S5DockerManager::pull_gost_image()
            .map_err(|e| ProxyError::ImagePullFailed(format!("{}", e)))?;

        // 启动 GOST 容器
        S5DockerManager::start_gost_container("gost-s5", config.port, &config.password)
            .map_err(|e| ProxyError::ContainerStartFailed(format!("{}", e)))?;

        // 显示配置
        Self::display_proxy_configuration(&config);

        // 测试连接
        if let Err(_) = Self::test_proxy_connectivity(&config) {
            println!();
            println!("{}", "⚠ Connectivity test failed. The proxy may still work depending on your network environment.".yellow().bold());
        }

        Ok(())
    }

    /// 设置代理配置
    fn setup_proxy_configuration(interactive: bool) -> Result<ProxyConfig, ProxyError> {
        // 生成默认值
        let password = S5NetworkUtils::generate_random_password(20);
        let port = S5NetworkUtils::find_available_port(30000, 40000)
            .ok_or(ProxyError::NoAvailablePorts)?;

        // 获取公网 IP
        let public_ip = S5NetworkUtils::get_public_ip().unwrap_or_else(|_| "127.0.0.1".to_string());

        let mut config = ProxyConfig {
            password,
            port,
            public_ip,
        };

        if interactive {
            println!();
            println!("{}", "Configuration:".cyan().bold());

            config.password = Self::get_user_input("Password", &config.password);
            let port_str = Self::get_user_input("Port", &config.port.to_string());

            config.port = port_str
                .parse()
                .map_err(|_| ProxyError::ConfigurationFailed("Invalid port number".to_string()))?;

            Self::validate_port_range(config.port)?;

            if !S5NetworkUtils::is_port_available(config.port) {
                return Err(ProxyError::PortInUse(config.port));
            }

            println!();
            print!("{}", "Press Enter to confirm and start the proxy...".yellow());
            io::stdout().flush().ok();
            let mut _input = String::new();
            io::stdin().read_line(&mut _input).ok();
        } else {
            println!();
            println!("{}", "Auto-generated Configuration:".cyan().bold());
            println!("{}", format!("ℹ Password: {}", config.password).blue().bold());
            println!("{}", format!("ℹ Port: {}", config.port).blue().bold());
            println!();
        }

        Ok(config)
    }

    /// 验证端口范围
    fn validate_port_range(port: u16) -> Result<(), ProxyError> {
        if port < 30000 || port > 40000 {
            Err(ProxyError::InvalidPortRange)
        } else {
            Ok(())
        }
    }

    /// 获取用户输入
    fn get_user_input(prompt: &str, default_value: &str) -> String {
        print!("{} (default: {}): {}", prompt, default_value, default_value);
        io::stdout().flush().ok();

        let mut input = String::new();
        io::stdin().read_line(&mut input).ok();
        let input = input.trim();

        println!();

        if input.is_empty() {
            default_value.to_string()
        } else {
            input.to_string()
        }
    }

    /// 显示代理配置
    fn display_proxy_configuration(config: &ProxyConfig) {
        let socks5_url = format!("socks5://admin:{}@{}:{}", config.password, config.public_ip, config.port);
        let http_url = format!("http://admin:{}@{}:{}", config.password, config.public_ip, config.port);
        println!();
        println!("{}", "SOCKS5 + HTTP Proxy Configuration (same port):".green().bold());
        println!("{}", "{".cyan());
        println!("{}", "  \"type\": \"socks5+http\",".cyan());
        println!("  \"IP\": \"{}\",", config.public_ip.cyan().bold());
        println!("  \"port\": {},", config.port.to_string().cyan().bold());
        println!("  \"username\": \"{}\",", "admin".cyan().bold());
        println!("  \"password\": \"{}\",", config.password.cyan().bold());
        println!("  \"socks5\": \"{}\",", socks5_url.cyan().bold());
        println!("  \"http\": \"{}\"", http_url.cyan().bold());
        println!("{}", "}".cyan());
        println!();
    }

    /// 测试代理连接（SOCKS5 与 HTTP 共用同一端口，两者都需通过）
    fn test_proxy_connectivity(config: &ProxyConfig) -> Result<(), ProxyError> {
        println!("{}", "ℹ Testing SOCKS5 connectivity...".blue().bold());
        S5NetworkUtils::test_socks5_connectivity(&config.public_ip, config.port, "admin", &config.password)
            .map_err(|_| {
                println!("{}", "✗ SOCKS5 proxy connectivity test failed".red().bold());
                println!("{}", "ℹ   This might be due to network restrictions or proxy configuration issues".blue().bold());
                ProxyError::ConfigurationFailed("SOCKS5 connectivity test failed".to_string())
            })?;
        println!("{}", "✓ SOCKS5 proxy connectivity test passed".green().bold());

        println!("{}", "ℹ Testing HTTP connectivity (same port)...".blue().bold());
        S5NetworkUtils::test_http_connectivity(&config.public_ip, config.port, "admin", &config.password)
            .map_err(|_| {
                println!("{}", "✗ HTTP proxy connectivity test failed".red().bold());
                println!("{}", "ℹ   This might be due to network restrictions or proxy configuration issues".blue().bold());
                ProxyError::ConfigurationFailed("HTTP connectivity test failed".to_string())
            })?;
        println!("{}", "✓ HTTP proxy connectivity test passed".green().bold());

        Ok(())
    }
}
