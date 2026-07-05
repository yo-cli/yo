// ~/.bashrc 里代理环境变量的幂等注入 / 移除。注入内容用带标记的块包裹，
// down 时可精确移除本工具写入的部分，不碰用户手写的其他配置。改前自动备份。

use anyhow::{Context, Result};
use colored::Colorize;

use super::ForwardConfig;
use crate::ob::utils;

const BLOCK_START: &str = "# >>> yo-forward proxy >>>";
const BLOCK_END: &str = "# <<< yo-forward proxy <<<";

/// ~/.bashrc 路径
fn bashrc_path() -> String {
    match dirs_next::home_dir() {
        Some(home) => home.join(".bashrc").to_string_lossy().to_string(),
        None => "/root/.bashrc".to_string(),
    }
}

/// bashrc 是否已注入本工具的代理块
pub fn is_injected() -> bool {
    std::fs::read_to_string(bashrc_path())
        .map(|content| content.contains(BLOCK_START))
        .unwrap_or(false)
}

/// 生成注入块
fn block(config: &ForwardConfig) -> String {
    format!(
        "{start}\n\
         export http_proxy=\"http://127.0.0.1:{port}\"\n\
         export https_proxy=\"http://127.0.0.1:{port}\"\n\
         export all_proxy=\"http://127.0.0.1:{port}\"\n\
         export no_proxy=\"localhost,127.0.0.1\"\n\
         {end}\n",
        start = BLOCK_START,
        end = BLOCK_END,
        port = config.local_port
    )
}

/// 幂等注入：先移除旧块再追加新块（改前备份）
pub fn inject(config: &ForwardConfig) -> Result<()> {
    let path = bashrc_path();
    let current = utils::read_file_safe(&path);

    backup_if_exists(&path)?;

    let mut next = strip_block(&current);
    if !next.is_empty() && !next.ends_with('\n') {
        next.push('\n');
    }
    next.push_str(&block(config));

    utils::write_file(&path, &next).with_context(|| format!("failed to write {}", path))?;
    println!("{} proxy variables written to {}", "✓".green(), path);
    Ok(())
}

/// 移除注入块（改前备份）
pub fn remove() -> Result<()> {
    let path = bashrc_path();
    if !is_injected() {
        println!("{} no yo-forward block in bashrc, skipping", "ℹ".blue());
        return Ok(());
    }

    let current = utils::read_file_safe(&path);
    backup_if_exists(&path)?;

    utils::write_file(&path, &strip_block(&current))
        .with_context(|| format!("failed to write {}", path))?;
    println!("{} proxy variables removed from {}", "✓".green(), path);
    Ok(())
}

fn backup_if_exists(path: &str) -> Result<()> {
    if std::path::Path::new(path).exists() {
        let backup = utils::backup_file(path)?;
        if !backup.is_empty() {
            println!("{} backed up {} → {}", "ℹ".blue(), path, backup);
        }
    }
    Ok(())
}

/// 去掉 BLOCK_START..=BLOCK_END 之间（含边界）的内容
fn strip_block(content: &str) -> String {
    let mut result = String::new();
    let mut in_block = false;
    for line in content.lines() {
        match line.trim() {
            BLOCK_START => in_block = true,
            BLOCK_END => in_block = false,
            _ if !in_block => {
                result.push_str(line);
                result.push('\n');
            }
            _ => {}
        }
    }
    result
}
