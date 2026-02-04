//! 天气数据类型

use serde::{Deserialize, Serialize};

/// 天气信息
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WeatherInfo {
    /// 天气状况
    pub weather: String,
    /// 温度 (摄氏度)
    pub temp: i32,
    /// 体感温度
    pub feels_like: i32,
    /// 相对湿度 (%)
    pub humidity: i32,
    /// 风向
    pub wind_dir: String,
    /// 风力等级
    pub wind_scale: String,
}

/// 和风天气 API 响应
#[derive(Debug, Deserialize)]
pub struct QWeatherResponse {
    pub code: String,
    pub now: Option<QWeatherNow>,
}

#[derive(Debug, Deserialize)]
pub struct QWeatherNow {
    pub text: String,
    pub temp: String,
    #[serde(rename = "feelsLike")]
    pub feels_like: String,
    pub humidity: String,
    #[serde(rename = "windDir")]
    pub wind_dir: String,
    #[serde(rename = "windScale")]
    pub wind_scale: String,
}
