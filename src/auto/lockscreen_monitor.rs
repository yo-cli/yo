use crate::auto::lockscreen_state::LockscreenState;
use crate::auto::tts::VolcengineTtsClient;
use colored::Colorize;
use std::collections::HashMap;
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

/// 监控器模式
#[derive(Debug, Clone)]
pub enum MonitorMode {
    /// 自适应锁屏模式（减半间隔）
    Adaptive { min_interval_seconds: u32 },
    /// 重复锁屏模式（简单计数，可触发关机）
    Repeated {
        max_unlocks: Option<u32>,
        tts_api_key: Option<String>,
        tts_voice: Option<String>,
    },
}

/// 任务监控信息
#[derive(Clone)]
struct TaskMonitorInfo {
    state: Arc<Mutex<LockscreenState>>,
    mode: MonitorMode,
}

/// Windows 锁屏监控器
pub struct LockscreenMonitor;

impl LockscreenMonitor {
    /// 注册任务到全局监控
    pub fn register_task(
        task_name: String,
        state: Arc<Mutex<LockscreenState>>,
        mode: MonitorMode,
    ) {
        let mut tasks = MONITOR_TASKS.lock().unwrap();
        tasks.insert(task_name.clone(), TaskMonitorInfo { state, mode });
        println!(
            "{}",
            format!("  📝 Registered task '{}' for unlock monitoring", task_name)
                .blue()
        );
    }

    /// 启动全局监控（只启动一次）
    pub fn start_global_monitor() -> Result<(), MonitorError> {
        let mut started = MONITOR_STARTED.lock().unwrap();
        if *started {
            return Ok(()); // 已经启动
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

            let mut msg = MSG::default();
            while GetMessageW(&mut msg, HWND(0), 0, 0).as_bool() {
                TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }

            // 清理
            WTSUnRegisterSessionNotification(hwnd).ok();
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

            match event {
                WTS_SESSION_LOCK => {
                    // 锁屏事件
                    println!("{}", "🔒 Screen locked detected".yellow().bold());
                }
                WTS_SESSION_UNLOCK => {
                    // 解锁事件 - 遍历所有注册的任务，找到活跃的任务
                    let tasks = MONITOR_TASKS.lock().unwrap();
                    let mut found_active = false;

                    for (task_name, info) in tasks.iter() {
                        let in_window = {
                            let s = info.state.lock().unwrap();
                            s.in_time_window
                        };

                        if in_window {
                            found_active = true;
                            Self::handle_unlock_for_task(task_name, info);
                        }
                    }

                    if !found_active {
                        println!("{}", "🔓 Screen unlocked (no active task in time window)".blue());
                    }
                }
                _ => {}
            }
        }

        DefWindowProcW(hwnd, msg, wparam, lparam)
    }

    /// 处理单个任务的解锁事件
    fn handle_unlock_for_task(task_name: &str, info: &TaskMonitorInfo) {
        match &info.mode {
            MonitorMode::Adaptive { min_interval_seconds } => {
                // 自适应模式：减半间隔
                let mut s = info.state.lock().unwrap();
                s.record_unlock(*min_interval_seconds);
                println!(
                    "{}",
                    format!(
                        "🔓 [{}] Unlock count: {}, New interval: {} seconds",
                        task_name,
                        s.unlock_count,
                        s.current_interval_seconds
                    )
                    .yellow()
                    .bold()
                );
            }
            MonitorMode::Repeated { max_unlocks, ref tts_api_key, ref tts_voice } => {
                // 重复锁屏模式：简单计数
                let (count, reached_max) = {
                    let mut s = info.state.lock().unwrap();
                    s.record_unlock_simple()
                };

                if let Some(max) = max_unlocks {
                    let remaining = max.saturating_sub(count);

                    if reached_max {
                        println!(
                            "{}",
                            format!(
                                "⚠️ [{}] Maximum unlock count ({}) reached! Shutdown will be triggered on next lock screen.",
                                task_name, max
                            )
                            .red()
                            .bold()
                        );
                        // 播放最终警告
                        Self::play_warning_tts(
                            tts_api_key.as_deref(),
                            tts_voice.as_deref(),
                            "已达到最大解锁次数，下次锁屏将触发关机",
                        );
                    } else {
                        println!(
                            "{}",
                            format!(
                                "🔓 [{}] Unlock count: {}/{}, {} remaining",
                                task_name, count, max, remaining
                            )
                            .yellow()
                            .bold()
                        );
                        // 每次解锁都播放剩余次数提醒
                        Self::play_warning_tts(
                            tts_api_key.as_deref(),
                            tts_voice.as_deref(),
                            &format!("第{}次解锁，再解锁{}次将触发关机", count, remaining),
                        );
                    }
                } else {
                    println!(
                        "{}",
                        format!("🔓 [{}] Unlock count: {}", task_name, count)
                            .yellow()
                            .bold()
                    );
                }
            }
        }
    }

    /// 非 Windows 平台的监控循环（占位符）
    #[cfg(not(target_os = "windows"))]
    fn monitor_loop() {
        println!("{}", "⚠ Lockscreen monitoring is only supported on Windows".yellow().bold());
    }

    /// 播放警告语音
    fn play_warning_tts(api_key: Option<&str>, voice: Option<&str>, text: &str) {
        if let (Some(key), Some(v)) = (api_key, voice) {
            let client = VolcengineTtsClient::new(key.to_string());
            if let Err(e) = client.synthesize_and_play(text, v) {
                println!(
                    "{}",
                    format!("⚠ Failed to play warning TTS: {}", e).yellow()
                );
            }
        } else {
            println!(
                "{}",
                "⚠ TTS warning skipped (no API key or voice configured)".yellow()
            );
        }
    }

}

// 全局状态存储（用于在 window_proc 中访问）
lazy_static::lazy_static! {
    /// 所有注册任务的监控信息
    static ref MONITOR_TASKS: Mutex<HashMap<String, TaskMonitorInfo>> = Mutex::new(HashMap::new());
    /// 监控器是否已启动
    static ref MONITOR_STARTED: Mutex<bool> = Mutex::new(false);
}
