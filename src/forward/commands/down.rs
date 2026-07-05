// yo-forward down：停用并移除本机代理转发（服务 + bashrc 注入块）。

use anyhow::Result;
use colored::Colorize;

use crate::forward::{shell_env, systemd_unit};

pub fn run() -> Result<()> {
    println!("{} disabling local forward proxy\n", "▶".cyan());

    systemd_unit::stop_and_remove()?;
    shell_env::remove()?;

    println!(
        "\n{} current shell still holds old vars; run this or reopen the terminal:\n  unset http_proxy https_proxy all_proxy",
        "ℹ".blue()
    );
    Ok(())
}
