//! 默认规则生成

use colored::Colorize;
use std::fs;
use std::path::Path;

const EXAMPLE_RULE: &str = r#"// 示例规则 - 可以复制此文件创建新规则
//
// name: 显示名称（在 Web UI 中显示）
// description: 简短描述（在列表中显示）
//
// state: 脚本状态（自动持久化）
//   - 直接读写: state.unlocks += 1
//   - 离开时间范围时 on_destroy 会被调用，可在此重置状态
//
// trigger 配置:
//   time_range: ["开始", "结束"]  - 支持跨午夜如 ["22:00", "06:00"]
//   interval_minutes: 5           - 执行间隔（分钟）
//   events: ["tick", "unlock"]    - 事件类型
//   weekdays: [1,2,3,4,5]         - 星期几 (1=周一)
//   enabled: true/false           - 是否启用
//
// 生命周期:
//   on_mount()   - 程序启动时调用
//   on_tick()    - 在时间范围内，每分钟调用
//   on_unlock()  - 在时间范围内，屏幕解锁时调用
//   on_destroy() - 离开时间范围时调用（用于重置状态）
//
// 时间 API: hour(), minute(), weekday(), is_weekend(), in_time_range(s,e)
// 屏幕 API: screen_locked(), lock_screen()
// 动作 API: speak(text), shutdown(secs), chime(hour)
// 工具 API: log(msg)
// 配置 API: get_env(key), has_env(key)
//
// TTS 配置 (Settings > Environment Variables):
//   TTS_API_KEY = "appid|token"
//   TTS_VOICE = "zh_female_wanwanxiaohe_moon_bigtts"

let name = "示例规则";
let description = "21:30-05:00 每5分钟锁屏示例";

// 脚本状态（自动持久化）
let state = #{
    unlocks: 0,
    max_unlocks: 3,
};

let trigger = #{
    time_range: ["21:30", "05:00"],
    interval_minutes: 5,
    events: ["tick", "unlock"],
    enabled: false,
};

// 程序启动时调用
fn on_mount() {
    log("规则已加载");
}

// 离开时间范围时调用
fn on_destroy() {
    state.unlocks = 0;
    log("已重置解锁次数");
}

fn on_tick() {
    if screen_locked() { return; }
    if state.unlocks >= state.max_unlocks {
        speak("关机警告");
        shutdown(30);
        return;
    }
    speak("提醒内容");
    lock_screen();
}

fn on_unlock() {
    state.unlocks += 1;
    let remaining = state.max_unlocks - state.unlocks;
    if remaining > 0 {
        speak(`第${state.unlocks}次解锁，还剩${remaining}次`);
    } else {
        speak("已达最大解锁次数，下次锁屏将关机");
    }
}
"#;

/// 创建默认规则文件
pub fn create(rules_dir: &Path) -> Result<(), String> {
    fs::write(rules_dir.join("example.rhai"), EXAMPLE_RULE)
        .map_err(|e| e.to_string())?;

    println!("{}", format!("✓ Created default rules in {}", rules_dir.display()).green().bold());
    println!("{}", "  Copy example.rhai to create your own rules".yellow());
    println!("{}", "  Configure TTS in Web UI > Settings > Environment Variables".yellow());

    Ok(())
}
