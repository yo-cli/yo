// yo-forward check：逐环体检，定位链路在哪一环断掉。纯读操作，无需 sudo。

use anyhow::Result;
use colored::Colorize;

use crate::forward::{gost_installer, probe, shell_env, systemd_unit, ForwardConfig, SERVICE_NAME};
use crate::s5::network_utils::S5NetworkUtils;

pub fn run(config: ForwardConfig) -> Result<()> {
    println!("{} proxy chain health check\n", "▶".cyan());

    report(
        systemd_unit::systemd_available(),
        "systemd available",
        "systemd unavailable (WSL needs [boot] systemd=true)",
    );

    match gost_installer::installed_version() {
        Some(version) => ok(&format!("gost installed: {}", version)),
        None => bad("gost not installed (run: yo-forward up)"),
    }

    report(
        systemd_unit::is_active(),
        &format!("service {} running", SERVICE_NAME),
        &format!("service {} not running", SERVICE_NAME),
    );

    // 端口不可 bind 即说明有进程在监听
    report(
        !S5NetworkUtils::is_port_available(config.local_port),
        &format!("local port {} listening", config.local_port),
        &format!("local port {} not listening", config.local_port),
    );

    report(
        probe::upstream_reachable(&config.upstream),
        &format!("upstream socks5 {} reachable", config.upstream),
        &format!("upstream socks5 {} not reachable", config.upstream),
    );

    report(
        shell_env::is_injected(),
        "bashrc has proxy variables",
        "bashrc missing proxy block (or hand-written)",
    );

    // 出口 IP —— 最终判据
    println!();
    match probe::egress_ip(config.local_port) {
        Ok(ip) => println!("{} exit IP: {} (chain OK)", "✓".green().bold(), ip.cyan()),
        Err(_) => println!(
            "{} egress via port {} failed — see the failed items above",
            "✗".red().bold(),
            config.local_port
        ),
    }

    Ok(())
}

fn report(condition: bool, ok_msg: &str, bad_msg: &str) {
    if condition {
        ok(ok_msg);
    } else {
        bad(bad_msg);
    }
}

fn ok(msg: &str) {
    println!("{} {}", "✓".green(), msg);
}

fn bad(msg: &str) {
    println!("{} {}", "✗".red(), msg);
}
