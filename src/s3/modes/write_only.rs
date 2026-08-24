// Mode `write-only`: no replication, no per-byte fee. Only PUT-class request
// fees accrue immediately (~$0.02 per TiB written), so the budget alone can
// never stop the run — a secondary bound must. Useful against S3-compatible
// endpoints, or to exercise write throughput without the transfer bill.

use anyhow::Result;
use async_trait::async_trait;

use super::{BurnMode, ModeCtx, ModeId};
use crate::s3::cost::{CostModel, Pricing};

pub struct WriteOnlyMode;

#[async_trait]
impl BurnMode for WriteOnlyMode {
    fn id(&self) -> ModeId {
        ModeId::WriteOnly
    }

    fn describe(&self) -> String {
        "纯写入,只产生请求费(存储费按月发酵,运行期无法精确控停)".to_string()
    }

    async fn preflight(&mut self, _ctx: &ModeCtx<'_>) -> Result<()> {
        Ok(())
    }

    fn cost_model(&self, _pricing: &Pricing) -> CostModel {
        CostModel::request_only()
    }
}
