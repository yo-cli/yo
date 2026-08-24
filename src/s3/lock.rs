// Single-instance guard for a state directory.
//
// The budget ledger is one checkpoint file plus each process's own in-memory
// BudgetMeter stacked on top of it (see budget.rs), so two processes sharing a
// state directory each burn the whole --budget: the hard ceiling the whole tool
// is built around silently doubles, in real money. flock is the guard, and it
// is picked over a PID file for one reason — the kernel drops it when the
// holder dies, `kill -9` included, so there is no stale lock to detect and no
// liveness heuristic to get wrong.

use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fmt;
use std::path::{Path, PathBuf};

#[cfg(unix)]
use {
    anyhow::Context,
    colored::Colorize,
    nix::errno::Errno,
    nix::fcntl::{Flock, FlockArg},
    std::fs::{File, OpenOptions},
    std::io::{Read, Seek, Write},
};

#[cfg(unix)]
const LOCK_FILE: &str = "run.lock";

/// Written into the lock file once the lock is held, so whoever gets refused
/// can name the holder instead of just reporting "busy".
#[derive(Debug, Serialize, Deserialize)]
struct Holder {
    cmd: String,
    pid: u32,
    host: String,
    started_at: DateTime<Utc>,
}

/// Held for as long as the guarded work runs. Released on drop — and by the
/// kernel if the process never gets to drop it.
#[derive(Default)]
pub struct RunLock {
    /// `None` when the filesystem cannot lock; see `try_acquire`.
    #[cfg(unix)]
    _flock: Option<Flock<File>>,
}

pub enum Acquired {
    /// The lock is ours (or the filesystem cannot lock and we proceeded).
    Held(RunLock),
    /// Another live process holds it.
    Busy(HolderInfo),
}

pub struct HolderInfo {
    path: PathBuf,
    holder: Option<Holder>,
}

impl fmt::Display for HolderInfo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.holder {
            Some(h) => write!(
                f,
                "{}(pid {} @ {},已运行 {})",
                h.cmd,
                h.pid,
                h.host,
                elapsed_since(h.started_at)
            ),
            // The holder locks before it writes, so a microsecond-wide window
            // exists where the file is still empty. Say so rather than lie.
            None => write!(
                f,
                "另一个 yo-s3 进程(锁文件 {} 尚未记录持有者)",
                self.path.display()
            ),
        }
    }
}

/// yo-s3 only runs on Linux (specs/yo_s3.md §11). This stub exists so `yo_lib`
/// still builds for the Windows `yo-git` target, where no burn ever runs.
#[cfg(not(unix))]
pub fn try_acquire(_dir: &Path, _cmd: &str) -> Result<Acquired> {
    Ok(Acquired::Held(RunLock::default()))
}

/// Take the exclusive lock on `dir`. `cmd` names this process in the lock file
/// for whoever gets refused next. `dir` must already exist.
#[cfg(unix)]
pub fn try_acquire(dir: &Path, cmd: &str) -> Result<Acquired> {
    let path = dir.join(LOCK_FILE);
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&path)
        .with_context(|| format!("打开锁文件失败: {}", path.display()))?;

    let mut flock = match Flock::lock(file, FlockArg::LockExclusiveNonblock) {
        Ok(f) => f,
        Err((file, Errno::EAGAIN)) => {
            return Ok(Acquired::Busy(HolderInfo {
                holder: read_holder(file),
                path,
            }))
        }
        // The filesystem cannot lock at all (an NFS home without lockd, some
        // container overlays). Refusing to run would block a legitimate run
        // over a guard we are unable to provide, so warn and go unguarded.
        Err((_, errno)) => {
            eprintln!(
                "{} 文件锁不可用({}: {}),本次跳过单实例保护——请自行确认没有第二个实例在跑",
                "⚠".yellow(),
                path.display(),
                errno
            );
            return Ok(Acquired::Held(RunLock::default()));
        }
    };

    // Best effort: failing to record who we are costs the *next* process a
    // good error message, which is no reason to fail a run that holds the lock.
    if let Err(e) = write_holder(&mut flock, cmd) {
        tracing::debug!("写入锁持有者信息失败 {}: {:#}", path.display(), e);
    }
    Ok(Acquired::Held(RunLock {
        _flock: Some(flock),
    }))
}

#[cfg(unix)]
fn write_holder(flock: &mut Flock<File>, cmd: &str) -> Result<()> {
    let holder = Holder {
        cmd: cmd.to_string(),
        pid: std::process::id(),
        host: hostname::get()
            .ok()
            .and_then(|h| h.into_string().ok())
            .unwrap_or_else(|| "unknown".to_string()),
        started_at: Utc::now(),
    };
    let json = serde_json::to_vec(&holder)?;
    flock.set_len(0)?;
    flock.rewind()?;
    flock.write_all(&json)?;
    flock.sync_data()?;
    Ok(())
}

#[cfg(unix)]
fn read_holder(mut file: File) -> Option<Holder> {
    let mut buf = String::new();
    file.read_to_string(&mut buf).ok()?;
    serde_json::from_str(&buf).ok()
}

fn elapsed_since(started_at: DateTime<Utc>) -> String {
    let secs = (Utc::now() - started_at).num_seconds().max(0) as u64;
    humantime::format_duration(std::time::Duration::from_secs(secs)).to_string()
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    fn tmp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "yo-s3-lock-test-{}-{}",
            name,
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn busy(acquired: Acquired) -> HolderInfo {
        match acquired {
            Acquired::Busy(h) => h,
            Acquired::Held(_) => panic!("期望锁被占用"),
        }
    }

    /// flock is bound to the open file description, not the process, so this
    /// holds even for a second acquire inside one process — which is what makes
    /// the guard testable without spawning anything.
    #[test]
    fn second_acquire_is_refused_and_names_the_holder() {
        let dir = tmp_dir("second");
        let _first = try_acquire(&dir, "yo-s3 run").unwrap();
        let info = busy(try_acquire(&dir, "yo-s3 cleanup").unwrap());
        let msg = info.to_string();
        assert!(msg.contains("yo-s3 run"), "{}", msg);
        assert!(msg.contains(&std::process::id().to_string()), "{}", msg);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn dropping_the_lock_frees_it() {
        let dir = tmp_dir("drop");
        drop(try_acquire(&dir, "yo-s3 run").unwrap());
        assert!(matches!(
            try_acquire(&dir, "yo-s3 run").unwrap(),
            Acquired::Held(_)
        ));
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A lock file left empty by a holder killed between flock and write must
    /// still produce a usable message, not a parse error.
    #[test]
    fn empty_lock_file_still_reports_busy() {
        let dir = tmp_dir("empty");
        let path = dir.join(LOCK_FILE);
        std::fs::write(&path, "").unwrap();
        let holder = File::open(&path).unwrap();
        let _held = Flock::lock(holder, FlockArg::LockExclusiveNonblock).unwrap();
        let msg = busy(try_acquire(&dir, "yo-s3 run").unwrap()).to_string();
        assert!(msg.contains("尚未记录持有者"), "{}", msg);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Separate state directories are separate budgets and must not block
    /// each other.
    #[test]
    fn different_dirs_do_not_block_each_other() {
        let a = tmp_dir("a");
        let b = tmp_dir("b");
        let _held_a = try_acquire(&a, "yo-s3 run").unwrap();
        assert!(matches!(
            try_acquire(&b, "yo-s3 run").unwrap(),
            Acquired::Held(_)
        ));
        std::fs::remove_dir_all(&a).ok();
        std::fs::remove_dir_all(&b).ok();
    }
}
