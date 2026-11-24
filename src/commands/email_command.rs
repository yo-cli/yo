// yo-file email: collect the email addresses in CSV files into emails.txt.
//
// Built to need no thought: with no argument it reads every CSV in the current
// directory one by one and merges the results into one deduplicated list.
// Every field of every row is scanned, so it works whether a file has one
// email column, several, or addresses buried in free text.

use colored::Colorize;
use regex::bytes::Regex;
use std::collections::HashSet;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use thiserror::Error;

/// Practical ASCII address: dot-atom local part (`'` allowed inside it, as in
/// o'brien@...), dotted domain, alphabetic TLD.
///
/// It runs on raw bytes, not text: Chinese Excel saves CSV as GBK, and an ASCII
/// pattern finds ASCII addresses in any ASCII-compatible encoding. This is also
/// why it is not `linkify`: that crate needs `&str`, and its local part accepts
/// any non-ASCII char, so it would read `邮箱：a@b.com` as a single address.
const EMAIL_PATTERN: &str = r"(?-u)[A-Za-z0-9_%+-]+(?:['.][A-Za-z0-9_%+-]+)*@(?:[A-Za-z0-9](?:[A-Za-z0-9-]*[A-Za-z0-9])?\.)+[A-Za-z]{2,}\b";

/// Written next to the first input. A fixed name, so a re-run refreshes it,
/// and a `.txt`, so a re-run never reads it back as input.
const OUTPUT_FILE: &str = "emails.txt";

/// How many line numbers of address-less rows the summary lists.
const MAX_LINES_SHOWN: usize = 10;

#[derive(Debug, Error)]
pub enum EmailError {
    #[error("Cannot read {path}: {source}")]
    ReadFailed { path: String, source: io::Error },
    #[error("Cannot parse {path}: {source}")]
    ParseFailed { path: String, source: csv::Error },
    #[error("Cannot write {path}: {source}")]
    WriteFailed { path: String, source: io::Error },
    #[error("No .csv file in {0}")]
    NoCsvFound(String),
}

/// Addresses merged across every file read: first-seen order and spelling,
/// deduplicated case-insensitively.
#[derive(Debug, Default)]
struct MergedEmails {
    emails: Vec<String>,
    seen: HashSet<String>,
    /// Repeats dropped, within a file or across files.
    duplicates: usize,
}

impl MergedEmails {
    fn insert(&mut self, email: String) {
        if self.seen.insert(email.to_ascii_lowercase()) {
            self.emails.push(email);
        } else {
            self.duplicates += 1;
        }
    }
}

/// One file's numbers, for its summary line.
#[derive(Debug, Default, PartialEq)]
struct CsvStats {
    /// Non-blank rows read, header included.
    rows: usize,
    /// Addresses found, repeats included.
    emails: usize,
    /// Line numbers (1-based) of data rows that had no address.
    rows_without_email: Vec<u64>,
}

pub struct EmailCommand;

impl EmailCommand {
    /// `paths` are CSV files or directories; none means the current directory.
    pub fn execute(paths: &[String]) -> Result<(), EmailError> {
        let csv_paths = Self::resolve_inputs(paths)?;

        let mut merged_emails = MergedEmails::default();
        for csv_path in &csv_paths {
            let shown_path = csv_path.display().to_string();
            let file_bytes = fs::read(csv_path).map_err(|source| EmailError::ReadFailed {
                path: shown_path.clone(),
                source,
            })?;
            let csv_stats = extract(&transcode_utf16(file_bytes), &mut merged_emails).map_err(|source| {
                EmailError::ParseFailed {
                    path: shown_path.clone(),
                    source,
                }
            })?;
            Self::print_csv_summary(&shown_path, &csv_stats);
        }
        println!();

        if merged_emails.emails.is_empty() {
            println!("{}", format!("⚠ No email found, {} not written", OUTPUT_FILE).yellow().bold());
            return Ok(());
        }

        let output_path = csv_paths[0].with_file_name(OUTPUT_FILE);
        let mut contents = merged_emails.emails.join("\n");
        contents.push('\n');
        fs::write(&output_path, contents).map_err(|source| EmailError::WriteFailed {
            path: output_path.display().to_string(),
            source,
        })?;

        let mut summary = format!(
            "🎉 {} saved to {}",
            plural(merged_emails.emails.len(), "email"),
            output_path.display()
        );
        if merged_emails.duplicates > 0 {
            summary.push_str(&format!(" ({} dropped)", plural(merged_emails.duplicates, "duplicate")));
        }
        println!("{}", summary.green().bold());
        Ok(())
    }

