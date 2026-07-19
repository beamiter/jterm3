/// 链接检测和交互模块
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// OSC 8 超链接（从 ANSI 转义序列解析）
/// Will be integrated in Phase 3
#[allow(dead_code)]
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Hyperlink {
    pub url: String,
    pub text: String,
    pub id: Option<String>,
}

impl Hyperlink {
    #[allow(dead_code)]
    pub fn to_ansi_string(&self) -> String {
        let id = self.id.as_deref().unwrap_or("");
        format!(
            "\x1b]8;{};{}\x1b\\{}\x1b]8;;\x1b\\",
            id, self.url, self.text
        )
    }

    #[allow(dead_code)]
    pub fn from_ansi_string(s: &str) -> Option<Self> {
        // 简化解析：\x1b]8;id;url\x1b\text\x1b]8;;\x1b\
        if !s.contains("\x1b]8;") {
            return None;
        }

        // 提取 URL 和文本
        let parts: Vec<&str> = s.split("\x1b\\").collect();
        if parts.len() >= 2 {
            let url_part = parts[0];
            let text = parts[1];

            if let Some(url_start) = url_part.find(';') {
                let url = &url_part[url_start + 1..];
                return Some(Hyperlink {
                    url: url.to_string(),
                    text: text.to_string(),
                    id: None,
                });
            }
        }

        None
    }
}

/// 链接类型
#[derive(Clone, Debug, Copy, PartialEq, Eq)]
pub enum LinkType {
    /// URL (http/https/ftp)
    Url,
    /// 本地文件路径
    FilePath,
    /// IP 地址
    IpAddress,
}

/// 单个链接的信息
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Link {
    /// 链接所在行
    pub line: usize,
    /// 列起始位置
    pub col_start: usize,
    /// 列结束位置（不含）
    pub col_end: usize,
    /// 链接类型
    pub link_type: LinkType,
    /// 链接文本/URL
    pub text: String,
}

/// 链接检测配置
#[derive(Clone, Debug)]
pub struct LinkDetectionConfig {
    /// 是否检测 URL
    pub detect_urls: bool,
    /// 是否检测文件路径
    pub detect_file_paths: bool,
    /// 是否检测 IP 地址
    pub detect_ip_addresses: bool,
}

impl Default for LinkDetectionConfig {
    fn default() -> Self {
        Self {
            detect_urls: true,
            detect_file_paths: true,
            detect_ip_addresses: true,
        }
    }
}

/// 链接检测引擎
pub struct LinkDetector {
    config: LinkDetectionConfig,
    url_regex: Regex,
    ip_regex: Regex,
    file_path_regex: Regex,
}

impl LinkDetector {
    pub fn new(config: LinkDetectionConfig) -> Self {
        // URL 正则：http(s)?:// 或 ftp://
        // Parens are allowed in the body (e.g. Wikipedia `/wiki/Foo_(bar)`); an
        // unbalanced trailing `)` is trimmed in code below.
        let url_regex =
            Regex::new(r"(?:https?|ftp)://[^\s<>\[\]{}|\\^`]*[^\s<>\[\]{}|\\^`.,;:!?\-]").unwrap();

        // IP 地址正则：x.x.x.x 格式
        let ip_regex = Regex::new(
            r"\b(?:(?:25[0-5]|2[0-4][0-9]|[01]?[0-9][0-9]?)\.){3}(?:25[0-5]|2[0-4][0-9]|[01]?[0-9][0-9]?)\b"
        ).unwrap();

        // File paths: absolute, ~/..., ./..., ../..., and the relative paths
        // commonly printed by compilers (`src/main.rs:42:7`). A colon is
        // excluded from the first relative component so URL schemes are not
        // reclassified as file paths.
        let file_path_regex = Regex::new(
            r"(?:^|\s)(?:(?:/|\./|\.\./|~/)[^\s<>\[\]{}|\\^`()]*|[^/:\s<>\[\]{}|\\^`()]+(?:/[^/\s<>\[\]{}|\\^`()]+)+)"
        ).unwrap();

        Self {
            config,
            url_regex,
            ip_regex,
            file_path_regex,
        }
    }

