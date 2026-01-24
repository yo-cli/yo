//! Web 类型定义

use crate::auto::rhai::RhaiScheduler;
use std::sync::{Arc, Mutex};

/// Web 应用状态
pub struct WebState {
    pub scheduler: Arc<Mutex<RhaiScheduler>>,
}

impl WebState {
    pub fn new(scheduler: Arc<Mutex<RhaiScheduler>>) -> Self {
        Self { scheduler }
    }
}