    /// Directories expand to the CSVs directly inside them; files are taken as given.
    fn resolve_inputs(paths: &[String]) -> Result<Vec<PathBuf>, EmailError> {
        if paths.is_empty() {
            return csv_files_in(Path::new(""));
        }
        let mut csv_paths = Vec::new();
        for path in paths.iter().map(PathBuf::from) {
            if path.is_dir() {
                csv_paths.extend(csv_files_in(&path)?);
            } else {
                csv_paths.push(path);
            }
        }
        Ok(csv_paths)
    }

    fn print_csv_summary(shown_path: &str, csv_stats: &CsvStats) {
        if csv_stats.emails == 0 {
            println!("  {} {}: no email in {}", "⚠".yellow(), shown_path, plural(csv_stats.rows, "row"));
            return;
        }
        println!(
            "  {} {}: {} in {}",
            "✓".green(),
            shown_path,
            plural(csv_stats.emails, "email").cyan(),
            plural(csv_stats.rows, "row"),
        );

        let missing = &csv_stats.rows_without_email;
        if !missing.is_empty() {
            let shown: Vec<String> = missing.iter().take(MAX_LINES_SHOWN).map(u64::to_string).collect();
            let more = match missing.len().saturating_sub(MAX_LINES_SHOWN) {
                0 => String::new(),
                rest => format!(" ... +{} more", rest),
            };
            println!(
                "    {} {} without an email, {} {}{}",
                "⚠".yellow(),
                plural(missing.len(), "row").yellow(),
                if missing.len() == 1 { "line" } else { "lines" },
                shown.join(", "),
                more,
            );
        }
    }
}

/// The `.csv` files directly inside `dir` ("" is the current directory), by
/// name. No recursion, so a project's fixtures or node_modules never leak in.
/// Hidden files (macOS `._*` copies) and Excel's `~$*` lock files are skipped.
fn csv_files_in(dir: &Path) -> Result<Vec<PathBuf>, EmailError> {
    let listed_dir = if dir.as_os_str().is_empty() { Path::new(".") } else { dir };
    let read_failed = |source| EmailError::ReadFailed {
        path: listed_dir.display().to_string(),
        source,
    };

    let mut csv_paths = Vec::new();
    for dir_entry in fs::read_dir(listed_dir).map_err(read_failed)? {
        let file_name = dir_entry.map_err(read_failed)?.file_name();
        let name = file_name.to_string_lossy();
        let csv_path = dir.join(&file_name);
        if !name.starts_with('.')
            && !name.starts_with("~$")
            && csv_path.extension().is_some_and(|ext| ext.eq_ignore_ascii_case("csv"))
            && csv_path.is_file()
        {
            csv_paths.push(csv_path);
        }
    }

    if csv_paths.is_empty() {
        let shown_dir = if dir.as_os_str().is_empty() {
            "the current directory".to_string()
        } else {
            dir.display().to_string()
        };
        return Err(EmailError::NoCsvFound(shown_dir));
    }
    csv_paths.sort();
    Ok(csv_paths)
}

/// UTF-16 (recognized by its BOM, as Windows tools write it) is transcoded to
/// UTF-8 so the ASCII pattern can see the addresses. Every other encoding the
/// pattern already reads as-is.
fn transcode_utf16(file_bytes: Vec<u8>) -> Vec<u8> {
    let little_endian = match file_bytes.get(..2) {
        Some([0xFF, 0xFE]) => true,
        Some([0xFE, 0xFF]) => false,
        _ => return file_bytes,
    };
    let code_units = file_bytes[2..].chunks_exact(2).map(|pair| {
        let pair = [pair[0], pair[1]];
        if little_endian {
            u16::from_le_bytes(pair)
        } else {
            u16::from_be_bytes(pair)
        }
    });
    char::decode_utf16(code_units)
        .map(|decoded| decoded.unwrap_or(char::REPLACEMENT_CHARACTER))
        .collect::<String>()
        .into_bytes()
}

