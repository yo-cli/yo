use rand::Rng;
use socket2::{Domain, Protocol, Socket, Type};
use std::fs;
use std::io::Write;
use std::net::SocketAddr;
use std::process::Command;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum NetworkError {
    #[error("Failed to get public IP: {0}")]
    PublicIPError(String),
    #[error("SOCKS5 connectivity test failed")]
    ConnectivityTestFailed,
    #[error("IO error: {0}")]
    IoError(#[from] std::io::Error),
}

pub struct S5NetworkUtils;

impl S5NetworkUtils {
    /// 生成随机密码
    pub fn generate_random_password(length: usize) -> String {
        const CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
        let mut rng = rand::rng();

        (0..length)
            .map(|_| {
                let idx = rng.random_range(0..CHARS.len());
                CHARS[idx] as char
            })
            .collect()
    }

    /// 检查端口是否可用
    pub fn is_port_available(port: u16) -> bool {
        let addr: SocketAddr = format!("0.0.0.0:{}", port).parse().unwrap();

        match Socket::new(Domain::IPV4, Type::STREAM, Some(Protocol::TCP)) {
            Ok(socket) => {
                socket.set_reuse_address(true).ok();
                socket.bind(&addr.into()).is_ok()
            }
            Err(_) => false,
        }
    }

    /// 查找可用端口
    pub fn find_available_port(start: u16, end: u16) -> Option<u16> {
        let mut rng = rand::rng();

        // 尝试随机端口 (最多100次)
        for _ in 0..100 {
            let port = rng.random_range(start..=end);
            if Self::is_port_available(port) {
                return Some(port);
            }
        }

        // 回退到顺序搜索
        for port in start..=end {
            if Self::is_port_available(port) {
                return Some(port);
            }
        }

        None
    }

    /// 检查是否为 WSL 环境
    pub fn is_wsl() -> bool {
        if let Ok(content) = fs::read_to_string("/proc/version") {
            content.to_lowercase().contains("microsoft")
        } else {
            false
        }
    }

    /// 获取公网 IP
    pub fn get_public_ip() -> Result<String, NetworkError> {
        // 创建临时目录
        fs::create_dir_all("/tmp/claude").ok();

        let output = Command::new("curl")
            .args(&["-s", "http://ipinfo.io/ip"])
            .output();

        match output {
            Ok(output) if output.status.success() => {
                let ip = String::from_utf8_lossy(&output.stdout);
                let ip = ip.trim().to_string();
                if !ip.is_empty() {
                    Ok(ip)
                } else {
                    Err(NetworkError::PublicIPError(
                        "Empty response from IP service".to_string(),
                    ))
                }
            }
            _ => Err(NetworkError::PublicIPError(
                "Failed to get public IP, using localhost".to_string(),
            )),
        }
    }

    /// 测试 SOCKS5 连接
    pub fn test_socks5_connectivity(
        host: &str,
        port: u16,
        username: &str,
        password: &str,
    ) -> Result<(), NetworkError> {
        Self::probe_proxy("socks5", host, port, username, password)
    }

    /// Test the HTTP CONNECT proxy (shares the SOCKS5 port via GOST auto://)
    pub fn test_http_connectivity(
        host: &str,
        port: u16,
        username: &str,
        password: &str,
    ) -> Result<(), NetworkError> {
        Self::probe_proxy("http", host, port, username, password)
    }

    /// Probe proxy reachability by curling httpbin through the given proxy scheme
    fn probe_proxy(
        scheme: &str,
        host: &str,
        port: u16,
        username: &str,
        password: &str,
    ) -> Result<(), NetworkError> {
        // 创建临时目录
        fs::create_dir_all("/tmp/claude")?;

        // 创建 curl 配置文件
        let config_file = "/tmp/claude/curl_config";
        let mut file = fs::File::create(config_file)?;
        writeln!(
            file,
            "proxy = \"{}://{}:{}@{}:{}\"",
            scheme, username, password, host, port
        )?;
        writeln!(file, "connect-timeout = 10")?;
        writeln!(file, "max-time = 15")?;
        drop(file);

        // 测试连接
        let status = Command::new("curl")
            .args(&[
                "-K",
                config_file,
                "-s",
                "--fail",
                "http://httpbin.org/ip",
                "-o",
                "/tmp/claude/proxy_test.txt",
            ])
            .status();

        // 清理临时文件
        fs::remove_file(config_file).ok();
        fs::remove_file("/tmp/claude/proxy_test.txt").ok();

        match status {
            Ok(status) if status.success() => Ok(()),
            _ => Err(NetworkError::ConnectivityTestFailed),
        }
    }
}
