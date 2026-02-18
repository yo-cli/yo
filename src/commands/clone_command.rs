use colored::Colorize;
use ignore::WalkBuilder;
use inquire::{Confirm, Select, Text};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum CloneError {
    #[error("IO error: {0}")]
    IoError(#[from] std::io::Error),
    #[error("Invalid path: {0}")]
    InvalidPath(String),
    #[error("Source not found: {0}")]
    #[allow(dead_code)]
    SourceNotFound(String),
    #[error("User cancelled operation")]
    UserCancelled,
    #[error("Regex error: {0}")]
    RegexError(#[from] regex::Error),
    #[error("Walk directory error: {0}")]
    WalkDirError(#[from] ignore::Error),
}

/// 字符类型
#[derive(Debug, Clone, Copy, PartialEq)]
enum CharType {
    Upper,
    Lower,
    Digit,
    Separator,
    Other,
}

/// 关键词的所有变体形式
#[derive(Debug, Clone)]
struct KeywordVariants {
    kebab_case: String,      // access-log
    snake_case: String,      // access_log
    pascal_case: String,     // AccessLog
    camel_case: String,      // accessLog
    screaming_snake: String, // ACCESS_LOG
}

impl KeywordVariants {
    /// 从任意格式的输入推断出所有变体
    fn from_input(input: &str) -> Self {
        let words = Self::parse_words(input);

        Self {
            kebab_case: words.join("-").to_lowercase(),
            snake_case: words.join("_").to_lowercase(),
            pascal_case: words.iter().map(|w| Self::capitalize(w)).collect::<Vec<_>>().join(""),
            camel_case: {
                let mut result = String::new();
                for (i, word) in words.iter().enumerate() {
                    if i == 0 {
                        result.push_str(&word.to_lowercase());
                    } else {
                        result.push_str(&Self::capitalize(word));
                    }
                }
                result
            },
            screaming_snake: words.join("_").to_uppercase(),
        }
    }

    /// 解析输入字符串为单词列表
    fn parse_words(input: &str) -> Vec<String> {
        let mut words = Vec::new();
        let mut current_word = String::new();
        let mut prev_char_type = CharType::Other;

        for ch in input.chars() {
            let char_type = if ch.is_uppercase() {
                CharType::Upper
            } else if ch.is_lowercase() {
                CharType::Lower
            } else if ch.is_numeric() {
                CharType::Digit
            } else if ch == '-' || ch == '_' || ch.is_whitespace() {
                CharType::Separator
            } else {
                CharType::Other
            };

            match char_type {
                CharType::Separator => {
                    // 遇到分隔符，保存当前单词
                    if !current_word.is_empty() {
                        words.push(current_word.to_lowercase());
                        current_word.clear();
                    }
                }
                CharType::Upper => {
                    // 大写字母的处理
                    match prev_char_type {
                        CharType::Lower | CharType::Digit => {
                            // 从小写/数字到大写：新单词开始 (myWord -> my, Word)
                            if !current_word.is_empty() {
                                words.push(current_word.to_lowercase());
                                current_word.clear();
                            }
                            current_word.push(ch);
                        }
                        CharType::Upper => {
                            // 连续大写字母 (HTTPServer)
                            current_word.push(ch);
                        }
                        _ => {
                            current_word.push(ch);
                        }
                    }
                }
                CharType::Lower | CharType::Digit => {
                    // 小写字母或数字
                    if prev_char_type == CharType::Upper && current_word.len() > 1 {
                        // HTTPServer: HTTP 和 Server 分开
                        // 将最后一个大写字母移到新单词
                        let last_char = current_word.pop().unwrap();
                        words.push(current_word.to_lowercase());
                        current_word.clear();
                        current_word.push(last_char);
                    }
                    current_word.push(ch);
                }
                CharType::Other => {
                    current_word.push(ch);
                }
            }

            prev_char_type = char_type;
        }

        // 保存最后一个单词
        if !current_word.is_empty() {
            words.push(current_word.to_lowercase());
        }

        words
    }

    /// 首字母大写
    fn capitalize(s: &str) -> String {
        let mut chars = s.chars();
        match chars.next() {
            None => String::new(),
            Some(first) => first.to_uppercase().chain(chars).collect(),
        }
    }

    /// 获取所有变体的映射表（从旧到新）
    fn get_replacement_map(&self, new_variants: &KeywordVariants) -> HashMap<String, String> {
        let mut map = HashMap::new();
        map.insert(self.kebab_case.clone(), new_variants.kebab_case.clone());
        map.insert(self.snake_case.clone(), new_variants.snake_case.clone());
        map.insert(self.pascal_case.clone(), new_variants.pascal_case.clone());
        map.insert(self.camel_case.clone(), new_variants.camel_case.clone());
        map.insert(self.screaming_snake.clone(), new_variants.screaming_snake.clone());
        map
    }
}

/// 克隆命令
pub struct CloneCommand;

impl CloneCommand {
    pub fn execute() -> Result<(), CloneError> {
        println!("{}", "📋 Template Clone Tool".cyan().bold());
        println!();

        // Step 1: Scan all entries (respecting .gitignore) → type glob to filter → select
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));

        print!("  {} Scanning...", "ℹ".blue());
        std::io::Write::flush(&mut std::io::stdout()).ok();

        let mut dir_entries: Vec<String> = Vec::new();
        let mut file_entries: Vec<String> = Vec::new();

        let walker = WalkBuilder::new(&cwd)
            .max_depth(Some(5))
            .hidden(true)       // skip hidden files/dirs
            .git_ignore(true)   // respect .gitignore
            .git_global(true)   // respect global gitignore
            .git_exclude(true)  // respect .git/info/exclude
            .build();

        for entry in walker {
            if let Ok(entry) = entry {
                let rel = entry.path().strip_prefix(&cwd)
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|_| entry.path().display().to_string());
                if rel.is_empty() {
                    continue;
                }
                match entry.file_type() {
                    Some(ft) if ft.is_dir() => dir_entries.push(format!("{}/", rel)),
                    Some(ft) if ft.is_file() => file_entries.push(rel),
                    _ => {}
                }
            }
        }

        dir_entries.sort();
        file_entries.sort();

        println!(" {} dirs, {} files",
            dir_entries.len().to_string().cyan(),
            file_entries.len().to_string().cyan(),
        );

        // Directories first, then files
        let mut candidates = dir_entries;
        candidates.extend(file_entries);

        if candidates.is_empty() {
            println!("{}", "  ✗ No entries found.".red());
            return Ok(());
        }

        // Custom scorer: supports * and ? glob on the file/dir name part
        let glob_scorer = &|input: &str, _opt: &String, string_value: &str, _idx: usize| -> Option<i64> {
            let input = input.trim();
            if input.is_empty() {
                return Some(0);
            }
            let is_dir = string_value.ends_with('/');
            let path = string_value.trim_end_matches('/');
            let matched = if input.contains('*') || input.contains('?') {
                // Wildcards → glob match against full path, auto-wrap with *
                let pattern = format!(
                    "{}{}{}",
                    if input.starts_with('*') { "" } else { "*" },
                    input,
                    if input.ends_with('*') { "" } else { "*" },
                );
                Self::glob_match(&pattern, path)
            } else {
                // No wildcards → substring match against full path
                path.to_lowercase().contains(&input.to_lowercase())
            };
            if matched {
                Some(if is_dir { 1000 } else { 0 })
            } else {
                None
            }
        };

        let selected = Select::new("Source (type to filter, supports *?):", candidates)
            .with_scorer(glob_scorer)
            .prompt()
            .map_err(|_| CloneError::UserCancelled)?;

        let source = cwd.join(selected.trim_end_matches('/'));

        // Step 2: Old keyword
        let old_keyword = Text::new("Original keyword:")
            .prompt()
            .map_err(|_| CloneError::UserCancelled)?;

        // Step 3: New keyword
        let new_keyword = Text::new("New keyword:")
            .prompt()
            .map_err(|_| CloneError::UserCancelled)?;

        println!();

        // Generate keyword variants
        let old_variants = KeywordVariants::from_input(&old_keyword);
        let new_variants = KeywordVariants::from_input(&new_keyword);
        let replacement_map = old_variants.get_replacement_map(&new_variants);

        // Step 4: Preview
        println!("{}", "🔄 Keyword variants:".blue().bold());
        println!(
            "  {} → {}",
            old_variants.kebab_case.yellow(),
            new_variants.kebab_case.green()
        );
        println!(
            "  {} → {}",
            old_variants.snake_case.yellow(),
            new_variants.snake_case.green()
        );
        println!(
            "  {} → {}",
            old_variants.pascal_case.yellow(),
            new_variants.pascal_case.green()
        );
        println!(
            "  {} → {}",
            old_variants.camel_case.yellow(),
            new_variants.camel_case.green()
        );
        println!(
            "  {} → {}",
            old_variants.screaming_snake.yellow(),
            new_variants.screaming_snake.green()
        );
        println!();

        let target = Self::get_target_path(&source, &replacement_map)?;
        println!(
            "  {} {} → {} {}",
            "Source:".bold(),
            source.display().to_string().yellow(),
            "Target:".bold(),
            target.display().to_string().green()
        );
        println!();

        // Dry-run: collect rename/content change stats
        let mut renamed_entries: Vec<(String, String)> = Vec::new();
        let mut total_files: usize = 0;
        let mut content_changed: usize = 0;

        for entry in Self::walk_source(&source) {
            let entry = entry.map_err(|e| CloneError::IoError(
                std::io::Error::new(std::io::ErrorKind::Other, e.to_string())
            ))?;
            let entry_path = entry.path();
            let relative = entry_path
                .strip_prefix(&source)
                .map_err(|_| CloneError::InvalidPath(entry_path.display().to_string()))?;

            if relative.as_os_str().is_empty() {
                continue;
            }

            let new_relative = Self::replace_in_path(relative, &replacement_map);

            if new_relative != relative {
                renamed_entries.push((
                    relative.display().to_string(),
                    new_relative.display().to_string(),
                ));
            }

            if entry.file_type().map_or(false, |ft| ft.is_file()) {
                total_files += 1;
                if Self::is_text_file(entry_path) {
                    if let Ok(content) = fs::read_to_string(entry_path) {
                        let new_content = Self::replace_in_string(&content, &replacement_map);
                        if content != new_content {
                            content_changed += 1;
                        }
                    }
                }
            }
        }

        if !renamed_entries.is_empty() {
            println!("{}", "  Renamed paths:".blue().bold());
            let show_count = renamed_entries.len().min(20);
            for (old_path, new_path) in &renamed_entries[..show_count] {
                println!("    {} → {}", old_path.yellow(), new_path.green());
            }
            if renamed_entries.len() > 20 {
                println!(
                    "    {} more ...",
                    (renamed_entries.len() - 20).to_string().cyan()
                );
            }
            println!();
        }

        println!(
            "  {} files, {} with content changes, {} path renames",
            total_files.to_string().cyan(),
            content_changed.to_string().cyan(),
            renamed_entries.len().to_string().cyan()
        );
        println!();

        // Check target exists
        if target.exists() {
            println!(
                "{}",
                format!("  ⚠ Target '{}' already exists!", target.display())
                    .yellow()
                    .bold()
            );
            return Ok(());
        }

        // Step 5: Confirm
        let confirmed = Confirm::new("Proceed with clone?")
            .with_default(true)
            .prompt()
            .map_err(|_| CloneError::UserCancelled)?;

        if !confirmed {
            println!("{}", "  Cancelled.".yellow());
            return Ok(());
        }

        // Step 6: Execute clone
        println!();
        if source.is_dir() {
            Self::clone_directory(&source, &target, &replacement_map)?;
        } else {
            Self::clone_file(&source, &target, &replacement_map)?;
        }

        println!();
        println!("{}", "🎉 Clone completed successfully!".green().bold());

        Ok(())
    }

    /// 根据替换规则获取目标路径
    fn get_target_path(source: &Path, replacement_map: &HashMap<String, String>) -> Result<PathBuf, CloneError> {
        let file_name = source
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| CloneError::InvalidPath(source.display().to_string()))?;

        let new_name = Self::replace_in_string(file_name, replacement_map);

        let parent = source.parent()
            .ok_or_else(|| CloneError::InvalidPath(source.display().to_string()))?;

        Ok(parent.join(new_name))
    }

    /// 克隆目录
    fn clone_directory(source: &Path, target: &Path, replacement_map: &HashMap<String, String>) -> Result<(), CloneError> {
        println!("   📁 Cloning directory...");

        fs::create_dir_all(target)?;

        let mut file_count = 0;
        let mut modified_count = 0;

        for entry in Self::walk_source(source) {
            let entry = entry.map_err(|e| CloneError::IoError(
                std::io::Error::new(std::io::ErrorKind::Other, e.to_string())
            ))?;
            let source_path = entry.path();

            let relative_path = source_path.strip_prefix(source)
                .map_err(|_| CloneError::InvalidPath(source_path.display().to_string()))?;

            let new_relative_path = Self::replace_in_path(relative_path, replacement_map);
            let target_path = target.join(new_relative_path);

            let is_dir = entry.file_type().map_or(false, |ft| ft.is_dir());
            let is_file = entry.file_type().map_or(false, |ft| ft.is_file());

            if is_dir {
                fs::create_dir_all(&target_path)?;
            } else if is_file {
                if Self::is_text_file(source_path) {
                    let content = fs::read_to_string(source_path)?;
                    let new_content = Self::replace_in_string(&content, replacement_map);

                    if let Some(parent) = target_path.parent() {
                        fs::create_dir_all(parent)?;
                    }

                    let has_changes = content != new_content;
                    fs::write(&target_path, &new_content)?;

                    if has_changes {
                        modified_count += 1;
                    }
                } else {
                    if let Some(parent) = target_path.parent() {
                        fs::create_dir_all(parent)?;
                    }
                    fs::copy(source_path, &target_path)?;
                }

                file_count += 1;
            }
        }

        println!("   ✓ Copied {} files ({} with content modifications)", file_count, modified_count);

        Ok(())
    }

    /// 克隆单个文件
    fn clone_file(source: &Path, target: &Path, replacement_map: &HashMap<String, String>) -> Result<(), CloneError> {
        println!("   📄 Cloning file...");

        if Self::is_text_file(source) {
            let content = fs::read_to_string(source)?;
            let new_content = Self::replace_in_string(&content, replacement_map);
            let has_changes = content != new_content;
            fs::write(target, &new_content)?;

            if has_changes {
                println!("   ✓ File copied with content modifications");
            } else {
                println!("   ✓ File copied (no content changes)");
            }
        } else {
            fs::copy(source, target)?;
            println!("   ✓ File copied (binary, no modifications)");
        }

        Ok(())
    }

    /// 在字符串中替换所有关键词变体
    fn replace_in_string(content: &str, replacement_map: &HashMap<String, String>) -> String {
        let mut result = content.to_string();

        // 按长度降序排列，优先替换较长的关键词（避免部分替换）
        let mut replacements: Vec<_> = replacement_map.iter().collect();
        replacements.sort_by(|a, b| b.0.len().cmp(&a.0.len()));

        for (old, new) in replacements {
            if old.contains('-') || old.contains('_') {
                // 带分隔符的变体（kebab_case, snake_case, SCREAMING_SNAKE）直接替换
                result = result.replace(old.as_str(), new.as_str());
            } else {
                // 无分隔符的变体（PascalCase, camelCase）用智能边界替换
                result = Self::replace_with_smart_boundary(&result, old, new);
            }
        }

        result
    }

    /// 智能边界替换：边界 = 非字母数字 或 大小写转换处
    fn replace_with_smart_boundary(content: &str, old: &str, new: &str) -> String {
        let mut result = String::new();
        let chars: Vec<char> = content.chars().collect();
        let old_chars: Vec<char> = old.chars().collect();
        let old_len = old_chars.len();
        let content_len = chars.len();

        let mut i = 0;
        while i < content_len {
            if i + old_len <= content_len && chars[i..i + old_len] == old_chars[..] {
                let before_ok = if i == 0 {
                    true
                } else {
                    let prev = chars[i - 1];
                    let curr = chars[i];
                    !prev.is_alphanumeric()
                        || (prev.is_lowercase() && curr.is_uppercase())
                };

                let after_ok = if i + old_len >= content_len {
                    true
                } else {
                    let last = chars[i + old_len - 1];
                    let next = chars[i + old_len];
                    !next.is_alphanumeric()
                        || (last.is_lowercase() && next.is_uppercase())
                };

                if before_ok && after_ok {
                    result.push_str(new);
                    i += old_len;
                    continue;
                }
            }

            result.push(chars[i]);
            i += 1;
        }

        result
    }

    /// 在路径中替换关键词
    fn replace_in_path(path: &Path, replacement_map: &HashMap<String, String>) -> PathBuf {
        let mut result = PathBuf::new();

        for component in path.components() {
            if let Some(os_str) = component.as_os_str().to_str() {
                let new_component = Self::replace_in_string(os_str, replacement_map);
                result.push(new_component);
            } else {
                result.push(component);
            }
        }

        result
    }

    /// Walk source directory respecting .gitignore
    fn walk_source(source: &Path) -> ignore::Walk {
        WalkBuilder::new(source)
            .hidden(true)
            .git_ignore(true)
            .git_global(true)
            .git_exclude(true)
            .build()
    }

    /// 检查是否是文本文件
    fn is_text_file(path: &Path) -> bool {
        // 首先检查扩展名
        if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
            let text_extensions = [
                "rs", "toml", "txt", "md", "json", "yaml", "yml", "xml",
                "js", "ts", "jsx", "tsx", "html", "css", "scss", "sass",
                "py", "rb", "go", "java", "c", "cpp", "h", "hpp",
                "sh", "bash", "zsh", "fish", "ps1", "bat", "cmd",
                "sql", "graphql", "proto", "conf", "config", "ini",
                "csv", "log", "properties", "gitignore", "dockerignore",
            ];

            if text_extensions.contains(&ext.to_lowercase().as_str()) {
                return true;
            }
        }

        // 尝试读取少量字节判断
        if let Ok(mut file) = fs::File::open(path) {
            use std::io::Read;
            let mut buffer = [0u8; 512];
            if let Ok(n) = file.read(&mut buffer) {
                // 检查是否包含null字节（二进制文件的特征）
                return !buffer[..n].contains(&0);
            }
        }

        false
    }

    /// Glob match: * matches any chars, ? matches one char (case-insensitive)
    fn glob_match(pattern: &str, name: &str) -> bool {
        let p: Vec<char> = pattern.to_lowercase().chars().collect();
        let n: Vec<char> = name.to_lowercase().chars().collect();
        let (plen, nlen) = (p.len(), n.len());

        // dp[i][j] = pattern[..i] matches name[..j]
        let mut dp = vec![vec![false; nlen + 1]; plen + 1];
        dp[0][0] = true;

        // Leading *s can match empty
        for i in 1..=plen {
            if p[i - 1] == '*' {
                dp[i][0] = dp[i - 1][0];
            } else {
                break;
            }
        }

        for i in 1..=plen {
            for j in 1..=nlen {
                if p[i - 1] == '*' {
                    dp[i][j] = dp[i - 1][j] || dp[i][j - 1];
                } else if p[i - 1] == '?' || p[i - 1] == n[j - 1] {
                    dp[i][j] = dp[i - 1][j - 1];
                }
            }
        }

        dp[plen][nlen]
    }
}
