//! 和风天气 API 客户端 (JWT Ed25519 认证)

use super::types::{QWeatherResponse, WeatherInfo};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use ed25519_dalek::{Signer, SigningKey};
use std::time::{SystemTime, UNIX_EPOCH};

/// 和风天气客户端
pub struct QWeatherClient {
    credential_id: String,  // 凭据ID，用于 JWT header 的 kid
    project_id: String,     // 项目ID，用于 JWT payload 的 sub
    private_key: String,
}

impl QWeatherClient {
    pub fn new(credential_id: String, project_id: String, private_key: String) -> Self {
        Self {
            credential_id,
            project_id,
            private_key,
        }
    }

    /// 生成 JWT Token
    fn generate_jwt(&self) -> Result<String, String> {
        // 解析 OpenSSH 私钥
        let signing_key = self.parse_openssh_private_key()?;

        // JWT Header (kid = 凭据ID)
        let header = serde_json::json!({
            "alg": "EdDSA",
            "kid": self.credential_id
        });
        let header_b64 = URL_SAFE_NO_PAD.encode(header.to_string().as_bytes());

        // JWT Payload (sub = 项目ID)
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let payload = serde_json::json!({
            "sub": self.project_id,
            "iat": now - 30,       // 建议设置为当前时间前30秒
            "exp": now + 300       // 5分钟有效期
        });
        let payload_b64 = URL_SAFE_NO_PAD.encode(payload.to_string().as_bytes());

        // 签名
        let message = format!("{}.{}", header_b64, payload_b64);
        let signature = signing_key.sign(message.as_bytes());
        let signature_b64 = URL_SAFE_NO_PAD.encode(signature.to_bytes());

        Ok(format!("{}.{}.{}", header_b64, payload_b64, signature_b64))
    }

    /// 解析 OpenSSH 格式的 Ed25519 私钥
    fn parse_openssh_private_key(&self) -> Result<SigningKey, String> {
        use ssh_key::PrivateKey;

        // 清理私钥字符串
        let key_str = self.private_key.trim();

        // 提取 base64 内容
        let base64_content = key_str
            .strip_prefix("-----BEGIN OPENSSH PRIVATE KEY-----")
            .and_then(|s| s.strip_suffix("-----END OPENSSH PRIVATE KEY-----"))
            .ok_or("私钥格式错误：缺少 PEM 标记")?
            .trim();

        // 移除所有空白字符，重新格式化
        let clean_base64: String = base64_content
            .chars()
            .filter(|c| !c.is_whitespace())
            .collect();

        // 重建 PEM 格式（每 70 字符换行）
        let mut formatted = String::from("-----BEGIN OPENSSH PRIVATE KEY-----\n");
        for chunk in clean_base64.as_bytes().chunks(70) {
            formatted.push_str(std::str::from_utf8(chunk).unwrap_or(""));
            formatted.push('\n');
        }
        formatted.push_str("-----END OPENSSH PRIVATE KEY-----\n");

        let private_key = PrivateKey::from_openssh(&formatted)
            .map_err(|e| format!("解析私钥失败: {}", e))?;

        match private_key.key_data() {
            ssh_key::private::KeypairData::Ed25519(kp) => {
                let secret_bytes = kp.private.to_bytes();
                Ok(SigningKey::from_bytes(&secret_bytes))
            }
            _ => Err("不支持的密钥类型，需要 Ed25519".to_string()),
        }
    }

    /// 获取实时天气
    pub fn get_weather(&self, location: &str) -> Result<WeatherInfo, String> {
        // 生成 JWT
        let token = self.generate_jwt()?;

        // 先查询城市 ID
        let location_id = self.lookup_location(location, &token)?;

        // 调用实时天气 API (使用自定义 API Host)
        let url = format!(
            "https://kt5g7d9gx5.re.qweatherapi.com/v7/weather/now?location={}",
            location_id
        );

        let client = reqwest::blocking::Client::new();
        let response = client
            .get(&url)
            .header("Authorization", format!("Bearer {}", token))
            .send()
            .map_err(|e| format!("请求失败: {}", e))?;

        let status = response.status();
        let body_bytes = response.bytes().map_err(|e| format!("读取响应失败: {}", e))?;

        if !status.is_success() {
            let body_preview = String::from_utf8(body_bytes.to_vec())
                .unwrap_or_else(|_| format!("[二进制数据 {} 字节]", body_bytes.len()));
            return Err(format!("HTTP {}: {}", status, body_preview));
        }

        let data: QWeatherResponse = serde_json::from_slice(&body_bytes)
            .map_err(|e| {
                let body_preview = String::from_utf8(body_bytes.to_vec())
                    .unwrap_or_else(|_| format!("[二进制数据 {} 字节]", body_bytes.len()));
                format!("解析JSON失败: {} (响应: {})", e, body_preview)
            })?;

        if data.code != "200" {
            return Err(format!("API 错误: code={}", data.code));
        }

        let now = data.now.ok_or("无天气数据")?;

        Ok(WeatherInfo {
            weather: now.text,
            temp: now.temp.parse().unwrap_or(0),
            feels_like: now.feels_like.parse().unwrap_or(0),
            humidity: now.humidity.parse().unwrap_or(0),
            wind_dir: now.wind_dir,
            wind_scale: now.wind_scale,
        })
    }

    /// 查询城市 location ID
    fn lookup_location(&self, location: &str, token: &str) -> Result<String, String> {
        // 如果已经是数字 ID，直接返回
        if location.chars().all(|c| c.is_ascii_digit()) {
            return Ok(location.to_string());
        }

        let url = format!(
            "https://kt5g7d9gx5.re.qweatherapi.com/geo/v2/city/lookup?location={}",
            urlencoding::encode(location)
        );

        let client = reqwest::blocking::Client::new();
        let response = client
            .get(&url)
            .header("Authorization", format!("Bearer {}", token))
            .send()
            .map_err(|e| format!("城市查询失败: {}", e))?;

        let status = response.status();
        let body_bytes = response.bytes().map_err(|e| format!("读取响应失败: {}", e))?;

        if !status.is_success() {
            // 尝试解析为文本，如果失败则显示为十六进制
            let body_preview = String::from_utf8(body_bytes.to_vec())
                .unwrap_or_else(|_| format!("[二进制数据 {} 字节]", body_bytes.len()));
            return Err(format!("城市查询 HTTP {}: {}", status, body_preview));
        }

        let data: serde_json::Value = serde_json::from_slice(&body_bytes)
            .map_err(|e| {
                let body_preview = String::from_utf8(body_bytes.to_vec())
                    .unwrap_or_else(|_| format!("[二进制数据 {} 字节]", body_bytes.len()));
                format!("城市查询解析失败: {} (响应: {})", e, body_preview)
            })?;

        if data["code"].as_str() != Some("200") {
            let body_preview = String::from_utf8(body_bytes.to_vec())
                .unwrap_or_else(|_| format!("[二进制数据 {} 字节]", body_bytes.len()));
            return Err(format!("城市查询错误: code={}, 响应: {}",
                data["code"].as_str().unwrap_or("unknown"),
                body_preview));
        }

        data["location"]
            .get(0)
            .and_then(|loc| loc["id"].as_str())
            .map(|s| s.to_string())
            .ok_or_else(|| "未找到城市".to_string())
    }
}
