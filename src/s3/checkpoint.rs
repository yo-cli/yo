// Checkpoint for resumable multi-day runs. Written atomically (tmp + rename)
// after every completed object and on exit, so a crash never leaves a torn file.

use anyhow::{bail, Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;

use super::config::ConfigSnapshot;

pub const CHECKPOINT_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Checkpoint {
    pub version: u32,
    pub run_id: String,
    pub config: ConfigSnapshot,
    pub completed_iterations: u64,
    /// Bytes of fully completed objects only — partial uploads never count.
    pub completed_bytes: u64,
    /// Immediate cost burned so far, micro-dollars (see budget.rs).
    pub burned_micro: u64,
    pub started_at: DateTime<Utc>,
    /// Accumulated active run time across resumes, for throughput accounting.
    pub active_secs: u64,
    pub slowdown_total: u64,
    pub error_total: u64,
}

impl Checkpoint {
    pub fn new(run_id: String, config: ConfigSnapshot) -> Self {
        Self {
            version: CHECKPOINT_VERSION,
            run_id,
            config,
            completed_iterations: 0,
            completed_bytes: 0,
            burned_micro: 0,
            started_at: Utc::now(),
            active_secs: 0,
            slowdown_total: 0,
            error_total: 0,
        }
    }

    /// Atomic write: serialize to a temp file, fsync, then rename over `path`.
    /// The temp name carries the pid so that even a run forced past the
    /// single-instance lock cannot have two processes writing one temp file —
    /// the second one's `create` truncates what the first is about to rename.
    pub fn save(&self, path: &Path) -> Result<()> {
        let tmp = path.with_file_name(format!(
            "{}.{}.tmp",
            path.file_name().unwrap_or_default().to_string_lossy(),
            std::process::id()
        ));
        let json = serde_json::to_string_pretty(self)?;
        {
            use std::io::Write;
            let mut f = fs::File::create(&tmp)
                .with_context(|| format!("创建临时 checkpoint 失败: {}", tmp.display()))?;
            f.write_all(json.as_bytes())?;
            f.sync_all()?;
        }
        fs::rename(&tmp, path)
            .with_context(|| format!("checkpoint 原子替换失败: {}", path.display()))?;
        Ok(())
    }

    pub fn load(path: &Path) -> Result<Self> {
        let raw = fs::read_to_string(path)
            .with_context(|| format!("读取 checkpoint 失败: {}", path.display()))?;
        let ckpt: Checkpoint = serde_json::from_str(&raw)
            .with_context(|| format!("checkpoint 格式非法: {}", path.display()))?;
        if ckpt.version != CHECKPOINT_VERSION {
            bail!(
                "checkpoint 版本 {} 与当前工具版本 {} 不兼容",
                ckpt.version,
                CHECKPOINT_VERSION
            );
        }
        Ok(ckpt)
    }

    /// Refuse to resume when the effective config differs from the snapshot —
    /// silently continuing with different layout/target would corrupt the run.
    pub fn validate_config(&self, current: &ConfigSnapshot) -> Result<()> {
        let diffs = self.config.diff(current);
        if !diffs.is_empty() {
            bail!(
                "配置与 checkpoint 快照不一致,拒绝续跑:\n  {}\n(想全新开始请删除 checkpoint 文件后重跑)",
                diffs.join("\n  ")
            );
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::s3::config::RateMode;
    use crate::s3::modes::ModeId;
    use crate::s3::MIB;

    fn snapshot() -> ConfigSnapshot {
        ConfigSnapshot {
            mode: ModeId::Crr,
            transfer_acceleration: false,
            bucket: "b".into(),
            key_prefix: "p/".into(),
            budget_micro: 1_000_000,
            endpoint_url: None,
            object_size: 100 * MIB,
            part_size: 10 * MIB,
            rate_min: MIB,
            rate_max: 2 * MIB,
            rate_mode: RateMode::Continuous,
            retain_secs: 86400,
        }
    }

    fn tmp_path(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("yo-s3-ckpt-test-{}-{}.json", name, uuid::Uuid::new_v4()))
    }

    #[test]
    fn roundtrip_and_no_tmp_left_behind() {
        let path = tmp_path("roundtrip");
        let mut ckpt = Checkpoint::new("run-1".into(), snapshot());
        ckpt.completed_iterations = 3;
        ckpt.burned_micro = 123_456;
        ckpt.save(&path).unwrap();
        let tmp = path.with_file_name(format!(
            "{}.{}.tmp",
            path.file_name().unwrap().to_string_lossy(),
            std::process::id()
        ));
        assert!(!tmp.exists());

        let loaded = Checkpoint::load(&path).unwrap();
        assert_eq!(loaded.completed_iterations, 3);
        assert_eq!(loaded.burned_micro, 123_456);
        assert_eq!(loaded.run_id, "run-1");
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn overwrite_is_atomic_replacement() {
        let path = tmp_path("overwrite");
        let mut ckpt = Checkpoint::new("run-1".into(), snapshot());
        ckpt.save(&path).unwrap();
        ckpt.completed_iterations = 9;
        ckpt.save(&path).unwrap();
        assert_eq!(Checkpoint::load(&path).unwrap().completed_iterations, 9);
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn config_mismatch_is_rejected_with_diff() {
        let ckpt = Checkpoint::new("run-1".into(), snapshot());
        let mut other = snapshot();
        other.part_size = 20 * MIB;
        let err = ckpt.validate_config(&other).unwrap_err().to_string();
        assert!(err.contains("part_size"), "{}", err);
    }

    #[test]
    fn corrupt_file_is_a_clear_error() {
        let path = tmp_path("corrupt");
        std::fs::write(&path, "{not json").unwrap();
        assert!(Checkpoint::load(&path).is_err());
        std::fs::remove_file(&path).unwrap();
    }
}
