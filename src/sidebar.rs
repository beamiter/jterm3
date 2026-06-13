//! 文件树侧边栏数据模型（端口自 jterm2）。仅作为独立模块保留，未接入 UI，
//! 与 jterm2 现状一致（该模块在 jterm2 中同样未被引用）。
#![allow(dead_code)]
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// 文件树节点
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FileTreeNode {
    pub name: String,
    pub path: PathBuf,
    pub is_dir: bool,
    pub children: Vec<FileTreeNode>,
    pub expanded: bool,
}

/// 侧边栏状态
#[derive(Clone, Debug)]
pub struct Sidebar {
    pub visible: bool,
    pub width: f32,
    pub current_dir: PathBuf,
    pub root: Option<FileTreeNode>,
    pub selected_path: Option<PathBuf>,
}

impl Sidebar {
    pub fn new() -> Self {
        let current_dir = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"));

        Sidebar {
            visible: true,
            width: 200.0,
            current_dir: current_dir.clone(),
            root: Self::build_tree(&current_dir, 0),
            selected_path: None,
        }
    }

    pub fn set_current_dir(&mut self, path: PathBuf) {
        self.current_dir = path.clone();
        self.root = Self::build_tree(&path, 0);
    }

    /// 构建文件树（限制深度以提高性能）
    fn build_tree(dir: &Path, depth: usize) -> Option<FileTreeNode> {
        if depth > 3 {
            return None; // 限制深度为 3
        }

        let name = dir
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("/")
            .to_string();

        let mut children = Vec::new();

        if let Ok(entries) = std::fs::read_dir(dir) {
            let mut items: Vec<_> = entries
                .filter_map(|e| {
                    let entry = e.ok()?;
                    let path = entry.path();
                    let is_dir = entry.file_type().ok()?.is_dir();
                    let name = path.file_name()?.to_str()?.to_string();

                    // 跳过隐藏文件和系统文件夹
                    if name.starts_with('.') {
                        return None;
                    }

                    Some((name, path, is_dir))
                })
                .collect();

            // 排序：文件夹优先，然后按名称排序
            items.sort_by(|a, b| match (a.2, b.2) {
                (true, false) => std::cmp::Ordering::Less,
                (false, true) => std::cmp::Ordering::Greater,
                _ => a.0.cmp(&b.0),
            });

            // 仅保留前 20 个项目，避免过多
            for (name, path, is_dir) in items.iter().take(20) {
                let node = if *is_dir && depth < 2 {
                    // 递归构建子目录：取子目录自身的条目作为 children，
                    // 而不是把子目录节点整体再包一层。
                    FileTreeNode {
                        name: name.clone(),
                        path: path.clone(),
                        is_dir: true,
                        children: Self::build_tree(path, depth + 1)
                            .map(|n| n.children)
                            .unwrap_or_default(),
                        expanded: false,
                    }
                } else {
                    FileTreeNode {
                        name: name.clone(),
                        path: path.clone(),
                        is_dir: *is_dir,
                        children: vec![],
                        expanded: false,
                    }
                };
                children.push(node);
            }
        }

        Some(FileTreeNode {
            name,
            path: dir.to_path_buf(),
            is_dir: true,
            children,
            expanded: true,
        })
    }

    /// 切换节点展开状态
    pub fn toggle_node(&mut self, path: &Path) {
        if let Some(root) = &mut self.root {
            Self::toggle_recursive(root, path);
        }
    }

    fn toggle_recursive(node: &mut FileTreeNode, target: &Path) -> bool {
        if node.path == target {
            node.expanded = !node.expanded;
            return true;
        }

        for child in &mut node.children {
            if Self::toggle_recursive(child, target) {
                return true;
            }
        }

        false
    }

    /// 获取 Git 状态（简化版 - 调用 git status）
    pub fn get_git_status(path: &Path) -> Option<String> {
        use std::process::Command;

        let output = Command::new("git")
            .arg("status")
            .arg("--porcelain")
            .arg(path)
            .output()
            .ok()?;

        if output.status.success() {
            let stdout = String::from_utf8(output.stdout).ok()?;
            if !stdout.is_empty() {
                return Some("●".to_string()); // 表示有改动
            }
        }

        None
    }

    /// 刷新当前目录
    pub fn refresh(&mut self) {
        self.root = Self::build_tree(&self.current_dir, 0);
    }
}

impl Default for Sidebar {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sidebar_creation() {
        let sidebar = Sidebar::new();
        assert!(sidebar.visible);
        assert!(sidebar.root.is_some());
    }
}
