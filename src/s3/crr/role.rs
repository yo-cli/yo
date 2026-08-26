// The replication IAM role. One role per source bucket, created by setup and
// removed by teardown — both sides live here so they can never disagree about
// its name or about which policy hangs off it.

use anyhow::{anyhow, bail, Context, Result};
use aws_config::SdkConfig;
use aws_sdk_s3::error::ProvideErrorMetadata;
use colored::Colorize;

/// Inline policy attached to the replication role.
const ROLE_POLICY_NAME: &str = "yo-s3-crr-policy";

/// The replication role for a source bucket. One role per source bucket, so
/// removing it never disturbs another bucket's replication.
pub fn replication_role_name(source_bucket: &str) -> String {
    let name = format!("yo-s3-crr-{}", source_bucket);
    name[..name.len().min(64)].to_string()
}

/// One role for the whole fan-out: the write statement must list EVERY
/// destination, or the rules pointing at the missing ones silently fail to
/// replicate — and a destination that never receives bytes never bills.
pub(super) async fn ensure_replication_role(
    shared: &SdkConfig,
    source_bucket: &str,
    dest_buckets: &[String],
) -> Result<String> {
    let iam = aws_sdk_iam::Client::new(shared);
    let role_name = replication_role_name(source_bucket);
    let trust = r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Principal":{"Service":"s3.amazonaws.com"},"Action":"sts:AssumeRole"}]}"#;
    let dst_resources = dest_buckets
        .iter()
        .map(|b| format!("\"arn:aws:s3:::{}/*\"", b))
        .collect::<Vec<_>>()
        .join(",");
    let policy = format!(
        r#"{{"Version":"2012-10-17","Statement":[
  {{"Effect":"Allow","Action":["s3:GetReplicationConfiguration","s3:ListBucket"],"Resource":"arn:aws:s3:::{src}"}},
  {{"Effect":"Allow","Action":["s3:GetObjectVersionForReplication","s3:GetObjectVersionAcl","s3:GetObjectVersionTagging"],"Resource":"arn:aws:s3:::{src}/*"}},
  {{"Effect":"Allow","Action":["s3:ReplicateObject","s3:ReplicateDelete","s3:ReplicateTags"],"Resource":[{dst}]}}
]}}"#,
        src = source_bucket,
        dst = dst_resources
    );

    let role_arn = match iam
        .create_role()
        .role_name(&role_name)
        .assume_role_policy_document(trust)
        .send()
        .await
    {
        Ok(out) => {
            let arn = out
                .role()
                .map(|r| r.arn().to_string())
                .ok_or_else(|| anyhow!("CreateRole 未返回角色"))?;
            println!("{} 已创建复制角色 {}", "✓".green(), role_name);
            arn
        }
        Err(e) if e.code() == Some("EntityAlreadyExists") => {
            let out = iam
                .get_role()
                .role_name(&role_name)
                .send()
                .await
                .with_context(|| format!("读取已有角色 {} 失败", role_name))?;
            println!("{} 复制角色 {} 已存在", "✓".green(), role_name);
            out.role()
                .map(|r| r.arn().to_string())
                .ok_or_else(|| anyhow!("GetRole 未返回角色"))?
        }
        Err(e) => bail!(
            "创建复制角色失败: {}\n  提示:当前身份需要 IAM 权限(iam:CreateRole / iam:PutRolePolicy);\
             可用有管理员权限的身份先跑一次带 --dest-region 的命令把复制配好",
            e
        ),
    };

    iam.put_role_policy()
        .role_name(&role_name)
        .policy_name(ROLE_POLICY_NAME)
        .policy_document(policy)
        .send()
        .await
        .context("写入复制角色策略失败")?;
    Ok(role_arn)
}

/// Drop the role, inline policy first — IAM refuses to delete a role that still
/// has one attached. Reports its own failures rather than propagating: teardown
/// runs every step it can, since a half-removed setup is worse than a
/// fully-attempted one.
pub(super) async fn delete_replication_role(shared: &SdkConfig, role_name: &str) {
    let iam = aws_sdk_iam::Client::new(shared);
    if let Err(e) = iam
        .delete_role_policy()
        .role_name(role_name)
        .policy_name(ROLE_POLICY_NAME)
        .send()
        .await
    {
        tracing::debug!("删除角色策略失败(可能本就不存在): {}", e);
    }
    match iam.delete_role().role_name(role_name).send().await {
        Ok(_) => println!("{} 已删除复制角色 {}", "✓".green(), role_name),
        Err(e) => eprintln!(
            "{} 删除复制角色 {} 失败: {}(需要 iam:DeleteRole 权限)",
            "✗".red(),
            role_name,
            e
        ),
    }
}

/// Does the role exist right now?
pub(super) async fn role_exists(shared: &SdkConfig, role_name: &str) -> bool {
    aws_sdk_iam::Client::new(shared)
        .get_role()
        .role_name(role_name)
        .send()
        .await
        .is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Setup creates the role under this name and teardown deletes it under
    /// the same one. If the two ever computed it differently, teardown would
    /// silently strand an IAM role nothing else cleans up.
    #[test]
    fn role_name_is_stable_and_capped_at_the_iam_limit() {
        assert_eq!(replication_role_name("burn"), "yo-s3-crr-burn");
        let capped = replication_role_name(&"a".repeat(80));
        assert_eq!(capped.len(), 64);
        assert!(capped.starts_with("yo-s3-crr-a"));
    }
}
