//! 密码管理器 - 修改/恢复 Windows 用户密码
//!
//! 原密码从 GlobalConfig (WINDOWS_PASSWORD) 读取
//! change_password(): 改为固定难输入字符串
//! restore_password(): 改回原密码

use crate::auto::config::GlobalConfig;
use colored::Colorize;
use std::path::PathBuf;

/// 固定的替代密码（26个字母倒序）
const LOCK_PASSWORD: &str = "zyxwvutsrqponmlkjihgfedcba";
/// GlobalConfig 中的密码键名
const PASSWORD_KEY: &str = "WINDOWS_PASSWORD";
/// 标记文件名
const MARKER_FILE: &str = "password_changed";

pub struct PasswordManager;

impl PasswordManager {
    fn marker_path() -> PathBuf {
        let home = std::env::var("USERPROFILE")
            .or_else(|_| std::env::var("HOME"))
            .unwrap_or_else(|_| ".".to_string());
        PathBuf::from(home).join(".yo").join(MARKER_FILE)
    }

    /// 密码是否已被修改
    pub fn is_password_changed() -> bool {
        Self::marker_path().exists()
    }

    /// 从 GlobalConfig 读取原密码
    fn load_password() -> Result<String, String> {
        let config = GlobalConfig::load();
        config
            .get(PASSWORD_KEY)
            .cloned()
            .filter(|p| !p.is_empty())
            .ok_or_else(|| format!("未配置 {}，请在 ~/.yo/config.json 中设置", PASSWORD_KEY))
    }

    fn set_marker() -> Result<(), String> {
        std::fs::write(Self::marker_path(), "1")
            .map_err(|e| format!("写入标记文件失败: {}", e))
    }

    fn clear_marker() {
        let _ = std::fs::remove_file(Self::marker_path());
    }

    /// 修改密码为固定字符串
    pub fn change_password() -> Result<(), String> {
        if Self::is_password_changed() {
            return Ok(());
        }
        let original = Self::load_password()?;
        match Self::win32_change_password(&original, LOCK_PASSWORD) {
            Ok(()) => {
                Self::set_marker()?;
                println!("{}", "🔒 密码已修改".cyan());
                Ok(())
            }
            Err(e) if e.contains("86") => {
                // error 86 = 原密码不匹配，密码可能已是 LOCK_PASSWORD 但 marker 丢失
                // 尝试自愈：先恢复再改密
                println!("{}", "⚠ 检测到密码状态不一致，尝试自愈...".yellow());
                match Self::win32_change_password(LOCK_PASSWORD, &original) {
                    Ok(()) => {
                        // 恢复成功，说明确实是 LOCK_PASSWORD，重新改密
                        Self::win32_change_password(&original, LOCK_PASSWORD)?;
                        Self::set_marker()?;
                        println!("{}", "🔒 密码已修改（自愈成功）".cyan());
                        Ok(())
                    }
                    Err(_) => {
                        // 恢复也失败，配置的密码可能有误
                        Err(format!("密码状态不一致且自愈失败，请检查 {} 配置", PASSWORD_KEY))
                    }
                }
            }
            Err(e) => Err(e),
        }
    }

    /// 恢复原密码
    pub fn restore_password() -> Result<(), String> {
        if !Self::is_password_changed() {
            return Ok(());
        }
        let original = Self::load_password()?;
        Self::win32_change_password(LOCK_PASSWORD, &original)?;
        Self::clear_marker();
        println!("{}", "🔓 密码已恢复".green());
        Ok(())
    }

    /// 启动时检查并恢复
    pub fn check_and_restore() {
        if Self::is_password_changed() {
            println!("{}", "⚠ 检测到未恢复的密码，正在自动恢复...".yellow());
            match Self::restore_password() {
                Ok(()) => {}
                Err(e) => println!("{}", format!("✗ 密码恢复失败: {}", e).red().bold()),
            }
        }
    }

    #[cfg(target_os = "windows")]
    fn win32_change_password(old_password: &str, new_password: &str) -> Result<(), String> {
        use windows::core::HSTRING;
        use windows::Win32::NetworkManagement::NetManagement::NetUserChangePassword;

        let username = Self::get_current_username()?;
        let result = unsafe {
            NetUserChangePassword(
                None,
                &HSTRING::from(&username),
                &HSTRING::from(old_password),
                &HSTRING::from(new_password),
            )
        };

        if result == 0 {
            Ok(())
        } else {
            Err(format!("NetUserChangePassword 失败 (错误码: {})", result))
        }
    }

    #[cfg(target_os = "windows")]
    fn get_current_username() -> Result<String, String> {
        use windows::core::PWSTR;
        use windows::Win32::System::WindowsProgramming::GetUserNameW;

        unsafe {
            let mut size: u32 = 0;
            let _ = GetUserNameW(None, &mut size);
            if size == 0 {
                return Err("获取用户名缓冲区大小失败".into());
            }
            let mut buffer = vec![0u16; size as usize];
            GetUserNameW(Some(PWSTR(buffer.as_mut_ptr())), &mut size)
                .map_err(|e| format!("GetUserNameW 失败: {}", e))?;
            Ok(String::from_utf16_lossy(
                &buffer[..(size as usize).saturating_sub(1)],
            ))
        }
    }

    #[cfg(not(target_os = "windows"))]
    fn win32_change_password(_old_password: &str, _new_password: &str) -> Result<(), String> {
        Err("密码修改仅支持 Windows".into())
    }
}
