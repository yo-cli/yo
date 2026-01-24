//! 默认规则生成

use colored::Colorize;
use std::fs;
use std::path::PathBuf;

const EXAMPLE_RULE: &str = r#"// 示例规则 - 可以复制此文件创建新规则
//
// name: 显示名称（在 Web UI 中显示）
// description: 简短描述（在列表中显示）
//
// trigger 配置:
//   time_range: ["开始", "结束"]  - 支持跨午夜如 ["22:00", "06:00"]
//   interval_minutes: 5           - 执行间隔（分钟）
//   events: ["tick", "unlock"]    - 事件类型
//   weekdays: [1,2,3,4,5]         - 星期几 (1=周一)
//   enabled: true/false           - 是否启用
//
// 时间 API: hour(), minute(), weekday(), is_weekend(), in_time_range(s,e)
// 状态 API: inc_counter(n), get_counter(n), set_flag(n,b), get_flag(n)
// 屏幕 API: screen_locked(), lock_screen()
// 动作 API: speak(text), shutdown(secs), chime(hour)
// 工具 API: reset_if_new_day(prefix), log(msg)
// 配置 API: get_env(key), has_env(key)
//
// TTS 配置 (Settings > Environment Variables):
//   TTS_API_KEY = "appid|token"
//   TTS_VOICE = "zh_female_wanwanxiaohe_moon_bigtts"

let name = "示例规则";
let description = "21:30-05:00 每5分钟锁屏示例";

let trigger = #{
    time_range: ["21:30", "05:00"],
    interval_minutes: 5,
    events: ["tick", "unlock"],
    enabled: false,
};

fn on_tick() {
    reset_if_new_day("example");
    if get_flag("example_shutdown") {
        speak("关机警告");
        shutdown(30);
        return;
    }
    speak("提醒内容");
    lock_screen();
}

fn on_unlock() {
    let count = inc_counter("example_unlock");
    let remaining = 3 - count;
    if remaining <= 0 {
        set_flag("example_shutdown", true);
    }
    speak("第" + count + "次解锁");
}
"#;

/// 创建默认规则文件
pub fn create(rules_dir: &PathBuf) -> Result<(), String> {
    fs::write(rules_dir.join("example.rhai"), EXAMPLE_RULE)
        .map_err(|e| e.to_string())?;

    println!("{}", format!("✓ Created default rules in {}", rules_dir.display()).green().bold());
    println!("{}", "  Copy example.rhai to create your own rules".yellow());
    println!("{}", "  Configure TTS in Web UI > Settings > Environment Variables".yellow());

    Ok(())
}
