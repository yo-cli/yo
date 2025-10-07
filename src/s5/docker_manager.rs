use colored::Colorize;
use std::process::Command;
use std::thread;
use std::time::Duration;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum DockerError {
    #[error("Command failed: {0}")]
    CommandFailed(String),
    #[error("Failed to install Docker")]
    InstallationFailed,
    #[error("Failed to start Docker service")]
    ServiceStartFailed,
    #[error("Failed to pull image")]
    ImagePullFailed,
    #[error("Container failed to start")]
    ContainerStartFailed,
}

pub struct S5DockerManager;

impl S5DockerManager {
    /// 检查 Docker 是否可用
    pub fn is_docker_available() -> bool {
        Command::new("docker")
            .arg("--version")
            .output()
            .map(|output| output.status.success())
            .unwrap_or(false)
    }

    /// 执行命令（静默）
    fn execute_command_silent(command: &str) -> Result<(), DockerError> {
        let status = if cfg!(target_os = "windows") {
            Command::new("cmd")
                .args(&["/C", command])
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
        } else {
            Command::new("sh")
                .args(&["-c", command])
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
        };

        match status {
            Ok(status) if status.success() => Ok(()),
            _ => Err(DockerError::CommandFailed(command.to_string())),
        }
    }

    /// 安装 Docker
    fn install_docker() -> Result<(), DockerError> {
        println!("{}", "ℹ Docker not found. Starting automatic installation...".blue().bold());

        // 创建临时目录
        Command::new("mkdir").args(&["-p", "/tmp/claude"]).status().ok();

        // 下载 Docker 安装脚本
        println!("{}", "ℹ Downloading Docker installation script...".blue().bold());
        Self::execute_command_silent("curl -fsSL https://get.docker.com -o /tmp/claude/get-docker.sh")
            .map_err(|_| DockerError::InstallationFailed)?;

        // 安装 Docker
        println!("{}", "ℹ Installing Docker (this may take a few minutes)...".blue().bold());
        Self::execute_command_silent("sudo sh /tmp/claude/get-docker.sh")
            .map_err(|_| DockerError::InstallationFailed)?;

        // 启动 Docker 服务
        println!("{}", "ℹ Starting Docker service...".blue().bold());
        if Self::execute_command_silent("sudo systemctl start docker").is_err() {
            println!("{}", "⚠ Failed to start Docker service, trying alternative method...".yellow().bold());
            Self::execute_command_silent("sudo service docker start")
                .map_err(|_| DockerError::ServiceStartFailed)?;
        }

        // 启用 Docker 服务自启动
        println!("{}", "ℹ Enabling Docker service to start on boot...".blue().bold());
        Self::execute_command_silent("sudo systemctl enable docker 2>/dev/null || sudo chkconfig docker on 2>/dev/null").ok();

        // 清理
        Command::new("rm").args(&["-f", "/tmp/claude/get-docker.sh"]).status().ok();

        // 等待 Docker 完全启动
        println!("{}", "ℹ Waiting for Docker to fully initialize...".blue().bold());
        thread::sleep(Duration::from_secs(5));

        println!("{}", "✓ Docker installation completed successfully!".green().bold());
        Ok(())
    }

    /// 启动 Docker 服务
    pub fn start_docker_service() -> Result<(), DockerError> {
        let check = Command::new("docker")
            .arg("info")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();

        if let Ok(status) = check {
            if !status.success() {
                println!("{}", "✗ Docker is not running. Attempting to start...".red().bold());

                if Self::execute_command_silent("sudo systemctl start docker 2>/dev/null || sudo service docker start 2>/dev/null").is_err() {
                    return Err(DockerError::ServiceStartFailed);
                }

                // 等待并再次检查
                thread::sleep(Duration::from_secs(3));
                let recheck = Command::new("docker")
                    .arg("info")
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .status();

                if let Ok(status) = recheck {
                    if !status.success() {
                        return Err(DockerError::ServiceStartFailed);
                    }
                }

                println!("{}", "✓ Docker service started successfully!".green().bold());
            }
        }

        Ok(())
    }

    /// 确保 Docker 可用
    pub fn ensure_docker_available(interactive: bool) -> Result<(), DockerError> {
        if !Self::is_docker_available() {
            println!("{}", "⚠ Docker is not installed.".yellow().bold());

            if interactive {
                use std::io::{self, Write};
                print!("{}", "Would you like to install Docker automatically? (Y/n): ".yellow());
                io::stdout().flush().ok();

                let mut response = String::new();
                io::stdin().read_line(&mut response).ok();
                let response = response.trim().to_lowercase();

                if response.is_empty() || response == "y" || response == "yes" {
                    Self::install_docker()?;
                } else {
                    println!("{}", "ℹ Docker installation skipped. Please install Docker manually:".blue().bold());
                    println!("{}", "ℹ Visit: https://docs.docker.com/get-docker/".blue().bold());
                    return Err(DockerError::InstallationFailed);
                }
            } else {
                println!("{}", "ℹ Automatic mode: Installing Docker automatically...".blue().bold());
                Self::install_docker()?;
            }
        }

        Self::start_docker_service()
    }

    /// 清理现有容器
    pub fn cleanup_existing_container(container_name: &str) -> Result<(), DockerError> {
        println!("{}", format!("ℹ Checking for existing {} container...", container_name).blue().bold());
        Command::new("docker")
            .args(&["stop", container_name])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .ok();
        Command::new("docker")
            .args(&["rm", container_name])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .ok();
        Ok(())
    }

    /// 拉取 GOST 镜像
    pub fn pull_gost_image() -> Result<(), DockerError> {
        println!("{}", "ℹ Pulling GOST image...".blue().bold());
        Self::execute_command_silent("docker pull gogost/gost > /dev/null 2>&1")
            .map_err(|_| DockerError::ImagePullFailed)
    }

    /// 启动 GOST 容器
    pub fn start_gost_container(
        container_name: &str,
        port: u16,
        password: &str,
    ) -> Result<(), DockerError> {
        println!("{}", "ℹ Starting GOST SOCKS5 proxy...".blue().bold());

        let command = format!(
            "docker run -d --name {} -p {}:{} gogost/gost -L \"socks5://admin:{}@:{}\" > /dev/null 2>&1",
            container_name, port, port, password, port
        );

        Self::execute_command_silent(&command)
            .map_err(|_| DockerError::ContainerStartFailed)?;

        thread::sleep(Duration::from_secs(2));

        if !Self::is_container_running(container_name) {
            println!("{}", "✗ Failed to start GOST container".red().bold());
            Command::new("docker")
                .args(&["logs", container_name])
                .status()
                .ok();
            return Err(DockerError::ContainerStartFailed);
        }

        Ok(())
    }

    /// 检查容器是否运行
    pub fn is_container_running(container_name: &str) -> bool {
        let output = Command::new("docker")
            .args(&["ps", "--format", "{{.Names}}"])
            .output();

        if let Ok(output) = output {
            let names = String::from_utf8_lossy(&output.stdout);
            names.lines().any(|line| line.trim() == container_name)
        } else {
            false
        }
    }
}
