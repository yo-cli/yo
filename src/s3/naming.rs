// Bucket names the tool invents when the user has none in mind.
//
// The bucket this tool fills is disposable and gets created when it is missing,
// so a name the tool invents is worth exactly as much as one the user invents —
// and "I have not made a bucket yet" must never be the thing that ends the run.
//
// The names are assembled from words a real deployment would use, not from
// random characters: `northpeak-db-backups-prod` is the kind of name a person
// types, `db-backup-k3f9x2a1` is not. Same reason the default prefix is
// `backup/` and the objects are `db-backup-*.tar.gz` — everything this tool
// leaves in an account should read like the backup job it imitates.
//
// Uniqueness comes from the width of the combination rather than from a random
// tail: bucket names are global, so a name has to be free on the first try, and
// four segments drawn from these lists give millions of combinations while a
// plain `acme-backups` would have been taken years ago.

use chrono::{Datelike, Utc};
use rand::Rng;

/// Company-ish name halves. Concatenated without a hyphen (`northpeak`,
/// `cedargrove`), which is how the org part of a real bucket name reads.
const ORG_HEADS: &[&str] = &[
    "north", "south", "east", "west", "blue", "green", "red", "silver", "gold", "iron", "stone",
    "river", "lake", "pine", "cedar", "oak", "maple", "summit", "ridge", "crest", "bright",
    "clear", "swift", "true", "prime", "grand", "star", "moon", "sun", "sky", "cloud", "wind",
];
const ORG_TAILS: &[&str] = &[
    "peak", "vale", "wave", "ridge", "field", "bridge", "gate", "port", "harbor", "creek",
    "brook", "grove", "hill", "point", "shore", "bay", "line", "path", "way", "works", "labs",
    "tech", "soft", "data", "byte", "logic", "core", "forge", "craft", "house", "group",
    "systems",
];

/// What is being backed up.
const ROLES: &[&str] = &[
    "db", "pg", "mysql", "rds", "app", "api", "svc", "web", "log", "etl", "media", "assets",
];
/// What the bucket is for.
const PURPOSES: &[&str] = &[
    "backup", "backups", "archive", "snapshot", "snapshots", "vault", "dumps", "restore",
];
/// Which environment it belongs to.
const ENVS: &[&str] = &["prod", "prd", "staging", "stage", "dev", "live", "ops"];

fn pick<'a>(rng: &mut impl Rng, words: &[&'a str]) -> &'a str {
    words[rng.random_range(0..words.len())]
}

/// One plausible, S3-legal, almost certainly unclaimed bucket name.
pub fn suggest_bucket_name() -> String {
    let mut rng = rand::rng();

    let head = pick(&mut rng, ORG_HEADS);
    // `ridgeridge` is the one combination that gives the invention away.
    let org = loop {
        let tail = pick(&mut rng, ORG_TAILS);
        if tail != head {
            break format!("{}{}", head, tail);
        }
    };
    let role = pick(&mut rng, ROLES);
    let purpose = pick(&mut rng, PURPOSES);
    let env = pick(&mut rng, ENVS);
    // Read off the clock rather than hardcoded: a name minted in 2030 that says
    // `-2018` is the one detail that would not survive a second look.
    let year = Utc::now().year() - rng.random_range(0..3);

    // Every shape carries four segments: three would be a name somebody else
    // has already registered.
    match rng.random_range(0..4) {
        0 => format!("{}-{}-{}-{}", org, role, purpose, env),
        1 => format!("{}-{}-{}-{}", org, role, purpose, year),
        2 => format!("{}-{}-{}-{}", org, env, role, purpose),
        _ => format!("{}-{}-{}-{}", org, purpose, env, year),
    }
}

/// A batch to choose from. Picking beats accepting: the one name a prompt could
/// have pre-filled is a name the user can only reject by inventing their own.
///
/// Redraws on a repeat inside the batch — `count` is a handful and the
/// combination space is millions wide, so the loop finishes immediately.
pub fn suggest_bucket_names(count: usize) -> Vec<String> {
    let mut names: Vec<String> = Vec::with_capacity(count);
    while names.len() < count {
        let name = suggest_bucket_name();
        if !names.contains(&name) {
            names.push(name);
        }
    }
    names
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::s3::crr::dest_bucket_name;
    use crate::s3::modes::crr::DEST_POOL;

    /// A suggested name is created without further review, so an illegal one
    /// fails at CreateBucket with an AWS error the user did not cause.
    #[test]
    fn suggestions_obey_the_s3_bucket_naming_rules() {
        for name in suggest_bucket_names(200) {
            assert!((3..=63).contains(&name.len()), "{} 长度非法", name);
            assert!(
                name.bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-'),
                "{} 含非法字符",
                name
            );
            let first = name.as_bytes()[0];
            let last = name.as_bytes()[name.len() - 1];
            assert!(first.is_ascii_lowercase(), "{} 首字符非法", name);
            assert!(last.is_ascii_alphanumeric(), "{} 尾字符非法", name);
            // Reserved affixes, and the "--" that only the special forms use.
            assert!(!name.contains("--"), "{} 含保留形式", name);
            assert!(!name.starts_with("xn--"), "{} 前缀保留", name);
            assert!(!name.starts_with("sthree-"), "{} 前缀保留", name);
            assert!(!name.starts_with("amzn-s3-demo-"), "{} 前缀保留", name);
            assert!(!name.ends_with("-s3alias"), "{} 后缀保留", name);
        }
    }

    /// Destination buckets are named `<源桶>-crr-<region>`, truncated to fit 63
    /// chars. A suggested name long enough to be cut would still work, but its
    /// destinations would carry a mangled half of it — so the widest wording
    /// these lists can produce has to survive whole, in every region of the pool.
    #[test]
    fn even_the_widest_wording_survives_in_a_destination_name() {
        let longest = |words: &[&str]| words.iter().map(|w| w.len()).max().unwrap_or(0);
        let widest = longest(ORG_HEADS)
            + longest(ORG_TAILS)
            + 1
            + longest(ROLES)
            + 1
            + longest(PURPOSES)
            + 1
            + longest(ENVS).max(4); // 4 = a year
        let source = "b".repeat(widest);
        for region in DEST_POOL {
            let dest = dest_bucket_name(&source, region);
            assert!(
                dest.starts_with(&source),
                "{} 在 {} 的目标桶名里被截断({})",
                source,
                region,
                dest
            );
        }
    }
}
