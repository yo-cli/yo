// Credential resolution: what to do when the standard AWS chain comes up empty.
//
// The tool already prompts for the two required parameters (budget, bucket)
// rather than failing on them; credentials are just as required and far more
// often the thing that blocks a first run, so they get the same treatment.
//
// The menu is built from what is actually actionable HERE: "attach an IAM role"
// is only offered on EC2, "pick a profile" only when ~/.aws holds any. Detect
// the situation, then offer — never make the user work out which of a fixed
// list of options applies to their machine.

use anyhow::{bail, Context, Result};
use aws_config::SdkConfig;
use aws_sdk_sts::config::Credentials;
use colored::Colorize;
use std::fmt::Write as _;
use std::path::PathBuf;

use super::client::{caller_identity, credential_hint, load_shared_config, resolved_region};

/// Profile written when the user asks to remember pasted keys.
const SAVED_PROFILE: &str = "yo-s3";

pub struct AuthOpts<'a> {
    pub region: Option<&'a str>,
    pub profile: Option<&'a str>,
    /// Unattended: never prompt. A nohup'd multi-day burn that stops to ask a
    /// question is worse than one that fails loudly.
    pub yes: bool,
    /// --dry-run: a rehearsal may proceed without credentials.
    pub lenient: bool,
}

/// Make `shared` carry working credentials, prompting if needed. On success the
/// caller identity has been printed and `shared` is usable.
///
/// Returns the profile the user picked at the menu, for the caller to remember
/// like every other answer that has no default. The auto-reused [yo-s3] profile
/// is deliberately NOT returned: it is the fallback for an empty chain, and
/// pinning it would keep spending through stale pasted keys long after the
/// machine got a proper IAM role.
pub async fn ensure_credentials(
    shared: &mut SdkConfig,
    opts: &AuthOpts<'_>,
) -> Result<Option<String>> {
    let mut last_err = match caller_identity(shared).await {
        Ok(id) => {
            print_identity(&id);
            return Ok(None);
        }
        Err(e) => e,
    };

    let mut region: Option<String> = opts.region.map(str::to_string);
    // Which profile the config is being built from. Tracked for the whole
    // function because every later rebuild — the region prompt included — has to
    // keep whatever profile got us that far, instead of silently dropping back
    // to the default chain that already came up empty.
    let mut profile: Option<String> = opts.profile.map(str::to_string);
    // Only what the user actively chose, which is a narrower thing than the
    // above and the only one worth handing back to be remembered.
    let mut picked_profile: Option<String> = None;

    // Keys pasted on an earlier run were saved to our own [yo-s3] profile, which
    // the default chain does not read. Reusing them here is what makes "记住这组
    // 凭据" mean anything — otherwise every run asks for the same keys again.
    // Before the --yes bail on purpose: an unattended restart (nohup'd multi-day
    // burn, reboot) is exactly the case that cannot stop to paste them.
    if profile.is_none() && saved_profile_exists() {
        profile = Some(SAVED_PROFILE.to_string());
        let candidate = load_shared_config(region.as_deref(), profile.as_deref()).await;
        match caller_identity(&candidate).await {
            Ok(id) => {
                println!(
                    "{} 用上次记住的凭据({} 的 [{}] profile)",
                    "✓".green(),
                    aws_credentials_path()
                        .map(|p| p.display().to_string())
                        .unwrap_or_else(|_| "~/.aws/credentials".to_string()),
                    SAVED_PROFILE
                );
                print_identity(&id);
                *shared = candidate;
                return Ok(None);
            }
            // Working keys that just carry no region: keep the profile and let
            // the region prompt below finish the job.
            Err(e) if is_missing_region(&e) => last_err = e,
            Err(e) => {
                eprintln!(
                    "{} 上次记住的 [{}] 凭据已不可用: {}",
                    "⚠".yellow(),
                    SAVED_PROFILE,
                    brief(&e)
                );
                if let Ok(p) = aws_credentials_path() {
                    eprintln!(
                        "  {}",
                        format!(
                            "重新粘贴会覆盖它;彻底不要就删掉 {} 里的 [{}] 段",
                            p.display(),
                            SAVED_PROFILE
                        )
                        .dimmed()
                    );
                }
                profile = None;
            }
        }
    }

    if opts.yes {
        // Say what is missing, then get out of the way.
        if opts.lenient {
            eprintln!("{} {}", "⚠".yellow(), credential_hint(&last_err).yellow());
            eprintln!("  (--dry-run 模式,继续)");
            return Ok(None);
        }
        bail!("{}", credential_hint(&last_err));
    }

    let on_ec2 = on_ec2().await;
    loop {
        // A missing region is NOT a credential problem, and treating it as one
        // sends a user who already has working credentials off to paste keys
        // they do not need. Ask for the one thing that is actually missing.
        if is_missing_region(&last_err) {
            println!(
                "\n{} 凭据可用,但没有区域(AWS_REGION / profile 里的 region / EC2 元数据都没有)",
                "✗".red().bold()
            );
            let region_input = inquire::Text::new("AWS 区域?")
                .with_default("us-east-1")
                .prompt()?;
            region = Some(region_input.trim().to_string());
            let candidate = load_shared_config(region.as_deref(), profile.as_deref()).await;
            match caller_identity(&candidate).await {
                Ok(id) => {
                    print_identity(&id);
                    println!(
                        "  {}",
                        format!("下次可直接加 --region {}", region.as_deref().unwrap_or("")).dimmed()
                    );
                    *shared = candidate;
                    return Ok(picked_profile);
                }
                Err(e) => {
                    last_err = e;
                    continue;
                }
            }
        }

        println!("\n{} {}", "✗".red().bold(), situation(&last_err, on_ec2));
        match choose(on_ec2)? {
            Choice::Paste => match paste_keys(region.as_deref()).await {
                Ok((config, creds)) => {
                    let id = match caller_identity(&config).await {
                        Ok(id) => id,
                        Err(e) => {
                            // Nothing has been written: bad keys never reach disk.
                            eprintln!("{} 这组凭据没通过校验: {}", "✗".red(), brief(&e));
                            last_err = e;
                            continue;
                        }
                    };
                    print_identity(&id);
                    offer_to_save(&creds)?;
                    *shared = config;
                    return Ok(None);
                }
                Err(e) => {
                    eprintln!("{} {:#}", "✗".red(), e);
                    continue;
                }
            },
            Choice::Profile(name) => {
                profile = Some(name.clone());
                picked_profile = Some(name.clone());
                let config = load_shared_config(region.as_deref(), profile.as_deref()).await;
                match caller_identity(&config).await {
                    Ok(id) => {
                        print_identity(&id);
                        println!(
                            "  {}",
                            format!("下次可直接加 --profile {} 跳过这一步", name).dimmed()
                        );
                        *shared = config;
                        return Ok(picked_profile);
                    }
                    Err(e) => {
                        eprintln!("{} profile {} 不可用: {}", "✗".red(), name, brief(&e));
                        last_err = e;
                        continue;
                    }
                }
            }
            Choice::AttachRole => {
                print_attach_role_steps().await;
                bail!("已退出:挂好 IAM Role 后重跑同一条命令即可(不用重启实例)");
            }
            Choice::Quit => {
                if opts.lenient {
                    eprintln!("{} 未提供凭据,--dry-run 模式继续", "⚠".yellow());
                    return Ok(None);
                }
                bail!("{}", credential_hint(&last_err));
            }
        }
    }
}

