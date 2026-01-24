//! 单实例锁

use std::fs;
use std::io::{Read, Write};
use std::path::PathBuf;
use thiserror::Error;

#[cfg(windows)]
use std::process::Command;

#[derive(Debug, Error)]
pub enum LockError {
    #[error("Another instance is already running (PID: {0})")]
    AlreadyRunning(u32),
    #[error("Failed to create lock file: {0}")]
    CreateError(String),
    #[error("Failed to read lock file: {0}")]
    ReadError(String),
    #[error("HOME environment variable not set")]
    HomeNotSet,
}

pub struct InstanceLock {
    lock_path: PathBuf,
}

impl InstanceLock {
    /// Create a new instance lock
    pub fn new() -> Result<Self, LockError> {
        let home = std::env::var("HOME").map_err(|_| LockError::HomeNotSet)?;
        let lock_path = PathBuf::from(home).join(".yo").join("yo-auto.pid");
        Ok(Self { lock_path })
    }

    /// Try to acquire the lock
    pub fn try_acquire(&self) -> Result<(), LockError> {
        if let Some(parent) = self.lock_path.parent() {
            fs::create_dir_all(parent).map_err(|e| LockError::CreateError(e.to_string()))?;
        }

        if self.lock_path.exists() {
            let mut file =
                fs::File::open(&self.lock_path).map_err(|e| LockError::ReadError(e.to_string()))?;
            let mut content = String::new();
            file.read_to_string(&mut content)
                .map_err(|e| LockError::ReadError(e.to_string()))?;

            if let Ok(pid) = content.trim().parse::<u32>() {
                if Self::is_process_running(pid) {
                    return Err(LockError::AlreadyRunning(pid));
                }
            }
            let _ = fs::remove_file(&self.lock_path);
        }

        let current_pid = std::process::id();
        let mut file =
            fs::File::create(&self.lock_path).map_err(|e| LockError::CreateError(e.to_string()))?;
        write!(file, "{}", current_pid).map_err(|e| LockError::CreateError(e.to_string()))?;

        Ok(())
    }

    /// Release the lock
    pub fn release(&self) {
        let _ = fs::remove_file(&self.lock_path);
    }

    /// Get the path to the lock file
    #[allow(dead_code)]
    pub fn lock_path(&self) -> &PathBuf {
        &self.lock_path
    }

    /// Check if a process with given PID is running
    #[cfg(windows)]
    pub(crate) fn is_process_running(pid: u32) -> bool {
        let output = Command::new("tasklist")
            .args(["/FI", &format!("PID eq {}", pid), "/NH"])
            .output();

        match output {
            Ok(output) => {
                let stdout = String::from_utf8_lossy(&output.stdout);
                !stdout.contains("No tasks") && stdout.contains(&pid.to_string())
            }
            Err(_) => false,
        }
    }

    #[cfg(not(windows))]
    pub(crate) fn is_process_running(pid: u32) -> bool {
        use std::path::Path;
        Path::new(&format!("/proc/{}", pid)).exists()
    }
}

impl Drop for InstanceLock {
    fn drop(&mut self) {
        self.release();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_process_running_current() {
        let current_pid = std::process::id();
        assert!(InstanceLock::is_process_running(current_pid));
    }

    #[test]
    fn test_is_process_running_invalid() {
        assert!(!InstanceLock::is_process_running(999999999));
    }
}
