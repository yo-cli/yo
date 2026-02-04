//! 中国节日

use chrono::{Datelike, Local, NaiveDate};

/// 获取今日节日（如果有）
pub fn get_today_festival() -> Option<String> {
    let today = Local::now().date_naive();
    let (month, day) = (today.month(), today.day());

    // 公历节日
    let solar_festival = match (month, day) {
        (1, 1) => Some("元旦"),
        (2, 14) => Some("情人节"),
        (3, 8) => Some("妇女节"),
        (3, 12) => Some("植树节"),
        (4, 1) => Some("愚人节"),
        (5, 1) => Some("劳动节"),
        (5, 4) => Some("青年节"),
        (6, 1) => Some("儿童节"),
        (7, 1) => Some("建党节"),
        (8, 1) => Some("建军节"),
        (9, 10) => Some("教师节"),
        (10, 1) => Some("国庆节"),
        (12, 25) => Some("圣诞节"),
        _ => None,
    };

    if let Some(f) = solar_festival {
        return Some(f.to_string());
    }

    // 农历节日（预计算的日期，需要定期更新）
    let lunar_festival = get_lunar_festival(today);
    if let Some(f) = lunar_festival {
        return Some(f.to_string());
    }

    None
}

/// 获取农历节日（通过预计算的公历日期）
fn get_lunar_festival(date: NaiveDate) -> Option<&'static str> {
    let year = date.year();
    let (month, day) = (date.month(), date.day());

    // 春节日期 (正月初一)
    let spring_festivals = [
        (2024, 2, 10),
        (2025, 1, 29),
        (2026, 2, 17),
        (2027, 2, 6),
        (2028, 1, 26),
        (2029, 2, 13),
        (2030, 2, 3),
    ];

    // 元宵节 (正月十五，春节+14天)
    // 清明节（公历4月4日或5日）
    // 端午节 (五月初五)
    let dragon_boats = [
        (2024, 6, 10),
        (2025, 5, 31),
        (2026, 6, 19),
        (2027, 6, 9),
        (2028, 5, 28),
        (2029, 6, 16),
        (2030, 6, 5),
    ];

    // 中秋节 (八月十五)
    let mid_autumns = [
        (2024, 9, 17),
        (2025, 10, 6),
        (2026, 9, 25),
        (2027, 9, 15),
        (2028, 10, 3),
        (2029, 9, 22),
        (2030, 9, 12),
    ];

    // 重阳节 (九月初九)
    let double_ninths = [
        (2024, 10, 11),
        (2025, 10, 29),
        (2026, 10, 18),
        (2027, 10, 8),
        (2028, 10, 26),
        (2029, 10, 16),
        (2030, 10, 5),
    ];

    // 除夕 (春节前一天)
    for (y, m, d) in &spring_festivals {
        if year == *y && month == *m as u32 && day == *d as u32 {
            return Some("春节");
        }
        // 除夕
        if let Some(eve) = NaiveDate::from_ymd_opt(*y, *m as u32, *d as u32)
            .and_then(|d| d.pred_opt())
        {
            if date == eve {
                return Some("除夕");
            }
        }
        // 元宵节
        if let Some(lantern) = NaiveDate::from_ymd_opt(*y, *m as u32, *d as u32)
            .and_then(|d| d.checked_add_days(chrono::Days::new(14)))
        {
            if date == lantern {
                return Some("元宵节");
            }
        }
    }

    // 清明节 (公历4月4日或5日)
    if month == 4 && (day == 4 || day == 5) {
        // 简化处理，实际清明节日期需要计算
        if day == 4 && (year % 4 == 0 && (year % 100 != 0 || year % 400 == 0)) {
            return Some("清明节");
        } else if day == 5 {
            return Some("清明节");
        } else if day == 4 {
            return Some("清明节");
        }
    }

    // 端午节
    for (y, m, d) in &dragon_boats {
        if year == *y && month == *m as u32 && day == *d as u32 {
            return Some("端午节");
        }
    }

    // 中秋节
    for (y, m, d) in &mid_autumns {
        if year == *y && month == *m as u32 && day == *d as u32 {
            return Some("中秋节");
        }
    }

    // 重阳节
    for (y, m, d) in &double_ninths {
        if year == *y && month == *m as u32 && day == *d as u32 {
            return Some("重阳节");
        }
    }

    None
}

/// 计算距离下一个春节的天数
pub fn days_until_spring_festival() -> i64 {
    let today = Local::now().date_naive();

    // 春节日期表
    let spring_festivals = [
        (2024, 2, 10),
        (2025, 1, 29),
        (2026, 2, 17),
        (2027, 2, 6),
        (2028, 1, 26),
        (2029, 2, 13),
        (2030, 2, 3),
    ];

    for (y, m, d) in &spring_festivals {
        if let Some(festival_date) = NaiveDate::from_ymd_opt(*y, *m as u32, *d as u32) {
            if festival_date > today {
                return (festival_date - today).num_days();
            }
        }
    }

    // 如果超出预设范围，返回估算值
    365
}
