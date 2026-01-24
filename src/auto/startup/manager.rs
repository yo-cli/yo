//! 自启动管理器

use super::error::AutostartError;
use super::types::{AutostartConfig, AutostartStatus};
use std::fs;
use std::io::Write;
use std::path::PathBuf;

#[cfg(windows)]
use std::process::Command;

const AUTOSTART_SCRIPT_NAME: &str = "yo-auto.vbs";

pub struct AutostartManager;

impl AutostartManager {
    /// Detect Git Bash installation path
    #[cfg(windows)]
    pub fn detect_git_bash() -> Result<PathBuf, AutostartError> {
        // Method 1: Check registry
        if let Some(path) = Self::detect_from_registry() {
            let git_bash = path.join("git-bash.exe");
            if git_bash.exists() {
                return Ok(git_bash);
            }
        }

        // Method 2: Check common installation paths
        let common_paths = [
            r"C:\Program Files\Git\git-bash.exe",
            r"C:\Program Files (x86)\Git\git-bash.exe",
            r"D:\Program Files\Git\git-bash.exe",
            r"D:\Git\git-bash.exe",
        ];

        for path in &common_paths {
            let path = PathBuf::from(path);
            if path.exists() {
                return Ok(path);
            }
        }

        // Method 3: Check PATH environment
        if let Ok(output) = Command::new("where").arg("git").output() {
            let stdout = String::from_utf8_lossy(&output.stdout);
            for line in stdout.lines() {
                let git_path = PathBuf::from(line.trim());
                if let Some(git_dir) = git_path.parent().and_then(|p| p.parent()) {
                    let git_bash = git_dir.join("git-bash.exe");
                    if git_bash.exists() {
                        return Ok(git_bash);
                    }
                }
            }
        }

        Err(AutostartError::GitBashNotFound)
    }

    #[cfg(not(windows))]
    pub fn detect_git_bash() -> Result<PathBuf, AutostartError> {
        Err(AutostartError::NotWindows)
    }

    #[cfg(windows)]
    fn detect_from_registry() -> Option<PathBuf> {
        let output = Command::new("reg")
            .args([
                "query",
                r"HKEY_LOCAL_MACHINE\SOFTWARE\GitForWindows",
                "/v",
                "InstallPath",
            ])
            .output()
            .ok()?;

        let stdout = String::from_utf8_lossy(&output.stdout);
        for line in stdout.lines() {
            if line.contains("InstallPath") && line.contains("REG_SZ") {
                let parts: Vec<&str> = line.split("REG_SZ").collect();
                if parts.len() >= 2 {
                    return Some(PathBuf::from(parts[1].trim()));
                }
            }
        }

        None
    }

    #[cfg(windows)]
    pub fn get_startup_folder() -> Result<PathBuf, AutostartError> {
        let appdata = std::env::var("APPDATA")
            .map_err(|_| AutostartError::StartupFolderError("APPDATA not set".to_string()))?;

        let startup = PathBuf::from(appdata)
            .join("Microsoft")
            .join("Windows")
            .join("Start Menu")
            .join("Programs")
            .join("Startup");

        if !startup.exists() {
            return Err(AutostartError::StartupFolderError(format!(
                "Startup folder not found: {:?}",
                startup
            )));
        }

        Ok(startup)
    }

    #[cfg(not(windows))]
    pub fn get_startup_folder() -> Result<PathBuf, AutostartError> {
        Err(AutostartError::NotWindows)
    }

    /// Install autostart script
    #[cfg(windows)]
    pub fn install() -> Result<AutostartConfig, AutostartError> {
        let git_bash_path = Self::detect_git_bash()?;
        let startup_folder = Self::get_startup_folder()?;
        let script_path = startup_folder.join(AUTOSTART_SCRIPT_NAME);

        let yo_path = std::env::current_exe()
            .map_err(|e| AutostartError::CreateScriptError(e.to_string()))?;

        // VBS script runs: git-bash -c 'yo run auto'
        let vbs_content = format!(
            r#"Set WshShell = CreateObject("WScript.Shell")
WshShell.Run """{}""" & " -c ""'{}' run auto""", 1, False
"#,
            git_bash_path.display().to_string().replace("\\", "\\\\"),
            yo_path.display().to_string().replace("\\", "/"),
        );

        let mut file = fs::File::create(&script_path)
            .map_err(|e| AutostartError::CreateScriptError(e.to_string()))?;
        file.write_all(vbs_content.as_bytes())
            .map_err(|e| AutostartError::CreateScriptError(e.to_string()))?;

        Ok(AutostartConfig {
            git_bash_path,
            startup_folder,
            script_path,
        })
    }

    #[cfg(not(windows))]
    pub fn install() -> Result<AutostartConfig, AutostartError> {
        Err(AutostartError::NotWindows)
    }

    /// Remove autostart script
    #[cfg(windows)]
    pub fn remove() -> Result<(), AutostartError> {
        let startup_folder = Self::get_startup_folder()?;
        let script_path = startup_folder.join(AUTOSTART_SCRIPT_NAME);

        if script_path.exists() {
            fs::remove_file(&script_path)
                .map_err(|e| AutostartError::RemoveScriptError(e.to_string()))?;
        }

        Ok(())
    }

    #[cfg(not(windows))]
    pub fn remove() -> Result<(), AutostartError> {
        Err(AutostartError::NotWindows)
    }

    /// Get current autostart status
    #[cfg(windows)]
    pub fn status() -> Result<AutostartStatus, AutostartError> {
        let startup_folder = Self::get_startup_folder()?;
        let script_path = startup_folder.join(AUTOSTART_SCRIPT_NAME);

        if !script_path.exists() {
            return Ok(AutostartStatus {
                enabled: false,
                script_path: None,
                git_bash_path: None,
            });
        }

        let git_bash_path = Self::detect_git_bash().ok();

        Ok(AutostartStatus {
            enabled: true,
            script_path: Some(script_path),
            git_bash_path,
        })
    }

    #[cfg(not(windows))]
    pub fn status() -> Result<AutostartStatus, AutostartError> {
        Err(AutostartError::NotWindows)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg(windows)]
    fn test_detect_git_bash() {
        let result = AutostartManager::detect_git_bash();
        println!("Git Bash detection result: {:?}", result);
    }

    #[test]
    #[cfg(windows)]
    fn test_get_startup_folder() {
        let result = AutostartManager::get_startup_folder();
        assert!(result.is_ok());
        println!("Startup folder: {:?}", result.unwrap());
    }
}