fn print_identity(id: &super::client::CallerIdentity) {
    println!(
        "{} 当前认证身份: {}  (账号 {})",
        "✓".green(),
        id.arn.bold(),
        id.account
    );
}

/// One line naming what is actually wrong, not a generic "no credentials".
fn situation(err: &anyhow::Error, on_ec2: bool) -> String {
    let detail = brief(err);
    if on_ec2 && detail.contains("404") {
        return "没有可用的 AWS 凭据:这台机器在 EC2 上,但没挂 IAM Role(IMDS 返回 404)".to_string();
    }
    // Only claim credentials are missing when that is what the SDK said;
    // anything else gets reported as itself.
    if detail.contains("no providers in chain") || detail.contains("CredentialsNotLoaded") {
        return if on_ec2 {
            "没有可用的 AWS 凭据(EC2 实例,凭据链为空)".to_string()
        } else {
            "没有可用的 AWS 凭据(凭据链为空)".to_string()
        };
    }
    format!("无法建立 AWS 会话({})", detail)
}

/// The SDK reports this as a config error, not a credential error.
fn is_missing_region(err: &anyhow::Error) -> bool {
    err.to_string().contains("Missing Region")
}

fn brief(err: &anyhow::Error) -> String {
    let s = err.to_string();
    // The SDK chain is long; the first sentence carries the diagnosis.
    let head = s.split(" (").next().unwrap_or(&s).trim();
    // Except when that sentence is the SDK's placeholder for a service error:
    // "unhandled error" says nothing, while the code and message it wrapped are
    // the difference between "this key was deleted" and "you lack permission".
    if head.ends_with("unhandled error") {
        if let Some(detail) = service_error(&s) {
            return detail;
        }
    }
    head.to_string()
}

