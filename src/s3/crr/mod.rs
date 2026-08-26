// Cross-Region Replication: the burn engine. Inter-region transfer (~$0.02/GB)
// is the only cost that accrues immediately and linearly with bytes written,
// which is what lets the budget stop a run precisely.
//
// Split by what each part does to the account, because those are the parts that
// have to stay consistent with each other:
//
//   identity — which bucket is ours: name derivation, and the created tag
//   role     — the replication IAM role: named, created, deleted
//   detect   — read what is already there, change nothing
//   setup    — build the fan-out
//   teardown — take it back down, including what earlier teardowns missed
//
// Submodules are private and re-exported flat: callers say `crr::setup`, not
// `crr::setup::setup`, and the internal layout stays free to move.

use aws_config::SdkConfig;

mod detect;
mod identity;
mod role;
mod setup;
mod teardown;

pub use detect::{detect, detect_covering, sample_backlog, versioning_enabled};
pub use identity::{dest_bucket_name, dest_bucket_prefix, dest_region_of};
pub use role::replication_role_name;
pub use setup::{create_bucket, setup, validate_dest_regions};
pub use teardown::{
    find_orphan_dests, teardown, teardown_plan, DestTeardown, OrphanDest, TeardownPlan,
};

/// A region-pinned client that KEEPS the SDK's default retries, unlike
/// `client::build_s3_client`, which disables them so the burn loop can account
/// for every request exactly. Provisioning and discovery are one-shot slow-path
/// control-plane calls with no retry layer of their own: a retried CreateBucket
/// is strictly better than a replication target that failed to appear.
fn retrying_region_client(shared: &SdkConfig, region: &str) -> aws_sdk_s3::Client {
    let builder = aws_sdk_s3::config::Builder::from(shared)
        .region(aws_config::Region::new(region.to_string()));
    aws_sdk_s3::Client::from_conf(builder.build())
}
