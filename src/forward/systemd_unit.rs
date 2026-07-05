// gost.service 的写入 / 启停 / 移除。unit 内容与用户现有配置逐字对齐。
// 程序非 root，写 /etc/systemd 与 systemctl 操作统一走 sudo。

use anyhow::{bail, Context, Result};
use colored::Colorize;
use std::path::Path;
use std::process::Command;

use super::{ForwardConfig, GOST_BIN, SERVICE_NAME, SERVICE_PATH};

/// systemd 是否可用（WSL 需开启 [boot] systemd=true）
pub fn systemd_available() -> bool {
    Path::new("/run/systemd/system").exists()
}

/// 服务是否处于 active 状态
pub fn is_active() -> bool {
    Command::new("systemctl")
        .args(["is-active", "--quiet", SERVICE_NAME])
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// 生成 unit 文件内容
fn unit_content(config: &ForwardConfig) -> String {
    format!(
        "[Unit]\n\
         Description=gost proxy\n\
         After=network.target\n\
         \n\
         [Service]\n\
         ExecStart={bin} -L http://:{port} -F \"socks5://{upstream}?nodelay=false\"\n\
         Restart=always\n\
         RestartSec=3\n\
         \n\
         [Install]\n\
         WantedBy=multi-user.target\n",
        bin = GOST_BIN,
        port = config.local_port,
        upstream = config.upstream
    )
}

/// 写入 unit + daemon-reload + enable --now
pub fn install_and_start(config: &ForwardConfig) -> Result<()> {
    if !systemd_available() {
        bail!(
            "systemd unavailable. On WSL, add to /etc/wsl.conf then run `wsl --shutdown`:\n  [boot]\n  systemd=true"
        );
    }

    write_root_file(SERVICE_PATH, &unit_content(config))
        .with_context(|| format!("failed to write {}", SERVICE_PATH))?;

    run_sudo(&["systemctl", "daemon-reload"])?;
    run_sudo(&["systemctl", "enable", "--now", SERVICE_NAME])?;
    println!(
        "{} service started and enabled on boot: {}",
        "✓".green(),
        SERVICE_NAME
    );
    Ok(())
}

/// 停止并移除服务
pub fn stop_and_remove() -> Result<()> {
    if is_active() || Path::new(SERVICE_PATH).exists() {
        run_sudo(&["systemctl", "disable", "--now", SERVICE_NAME]).ok();
        run_sudo(&["rm", "-f", SERVICE_PATH]).ok();
        run_sudo(&["systemctl", "daemon-reload"]).ok();
        println!(
            "{} service stopped and removed: {}",
            "✓".green(),
            SERVICE_NAME
        );
    } else {
        println!("{} service not installed, skipping", "ℹ".blue());
    }
    Ok(())
}

/// 以 `sudo tee` 写入 root 拥有的文件（stdin 灌入内容）
fn write_root_file(path: &str, content: &str) -> Result<()> {
    use std::io::Write;
    use std::process::Stdio;

    let mut child = Command::new("sudo")
        .args(["tee", path])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .spawn()
        .context("failed to spawn sudo tee")?;

    child
        .stdin
        .as_mut()
        .context("failed to open sudo tee stdin")?
        .write_all(content.as_bytes())?;

    if !child.wait()?.success() {
        bail!("sudo tee failed to write {}", path);
    }
    Ok(())
}

/// 执行一条 sudo 命令，失败即报错
fn run_sudo(args: &[&str]) -> Result<()> {
    let status = Command::new("sudo")
        .args(args)
        .status()
        .context("failed to run sudo command")?;
    if !status.success() {
        bail!("command failed: sudo {}", args.join(" "));
    }
    Ok(())
}
