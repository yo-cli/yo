// gost 裸二进制的检测 / 下载 / 安装。装 gost 时代理可能尚未生效，
// 下载全程用 --noproxy '*' 绕开自身代理，并在直连失败时走镜像兜底。

use anyhow::{anyhow, bail, Context, Result};
use colored::Colorize;
use std::process::Command;

use super::{GOST_BIN, GOST_VERSION};

/// gost 是否已安装且为 v3
pub fn is_installed() -> bool {
    installed_version()
        .map(|v| v.contains("v3"))
        .unwrap_or(false)
}

/// 读取已安装 gost 的版本串（形如 "gost v3.2.6 (...)"），未安装返回 None
pub fn installed_version() -> Option<String> {
    let output = Command::new(GOST_BIN).arg("-V").output().ok()?;
    if !output.status.success() {
        return None;
    }
    let version = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if version.is_empty() {
        None
    } else {
        Some(version)
    }
}

/// 确保 gost 已安装：已装(v3)则跳过，否则下载安装到 GOST_BIN
pub fn ensure_installed() -> Result<()> {
    if is_installed() {
        println!(
            "{} gost already installed: {}",
            "✓".green(),
            installed_version().unwrap_or_default()
        );
        return Ok(());
    }

    println!(
        "{} gost not found, installing v{} ...",
        "ℹ".blue(),
        GOST_VERSION
    );

    let arch = detect_arch()?;
    let file = format!("gost_{}_linux_{}.tar.gz", GOST_VERSION, arch);
    let url = format!(
        "https://github.com/go-gost/gost/releases/download/v{}/{}",
        GOST_VERSION, file
    );

    let tmp_dir = "/tmp/claude";
    std::fs::create_dir_all(tmp_dir).ok();
    let tarball = format!("{}/{}", tmp_dir, file);

    // 先直连 GitHub，失败则走镜像兜底
    if !download(&url, &tarball) {
        println!("{} direct GitHub download failed, trying mirror ...", "⚠".yellow());
        let mirror = format!("https://ghfast.top/{}", url);
        if !download(&mirror, &tarball) {
            bail!(
                "failed to download gost; manually download {} and extract to {}",
                url,
                GOST_BIN
            );
        }
    }

    // 解压出 gost 二进制
    let status = Command::new("tar")
        .args(["-xzf", &tarball, "-C", tmp_dir, "gost"])
        .status()
        .context("failed to extract gost tarball")?;
    if !status.success() {
        bail!("tar extraction failed: {}", tarball);
    }

    // 安装到 /usr/local/bin（需 sudo）
    let extracted = format!("{}/gost", tmp_dir);
    let status = Command::new("sudo")
        .args(["install", "-m", "755", &extracted, GOST_BIN])
        .status()
        .context("failed to install gost to /usr/local/bin (sudo required)")?;
    if !status.success() {
        bail!("sudo install failed; make sure you have sudo privileges");
    }

    std::fs::remove_file(&tarball).ok();
    std::fs::remove_file(&extracted).ok();

    let version = installed_version().ok_or_else(|| anyhow!("gost -V still fails after install"))?;
    println!("{} gost installed: {}", "✓".green(), version);
    Ok(())
}

/// curl 下载（-L 跟随重定向，--noproxy '*' 绕开尚未生效/环境里的代理）
fn download(url: &str, dest: &str) -> bool {
    println!("{} downloading {}", "ℹ".blue(), url);
    Command::new("curl")
        .args(["-fL", "--noproxy", "*", "--max-time", "120", "-o", dest, url])
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// 检测 CPU 架构，映射到 gost 的 release 命名
fn detect_arch() -> Result<&'static str> {
    let output = Command::new("uname")
        .arg("-m")
        .output()
        .context("failed to run uname -m")?;
    let arch = String::from_utf8_lossy(&output.stdout).trim().to_string();
    match arch.as_str() {
        "x86_64" | "amd64" => Ok("amd64"),
        "aarch64" | "arm64" => Ok("arm64"),
        other => Err(anyhow!("unsupported architecture: {}", other)),
    }
}
