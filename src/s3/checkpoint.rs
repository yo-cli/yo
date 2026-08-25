// Checkpoint for resumable multi-day runs. Written atomically (tmp + rename)
// after every completed object and on exit, so a crash never leaves a torn file.

use anyhow::{bail, Context, Result};
use colored::Colorize;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;

use super::config::ConfigSnapshot;
use super::quota::PlanLedger;

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
    /// `--days` ledger: which hour and day are being spent, and how much of
    /// each is gone. Default (all zero) when the run has no ceilings, and in
    /// checkpoints written before `--days` existed. Without it a restart would
    /// draw itself a second hour's quota, and a second day's.
    #[serde(default)]
    pub plan: PlanLedger,
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
            plan: PlanLedger::default(),
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

    /// Refuse to resume only when the data would move somewhere the retention
    /// sweeper no longer looks — everything already written under the old
    /// bucket/prefix would then bill storage forever with nothing collecting it.
    ///
    /// Everything else (pricing, object size, naming, budget) is reported and
    /// allowed: `burned_micro` is a scalar that only accumulates, so none of it
    /// can make the money already spent wrong.
    pub fn validate_config(&self, current: &ConfigSnapshot) -> Result<()> {
        let diff = self.config.diff(current);
        if !diff.blocking.is_empty() {
            bail!(
                "数据位置与 checkpoint 不一致,拒绝续跑:\n  {}\n\
                 旧位置写下的对象会掉出清扫范围、永远计存储费。\n\
                 想换位置请先 yo-s3 cleanup 清掉旧数据,再删除 checkpoint 重跑",
                diff.blocking.join("\n  ")
            );
        }
        for note in &diff.notes {
            println!("{} 续跑时改动: {}", "ℹ".blue(), note);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::s3::modes::ModeId;
    use crate::s3::MIB;

    fn snapshot() -> ConfigSnapshot {
        ConfigSnapshot {
            mode: ModeId::Crr,
            transfer_acceleration: false,
            bucket: "b".into(),
            key_prefix: "p/".into(),
            budget_micro: 1_000_000,
            daily_cap_micro: 0,
            endpoint_url: None,
            object_size_min: 100 * MIB,
            object_size_max: 100 * MIB,
            object_name: "db-backup".into(),
            object_ext: "tar.gz".into(),
            part_size: 10 * MIB,
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

    /// Renaming, resizing, repricing — none of it can make already-burned
    /// money wrong, so none of it may cost the user their ledger.
    #[test]
    fn renaming_and_resizing_never_costs_the_ledger() {
        let ckpt = Checkpoint::new("run-1".into(), snapshot());
        let mut other = snapshot();
        other.part_size = 20 * MIB;
        other.object_name = "totally-different".into();
        other.object_ext = "zip".into();
        other.object_size_min = 5 * MIB;
        other.object_size_max = 5 * MIB;
        other.budget_micro = 9_000_000;
        ckpt.validate_config(&other).unwrap();
    }

    /// Moving the data is the one thing that is refused.
    #[test]
    fn moving_the_data_is_rejected_with_the_reason() {
        let ckpt = Checkpoint::new("run-1".into(), snapshot());
        let mut other = snapshot();
        other.key_prefix = "elsewhere/".into();
        let err = ckpt.validate_config(&other).unwrap_err().to_string();
        assert!(err.contains("key_prefix"), "{}", err);
        assert!(err.contains("存储费"), "要说清后果: {}", err);
    }

    /// Pace is NOT part of the snapshot: a run that turns out to be outrunning
    /// the network must be slowable without throwing away the budget already
    /// burned. Nothing in the ledger depends on rate — burned_micro is bytes ×
    /// price — so allowing it costs no accuracy.
    #[test]
    fn changing_the_pace_does_not_block_resume() {
        let ckpt = Checkpoint::new("run-1".into(), snapshot());
        let same = snapshot();
        ckpt.validate_config(&same).unwrap();
        // A checkpoint written by an older build still carries rate_* keys in
        // its JSON; serde ignores them, so the resume must still succeed.
        let legacy = serde_json::json!({
            "version": 1,
            "run_id": "run-1",
            "config": {
                "mode": "crr", "transfer_acceleration": false,
                "bucket": "b", "key_prefix": "p/", "budget_micro": 1000000,
                "endpoint_url": null,
                "object_size_min": 104857600, "object_size_max": 104857600,
                "object_name": "db-backup", "object_ext": "tar.gz",
                "part_size": 10485760,
                "rate_min": 209715200, "rate_max": 524288000, "rate_mode": "continuous",
                "retain_secs": 86400
            },
            "completed_iterations": 3, "completed_bytes": 1, "burned_micro": 42,
            "started_at": "2026-08-25T00:00:00Z",
            "active_secs": 1, "slowdown_total": 0, "error_total": 0
        });
        let old: Checkpoint = serde_json::from_value(legacy).unwrap();
        assert_eq!(old.burned_micro, 42, "已烧金额必须保住");
        old.validate_config(&snapshot()).unwrap();
    }

    /// What the snapshot still exists for: where the data lives.
    #[test]
    fn layout_and_accounting_changes_still_block_resume() {
        let ckpt = Checkpoint::new("run-1".into(), snapshot());
        for (name, mutate) in [
            ("bucket", (|s: &mut ConfigSnapshot| s.bucket = "other".into()) as fn(&mut ConfigSnapshot)),
            ("key_prefix", |s: &mut ConfigSnapshot| s.key_prefix = "z/".into()),
        ] {
            let mut other = snapshot();
            mutate(&mut other);
            assert!(
                ckpt.validate_config(&other).is_err(),
                "{} 变了却允许续跑",
                name
            );
        }
    }

    /// The `--days` ledger has to survive the process, or a run killed at 12:30
    /// comes back at 12:31 with a fresh hour's — and day's — quota to spend.
    #[test]
    fn the_period_ledger_survives_a_restart() {
        let path = tmp_path("plan");
        let mut ckpt = Checkpoint::new("run-1".into(), snapshot());
        assert!(ckpt.plan.hour_start.is_none(), "没配 --days 时不该有账本");
        ckpt.plan = PlanLedger {
            hour_start: Some("2026-08-25T12:00:00Z".parse().unwrap()),
            hour_cap_micro: 1_300_000,
            hour_burned_micro: 900_000,
            day_start: Some("2026-08-25T00:00:00Z".parse().unwrap()),
            day_burned_micro: 7_500_000,
        };
        ckpt.save(&path).unwrap();

        let loaded = Checkpoint::load(&path).unwrap();
        assert_eq!(loaded.plan.hour_cap_micro, 1_300_000, "重启要用原来那个上限");
        assert_eq!(loaded.plan.hour_burned_micro, 900_000);
        assert_eq!(loaded.plan.day_burned_micro, 7_500_000);
        assert_eq!(loaded.plan.day_start, ckpt.plan.day_start);
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn corrupt_file_is_a_clear_error() {
        let path = tmp_path("corrupt");
        std::fs::write(&path, "{not json").unwrap();
        assert!(Checkpoint::load(&path).is_err());
        std::fs::remove_file(&path).unwrap();
    }
}
