use anyhow::{Context, Result};
use chrono::Local;
use std::fs;
use std::path::Path;
use std::process::Command;

/// Backup a file with timestamp
pub fn backup_file(file_path: &str) -> Result<String> {
    if !Path::new(file_path).exists() {
        return Ok(String::new());
    }

    let timestamp = Local::now().format("%Y%m%d-%H%M%S");
    let backup_path = format!("{}.backup-{}", file_path, timestamp);

    fs::copy(file_path, &backup_path)
        .with_context(|| format!("Failed to backup file: {}", file_path))?;

    Ok(backup_path)
}

/// Execute sysctl -p
#[allow(dead_code)]
pub fn apply_sysctl() -> Result<()> {
    let output = Command::new("sysctl")
        .arg("-p")
        .output()
        .context("Failed to execute sysctl -p")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow::anyhow!("sysctl -p failed: {}", stderr));
    }

    Ok(())
}

/// Execute sysctl --system (reload all sysctl configs)
pub fn apply_sysctl_system() -> Result<()> {
    let output = Command::new("sysctl")
        .arg("--system")
        .output()
        .context("Failed to execute sysctl --system")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow::anyhow!("sysctl --system failed: {}", stderr));
    }

    Ok(())
}

/// Verify sysctl parameter
pub fn verify_sysctl(key: &str, expected_value: &str) -> Result<bool> {
    let output = Command::new("sysctl")
        .arg(key)
        .output()
        .with_context(|| format!("Failed to read sysctl {}", key))?;

    if !output.status.success() {
        return Ok(false);
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let actual_value = stdout
        .trim()
        .strip_prefix(&format!("{} = ", key))
        .unwrap_or("")
        .trim();

    Ok(actual_value == expected_value)
}

/// Read file content, return empty string if not exists
pub fn read_file_safe(file_path: &str) -> String {
    fs::read_to_string(file_path).unwrap_or_default()
}

/// Write file
pub fn write_file(file_path: &str, content: &str) -> Result<()> {
    fs::write(file_path, content)
        .with_context(|| format!("Failed to write file: {}", file_path))?;
    Ok(())
}