    /// 将字节偏移转换为字符（列）偏移
    fn byte_offset_to_char_offset(line: &str, byte_offset: usize) -> usize {
        line[..byte_offset].chars().count()
    }

    /// 在单行文本中检测所有链接
    pub fn detect_links_in_line(&self, line: &str, line_idx: usize) -> Vec<Link> {
        let mut links = Vec::new();

        // 检测 URL
        if self.config.detect_urls {
            for mat in self.url_regex.find_iter(line) {
                let mut url = mat.as_str();
                // Drop trailing unbalanced `)` so a URL inside parentheses like
                // `(https://example.com)` doesn't swallow the closing paren,
                // while a balanced `/wiki/Foo_(bar)` is kept intact.
                while url.ends_with(')') && url.matches(')').count() > url.matches('(').count() {
                    url = &url[..url.len() - 1];
                }
                let col_start = Self::byte_offset_to_char_offset(line, mat.start());
                let col_end = Self::byte_offset_to_char_offset(line, mat.start() + url.len());
                links.push(Link {
                    line: line_idx,
                    col_start,
                    col_end,
                    link_type: LinkType::Url,
                    text: url.to_string(),
                });
            }
        }

        // 检测 IP 地址
        if self.config.detect_ip_addresses {
            for mat in self.ip_regex.find_iter(line) {
                let col_start = Self::byte_offset_to_char_offset(line, mat.start());
                let col_end = Self::byte_offset_to_char_offset(line, mat.end());
                // 避免与 URL 重复
                if !links
                    .iter()
                    .any(|l| l.col_start <= col_start && col_end <= l.col_end)
                {
                    links.push(Link {
                        line: line_idx,
                        col_start,
                        col_end,
                        link_type: LinkType::IpAddress,
                        text: mat.as_str().to_string(),
                    });
                }
            }
        }

        // 检测文件路径
        if self.config.detect_file_paths {
            for mat in self.file_path_regex.find_iter(line) {
                let raw = mat.as_str();
                let matched_text = raw.trim().trim_end_matches([',', ';', '!', '?']);
                if matched_text.is_empty() {
                    continue;
                }
                // The regex captures a leading `^|\s` boundary; skip it so the
                // highlight columns cover only the path, not the preceding space.
                let lead_ws = raw.len() - raw.trim_start().len();
                let start_byte = mat.start() + lead_ws;
                let col_start = Self::byte_offset_to_char_offset(line, start_byte);
                let col_end =
                    Self::byte_offset_to_char_offset(line, start_byte + matched_text.len());

                // 避免与 URL 重复
                if !links
                    .iter()
                    .any(|l| l.col_start <= col_start && col_end <= l.col_end)
                    && Self::is_valid_file_path(matched_text)
                {
                    links.push(Link {
                        line: line_idx,
                        col_start,
                        col_end,
                        link_type: LinkType::FilePath,
                        text: matched_text.to_string(),
                    });
                }
            }
        }

        links
    }

    /// 判断文本是否为有效的文件路径
    fn is_valid_file_path(text: &str) -> bool {
        let trimmed = text.trim();

        if trimmed.is_empty() || matches!(trimmed, "/" | "//" | "./" | "../" | "~/") {
            return false;
        }

        if trimmed.starts_with("//")
            || trimmed.contains("://")
            || trimmed.chars().all(|ch| matches!(ch, '/' | '.'))
        {
            return false;
        }

        // Must be rooted explicitly or contain at least one relative separator.
        trimmed.starts_with('/')
            || trimmed.starts_with("./")
            || trimmed.starts_with("../")
            || trimmed.starts_with("~/")
            || trimmed.contains('/')
    }

