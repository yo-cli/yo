// Remember the last run's parameters so a bare `yo-s3` does not make the user
// retype what they typed yesterday.
//
// Only the parameters that have NO documented default are remembered. The ones
// clap advertises a default for (object size, part size, rates, retention, …)
// keep that default forever — otherwise `--help` would say `[default: 100GiB]`
// while the real default is invisible state in a file, which is worse than
// typing the flag again.
//
// The record lives beside the per-bucket state directories rather than inside
// one, because the thing most worth remembering is *which bucket* — that cannot
// be keyed by bucket.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::time::Duration;

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct LastRun {
    pub bucket: Option<String>,
    pub budget_micro: Option<u64>,
    pub duration_secs: Option<u64>,
    pub days: Option<u64>,
    pub region: Option<String>,
    pub profile: Option<String>,
    pub dest_regions: Vec<String>,
    pub endpoint_url: Option<String>,
    pub total_size: Option<u64>,
    pub iterations: Option<u64>,
    pub max_duration_secs: Option<u64>,
}

impl LastRun {
    pub fn duration(&self) -> Option<Duration> {
        self.duration_secs.map(Duration::from_secs)
    }

    pub fn max_duration(&self) -> Option<Duration> {
        self.max_duration_secs.map(Duration::from_secs)
    }

    /// The flags a user would have had to type to get this run. Printed when
    /// anything is reused, so the recalled state is never invisible.
    pub fn describe_reused(&self, reused: &[&str]) -> String {
        let mut parts: Vec<String> = Vec::new();
        for field in reused {
            match *field {
                "region" => push(&mut parts, "--region", self.region.as_deref()),
                "profile" => push(&mut parts, "--profile", self.profile.as_deref()),
                "endpoint-url" => push(&mut parts, "--endpoint-url", self.endpoint_url.as_deref()),
                "dest-region" => {
                    if !self.dest_regions.is_empty() {
                        parts.push(format!("--dest-region {}", self.dest_regions.join(",")));
                    }
                }
                "duration" => {
                    if let Some(d) = self.duration() {
                        parts.push(format!("--duration {}", humantime::format_duration(d)));
                    }
                }
                "max-duration" => {
                    if let Some(d) = self.max_duration() {
                        parts.push(format!("--max-duration {}", humantime::format_duration(d)));
                    }
                }
                "total-size" => {
                    if let Some(v) = self.total_size {
                        parts.push(format!("--total-size {}", super::fmt_bytes(v)));
                    }
                }
                "iterations" => {
                    if let Some(v) = self.iterations {
                        parts.push(format!("--iterations {}", v));
                    }
                }
                _ => {}
            }
        }
        parts.join(" ")
    }
}

fn push(parts: &mut Vec<String>, flag: &str, value: Option<&str>) {
    if let Some(v) = value {
        parts.push(format!("{} {}", flag, v));
    }
}

pub fn path() -> Result<PathBuf> {
    Ok(dirs_next::home_dir()
        .context("无法定位 home 目录")?
        .join(".yo")
        .join("s3")
        .join("last-run.json"))
}

/// Never fails a run: a missing or unreadable record just means no memory.
pub fn load() -> LastRun {
    let Ok(p) = path() else {
        return LastRun::default();
    };
    std::fs::read_to_string(p)
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default()
}

/// Best effort: failing to remember must never abort a run that is otherwise
/// ready to go.
pub fn save(record: &LastRun) {
    let Ok(p) = path() else { return };
    if let Some(dir) = p.parent() {
        if std::fs::create_dir_all(dir).is_err() {
            return;
        }
    }
    if let Ok(json) = serde_json::to_string_pretty(record) {
        let _ = std::fs::write(&p, json);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn describe_lists_only_what_was_reused() {
        let last = LastRun {
            bucket: Some("b".into()),
            profile: Some("yo-s3".into()),
            duration_secs: Some(86_400),
            dest_regions: vec!["us-west-2".into(), "eu-west-1".into()],
            ..Default::default()
        };
        // bucket is prompted (and therefore visible), so it is never listed here
        let txt = last.describe_reused(&["profile", "duration", "dest-region"]);
        assert!(txt.contains("--profile yo-s3"), "{}", txt);
        assert!(txt.contains("--duration 1day"), "{}", txt);
        assert!(txt.contains("--dest-region us-west-2,eu-west-1"), "{}", txt);
        assert!(!txt.contains("--region "), "未复用的不该出现: {}", txt);
    }

    /// `--days` is remembered, but as the DEFAULT of its prompt — like
    /// `--bucket` and `--budget`, and unlike the flags recalled silently. A
    /// prompted answer is already on screen; listing it again as "沿用上次" would
    /// claim the user did not just type it.
    #[test]
    fn prompted_params_are_remembered_without_being_listed_as_recalled() {
        let last = LastRun {
            days: Some(30),
            bucket: Some("b".into()),
            budget_micro: Some(1),
            ..Default::default()
        };
        assert_eq!(last.describe_reused(&["days", "bucket", "budget"]), "");
    }

    #[test]
    fn describe_is_empty_when_nothing_was_reused() {
        assert_eq!(LastRun::default().describe_reused(&["profile"]), "");
    }

    /// A record written by an older build must not stop the tool from running.
    #[test]
    fn unknown_and_missing_fields_decode_to_defaults() {
        let last: LastRun =
            serde_json::from_str(r#"{"bucket":"b","future_field":42}"#).unwrap();
        assert_eq!(last.bucket.as_deref(), Some("b"));
        assert!(last.profile.is_none());
        assert!(last.dest_regions.is_empty());
    }
}
