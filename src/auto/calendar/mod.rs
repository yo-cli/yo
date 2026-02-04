//! 中国日历模块 - 节日、节气

mod festivals;
mod solar_terms;

pub use festivals::{get_today_festival, days_until_spring_festival};
pub use solar_terms::get_today_solar_term;

/// 获取今日特殊日期信息（节日或节气）
pub fn get_today_special() -> Option<String> {
    // 优先返回节日
    if let Some(festival) = get_today_festival() {
        return Some(festival);
    }
    // 其次返回节气
    if let Some(term) = get_today_solar_term() {
        return Some(term);
    }
    None
}

/// 获取星期几的中文名称
pub fn weekday_name(weekday: u32) -> &'static str {
    match weekday {
        1 => "星期一",
        2 => "星期二",
        3 => "星期三",
        4 => "星期四",
        5 => "星期五",
        6 => "星期六",
        7 => "星期日",
        _ => "未知",
    }
}