/// AWS error code + message, dug out of the SDK's debug formatting.
fn service_error(s: &str) -> Option<String> {
    let code = between(s, "code: \"", "\"")?;
    match between(s, "message: \"", "\"") {
        Some(message) => Some(format!("{}: {}", code, message)),
        None => Some(code.to_string()),
    }
}

fn between<'a>(s: &'a str, start: &str, end: &str) -> Option<&'a str> {
    s.split_once(start)?.1.split_once(end).map(|(inner, _)| inner)
}

enum Choice {
    Paste,
    Profile(String),
    AttachRole,
    Quit,
}

fn choose(on_ec2: bool) -> Result<Choice> {
    let profiles = list_profiles();
    let mut labels: Vec<String> = vec!["粘贴 Access Key / Secret Key".to_string()];
    if !profiles.is_empty() {
        labels.push(format!("用已有 profile({})", profiles.join(" / ")));
    }
    if on_ec2 {
        labels.push("去控制台给这台实例挂 IAM Role(打印步骤后退出)".to_string());
    }
    labels.push("退出".to_string());

    let picked = inquire::Select::new("怎么提供凭据?", labels.clone()).prompt()?;
    if picked.starts_with("粘贴") {
        return Ok(Choice::Paste);
    }
    if picked.starts_with("用已有 profile") {
        let name = if profiles.len() == 1 {
            profiles[0].clone()
        } else {
            inquire::Select::new("用哪个 profile?", profiles).prompt()?
        };
        return Ok(Choice::Profile(name));
    }
    if picked.starts_with("去控制台") {
        return Ok(Choice::AttachRole);
    }
    Ok(Choice::Quit)
}

/// Prompt for keys and build a config from them. Nothing is persisted here —
/// the caller validates first (see CLAUDE.md: 凭据先校验、后持久化).
async fn paste_keys(region: Option<&str>) -> Result<(SdkConfig, PastedCreds)> {
    let access_key = inquire::Text::new("AWS_ACCESS_KEY_ID?").prompt()?;
    let secret_key = inquire::Password::new("AWS_SECRET_ACCESS_KEY?")
        .with_display_mode(inquire::PasswordDisplayMode::Masked)
        .without_confirmation()
        .prompt()?;
    let session_token = inquire::Text::new("AWS_SESSION_TOKEN?(长期密钥留空)")
        .with_default("")
        .prompt()?;

    let access_key = access_key.trim().to_string();
    let secret_key = secret_key.trim().to_string();
    let session_token = {
        let t = session_token.trim();
        (!t.is_empty()).then(|| t.to_string())
    };
    if access_key.is_empty() || secret_key.is_empty() {
        bail!("Access Key 与 Secret Key 都不能为空");
    }

    let creds = PastedCreds {
        access_key,
        secret_key,
        session_token,
        region: None,
    };
    let config = build_config(region, &creds).await;

    // Keys alone are not enough: without a region every later call fails with a
    // far more confusing error than "which region?".
    if resolved_region(&config).is_none() {
        let picked = inquire::Text::new("AWS 区域?(凭据里没带,环境也没配)")
            .with_default("us-east-1")
            .prompt()?;
        let creds = PastedCreds {
            region: Some(picked.trim().to_string()),
            ..creds
        };
        let config = build_config(creds.region.as_deref(), &creds).await;
        return Ok((config, creds));
    }
    Ok((config, creds))
}

