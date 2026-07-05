// yo-forward: 把本机变成代理客户端 —— 起一个本地 gost，把 http://:port
// 的流量转发给一个已有的上游 SOCKS5 出口，并把 http_proxy 等写进 shell 配置。
// 与 yo-s5（代理服务端）是镜像对偶：一个对外提供代理，一个让本机用上代理。

pub mod commands;
pub mod gost_installer;
pub mod probe;
pub mod shell_env;
pub mod systemd_unit;

/// GOST 版本（与 yo-s5 的 gogost/gost:3 保持同一大版本）
pub const GOST_VERSION: &str = "3.2.6";
/// gost 二进制安装路径
pub const GOST_BIN: &str = "/usr/local/bin/gost";
/// systemd 服务名与单元文件路径
pub const SERVICE_NAME: &str = "gost";
pub const SERVICE_PATH: &str = "/etc/systemd/system/gost.service";

/// 默认上游 socks5 出口（机场客户端，通常在 Windows 宿主机）
pub const DEFAULT_UPSTREAM: &str = "127.0.0.1:30999";
/// 默认本地 http 代理端口
pub const DEFAULT_PORT: u16 = 8888;

/// yo-forward 配置：本地端口 + 上游出口
#[derive(Debug, Clone)]
pub struct ForwardConfig {
    pub upstream: String,
    pub local_port: u16,
}

impl ForwardConfig {
    /// 从可选参数构造，缺省时回落到零思考默认值
    pub fn new(upstream: Option<String>, local_port: Option<u16>) -> Self {
        Self {
            upstream: upstream.unwrap_or_else(|| DEFAULT_UPSTREAM.to_string()),
            local_port: local_port.unwrap_or(DEFAULT_PORT),
        }
    }
}