    /// 在整个网格中检测链接（带缓存）
    #[allow(dead_code)]
    pub fn detect_all_links(&self, grid: &crate::terminal::TerminalGrid) -> Vec<Link> {
        let mut all_links = Vec::new();

        for (line_idx, line) in grid.iter().enumerate() {
            let line_str: String = line.iter().map(|cell| cell.character).collect();
            let links = self.detect_links_in_line(&line_str, line_idx);
            all_links.extend(links);
        }

        all_links
    }

    /// 在当前可视内容中检测链接，处理跨行换行的URL。
    #[allow(dead_code)]
    pub fn detect_links_in_visible_cells(
        &self,
        visible_cells: &[Vec<crate::terminal::TerminalCell>],
    ) -> Vec<Link> {
        self.detect_links_in_visible_cells_with_wrapping(visible_cells, &[])
    }

    /// 在当前可视内容中检测链接，支持传入row_wrapped标志以正确处理跨行链接。
    pub fn detect_links_in_visible_cells_with_wrapping(
        &self,
        visible_cells: &[Vec<crate::terminal::TerminalCell>],
        row_wrapped: &[bool],
    ) -> Vec<Link> {
        let mut all_links = Vec::new();

        if row_wrapped.is_empty() || row_wrapped.len() != visible_cells.len() {
            for (line_idx, line) in visible_cells.iter().enumerate() {
                let line_str: String = line.iter().map(|cell| cell.character).collect();
                let links = self.detect_links_in_line(&line_str, line_idx);
                all_links.extend(links);
            }
            return all_links;
        }

        // 将连续的换行行合并为逻辑行，记录每行的列数累积偏移
        let mut logical_lines: Vec<(usize, usize, String, Vec<usize>)> = Vec::new();
        let mut current_start = 0;
        let mut current_text = String::new();
        let mut row_char_offsets: Vec<usize> = Vec::new(); // 每个物理行在逻辑行中的起始字符偏移

        for (line_idx, line) in visible_cells.iter().enumerate() {
            row_char_offsets.push(current_text.chars().count());
            let line_str: String = line.iter().map(|cell| cell.character).collect();
            current_text.push_str(&line_str);

            if line_idx == visible_cells.len() - 1 || !row_wrapped[line_idx] {
                logical_lines.push((
                    current_start,
                    line_idx,
                    current_text.clone(),
                    row_char_offsets.clone(),
                ));
                current_text.clear();
                row_char_offsets.clear();
                current_start = line_idx + 1;
            }
        }

        for (start_row, _end_row, logical_text, char_offsets) in logical_lines {
            let links = self.detect_links_in_line(&logical_text, 0);

            for link in links {
                let full_url = link.text.clone();
                let link_start = link.col_start;
                let link_end = link.col_end;

                // 将逻辑偏移分割到多个物理行
                for (i, &row_offset) in char_offsets.iter().enumerate() {
                    let row_idx = start_row + i;
                    let row_len = visible_cells[row_idx].len();
                    let row_end_offset = row_offset + row_len;

                    // 检查该链接是否与这个物理行重叠
                    if link_start < row_end_offset && link_end > row_offset {
                        let col_start = link_start.saturating_sub(row_offset);
                        let col_end = if link_end < row_end_offset {
                            link_end - row_offset
                        } else {
                            row_len
                        };

                        all_links.push(Link {
                            line: row_idx,
                            col_start,
                            col_end,
                            link_type: link.link_type,
                            text: full_url.clone(),
                        });
                    }
                }
            }
        }

        all_links
    }
}

/// 打开链接
pub fn open_link(link: &Link, cwd: Option<&Path>) -> Result<(), Box<dyn std::error::Error>> {
    match link.link_type {
        LinkType::Url => {
            open_url(&link.text)?;
        }
        LinkType::FilePath => {
            open_file_path(&link.text, cwd)?;
        }
        LinkType::IpAddress => {
            // IP 地址可以用浏览器打开或显示 whois 信息
            open_url(&format!("http://{}", &link.text))?;
        }
    }
    Ok(())
}

/// 打开 URL（使用系统默认浏览器）
#[cfg(target_os = "linux")]
fn open_url(url: &str) -> Result<(), Box<dyn std::error::Error>> {
    std::process::Command::new("xdg-open").arg(url).spawn()?;
    Ok(())
}

