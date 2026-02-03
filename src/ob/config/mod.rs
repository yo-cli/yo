pub mod ipv6;
pub mod oceanbase;
pub mod sysctl;

use anyhow::Result;

#[derive(Debug, Clone, PartialEq)]
pub enum ConfigStatus {
    Exists,   // Config exists with correct value
    Missing,  // Config does not exist (needs to be added)
    Conflict, // Config exists but value differs
}

#[derive(Debug, Clone)]
pub struct ConfigItem {
    pub key: String,
    pub value: String,
    pub status: ConfigStatus,
    pub current_value: Option<String>,
}

impl ConfigItem {
    pub fn new(key: String, value: String) -> Self {
        Self {
            key,
            value,
            status: ConfigStatus::Missing,
            current_value: None,
        }
    }

    pub fn needs_update(&self) -> bool {
        matches!(self.status, ConfigStatus::Missing | ConfigStatus::Conflict)
    }
}

pub trait ConfigFile {
    fn path(&self) -> &str;
    fn expected_configs(&self) -> Vec<ConfigItem>;
    fn parse_existing(&mut self) -> Result<()>;
    fn generate_content(&self) -> String;
    fn check(&mut self) -> Result<Vec<ConfigItem>>;
    fn apply(&mut self, force: bool) -> Result<()>;
}
