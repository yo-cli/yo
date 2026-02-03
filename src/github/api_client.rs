use reqwest::blocking::Client;
use reqwest::header::{HeaderMap, HeaderValue, ACCEPT, AUTHORIZATION, CONTENT_TYPE, USER_AGENT};
use serde::{Deserialize, Serialize};
use std::time::Duration;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum APIError {
    #[error("HTTP error {code}: {message}")]
    HttpError { code: u16, message: String },
    #[error("Network error: {0}")]
    NetworkError(String),
    #[error("JSON parse error: {0}")]
    JsonError(String),
    #[error("Request failed: {0}")]
    RequestFailed(String),
    #[error("Token invalid or expired")]
    TokenInvalid,
    #[error("Token lacks required permissions. For fine-grained tokens, ensure 'Account permissions > Read access to profile' is enabled")]
    InsufficientTokenPermissions,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserInfo {
    #[serde(default)]
    pub login: Option<String>,
    pub name: Option<String>,
    #[serde(default)]
    pub id: Option<i64>,
}

impl UserInfo {
    /// Get the login name, returning "unknown" if not available
    pub fn get_login(&self) -> &str {
        self.login.as_deref().unwrap_or("unknown")
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepoInfo {
    pub name: String,
    pub full_name: String,
    #[serde(default)]
    pub owner: OwnerInfo,
    pub private: bool,
    #[serde(default)]
    pub permissions: RepoPermissions,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct OwnerInfo {
    pub login: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RepoPermissions {
    #[serde(default)]
    pub admin: bool,
    #[serde(default)]
    pub push: bool,
    #[serde(default)]
    pub pull: bool,
}

impl RepoInfo {
    pub fn get_permission_level(&self) -> String {
        if self.permissions.admin {
            "admin".to_string()
        } else if self.permissions.push {
            "write".to_string()
        } else {
            "read".to_string()
        }
    }
}

#[derive(Debug, Serialize)]
struct DeployKeyRequest {
    title: String,
    key: String,
    read_only: bool,
}

pub struct GitHubAPIClient {
    client: Client,
    token: String,
}

impl GitHubAPIClient {
    pub fn new(token: String) -> Result<Self, APIError> {
        let client = Client::builder()
            .timeout(Duration::from_secs(10))
            .build()
            .map_err(|e| APIError::NetworkError(format!("Failed to create client: {}", e)))?;

        Ok(Self { client, token })
    }

    fn build_headers(&self) -> Result<HeaderMap, APIError> {
        let mut headers = HeaderMap::new();

        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {}", self.token))
                .map_err(|e| APIError::RequestFailed(format!("Invalid token: {}", e)))?,
        );

        headers.insert(
            ACCEPT,
            HeaderValue::from_static("application/vnd.github.v3+json"),
        );

        headers.insert(USER_AGENT, HeaderValue::from_static("yo-github-tool/1.0"));

        Ok(headers)
    }

    /// 验证 token 并获取用户信息
    pub fn verify_token(&self) -> Result<UserInfo, APIError> {
        let headers = self.build_headers()?;

        let response = self
            .client
            .get("https://api.github.com/user")
            .headers(headers)
            .send()
            .map_err(|e| APIError::NetworkError(format!("Request failed: {}", e)))?;

        let status = response.status();
        let status_code = status.as_u16();

        if status.is_success() {
            let user_info: UserInfo = response
                .json()
                .map_err(|e| APIError::JsonError(format!("Failed to parse user info: {}", e)))?;

            // Check if we got a valid login (fine-grained tokens might return null)
            if user_info.login.is_none() {
                return Err(APIError::InsufficientTokenPermissions);
            }

            Ok(user_info)
        } else {
            let error_body = response.text().unwrap_or_default();
            let message = if let Ok(json) = serde_json::from_str::<serde_json::Value>(&error_body) {
                json.get("message")
                    .and_then(|m| m.as_str())
                    .unwrap_or("Unknown error")
                    .to_string()
            } else {
                format!("HTTP error {}", status_code)
            };

            match status_code {
                401 => Err(APIError::TokenInvalid),
                403 => Err(APIError::InsufficientTokenPermissions),
                _ => Err(APIError::HttpError {
                    code: status_code,
                    message,
                }),
            }
        }
    }

    /// 获取仓库信息
    pub fn get_repository_info(&self, owner: &str, repo: &str) -> Result<RepoInfo, APIError> {
        let headers = self.build_headers()?;
        let url = format!("https://api.github.com/repos/{}/{}", owner, repo);

        let response = self
            .client
            .get(&url)
            .headers(headers)
            .send()
            .map_err(|e| APIError::NetworkError(format!("Request failed: {}", e)))?;

        let status = response.status();
        let status_code = status.as_u16();

        if status.is_success() {
            response
                .json::<RepoInfo>()
                .map_err(|e| APIError::JsonError(format!("Failed to parse repo info: {}", e)))
        } else {
            let error_body = response.text().unwrap_or_default();
            let api_message = if let Ok(json) = serde_json::from_str::<serde_json::Value>(&error_body) {
                json.get("message")
                    .and_then(|m| m.as_str())
                    .unwrap_or("Unknown error")
                    .to_string()
            } else {
                "Unknown error".to_string()
            };

            let message = match status_code {
                401 => "Token invalid or expired".to_string(),
                403 => format!("Access denied to {}/{}. For fine-grained tokens, ensure the token has access to this repository.", owner, repo),
                404 => format!("Repository {}/{} not found or token lacks access. Check: 1) Repository exists 2) Token has repository access", owner, repo),
                _ => api_message,
            };

            Err(APIError::HttpError {
                code: status_code,
                message,
            })
        }
    }

    /// 添加 Deploy Key
    pub fn add_deploy_key(
        &self,
        owner: &str,
        repo: &str,
        title: &str,
        public_key: &str,
        read_only: bool,
    ) -> Result<(), APIError> {
        let headers = self.build_headers()?;
        let url = format!("https://api.github.com/repos/{}/{}/keys", owner, repo);

        let request_body = DeployKeyRequest {
            title: title.to_string(),
            key: public_key.to_string(),
            read_only,
        };

        let mut request_headers = headers.clone();
        request_headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));

        let response = self
            .client
            .post(&url)
            .headers(request_headers)
            .json(&request_body)
            .send()
            .map_err(|e| APIError::NetworkError(format!("Request failed: {}", e)))?;

        let status = response.status();
        let status_code = status.as_u16();

        if status.is_success() {
            Ok(())
        } else {
            let error_body = response.text().unwrap_or_default();
            let api_message = if let Ok(json) = serde_json::from_str::<serde_json::Value>(&error_body) {
                json.get("message")
                    .and_then(|m| m.as_str())
                    .unwrap_or("Unknown error")
                    .to_string()
            } else {
                "Unknown error".to_string()
            };

            let message = match status_code {
                401 => "Token invalid or expired".to_string(),
                403 => "Permission denied. Token needs 'admin' access to repository (Classic: 'repo' scope, Fine-grained: 'Administration: Read and write')".to_string(),
                404 => format!("Repository not found or no access: {}", api_message),
                422 => {
                    if api_message.contains("key is already in use") {
                        "Deploy key already exists for this repository".to_string()
                    } else {
                        api_message
                    }
                }
                _ => api_message,
            };

            Err(APIError::HttpError {
                code: status_code,
                message,
            })
        }
    }
}
