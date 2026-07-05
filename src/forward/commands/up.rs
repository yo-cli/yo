// yo-forward up：一键配置本机代理转发。全程幂等，可安全重复运行。

use anyhow::{bail, Result};
use colored::Colorize;
use inquire::Confirm;

use crate::forward::{gost_installer, probe, shell_env, systemd_unit, ForwardConfig, SERVICE_NAME};
use crate::s5::network_utils::S5NetworkUtils;

pub fn run(config: ForwardConfig, force: bool) -> Result<()> {
    println!(
        "{} configuring local forward proxy: http://:{} → socks5://{}\n",
        "▶".cyan(),
        config.local_port,
        config.upstream
    );

    // 1. systemd 前置检查（装服务的硬前提）
    if !systemd_unit::systemd_available() {
        println!("{} systemd is not available.", "✗".red().bold());
        println!(
            "{} On WSL, add to /etc/wsl.conf then run `wsl --shutdown`:\n  [boot]\n  systemd=true",
            "ℹ".blue()
        );
        bail!("systemd not enabled, cannot install service");
    }

    // 2. 端口占用提示（被别的进程占用时警告，本服务已在跑则不算冲突）
    if !S5NetworkUtils::is_port_available(config.local_port) && !systemd_unit::is_active() {
        println!(
            "{} port {} already in use (not by this service); continuing may fail",
            "⚠".yellow(),
            config.local_port
        );
        if !force && !confirm("Continue?") {
            return Ok(());
        }
    }

    // 3. 上游可达性预检（不阻断，仅提示）
    if !probe::upstream_reachable(&config.upstream) {
        println!(
            "{} upstream socks5 {} not reachable — make sure your proxy client is listening there",
            "⚠".yellow(),
            config.upstream
        );
        if !force && !confirm("Upstream not ready, configure anyway?") {
            return Ok(());
        }
    }

    // 4. 装 gost
    gost_installer::ensure_installed()?;

    // 5. systemd 服务
    systemd_unit::install_and_start(&config)?;

    // 6. shell 环境变量
    shell_env::inject(&config)?;

    // 7. 连通性测试收口
    println!();
    match probe::egress_ip(config.local_port) {
        Ok(ip) => {
            println!("{} proxy is live. exit IP: {}", "✓".green().bold(), ip.cyan());
            println!(
                "{} current shell hasn't loaded the new vars; run `source ~/.bashrc` or open a new terminal",
                "ℹ".blue()
            );
        }
        Err(_) => print_egress_troubleshooting(&config),
    }

    Ok(())
}

fn confirm(prompt: &str) -> bool {
    Confirm::new(prompt)
        .with_default(false)
        .prompt()
        .unwrap_or(false)
}

/// 出海测试失败时的结构化排查清单
fn print_egress_troubleshooting(config: &ForwardConfig) {
    println!(
        "{} link configured, but egress via port {} did not pass",
        "⚠".yellow().bold(),
        config.local_port
    );
    println!("  troubleshoot:");
    println!("  1. is upstream socks5 {} running (your proxy client)?", config.upstream);
    println!("  2. correct upstream port? re-run with --upstream host:port");
    println!("  3. is WSL on mirrored networking? otherwise 127.0.0.1 can't reach the host");
    println!("     → add [wsl2] networkingMode=mirrored to .wslconfig, then `wsl --shutdown`");
    println!("  4. service status: systemctl status {}", SERVICE_NAME);
}