async fn build_config(region: Option<&str>, creds: &PastedCreds) -> SdkConfig {
    let provider = Credentials::new(
        &creds.access_key,
        &creds.secret_key,
        creds.session_token.clone(),
        None,
        "yo-s3-manual",
    );
    let mut loader = aws_config::defaults(aws_config::BehaviorVersion::latest())
        .credentials_provider(provider);
    if let Some(r) = region {
        loader = loader.region(aws_config::Region::new(r.to_string()));
    }
    loader.load().await
}

pub struct PastedCreds {
    access_key: String,
    secret_key: String,
    session_token: Option<String>,
    region: Option<String>,
}

/// Offer to remember validated keys in the conventional place.
///
/// `~/.aws/credentials` rather than a private store: it is where every AWS tool
/// already looks, so `aws sts get-caller-identity` works right after, the user
/// can edit or delete it by hand, and nothing proprietary is invented. The
/// repo's own `crypto_utils` is deliberately NOT used — its key is
/// `SHA256(compile-time constant)`, identical on every machine, so calling the
/// result "encrypted" would be a lie for credentials of this weight.
fn offer_to_save(creds: &PastedCreds) -> Result<()> {
    let path = aws_credentials_path()?;
    let save = inquire::Confirm::new(&format!(
        "记住这组凭据?(写入 {} 的 [{}] profile,权限 600)",
        path.display(),
        SAVED_PROFILE
    ))
    .with_default(true)
    .prompt()?;
    if !save {
        println!(
            "  {}",
            "本次运行有效;下次重跑(含断点续跑)需要重新提供".dimmed()
        );
        return Ok(());
    }
    upsert_profile(&path, SAVED_PROFILE, creds)?;
    println!(
        "{} 已写入 {} 的 [{}] profile,下次自动使用(不想要了就 {} 删掉这一段)",
        "✓".green(),
        path.display(),
        SAVED_PROFILE,
        format!("vi {}", path.display()).bold()
    );
    Ok(())
}

/// Did an earlier run leave keys we can reuse? Only our own profile counts: the
/// user's other profiles get offered in the menu, never picked for them.
fn saved_profile_exists() -> bool {
    list_profiles().iter().any(|name| name == SAVED_PROFILE)
}

fn aws_dir() -> Result<PathBuf> {
    Ok(dirs_next::home_dir()
        .context("无法定位 home 目录")?
        .join(".aws"))
}

/// Honour the same overrides the SDK does, or we would list one file and write
/// another the moment a user has these set.
fn aws_credentials_path() -> Result<PathBuf> {
    if let Ok(p) = std::env::var("AWS_SHARED_CREDENTIALS_FILE") {
        if !p.is_empty() {
            return Ok(PathBuf::from(p));
        }
    }
    Ok(aws_dir()?.join("credentials"))
}

fn aws_config_path() -> Result<PathBuf> {
    if let Ok(p) = std::env::var("AWS_CONFIG_FILE") {
        if !p.is_empty() {
            return Ok(PathBuf::from(p));
        }
    }
    Ok(aws_dir()?.join("config"))
}

/// Profile names visible to the SDK: `[name]` in credentials, `[profile name]`
/// in config.
fn list_profiles() -> Vec<String> {
    let files = [
        (aws_credentials_path().ok(), false),
        (aws_config_path().ok(), true),
    ];
    let mut names: Vec<String> = Vec::new();
    for (path, strip) in files {
        let Some(path) = path else { continue };
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        for name in parse_profile_names(&text, strip) {
            if !names.contains(&name) {
                names.push(name);
            }
        }
    }
    names
}

