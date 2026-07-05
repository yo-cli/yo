// 连通性探测（复用 yo-s5 probe_proxy 的 curl 思路，但无需认证，且返回出口 IP）

use anyhow::{anyhow, Result};
use std::process::Command;

/// 经本地 http 代理取出口 IP（验证是否真的翻出去）
pub fn egress_ip(local_port: u16) -> Result<String> {
    let proxy = format!("http://127.0.0.1:{}", local_port);
    let output = Command::new("curl")
        .args(["-x", &proxy, "-s", "--max-time", "12", "http://ipinfo.io/ip"])
        .output()?;

    if !output.status.success() {
        return Err(anyhow!("request via local proxy failed"));
    }
    let ip = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if ip.is_empty() {
        return Err(anyhow!("empty response via local proxy"));
    }
    Ok(ip)
}

/// 直连上游 socks5，判断出口是否可达（--socks5 忽略环境代理）
pub fn upstream_reachable(upstream: &str) -> bool {
    Command::new("curl")
        .args([
            "--socks5",
            upstream,
            "-s",
            "--max-time",
            "10",
            "-o",
            "/dev/null",
            "http://ipinfo.io/ip",
        ])
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}
