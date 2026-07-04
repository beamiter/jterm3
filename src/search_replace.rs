#![allow(dead_code)]
use serde::{Deserialize, Serialize};

/// 高级搜索配置
#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct SearchConfig {
    pub use_regex: bool,
    pub case_sensitive: bool,
    pub whole_word: bool,
    pub multi_line: bool,
}

/// 替换选项
#[derive(Clone, Debug)]
pub struct ReplaceOptions {
    pub replace_all: bool,
    pub preserve_case: bool,
}

impl Default for ReplaceOptions {
    fn default() -> Self {
        ReplaceOptions {
            replace_all: false,
            preserve_case: true,
        }
    }
}

/// 搜索和替换引擎
pub struct SearchAndReplaceEngine;

impl SearchAndReplaceEngine {
    /// 执行搜索和替换
    pub fn search_and_replace(
        text: &str,
        search_pattern: &str,
        replacement: &str,
        config: &SearchConfig,
        options: &ReplaceOptions,
    ) -> Result<(String, usize), String> {
        if config.use_regex {
            Self::regex_replace(text, search_pattern, replacement, config, options)
        } else {
            Self::literal_replace(text, search_pattern, replacement, config, options)
        }
    }

    /// 文字替换
    fn literal_replace(
        text: &str,
        pattern: &str,
        replacement: &str,
        config: &SearchConfig,
        options: &ReplaceOptions,
    ) -> Result<(String, usize), String> {
        let search_pattern = if config.case_sensitive {
            pattern.to_string()
        } else {
            pattern.to_lowercase()
        };

        // Empty pattern would match endlessly; treat as a no-op.
        if search_pattern.is_empty() {
            return Ok((text.to_string(), 0));
        }

        if !config.case_sensitive {
            use regex::RegexBuilder;

            let re = RegexBuilder::new(&regex::escape(pattern))
                .case_insensitive(true)
                .build()
                .map_err(|e| format!("Invalid search pattern: {}", e))?;

            let result = if options.replace_all {
                re.replace_all(text, |_: &regex::Captures| replacement)
                    .to_string()
            } else {
                re.replace(text, |_: &regex::Captures| replacement)
                    .to_string()
            };

            let count = if options.replace_all {
                re.find_iter(text).count()
            } else if re.is_match(text) {
                1
            } else {
                0
            };

            return Ok((result, count));
        }

        let mut result = String::with_capacity(text.len());
        let mut count = 0;
        let mut rest = text;

        loop {
            let hay = if config.case_sensitive {
                rest.to_string()
            } else {
                rest.to_lowercase()
            };

            if let Some(pos) = hay.find(&search_pattern) {
                // Copy the unmatched prefix, emit the replacement, then continue
                // AFTER the match so replaced text is never rescanned (the old
                // code rescanned from 0 and looped forever when the replacement
                // still contained the pattern, e.g. "a" -> "ba").
                result.push_str(&rest[..pos]);
                result.push_str(replacement);
                count += 1;
                rest = &rest[pos + pattern.len()..];

                if !options.replace_all {
                    break;
                }
            } else {
                break;
            }
        }
        result.push_str(rest);

        Ok((result, count))
    }

    /// 正则表达式替换
    fn regex_replace(
        text: &str,
        pattern: &str,
        replacement: &str,
        config: &SearchConfig,
        options: &ReplaceOptions,
    ) -> Result<(String, usize), String> {
        use regex::RegexBuilder;

        let regex = RegexBuilder::new(pattern)
            .case_insensitive(!config.case_sensitive)
            .multi_line(config.multi_line)
            .build()
            .map_err(|e| format!("Invalid regex: {}", e))?;

        let result = if options.replace_all {
            regex.replace_all(text, replacement).to_string()
        } else {
            regex.replace(text, replacement).to_string()
        };

        let count = if options.replace_all {
            regex.find_iter(text).count()
        } else if regex.is_match(text) {
            1
        } else {
            0
        };

        Ok((result, count))
    }

    /// 获取搜索匹配的上下文（用于预览）
    pub fn get_match_context(text: &str, pattern: &str, context_lines: usize) -> Vec<String> {
        let lines: Vec<&str> = text.lines().collect();
        let mut result = Vec::new();

        for (idx, line) in lines.iter().enumerate() {
            if line.contains(pattern) {
                let start = idx.saturating_sub(context_lines);
                let end = std::cmp::min(idx + context_lines + 1, lines.len());

                for i in start..end {
                    let prefix = if i == idx { "→ " } else { "  " };
                    result.push(format!("{}{:3}: {}", prefix, i + 1, lines[i]));
                }
                result.push(String::new()); // 空行分隔
            }
        }

        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_literal_replace() {
        let config = SearchConfig::default();
        let options = ReplaceOptions::default();

        let (result, count) = SearchAndReplaceEngine::search_and_replace(
            "hello world hello",
            "hello",
            "hi",
            &config,
            &options,
        )
        .unwrap();

        assert_eq!(count, 1);
        assert_eq!(result, "hi world hello");
    }

    #[test]
    fn test_replace_all() {
        let config = SearchConfig::default();
        let mut options = ReplaceOptions::default();
        options.replace_all = true;

        let (result, count) = SearchAndReplaceEngine::search_and_replace(
            "hello world hello",
            "hello",
            "hi",
            &config,
            &options,
        )
        .unwrap();

        assert_eq!(count, 2);
        assert_eq!(result, "hi world hi");
    }

    #[test]
    fn test_literal_replace_case_insensitive_unicode() {
        let mut config = SearchConfig::default();
        config.case_sensitive = false;
        let options = ReplaceOptions::default();

        let (result, count) =
            SearchAndReplaceEngine::search_and_replace("İB", "b", "X", &config, &options).unwrap();

        assert_eq!(count, 1);
        assert_eq!(result, "İX");
    }
}