/// `[name]` in credentials, `[profile name]` in config.
fn parse_profile_names(text: &str, strip: bool) -> Vec<String> {
    text.lines()
        .filter_map(|line| {
            let line = line.trim();
            let inner = line.strip_prefix('[')?.strip_suffix(']')?;
            let name = if strip {
                inner.strip_prefix("profile ").unwrap_or(inner)
            } else {
                inner
            }
            .trim();
            (!name.is_empty()).then(|| name.to_string())
        })
        .collect()
}

/// Replace (or append) one profile section, leaving every other profile in the
/// file untouched — this is a file the user may well share with other tools.
fn upsert_profile(path: &PathBuf, name: &str, creds: &PastedCreds) -> Result<()> {
    let existing = std::fs::read_to_string(path).unwrap_or_default();
    let header = format!("[{}]", name);

    let mut kept: Vec<&str> = Vec::new();
    let mut in_target = false;
    for line in existing.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            in_target = trimmed == header;
        }
        if !in_target {
            kept.push(line);
        }
    }

    let mut out = kept.join("\n");
    let out_trimmed = out.trim_end().to_string();
    out = out_trimmed;
    if !out.is_empty() {
        out.push_str("\n\n");
    }
    let _ = writeln!(out, "{}", header);
    let _ = writeln!(out, "aws_access_key_id = {}", creds.access_key);
    let _ = writeln!(out, "aws_secret_access_key = {}", creds.secret_key);
    if let Some(token) = &creds.session_token {
        let _ = writeln!(out, "aws_session_token = {}", token);
    }
    if let Some(region) = &creds.region {
        let _ = writeln!(out, "region = {}", region);
    }

    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("创建 {} 失败", dir.display()))?;
    }
    std::fs::write(path, out).with_context(|| format!("写入 {} 失败", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .with_context(|| format!("设置 {} 权限失败", path.display()))?;
    }
    Ok(())
}

/// Is this an EC2 instance? Decides whether attaching an IAM role is even an
/// option worth offering.
async fn on_ec2() -> bool {
    let imds = aws_config::imds::Client::builder().build();
    imds.get("/latest/meta-data/instance-id").await.is_ok()
}

