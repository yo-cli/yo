use std::thread;
use std::time::Duration;

#[cfg(target_os = "windows")]
fn is_screen_locked() -> bool {
    use windows::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Process32FirstW, Process32NextW,
        PROCESSENTRY32W, TH32CS_SNAPPROCESS,
    };
    use windows::Win32::Foundation::CloseHandle;

    unsafe {
        let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);

        if let Ok(snapshot) = snapshot {
            let mut process_entry = PROCESSENTRY32W {
                dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
                ..Default::default()
            };

            if Process32FirstW(snapshot, &mut process_entry).is_ok() {
                loop {
                    // 将进程名从 UTF-16 转换为字符串
                    let process_name = String::from_utf16_lossy(&process_entry.szExeFile);
                    let process_name = process_name.trim_end_matches('\0');

                    // 检查是否是 LogonUI.exe（锁屏界面进程）
                    if process_name.eq_ignore_ascii_case("LogonUI.exe") {
                        println!("Found LogonUI.exe process!");
                        let _ = CloseHandle(snapshot);
                        return true;
                    }

                    // 移动到下一个进程
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
fn is_screen_locked() -> bool {
    println!("Not supported on this platform");
    false
}

fn main() {
    println!("🔍 Testing lockscreen detection...");
    println!("Press Ctrl+C to stop\n");

    loop {
        let is_locked = is_screen_locked();

        let status = if is_locked {
            "🔒 LOCKED"
        } else {
            "🔓 UNLOCKED"
        };

        println!("[{}] Screen status: {}",
            chrono::Local::now().format("%H:%M:%S"),
            status
        );

        thread::sleep(Duration::from_secs(2));
    }
}
