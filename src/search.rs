/// 搜索功能模块
use crate::terminal::TerminalCell;
use regex::RegexBuilder;
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;

/// 单个搜索匹配项
#[derive(Clone, Debug, Copy, PartialEq, Eq)]
pub struct SearchMatch {
    /// 所在行（可见网格行索引）
    pub line: usize,
    /// 列起始位置
    pub col_start: usize,
    /// 列结束位置（不含）
    pub col_end: usize,
}

/// 搜索功能的完整状态
#[derive(Clone, Debug)]
pub struct SearchState {
    /// 搜索面板是否打开
    pub is_open: bool,

    /// 搜索输入框中的文本
    pub query: String,

    /// 是否使用正则表达式模式
    pub use_regex: bool,

    /// 是否大小写敏感
    pub case_sensitive: bool,

    /// 所有匹配项的列表
    pub matches: Vec<SearchMatch>,

    /// 当前选中的匹配项索引
    pub current_match_index: usize,

    /// 搜索历史队列（最近在前）
    pub history: VecDeque<SearchHistoryEntry>,

    /// 历史导航位置（None 表示在输入框，Some(i) 表示在历史第 i 项）
    pub history_nav_index: Option<usize>,

    /// 上次搜索词（用于检测搜索词变化）
    last_query: String,

    /// 搜索错误消息（正则表达式编译错误等）
    pub error_message: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SearchHistoryEntry {
    pub query: String,
    pub is_regex: bool,
    pub case_sensitive: bool,
    pub timestamp: String,
}

impl Default for SearchState {
    fn default() -> Self {
        Self::new()
    }
}

impl SearchState {
    /// 创建新的搜索状态
    pub fn new() -> Self {
        Self {
            is_open: false,
            query: String::new(),
            use_regex: false,
            case_sensitive: false,
            matches: Vec::new(),
            current_match_index: 0,
            history: VecDeque::new(),
            history_nav_index: None,
            last_query: String::new(),
            error_message: None,
        }
    }

    /// 打开或关闭搜索面板
    pub fn toggle(&mut self) {
        self.is_open = !self.is_open;
        if !self.is_open {
            self.close();
        }
    }

    /// 关闭搜索面板
    pub fn close(&mut self) {
        self.is_open = false;
        if !self.query.is_empty() && self.last_query != self.query {
            self.save_to_history();
            self.last_query = self.query.clone();
        }
    }

    /// 获取当前匹配项（如果有）
    pub fn current_match(&self) -> Option<SearchMatch> {
        if self.matches.is_empty() {
            None
        } else {
            Some(self.matches[self.current_match_index % self.matches.len()])
        }
    }

    /// 移动到下一个匹配项
    pub fn next_match(&mut self) {
        if !self.matches.is_empty() {
            self.current_match_index = (self.current_match_index + 1) % self.matches.len();
        }
    }

    /// 移动到上一个匹配项
    pub fn prev_match(&mut self) {
        if !self.matches.is_empty() {
            self.current_match_index = if self.current_match_index == 0 {
                self.matches.len() - 1
            } else {
                self.current_match_index - 1
            };
        }
    }

    /// 切换大小写敏感
    pub fn toggle_case_sensitive(&mut self) {
        self.case_sensitive = !self.case_sensitive;
        self.current_match_index = 0;
    }

    /// 切换正则表达式模式
    pub fn toggle_regex(&mut self) {
        self.use_regex = !self.use_regex;
        self.current_match_index = 0;
        self.error_message = None;
    }

    /// 保存当前搜索词到历史
    fn save_to_history(&mut self) {
        if self.query.is_empty() {
            return;
        }

        // 检查重复
        if !self.history.is_empty() && self.history[0].query == self.query {
            return;
        }

        self.history.push_front(SearchHistoryEntry {
            query: self.query.clone(),
            is_regex: self.use_regex,
            case_sensitive: self.case_sensitive,
            timestamp: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| format!("{}", d.as_secs()))
                .unwrap_or_else(|_| "unknown".to_string()),
        });

        // 限制历史大小
        while self.history.len() > 50 {
            self.history.pop_back();
        }
    }

    /// 从历史中加载前一条
    pub fn history_prev(&mut self) {
        if self.history.is_empty() {
            return;
        }

        if let Some(idx) = self.history_nav_index {
            if idx + 1 < self.history.len() {
                self.history_nav_index = Some(idx + 1);
                let entry = &self.history[idx + 1];
                self.query = entry.query.clone();
                self.use_regex = entry.is_regex;
                self.case_sensitive = entry.case_sensitive;
            }
        } else {
            self.history_nav_index = Some(0);
            let entry = &self.history[0];
            self.query = entry.query.clone();
            self.use_regex = entry.is_regex;
            self.case_sensitive = entry.case_sensitive;
        }
    }