#[cfg(target_os = "macos")]
fn open_url(url: &str) -> Result<(), Box<dyn std::error::Error>> {
    std::process::Command::new("open").arg(url).spawn()?;
    Ok(())
}

#[cfg(target_os = "windows")]
fn open_url(url: &str) -> Result<(), Box<dyn std::error::Error>> {
    std::process::Command::new("cmd")
        .args(&["/C", "start", url])
        .spawn()?;
    Ok(())
}

/// 打开文件路径（使用系统默认应用）
#[cfg(target_os = "linux")]
fn open_file_path(path: &str, cwd: Option<&Path>) -> Result<(), Box<dyn std::error::Error>> {
    let expanded_path = resolve_existing_file_path(path, cwd)?;

    std::process::Command::new("xdg-open")
        .arg(&expanded_path)
        .spawn()?;
    Ok(())
}

#[cfg(target_os = "macos")]
fn open_file_path(path: &str, cwd: Option<&Path>) -> Result<(), Box<dyn std::error::Error>> {
    let expanded_path = resolve_existing_file_path(path, cwd)?;
    std::process::Command::new("open")
        .arg(&expanded_path)
        .spawn()?;
    Ok(())
}

#[cfg(target_os = "windows")]
fn open_file_path(path: &str, cwd: Option<&Path>) -> Result<(), Box<dyn std::error::Error>> {
    let expanded_path = resolve_existing_file_path(path, cwd)?;
    std::process::Command::new("explorer")
        .arg(&expanded_path)
        .spawn()?;
    Ok(())
}

/// Expand a leading `~/` without invoking a shell.
fn expand_path(path: &str) -> PathBuf {
    if let Some(relative) = path.strip_prefix("~/") {
        if let Some(home) = dirs::home_dir() {
            return home.join(relative);
        }
    }
    PathBuf::from(path)
}

/// Drop a conventional compiler/editor source location (`:line` or
/// `:line:column`) while leaving arbitrary colons alone.
fn strip_source_location(path: &str) -> Option<&str> {
    let (without_last, last) = path.rsplit_once(':')?;
    if last.is_empty() || !last.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    if let Some((without_line, line)) = without_last.rsplit_once(':') {
        if !line.is_empty() && line.bytes().all(|byte| byte.is_ascii_digit()) {
            return Some(without_line);
        }
    }
    Some(without_last)
}

fn absolutize_file_path(path: PathBuf, cwd: Option<&Path>) -> PathBuf {
    if path.is_absolute() {
        path
    } else if let Some(cwd) = cwd {
        cwd.join(path)
    } else {
        path
    }
}

/// Resolve a link exactly as displayed first. If that path does not exist,
/// retry after removing a trailing source location. Relative paths are scoped
/// to the focused terminal's cwd instead of jterm3's launcher directory.
fn resolve_existing_file_path(
    path: &str,
    cwd: Option<&Path>,
) -> Result<PathBuf, Box<dyn std::error::Error>> {
    let exact = absolutize_file_path(expand_path(path), cwd);
    if exact.exists() {
        return Ok(exact);
    }
    if let Some(source_path) = strip_source_location(path) {
        let source = absolutize_file_path(expand_path(source_path), cwd);
        if source.exists() {
            return Ok(source);
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::NotFound,
        format!("file link does not exist: {}", exact.display()),
    )
    .into())
}