async fn print_attach_role_steps() {
    let imds = aws_config::imds::Client::builder().build();
    let instance_id = imds
        .get("/latest/meta-data/instance-id")
        .await
        .map(|v| v.as_ref().trim().to_string())
        .unwrap_or_else(|_| "<实例 ID>".to_string());

    println!(
        "\n{}\n  \
         1. EC2 控制台 → 实例 {} → 操作 → 安全 → 修改 IAM 角色\n  \
         2. 没有可选角色时先点「Create new IAM role」:受信任实体选 AWS service → 用例选 EC2,\n     \
            权限勾 AdministratorAccess(或 README 里那份最小权限),命名后创建\n  \
         3. 回到修改页点 ⟳ 刷新,选中角色 → Update IAM role\n  \
         4. 不用重启实例,几秒后重跑同一条命令即可",
        "给这台实例挂 IAM Role:".cyan().bold(),
        instance_id.bold()
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn creds(key: &str) -> PastedCreds {
        PastedCreds {
            access_key: key.to_string(),
            secret_key: "secret".to_string(),
            session_token: None,
            region: None,
        }
    }

    fn tmp_file(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("yo-s3-auth-{}-{}", name, uuid::Uuid::new_v4()))
    }

    /// ~/.aws/credentials is a file other tools own too. Writing our profile
    /// must never cost the user their existing ones.
    #[test]
    fn upsert_keeps_other_profiles() {
        let path = tmp_file("keep");
        std::fs::write(
            &path,
            "[default]\naws_access_key_id = AAA\naws_secret_access_key = a\n\n\
             [prod]\naws_access_key_id = BBB\naws_secret_access_key = b\n",
        )
        .unwrap();

        upsert_profile(&path, "yo-s3", &creds("NEW")).unwrap();

        let out = std::fs::read_to_string(&path).unwrap();
        assert!(out.contains("[default]") && out.contains("AAA"), "{}", out);
        assert!(out.contains("[prod]") && out.contains("BBB"), "{}", out);
        assert!(out.contains("[yo-s3]") && out.contains("NEW"), "{}", out);
        std::fs::remove_file(&path).ok();
    }

    /// Re-running must replace our section, not stack duplicates that later
    /// shadow each other unpredictably.
    #[test]
    fn upsert_replaces_rather_than_duplicates() {
        let path = tmp_file("replace");
        upsert_profile(&path, "yo-s3", &creds("FIRST")).unwrap();
        upsert_profile(&path, "yo-s3", &creds("SECOND")).unwrap();

        let out = std::fs::read_to_string(&path).unwrap();
        assert_eq!(out.matches("[yo-s3]").count(), 1, "{}", out);
        assert!(out.contains("SECOND") && !out.contains("FIRST"), "{}", out);
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn session_token_and_region_round_trip() {
        let path = tmp_file("token");
        let with_extras = PastedCreds {
            session_token: Some("TOKEN".into()),
            region: Some("ap-south-1".into()),
            ..creds("KEY")
        };
        upsert_profile(&path, "yo-s3", &with_extras).unwrap();
        let out = std::fs::read_to_string(&path).unwrap();
        assert!(out.contains("aws_session_token = TOKEN"), "{}", out);
        assert!(out.contains("region = ap-south-1"), "{}", out);
        std::fs::remove_file(&path).ok();
    }

    /// Remembering keys only helps if the name written out is the name the
    /// reuse path looks for — a rename on either side silently brings back
    /// "paste the same keys on every run".
    #[test]
    fn saved_profile_is_discoverable_after_writing() {
        let path = tmp_file("discover");
        upsert_profile(&path, SAVED_PROFILE, &creds("KEY")).unwrap();

        let text = std::fs::read_to_string(&path).unwrap();
        assert!(
            parse_profile_names(&text, false).contains(&SAVED_PROFILE.to_string()),
            "{}",
            text
        );
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn profile_names_parse_from_both_file_shapes() {
        assert_eq!(
            parse_profile_names("[default]\nx=1\n[staging]\n", false),
            vec!["default", "staging"]
        );
        // config uses the "profile " prefix for everything but default
        assert_eq!(
            parse_profile_names("[default]\n[profile prod]\n", true),
            vec!["default", "prod"]
        );
    }

    /// A rejected key has to be named as such. "service error: unhandled error"
    /// is the SDK's placeholder and tells the user nothing about whether the
    /// remembered keys were deleted, disabled, or simply lack permission.
    #[test]
    fn a_service_error_is_reported_by_its_code_and_message() {
        let raw = "service error: unhandled error (InvalidClientTokenId): Error { \
                   code: \"InvalidClientTokenId\", \
                   message: \"The security token included in the request is invalid.\", \
                   aws_request_id: \"abc\" } (ServiceError(ServiceError { .. }))";
        assert_eq!(
            brief(&anyhow::anyhow!("{}", raw)),
            "InvalidClientTokenId: The security token included in the request is invalid."
        );
    }

    /// Everything else keeps the first sentence — that is already the diagnosis.
    #[test]
    fn other_errors_keep_their_first_sentence() {
        let e = anyhow::anyhow!("{}", "dispatch failure: io error: connection refused (Inner { .. })");
        assert_eq!(brief(&e), "dispatch failure: io error: connection refused");
    }

    /// The region branch must not fire on a plain credential failure, or the
    /// user gets asked for a region when what is missing is keys.
    #[test]
    fn missing_region_is_distinguished_from_missing_credentials() {
        let region_err = anyhow::anyhow!("dispatch failure: other: Invalid Configuration: Missing Region");
        let cred_err = anyhow::anyhow!("no providers in chain provided credentials");
        assert!(is_missing_region(&region_err));
        assert!(!is_missing_region(&cred_err));
        assert!(situation(&cred_err, false).contains("没有可用的 AWS 凭据"));
        // Anything else must not be reported as a credential problem.
        let other = anyhow::anyhow!("dispatch failure: io error: connection refused");
        assert!(situation(&other, false).contains("无法建立 AWS 会话"));
    }
}