    /// 从历史中加载后一条
    pub fn history_next(&mut self) {
        if let Some(idx) = self.history_nav_index {
            if idx > 0 {
                self.history_nav_index = Some(idx - 1);
                let entry = &self.history[idx - 1];
                self.query = entry.query.clone();
                self.use_regex = entry.is_regex;
                self.case_sensitive = entry.case_sensitive;
            } else {
                // 返回输入框
                self.history_nav_index = None;
                self.query.clear();
            }
        }
    }
}

/// 搜索引擎（用于在可见网格中进行搜索）
pub struct SearchEngine;

impl SearchEngine {
    /// 在网格中搜索文本
    pub fn search(
        grid: &[Vec<TerminalCell>],
        query: &str,
        use_regex: bool,
        case_sensitive: bool,
    ) -> (Vec<SearchMatch>, Option<String>) {
        if query.is_empty() {
            return (Vec::new(), None);
        }

        if use_regex {
            Self::search_regex(grid, query, case_sensitive)
        } else {
            (Self::search_plaintext(grid, query, case_sensitive), None)
        }
    }

    /// 普通文本搜索
    fn search_plaintext(
        grid: &[Vec<TerminalCell>],
        query: &str,
        case_sensitive: bool,
    ) -> Vec<SearchMatch> {
        let mut matches = Vec::new();

        let search_query = if case_sensitive {
            query.to_string()
        } else {
            query.to_lowercase()
        };

        let query_chars = search_query.chars().count();
        for (line_idx, line) in grid.iter().enumerate() {
            let line_str = Self::grid_line_to_string(line);
            let search_line = if case_sensitive {
                line_str.clone()
            } else {
                line_str.to_lowercase()
            };

            let mut start_byte = 0;
            while let Some(rel) = search_line[start_byte..].find(&search_query) {
                let match_byte = start_byte + rel;
                // Grid columns are char indices, not byte offsets.
                let col_start = search_line[..match_byte].chars().count();
                matches.push(SearchMatch {
                    line: line_idx,
                    col_start,
                    col_end: col_start + query_chars,
                });
                // Advance one char past the match start, staying on a UTF-8
                // boundary (a raw +1 would panic inside a multi-byte char).
                let step = search_line[match_byte..]
                    .chars()
                    .next()
                    .map(|c| c.len_utf8())
                    .unwrap_or(1);
                start_byte = match_byte + step;
            }
        }

        matches
    }

    /// 正则表达式搜索
    fn search_regex(
        grid: &[Vec<TerminalCell>],
        pattern: &str,
        case_sensitive: bool,
    ) -> (Vec<SearchMatch>, Option<String>) {
        let mut matches = Vec::new();

        // 编译正则表达式
        let mut builder = RegexBuilder::new(pattern);
        if !case_sensitive {
            builder.case_insensitive(true);
        }

        let regex = match builder.build() {
            Ok(r) => r,
            Err(e) => {
                return (Vec::new(), Some(format!("Invalid regex: {}", e)));
            }
        };

        for (line_idx, line) in grid.iter().enumerate() {
            let line_str = Self::grid_line_to_string(line);

            for mat in regex.find_iter(&line_str) {
                // Map byte offsets to char/column indices.
                let col_start = line_str[..mat.start()].chars().count();
                let col_end = line_str[..mat.end()].chars().count();
                matches.push(SearchMatch {
                    line: line_idx,
                    col_start,
                    col_end,
                });
            }
        }

        (matches, None)
    }

    /// 将网格行转换为字符串
    fn grid_line_to_string(line: &[TerminalCell]) -> String {
        line.iter().map(|cell| cell.character).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_search_state_toggle() {
        let mut state = SearchState::new();
        assert!(!state.is_open);
        state.toggle();
        assert!(state.is_open);
        state.toggle();
        assert!(!state.is_open);
    }

    #[test]
    fn test_match_navigation() {
        let mut state = SearchState::new();
        state.matches = vec![
            SearchMatch {
                line: 0,
                col_start: 0,
                col_end: 5,
            },
            SearchMatch {
                line: 1,
                col_start: 10,
                col_end: 15,
            },
        ];

        assert_eq!(state.current_match_index, 0);
        state.next_match();
        assert_eq!(state.current_match_index, 1);
        state.next_match();
        assert_eq!(state.current_match_index, 0); // 循环

        state.prev_match();
        assert_eq!(state.current_match_index, 1);
    }

    #[test]
    fn test_case_sensitive_toggle() {
        let mut state = SearchState::new();
        assert!(!state.case_sensitive);
        state.toggle_case_sensitive();
        assert!(state.case_sensitive);
    }

    #[test]
    fn test_regex_toggle() {
        let mut state = SearchState::new();
        assert!(!state.use_regex);
        state.toggle_regex();
        assert!(state.use_regex);
    }
}
