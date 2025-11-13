use colored::Colorize;
use inquire::Text;
use regex::Regex;
use std::collections::HashMap;
use std::fs;
use std::io::{self, Write as IoWrite};
use std::path::{Path, PathBuf};
use thiserror::Error;
use walkdir::WalkDir;

#[derive(Debug, Error)]
pub enum CloneError {
    #[error("IO error: {0}")]
    IoError(#[from] std::io::Error),
    #[error("Invalid path: {0}")]
    InvalidPath(String),
    #[error("Source not found: {0}")]
    SourceNotFound(String),
    #[error("User cancelled operation")]
    UserCancelled,
    #[error("Regex error: {0}")]
    RegexError(#[from] regex::Error),
    #[error("Walk directory error: {0}")]
    WalkDirError(#[from] walkdir::Error),
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

        // 1. 询问原关键词
        let old_keyword = Text::new("Enter original keyword:")
            .prompt()
            .map_err(|_| CloneError::UserCancelled)?;

        // 2. 询问新关键词
        let new_keyword = Text::new("Enter new keyword:")
            .prompt()
            .map_err(|_| CloneError::UserCancelled)?;

        println!();

        // 生成关键词变体
        let old_variants = KeywordVariants::from_input(&old_keyword);
        let new_variants = KeywordVariants::from_input(&new_keyword);

        println!("{}", "🔄 Keyword variants:".blue().bold());
        println!("  Old: {} | {} | {} | {} | {}",
            old_variants.kebab_case.yellow(),
            old_variants.snake_case.yellow(),
            old_variants.pascal_case.yellow(),
            old_variants.camel_case.yellow(),
            old_variants.screaming_snake.yellow()
        );
        println!("  New: {} | {} | {} | {} | {}",
            new_variants.kebab_case.green(),
            new_variants.snake_case.green(),
            new_variants.pascal_case.green(),
            new_variants.camel_case.green(),
            new_variants.screaming_snake.green()
        );
        println!();

        // 3. 循环收集源路径
        let mut sources: Vec<PathBuf> = Vec::new();
        let mut index = 1;

        println!("{}", "📂 Add source paths (enter empty to finish):".blue().bold());
        loop {
            let prompt = format!("Source path #{}", index);
            let input = Text::new(&prompt)
                .with_default("")
                .prompt()
                .map_err(|_| CloneError::UserCancelled)?;

            if input.trim().is_empty() {
                break;
            }

            let path = PathBuf::from(input.trim());
            if !path.exists() {
                println!("{}", format!("  ⚠️  Path does not exist: {}", path.display()).yellow());
                continue;
            }

            sources.push(path);
            index += 1;
        }

        if sources.is_empty() {
            println!("{}", "❌ No sources specified. Exiting.".red());
            return Ok(());
        }

        println!();
        println!("{}", format!("✅ Collected {} source(s)", sources.len()).green().bold());
        println!();

        // 4. 处理每个源
        let replacement_map = old_variants.get_replacement_map(&new_variants);

        for (idx, source) in sources.iter().enumerate() {
            println!("{}", format!("✅ Processing source #{} ...", idx + 1).cyan().bold());
            println!("   Source: {}", source.display().to_string().yellow());

            let target = Self::get_target_path(source, &replacement_map)?;
            println!("   Target: {}", target.display().to_string().green());
            println!();

            // 检查目标是否存在
            if target.exists() {
                println!("{}", format!("   ⚠️  Target '{}' already exists!", target.display()).yellow().bold());
                print!("   Please delete it manually, then press Y to continue (or N to skip): ");
                io::stdout().flush()?;

                let mut response = String::new();
                io::stdin().read_line(&mut response)?;

                if !response.trim().eq_ignore_ascii_case("y") {
                    println!("{}", "   ⏭️  Skipped.".yellow());
                    println!();
                    continue;
                }

                // 再次检查
                if target.exists() {
                    println!("{}", "   ❌ Target still exists. Skipping.".red());
                    println!();
                    continue;
                }
            }

            // 执行克隆
            if source.is_dir() {
                Self::clone_directory(source, &target, &replacement_map)?;
            } else {
                Self::clone_file(source, &target, &replacement_map)?;
            }

            println!("{}", "   ✓ Clone completed!".green().bold());
            println!();
        }

        println!("{}", "🎉 All clones completed successfully!".green().bold());

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

        // 创建目标目录
        fs::create_dir_all(target)?;

        let mut file_count = 0;
        let mut modified_count = 0;

        // 遍历源目录
        for entry in WalkDir::new(source).into_iter().filter_entry(|e| Self::should_include(e)) {
            let entry = entry?;
            let source_path = entry.path();

            // 计算相对路径
            let relative_path = source_path.strip_prefix(source)
                .map_err(|_| CloneError::InvalidPath(source_path.display().to_string()))?;

            // 应用替换到相对路径的每个部分
            let new_relative_path = Self::replace_in_path(relative_path, replacement_map);
            let target_path = target.join(new_relative_path);

            if source_path.is_dir() {
                fs::create_dir_all(&target_path)?;
            } else if source_path.is_file() {
                // 检查是否是文本文件
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
                    // 二进制文件直接复制
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

    /// 在字符串中替换关键词（完整单词边界匹配）
    fn replace_in_string(content: &str, replacement_map: &HashMap<String, String>) -> String {
        let mut result = content.to_string();

        // 按长度降序排列，优先替换较长的关键词（避免部分替换）
        let mut replacements: Vec<_> = replacement_map.iter().collect();
        replacements.sort_by(|a, b| b.0.len().cmp(&a.0.len()));

        for (old, new) in replacements {
            // 手动实现单词边界匹配（不使用 lookahead/lookbehind）
            result = Self::replace_with_boundary(&result, old, new);
        }

        result
    }

    /// 在字符串中替换关键词，确保单词边界
    /// 边界定义：分隔符（-_）、空白、或字符串边界
    fn replace_with_boundary(content: &str, old: &str, new: &str) -> String {
        let mut result = String::new();
        let chars: Vec<char> = content.chars().collect();
        let old_chars: Vec<char> = old.chars().collect();
        let old_len = old_chars.len();
        let content_len = chars.len();

        let mut i = 0;
        while i < content_len {
            // 检查是否匹配
            let mut matches = false;
            if i + old_len <= content_len {
                matches = true;
                for j in 0..old_len {
                    if chars[i + j] != old_chars[j] {
                        matches = false;
                        break;
                    }
                }
            }

            if matches {
                // 检查前边界
                let before_ok = if i == 0 {
                    true
                } else {
                    let prev_char = chars[i - 1];
                    Self::is_boundary_char(prev_char)
                };

                // 检查后边界
                let after_ok = if i + old_len >= content_len {
                    true
                } else {
                    let next_char = chars[i + old_len];
                    Self::is_boundary_char(next_char)
                };

                if before_ok && after_ok {
                    // 符合边界条件，执行替换
                    result.push_str(new);
                    i += old_len;
                    continue;
                }
            }

            // 不匹配或不符合边界，保留原字符
            result.push(chars[i]);
            i += 1;
        }

        result
    }

    /// 检查字符是否是边界字符
    fn is_boundary_char(ch: char) -> bool {
        ch == '-' || ch == '_' || ch.is_whitespace()
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

    /// 检查是否应该包含此目录项
    fn should_include(entry: &walkdir::DirEntry) -> bool {
        let file_name = entry.file_name().to_str().unwrap_or("");

        // 排除的目录和文件
        let excluded = [
            ".git",
            "node_modules",
            "target",
            "build",
            "dist",
            ".idea",
            ".vscode",
            "__pycache__",
            ".pytest_cache",
            ".mypy_cache",
            "coverage",
            ".coverage",
            "*.pyc",
            "*.pyo",
            "*.pyd",
        ];

        for pattern in &excluded {
            if file_name == *pattern || file_name.ends_with(pattern.trim_start_matches('*')) {
                return false;
            }
        }

        true
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
}
