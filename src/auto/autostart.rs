use std::fs;
use std::io::Write;
use std::path::PathBuf;
use thiserror::Error;

#[cfg(windows)]
use std::process::Command;

const AUTOSTART_SCRIPT_NAME: &str = "yo-auto-web.vbs";

#[derive(Debug, Error)]
pub enum AutostartError {
    #[error("Git Bash not found. Please install Git for Windows.")]
    GitBashNotFound,
    #[error("Failed to get startup folder path: {0}")]
    StartupFolderError(String),
    #[error("Failed to create autostart script: {0}")]
    CreateScriptError(String),
    #[error("Failed to remove autostart script: {0}")]
    RemoveScriptError(String),
    #[error("Failed to read autostart script: {0}")]
    ReadScriptError(String),
    #[cfg(not(windows))]
    #[error("This feature is only supported on Windows")]
    NotWindows,
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct AutostartConfig {
    pub git_bash_path: PathBuf,
    pub startup_folder: PathBuf,
    pub script_path: PathBuf,
    pub port: u16,
}

#[derive(Debug, Clone)]
pub struct AutostartStatus {
    pub enabled: bool,
    pub script_path: Option<PathBuf>,
    pub port: Option<u16>,
    pub git_bash_path: Option<PathBuf>,
}

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
                // git.exe is usually in Git\cmd\git.exe, git-bash.exe is in Git\git-bash.exe
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

    /// Detect Git installation path from Windows registry
    #[cfg(windows)]
    fn detect_from_registry() -> Option<PathBuf> {
        // Try to read from registry using reg query
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
        // Parse output like: "    InstallPath    REG_SZ    C:\Program Files\Git"
        for line in stdout.lines() {
            if line.contains("InstallPath") && line.contains("REG_SZ") {
                let parts: Vec<&str> = line.split("REG_SZ").collect();
                if parts.len() >= 2 {
                    let path = parts[1].trim();
                    return Some(PathBuf::from(path));
                }
            }
        }

        None
    }

    /// Get Windows Startup folder path
    #[cfg(windows)]
    pub fn get_startup_folder() -> Result<PathBuf, AutostartError> {
        // %APPDATA%\Microsoft\Windows\Start Menu\Programs\Startup
        let appdata = std::env::var("APPDATA")
            .map_err(|_| AutostartError::StartupFolderError("APPDATA not set".to_string()))?;

        let startup = PathBuf::from(appdata)
            .join("Microsoft")
            .join("Windows")
            .join("Start Menu")
            .join("Programs")
            .join("Startup");

        if !startup.exists() {
            return Err(AutostartError::StartupFolderError(
                format!("Startup folder not found: {:?}", startup)
            ));
        }

        Ok(startup)
    }

    #[cfg(not(windows))]
    pub fn get_startup_folder() -> Result<PathBuf, AutostartError> {
        Err(AutostartError::NotWindows)
    }

    /// Install autostart script
    #[cfg(windows)]
    pub fn install(port: u16) -> Result<AutostartConfig, AutostartError> {
        let git_bash_path = Self::detect_git_bash()?;
        let startup_folder = Self::get_startup_folder()?;
        let script_path = startup_folder.join(AUTOSTART_SCRIPT_NAME);

        // Get yo executable path
        let yo_path = std::env::current_exe()
            .map_err(|e| AutostartError::CreateScriptError(e.to_string()))?;

        // Create VBS script content
        // VBS script will start git-bash with yo command
        let vbs_content = format!(
            r#"Set WshShell = CreateObject("WScript.Shell")
WshShell.Run """{}""" & " -c ""'{}' run auto --web {}""", 1, False
"#,
            git_bash_path.display().to_string().replace("\\", "\\\\"),
            yo_path.display().to_string().replace("\\", "/"),
            port
        );

        // Write VBS script
        let mut file = fs::File::create(&script_path)
            .map_err(|e| AutostartError::CreateScriptError(e.to_string()))?;
        file.write_all(vbs_content.as_bytes())
            .map_err(|e| AutostartError::CreateScriptError(e.to_string()))?;

        Ok(AutostartConfig {
            git_bash_path,
            startup_folder,
            script_path,
            port,
        })
    }

    #[cfg(not(windows))]
    pub fn install(_port: u16) -> Result<AutostartConfig, AutostartError> {
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
                port: None,
                git_bash_path: None,
            });
        }

        // Read script to extract port
        let content = fs::read_to_string(&script_path)
            .map_err(|e| AutostartError::ReadScriptError(e.to_string()))?;

        // Parse port from script content (look for "--web XXXX")
        let port = content
            .split("--web ")
            .nth(1)
            .and_then(|s| s.split('"').next())
            .and_then(|s| s.trim().parse::<u16>().ok());

        // Try to detect git bash path
        let git_bash_path = Self::detect_git_bash().ok();

        Ok(AutostartStatus {
            enabled: true,
            script_path: Some(script_path),
            port,
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
        // This test may fail if Git is not installed
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
