use crate::auto::lockscreen_state::LockscreenState;
use colored::Colorize;
use std::sync::{Arc, Mutex};
use std::thread;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum MonitorError {
    #[error("Failed to start monitor thread: {0}")]
    #[allow(dead_code)]
    ThreadError(String),
    #[error("Windows API error: {0}")]
    #[allow(dead_code)]
    WindowsApiError(String),
}

/// Windows 锁屏监控器
pub struct LockscreenMonitor {
    state: Arc<Mutex<LockscreenState>>,
    min_interval_seconds: u32,
    is_running: Arc<Mutex<bool>>,
}

impl LockscreenMonitor {
    /// 创建新的监控器
    pub fn new(state: Arc<Mutex<LockscreenState>>, min_interval_seconds: u32) -> Self {
        Self {
            state,
            min_interval_seconds,
            is_running: Arc::new(Mutex::new(false)),
        }
    }

    /// 启动监控（在后台线程）
    pub fn start(&self) -> Result<(), MonitorError> {
        let state = Arc::clone(&self.state);
        let min_interval = self.min_interval_seconds;
        let is_running = Arc::clone(&self.is_running);

        {
            let mut running = is_running.lock().unwrap();
            if *running {
                return Ok(()); // 已经在运行
            }
            *running = true;
        }

        thread::spawn(move || {
            Self::monitor_loop(state, min_interval, is_running);
        });

        Ok(())
    }

    /// 监控循环（Windows 平台）
    #[cfg(target_os = "windows")]
    fn monitor_loop(
        state: Arc<Mutex<LockscreenState>>,
        min_interval_seconds: u32,
        is_running: Arc<Mutex<bool>>,
    ) {
        use windows::Win32::System::RemoteDesktop::{
            WTSRegisterSessionNotification, WTSUnRegisterSessionNotification,
        };
        use windows::Win32::UI::WindowsAndMessaging::{
            CreateWindowExW, DispatchMessageW, GetMessageW,
            TranslateMessage, HWND_MESSAGE, MSG, WINDOW_EX_STYLE, WINDOW_STYLE,
            WNDCLASSW, CS_HREDRAW, CS_VREDRAW,
        };
        use windows::Win32::Foundation::HWND;
        use windows::core::{w, PCWSTR};

        println!("{}", "🔍 Starting Windows session monitor...".cyan().bold());

        // 创建隐藏窗口用于接收消息
        let class_name = w!("YoLockscreenMonitor");

        unsafe {
            let wc = WNDCLASSW {
                style: CS_HREDRAW | CS_VREDRAW,
                lpfnWndProc: Some(Self::window_proc),
                cbClsExtra: 0,
                cbWndExtra: 0,
                hInstance: windows::Win32::System::LibraryLoader::GetModuleHandleW(None).unwrap().into(),
                hIcon: Default::default(),
                hCursor: Default::default(),
                hbrBackground: Default::default(),
                lpszMenuName: PCWSTR::null(),
                lpszClassName: class_name,
            };

            if windows::Win32::UI::WindowsAndMessaging::RegisterClassW(&wc) == 0 {
                // Ignore already exists error (1410)
                // Just continue if registration failed
            }

            let hwnd = CreateWindowExW(
                WINDOW_EX_STYLE(0),
                class_name,
                w!("YoLockscreenMonitor"),
                WINDOW_STYLE(0),
                0, 0, 0, 0,
                HWND_MESSAGE,
                None,
                windows::Win32::System::LibraryLoader::GetModuleHandleW(None).unwrap(),
                None,
            );

            if hwnd.0 == 0 {
                println!("{}", "✗ Failed to create window".red());
                return;
            }

            // 注册会话通知
            if WTSRegisterSessionNotification(hwnd, 0).is_err() {
                println!("{}", "✗ Failed to register session notification".red());
                return;
            }

            println!("{}", "✓ Windows session monitor started successfully".green().bold());

            // 消息循环
            let state_clone = Arc::clone(&state);
            let min_interval = min_interval_seconds;

            // 设置窗口的用户数据，存储 state 和 min_interval
            // 注意：这里使用全局变量或其他方式传递状态
            MONITOR_STATE.lock().unwrap().replace((state_clone, min_interval));

            let mut msg = MSG::default();
            while GetMessageW(&mut msg, HWND(0), 0, 0).as_bool() {
                TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }

            // 清理
            WTSUnRegisterSessionNotification(hwnd).ok();
            *is_running.lock().unwrap() = false;
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
        // Session change constants
        #[allow(dead_code)]
        const WTS_SESSION_LOCK: u32 = 0x7;
        #[allow(dead_code)]
        const WTS_SESSION_UNLOCK: u32 = 0x8;
        use windows::Win32::UI::WindowsAndMessaging::{DefWindowProcW, WM_WTSSESSION_CHANGE};

        if msg == WM_WTSSESSION_CHANGE {
            let event = wparam.0 as u32;

            if let Some((state, min_interval)) = MONITOR_STATE.lock().unwrap().as_ref() {
                let state = Arc::clone(state);
                let min_interval = *min_interval;

                match event {
                    WTS_SESSION_LOCK => {
                        // 锁屏事件
                        println!("{}", "🔒 Screen locked detected".yellow().bold());
                    }
                    WTS_SESSION_UNLOCK => {
                        // 解锁事件 - 只在时间窗口内记录
                        let in_window = {
                            let s = state.lock().unwrap();
                            s.in_time_window
                        };

                        if in_window {
                            let mut s = state.lock().unwrap();
                            s.record_unlock(min_interval);
                            println!(
                                "{}",
                                format!(
                                    "🔓 Screen unlocked detected! Unlock count: {}, New interval: {} seconds",
                                    s.unlock_count,
                                    s.current_interval_seconds
                                )
                                .yellow()
                                .bold()
                            );
                        } else {
                            println!("{}", "🔓 Screen unlocked detected (outside time window, not counted)".blue());
                        }
                    }
                    _ => {}
                }
            }
        }

        DefWindowProcW(hwnd, msg, wparam, lparam)
    }

    /// 非 Windows 平台的监控循环（占位符）
    #[cfg(not(target_os = "windows"))]
    fn monitor_loop(
        _state: Arc<Mutex<LockscreenState>>,
        _min_interval_seconds: u32,
        _is_running: Arc<Mutex<bool>>,
    ) {
        println!("{}", "⚠ Lockscreen monitoring is only supported on Windows".yellow().bold());
    }

    /// 停止监控
    #[allow(dead_code)]
    pub fn stop(&self) {
        let mut running = self.is_running.lock().unwrap();
        *running = false;
    }
}

// 全局状态存储（用于在 window_proc 中访问）
#[cfg(target_os = "windows")]
lazy_static::lazy_static! {
    static ref MONITOR_STATE: Mutex<Option<(Arc<Mutex<LockscreenState>>, u32)>> = Mutex::new(None);
}
