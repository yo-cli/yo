//! Windows 锁屏监控器

use crate::auto::rhai::{trigger_lock_event, trigger_unlock_event};
use colored::Colorize;
use std::sync::Mutex;
use std::thread;

/// 监控器是否已启动
static MONITOR_STARTED: Mutex<bool> = Mutex::new(false);

/// 获取当前屏幕锁定状态（通过检测 LogonUI.exe 进程）
#[cfg(target_os = "windows")]
pub fn is_screen_locked() -> bool {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
        TH32CS_SNAPPROCESS,
    };

    unsafe {
        let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);

        if let Ok(snapshot) = snapshot {
            let mut process_entry = PROCESSENTRY32W {
                dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
                ..Default::default()
            };

            if Process32FirstW(snapshot, &mut process_entry).is_ok() {
                loop {
                    let process_name = String::from_utf16_lossy(&process_entry.szExeFile);
                    let process_name = process_name.trim_end_matches('\0');

                    // LogonUI.exe = Windows 锁屏界面进程
                    if process_name.eq_ignore_ascii_case("LogonUI.exe") {
                        let _ = CloseHandle(snapshot);
                        return true;
                    }

                    if Process32NextW(snapshot, &mut process_entry).is_err() {
                        break;
                    }
                }
            }
            let _ = CloseHandle(snapshot);
        }
    }
    false
}

#[cfg(not(target_os = "windows"))]
pub fn is_screen_locked() -> bool {
    false
}

/// Windows 锁屏监控器
pub struct LockscreenMonitor;

impl LockscreenMonitor {
    /// 启动全局监控（只启动一次）
    pub fn start_global_monitor() -> Result<(), String> {
        let mut started = MONITOR_STARTED.lock().unwrap();
        if *started {
            return Ok(());
        }
        *started = true;
        drop(started);

        thread::spawn(move || {
            Self::monitor_loop();
        });

        Ok(())
    }

    /// 监控循环（Windows 平台）
    #[cfg(target_os = "windows")]
    fn monitor_loop() {
        use windows::core::{w, PCWSTR};
        use windows::Win32::System::RemoteDesktop::{
            WTSRegisterSessionNotification, WTSUnRegisterSessionNotification,
        };
        use windows::Win32::UI::WindowsAndMessaging::{
            CreateWindowExW, DispatchMessageW, GetMessageW, TranslateMessage, CS_HREDRAW,
            CS_VREDRAW, HWND_MESSAGE, MSG, WINDOW_EX_STYLE, WINDOW_STYLE, WNDCLASSW,
        };

        println!(
            "{}",
            "🔍 Starting Windows session monitor...".cyan().bold()
        );

        let class_name = w!("YoLockscreenMonitor");

        unsafe {
            let hmodule = windows::Win32::System::LibraryLoader::GetModuleHandleW(None).unwrap();
            let wc = WNDCLASSW {
                style: CS_HREDRAW | CS_VREDRAW,
                lpfnWndProc: Some(Self::window_proc),
                cbClsExtra: 0,
                cbWndExtra: 0,
                hInstance: hmodule.into(),
                hIcon: Default::default(),
                hCursor: Default::default(),
                hbrBackground: Default::default(),
                lpszMenuName: PCWSTR::null(),
                lpszClassName: class_name,
            };

            let _ = windows::Win32::UI::WindowsAndMessaging::RegisterClassW(&wc);

            let hinstance: windows::Win32::Foundation::HINSTANCE = hmodule.into();
            let hwnd = match CreateWindowExW(
                WINDOW_EX_STYLE(0),
                class_name,
                w!("YoLockscreenMonitor"),
                WINDOW_STYLE(0),
                0,
                0,
                0,
                0,
                Some(HWND_MESSAGE),
                None,
                Some(hinstance.into()),
                None,
            ) {
                Ok(h) => h,
                Err(_) => {
                    println!("{}", "✗ Failed to create monitor window".red());
                    return;
                }
            };

            if WTSRegisterSessionNotification(hwnd, 0).is_err() {
                println!("{}", "✗ Failed to register session notification".red());
                return;
            }

            println!(
                "{}",
                "✓ Windows session monitor started".green().bold()
            );

            let mut msg = MSG::default();
            while GetMessageW(&mut msg, None, 0, 0).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }

            let _ = WTSUnRegisterSessionNotification(hwnd);
        }
    }

    /// 窗口过程（处理会话变化消息）
    #[cfg(target_os = "windows")]
    unsafe extern "system" fn window_proc(
        hwnd: windows::Win32::Foundation::HWND,
        msg: u32,
        wparam: windows::Win32::Foundation::WPARAM,
        lparam: windows::Win32::Foundation::LPARAM,
    ) -> windows::Win32::Foundation::LRESULT {
        const WTS_SESSION_LOCK: u32 = 0x7;
        const WTS_SESSION_UNLOCK: u32 = 0x8;
        use windows::Win32::UI::WindowsAndMessaging::{DefWindowProcW, WM_WTSSESSION_CHANGE};

        if msg == WM_WTSSESSION_CHANGE {
            match wparam.0 as u32 {
                WTS_SESSION_LOCK => {
                    println!("{}", "🔒 Screen locked".yellow().bold());
                    trigger_lock_event();
                }
                WTS_SESSION_UNLOCK => {
                    println!("{}", "🔓 Screen unlocked".yellow().bold());
                    trigger_unlock_event();
                }
                _ => {}
            }
        }

        DefWindowProcW(hwnd, msg, wparam, lparam)
    }

    /// 非 Windows 平台
    #[cfg(not(target_os = "windows"))]
    fn monitor_loop() {
        println!(
            "{}",
            "⚠ Lockscreen monitoring only supported on Windows".yellow()
        );
    }
}
