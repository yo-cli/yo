// Burn modes: pluggable cost engines.
//
// A mode owns exactly the three things that differ between ways of burning
// money — what it must arm before the run, how bytes turn into immediate cost,
// and how one unit of work executes. Everything else (budget ledger, rate
// limiter, data pool, checkpoint, metrics, retention sweeper, reporting,
// graceful shutdown) is shared by every mode and lives outside this module.
//
// Adding a mode = one file implementing `BurnMode` + one arm in `ModeId::build`.

pub mod crr;
pub mod write_only;

use anyhow::Result;
use async_trait::async_trait;
use aws_config::SdkConfig;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::sync::Arc;

use super::config::BenchConfig;
use super::cost::{CostModel, Pricing};
use super::uploader::{upload_object, ObjectOutcome, UploadCtx};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub enum ModeId {
    /// 跨区复制流量($0.02/GB):即时、随写入线性,可精确控停(默认)
    #[default]
    Crr,
    /// 纯写入:只产生请求费,烧钱极慢,需配合 --total-size/--iterations/--max-duration
    WriteOnly,
}

impl ModeId {
    pub fn build(self) -> Box<dyn BurnMode> {
        match self {
            ModeId::Crr => Box::new(crr::CrrMode::default()),
            ModeId::WriteOnly => Box::new(write_only::WriteOnlyMode),
        }
    }
}

/// The CLI value, the config-diff line and the JSON summary all spell a mode
/// the same way.
impl fmt::Display for ModeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            ModeId::Crr => "crr",
            ModeId::WriteOnly => "write-only",
        })
    }
}

/// A replication/copy target the run must also sweep and report on.
pub struct DestTarget {
    pub bucket: String,
    /// Where it lives. Transfer is priced per source→destination pair, so this
    /// is what decides the fee — None means the region could not be discovered
    /// and the source's standard (highest) rate is assumed.
    pub region: Option<String>,
    pub client: aws_sdk_s3::Client,
}

/// What a mode gets to work with while arming its engine.
pub struct ModeCtx<'a> {
    pub shared: &'a SdkConfig,
    pub s3: &'a aws_sdk_s3::Client,
    pub cfg: &'a BenchConfig,
    /// Region of the source bucket, when it could be discovered.
    pub bucket_region: Option<&'a str>,
}

/// What a mode gets to sample while the run is live.
pub struct ObserveCtx<'a> {
    pub s3: &'a aws_sdk_s3::Client,
    pub bucket: &'a str,
    /// Recently completed object keys.
    pub keys: &'a [String],
}

/// A live sample a mode contributes to the periodic report.
pub struct Observation {
    /// Appended to the report line.
    pub text: String,
    /// Work the engine has accepted but not finished (replication backlog
    /// today); carried into the final JSON summary.
    pub pending: u64,
}

#[async_trait]
pub trait BurnMode: Send + Sync {
    fn id(&self) -> ModeId;

    /// One line for the estimate page: what this mode actually bills.
    fn describe(&self) -> String;

    /// Arm the engine before the first byte: detect or create whatever the mode
    /// needs. May prompt interactively unless `cfg.yes`.
    async fn preflight(&mut self, ctx: &ModeCtx<'_>) -> Result<()>;

    /// Cost shape, valid only after `preflight` — a mode that could not arm its
    /// engine must report `CostModel::request_only()` so the budget math and
    /// the "this will never stop" guard stay honest.
    fn cost_model(&self, pricing: &Pricing) -> CostModel;

    /// Extra buckets the run must sweep and report (replication targets).
    /// The count is the K in a fan-out mode's cost model.
    fn destinations(&self) -> &[DestTarget] {
        &[]
    }

    /// Execute one unit of work. The default writes one object via multipart —
    /// modes that bill a different action (cross-region reads, restores, …)
    /// override this.
    async fn run_unit(
        &self,
        ctx: Arc<UploadCtx>,
        iteration: u64,
        size: u64,
    ) -> Result<ObjectOutcome> {
        upload_object(ctx, iteration, size).await
    }

    /// Live sample for the report line; None when the mode has nothing to show.
    async fn observe(&self, _ctx: &ObserveCtx<'_>) -> Option<Observation> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mode_id_display_matches_cli_values() {
        assert_eq!(ModeId::Crr.to_string(), "crr");
        assert_eq!(ModeId::WriteOnly.to_string(), "write-only");
        assert_eq!(ModeId::default(), ModeId::Crr);
    }

    #[test]
    fn every_mode_builds() {
        for id in [ModeId::Crr, ModeId::WriteOnly] {
            assert_eq!(id.build().id(), id);
        }
    }
}