/// Reads one CSV into `merged_emails`. Fields are raw bytes (any
/// ASCII-compatible encoding works), and `flexible` tolerates the ragged rows
/// that spreadsheet exports often have.
fn extract(csv_bytes: &[u8], merged_emails: &mut MergedEmails) -> Result<CsvStats, csv::Error> {
    let email_regex = Regex::new(EMAIL_PATTERN).expect("EMAIL_PATTERN is a valid regex");
    let mut csv_reader = csv::ReaderBuilder::new()
        .has_headers(false) // a header-less file must not lose its first address
        .flexible(true)
        .from_reader(csv_bytes);

    let mut csv_stats = CsvStats::default();
    for byte_record in csv_reader.byte_records() {
        let byte_record = byte_record?;
        if byte_record.iter().all(|field| field.trim_ascii().is_empty()) {
            continue;
        }
        csv_stats.rows += 1;

        let emails_before_row = csv_stats.emails;
        for field in byte_record.iter() {
            for email_match in email_regex.find_iter(field) {
                if is_fragment(field, email_match.start(), email_match.end()) {
                    continue;
                }
                csv_stats.emails += 1;
                // The pattern only matches ASCII, so this never replaces anything.
                merged_emails.insert(String::from_utf8_lossy(email_match.as_bytes()).into_owned());
            }
        }

        // An address-less first row is the header, not missing data.
        if csv_stats.emails == emails_before_row && csv_stats.rows > 1 {
            let position = byte_record.position().expect("csv::Reader positions every record");
            csv_stats.rows_without_email.push(position.line());
        }
    }
    Ok(csv_stats)
}

/// A match that touches another address byte is a piece of a malformed token
/// (`a..b@x.com`, `a@b@x.com`, `josé.g@x.com`). Dropping it beats returning a
/// wrong address. A trailing `.` is fine: it ends sentences.
fn is_fragment(field: &[u8], start: usize, end: usize) -> bool {
    let continues_address = |byte: u8| byte.is_ascii_alphanumeric() || b"._%+-@".contains(&byte);
    let touches_before = start > 0 && continues_address(field[start - 1]);
    let touches_after = field.get(end).is_some_and(|&byte| byte != b'.' && continues_address(byte));
    touches_before || touches_after
}