/// 复制链接到剪贴板
#[allow(dead_code)]
pub fn copy_to_clipboard(text: &str) -> Result<(), Box<dyn std::error::Error>> {
    #[cfg(target_os = "linux")]
    {
        use std::io::Write;
        let mut child = std::process::Command::new("xclip")
            .args(["-selection", "clipboard"])
            .stdin(std::process::Stdio::piped())
            .spawn()?;

        if let Some(mut stdin) = child.stdin.take() {
            stdin.write_all(text.as_bytes())?;
        }
        child.wait()?;
        Ok(())
    }

    #[cfg(target_os = "macos")]
    {
        use std::io::Write;
        let mut child = std::process::Command::new("pbcopy")
            .stdin(std::process::Stdio::piped())
            .spawn()?;

        if let Some(mut stdin) = child.stdin.take() {
            stdin.write_all(text.as_bytes())?;
        }
        child.wait()?;
        Ok(())
    }

    #[cfg(target_os = "windows")]
    {
        use std::io::Write;
        let mut child = std::process::Command::new("clip")
            .stdin(std::process::Stdio::piped())
            .spawn()?;

        if let Some(mut stdin) = child.stdin.take() {
            stdin.write_all(text.as_bytes())?;
        }
        child.wait()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_url_detection() {
        let detector = LinkDetector::new(LinkDetectionConfig::default());
        let line = "Visit https://example.com for more info";
        let links = detector.detect_links_in_line(line, 0);

        assert_eq!(links.len(), 1);
        assert_eq!(links[0].link_type, LinkType::Url);
        assert_eq!(links[0].text, "https://example.com");
    }

    #[test]
    fn test_ip_detection() {
        let detector = LinkDetector::new(LinkDetectionConfig::default());
        let line = "Server at 192.168.1.1 is down";
        let links = detector.detect_links_in_line(line, 0);

        assert!(links.iter().any(|l| l.link_type == LinkType::IpAddress));
    }

    #[test]
    fn test_file_path_detection() {
        let detector = LinkDetector::new(LinkDetectionConfig::default());
        let line = "Check /etc/hosts file";
        let links = detector.detect_links_in_line(line, 0);

        assert!(links.iter().any(|l| l.link_type == LinkType::FilePath));
    }

    #[test]
    fn detects_relative_and_home_file_paths_without_reclassifying_urls() {
        let detector = LinkDetector::new(LinkDetectionConfig::default());
        let line = "at src/main.rs:42:7 and ~/notes/todo.md; see https://example.com/a/b";
        let links = detector.detect_links_in_line(line, 0);
        let files: Vec<&str> = links
            .iter()
            .filter(|link| link.link_type == LinkType::FilePath)
            .map(|link| link.text.as_str())
            .collect();

        assert_eq!(files, ["src/main.rs:42:7", "~/notes/todo.md"]);
        assert_eq!(
            links
                .iter()
                .filter(|link| link.link_type == LinkType::Url)
                .count(),
            1
        );
    }

    #[test]
    fn source_locations_resolve_against_the_terminal_cwd() {
        let unique = format!(
            "jterm3-link-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock should be after the Unix epoch")
                .as_nanos()
        );
        let root = std::env::temp_dir().join(unique);
        let source_dir = root.join("src");
        std::fs::create_dir_all(&source_dir).expect("create test source directory");
        let source = source_dir.join("main.rs");
        std::fs::write(&source, "fn main() {}\n").expect("write test source");

        let resolved = resolve_existing_file_path("src/main.rs:42:7", Some(&root))
            .expect("source location should resolve");
        assert_eq!(resolved, source);
        assert_eq!(strip_source_location("notes:today"), None);
        assert_eq!(strip_source_location("src/main.rs:42"), Some("src/main.rs"));

        std::fs::remove_dir_all(root).expect("remove test directory");
    }

    #[test]
    fn test_comment_slashes_are_not_file_paths() {
        let detector = LinkDetector::new(LinkDetectionConfig::default());
        let line = "// Selection rule:";
        let links = detector.detect_links_in_line(line, 0);

        assert!(!links.iter().any(|l| l.link_type == LinkType::FilePath));
    }

    #[test]
    fn test_link_detection_config() {
        let config = LinkDetectionConfig {
            detect_urls: false,
            ..LinkDetectionConfig::default()
        };

        let detector = LinkDetector::new(config);
        let line = "Visit https://example.com for more info";
        let links = detector.detect_links_in_line(line, 0);

        assert!(!links.iter().any(|l| l.link_type == LinkType::Url));
    }
}
