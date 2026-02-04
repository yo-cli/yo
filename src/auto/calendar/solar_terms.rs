//! 二十四节气

use chrono::{Datelike, Local, NaiveDate};

/// 节气名称
const SOLAR_TERMS: [&str; 24] = [
    "小寒", "大寒", "立春", "雨水", "惊蛰", "春分",
    "清明", "谷雨", "立夏", "小满", "芒种", "夏至",
    "小暑", "大暑", "立秋", "处暑", "白露", "秋分",
    "寒露", "霜降", "立冬", "小雪", "大雪", "冬至",
];

/// 获取今日节气（如果有）
pub fn get_today_solar_term() -> Option<String> {
    let today = Local::now().date_naive();
    let year = today.year();

    // 获取当年所有节气日期
    let terms = get_solar_terms_for_year(year);

    for (date, name) in terms {
        if date == today {
            return Some(name.to_string());
        }
    }

    None
}

/// 获取指定年份的所有节气日期
/// 使用寿星公式近似计算
fn get_solar_terms_for_year(year: i32) -> Vec<(NaiveDate, &'static str)> {
    let mut result = Vec::new();

    // 节气近似计算公式的世纪常数
    // 21世纪的C值
    let c_values: [f64; 24] = [
        5.4055,  // 小寒
        20.12,   // 大寒
        3.87,    // 立春
        18.73,   // 雨水
        5.63,    // 惊蛰
        20.646,  // 春分
        4.81,    // 清明
        20.1,    // 谷雨
        5.52,    // 立夏
        21.04,   // 小满
        5.678,   // 芒种
        21.37,   // 夏至
        7.108,   // 小暑
        22.83,   // 大暑
        7.5,     // 立秋
        23.13,   // 处暑
        7.646,   // 白露
        23.042,  // 秋分
        8.318,   // 寒露
        23.438,  // 霜降
        7.438,   // 立冬
        22.36,   // 小雪
        7.18,    // 大雪
        21.94,   // 冬至
    ];

    // 每个节气所在的月份
    let months: [u32; 24] = [
        1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6,
        7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12,
    ];

    let y = (year % 100) as f64;
    let century_offset = if year >= 2000 { 0.0 } else { 1.0 };

    for i in 0..24 {
        let c = c_values[i] - century_offset;
        let month = months[i];

        // 寿星公式: 日期 = [Y*D+C] - [Y/4]
        // D = 0.2422
        let d = 0.2422;
        let day = ((y * d + c).floor() - (y / 4.0).floor()) as u32;

        if let Some(date) = NaiveDate::from_ymd_opt(year, month, day) {
            result.push((date, SOLAR_TERMS[i]));
        }
    }

    result
}