/// `plural(1, "row")` is "1 row", `plural(3, "row")` is "3 rows".
fn plural(count: usize, noun: &str) -> String {
    if count == 1 {
        format!("1 {}", noun)
    } else {
        format!("{} {}s", count, noun)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(csv_bytes: &[u8]) -> (MergedEmails, CsvStats) {
        let mut merged_emails = MergedEmails::default();
        let csv_stats = extract(csv_bytes, &mut merged_emails).expect("in-memory CSV is always readable");
        (merged_emails, csv_stats)
    }

    fn tmp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("yo-file-email-test-{}-{}", name, uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn empty_input_yields_nothing() {
        let (merged_emails, csv_stats) = run(b"");
        assert!(merged_emails.emails.is_empty());
        assert_eq!(csv_stats, CsvStats::default());
    }

    #[test]
    fn header_row_is_not_reported_as_missing_email() {
        let (merged_emails, csv_stats) = run(b"name,email\r\nAlice,alice@example.com\r\nBob,bob@example.org\r\n");
        assert_eq!(merged_emails.emails, ["alice@example.com", "bob@example.org"]);
        assert_eq!(csv_stats, CsvStats { rows: 3, emails: 2, rows_without_email: vec![] });
    }

    #[test]
    fn headerless_file_keeps_its_first_address() {
        let (merged_emails, _) = run(b"alice@example.com\nbob@example.org\n");
        assert_eq!(merged_emails.emails, ["alice@example.com", "bob@example.org"]);
    }

    #[test]
    fn scans_every_column_and_embedded_forms() {
        let (merged_emails, _) = run(b"id,contact,notes\n\
            1,\"Alice <alice@example.com>\",mailto:alice.work+cn@sub.example.co.uk\n\
            2,(bob@example.org),\"cc: carol@example.net; dave@example.io.\"\n");
        assert_eq!(
            merged_emails.emails,
            [
                "alice@example.com",
                "alice.work+cn@sub.example.co.uk",
                "bob@example.org",
                "carol@example.net",
                "dave@example.io",
            ]
        );
    }

    #[test]
    fn dedups_case_insensitively_keeping_first_spelling() {
        let (merged_emails, csv_stats) =
            run(b"Alice@Example.com\nbob@example.org\nalice@example.COM\nbob@example.org\n");
        assert_eq!(merged_emails.emails, ["Alice@Example.com", "bob@example.org"]);
        assert_eq!(merged_emails.duplicates, 2);
        assert_eq!(csv_stats.emails, 4);
    }

    #[test]
    fn files_merge_in_read_order_and_repeats_across_files_drop() {
        let mut merged_emails = MergedEmails::default();
        extract(b"alice@example.com\nbob@example.org\n", &mut merged_emails).unwrap();
        let second_stats = extract(b"BOB@example.org\ncarol@example.net\n", &mut merged_emails).unwrap();
        assert_eq!(merged_emails.emails, ["alice@example.com", "bob@example.org", "carol@example.net"]);
        assert_eq!(merged_emails.duplicates, 1);
        assert_eq!(second_stats.emails, 2);
    }

    #[test]
    fn reports_line_numbers_of_rows_without_email() {
        // Bob's quoted field spans lines 3-4, so Carol's row starts on line 5;
        // the trailing ",," row is blank and neither counted nor reported.
        let (_, csv_stats) = run(b"name,email\nAlice,alice@example.com\nBob,\"no email\nyet\"\nCarol,\n,,\n");
        assert_eq!(csv_stats.rows_without_email, [3, 5]);
        assert_eq!(csv_stats.rows, 4);
    }

    #[test]
    fn gbk_text_glued_to_address_still_matches() {
        // "邮箱：" in GBK, the encoding Chinese Excel saves CSV in.
        let (merged_emails, _) = run(b"\xd3\xca\xcf\xe4\xa3\xbaalice@example.com\n");
        assert_eq!(merged_emails.emails, ["alice@example.com"]);
    }

    #[test]
    fn utf8_bom_and_chinese_text_glued_to_address_still_match() {
        let (merged_emails, _) = run("\u{feff}联系人bob@example.org\n".as_bytes());
        assert_eq!(merged_emails.emails, ["bob@example.org"]);
    }

    #[test]
    fn utf16_with_bom_is_transcoded() {
        let text = "name,email\r\n张三,zhangsan@example.cn\r\n";
        let mut little_endian = vec![0xFF, 0xFE];
        let mut big_endian = vec![0xFE, 0xFF];
        for code_unit in text.encode_utf16() {
            little_endian.extend(code_unit.to_le_bytes());
            big_endian.extend(code_unit.to_be_bytes());
        }
        for utf16_bytes in [little_endian, big_endian] {
            let (merged_emails, csv_stats) = run(&transcode_utf16(utf16_bytes));
            assert_eq!(merged_emails.emails, ["zhangsan@example.cn"]);
            assert_eq!(csv_stats.rows, 2);
        }
    }

    #[test]
    fn bytes_without_utf16_bom_pass_through() {
        let gbk_bytes = b"\xd3\xca\xcf\xe4\xa3\xbaalice@example.com\n".to_vec();
        assert_eq!(transcode_utf16(gbk_bytes.clone()), gbk_bytes);
    }

    #[test]
    fn apostrophe_inside_local_part_is_kept() {
        let (merged_emails, _) = run(b"o'brien@example.ie\n'quoted@example.com'\n");
        assert_eq!(merged_emails.emails, ["o'brien@example.ie", "quoted@example.com"]);
    }

    #[test]
    fn malformed_tokens_yield_nothing_rather_than_a_fragment() {
        let (merged_emails, _) = run(b"a..b@example.com\n\
            x@y@example.com\n\
            a@example.com@b\n\
            a@example.com-x\n\
            jos\xc3\xa9.garcia@example.com\n\
            foo.@example.com\n\
            foo@-bar.com\n\
            foo@bar\n\
            @handle\n\
            foo@1.2.3.4\n");
        assert!(merged_emails.emails.is_empty(), "{:?}", merged_emails.emails);
    }

    #[test]
    fn wrong_delimiter_still_finds_addresses() {
        let (merged_emails, _) = run(b"name;email\nAlice;alice@example.com\n");
        assert_eq!(merged_emails.emails, ["alice@example.com"]);
    }

    #[test]
    fn csv_files_in_lists_visible_csvs_directly_inside_by_name() {
        let dir = tmp_dir("list");
        for name in ["b.csv", "A.CSV", "notes.txt", ".hidden.csv", "._b.csv", "~$b.csv"] {
            fs::write(dir.join(name), "x@example.com\n").unwrap();
        }
        fs::create_dir_all(dir.join("sub")).unwrap();
        fs::write(dir.join("sub").join("c.csv"), "y@example.com\n").unwrap();
        fs::create_dir_all(dir.join("folder.csv")).unwrap();

        let csv_paths = csv_files_in(&dir).unwrap();
        assert_eq!(csv_paths, [dir.join("A.CSV"), dir.join("b.csv")]);
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn directory_without_csv_is_an_error() {
        let dir = tmp_dir("empty");
        fs::write(dir.join("notes.txt"), "x@example.com\n").unwrap();
        assert!(matches!(csv_files_in(&dir), Err(EmailError::NoCsvFound(_))));
        fs::remove_dir_all(&dir).ok();
    }
}
