use crate::s5::proxy_manager::S5ProxyManager;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum S5Error {
    #[error("S5 proxy setup failed: {0}")]
    ProxySetupFailed(String),
}

pub struct S5Command;

impl S5Command {
    /// 执行 S5 命令
    pub fn execute(interactive: bool) -> Result<(), S5Error> {
        S5ProxyManager::run_socks5_proxy(interactive)
            .map_err(|e| S5Error::ProxySetupFailed(format!("{}", e)))
    }
}
