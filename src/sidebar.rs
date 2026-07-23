//! Asynchronous, lazily-loaded file-tree sidebar.
//!
//! The UI owns [`Sidebar`] and sends [`DirectoryRequest`] values to a worker
//! task. Only one directory level is read per request, so opening the sidebar or
//! expanding a node never recursively walks the filesystem on the UI thread.

use std::path::{Path, PathBuf};

/// Loading lifecycle for a directory node.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DirectoryState {
    Unloaded,
    Loading,
    Loaded,
    Error(String),
}

/// One visible file-tree node.
#[derive(Clone, Debug)]
pub struct FileTreeNode {
    pub name: String,
    pub path: PathBuf,
    pub is_dir: bool,
    pub children: Vec<FileTreeNode>,
    pub expanded: bool,
    pub state: DirectoryState,
}

impl FileTreeNode {
    fn directory(path: PathBuf, expanded: bool) -> Self {
        let name = display_name(&path);
        Self {
            name,
            path,
            is_dir: true,
            children: Vec::new(),
            expanded,
            state: DirectoryState::Unloaded,
        }
    }

    fn entry(name: String, path: PathBuf, is_dir: bool) -> Self {
        Self {
            name,
            path,
            is_dir,
            children: Vec::new(),
            expanded: false,
            state: if is_dir {
                DirectoryState::Unloaded
            } else {
                DirectoryState::Loaded
            },
        }
    }
}

/// A filesystem request created by [`Sidebar`]. `generation` prevents a slow
/// response for an old cwd from replacing the tree after the user navigates.
#[derive(Clone, Debug)]
pub struct DirectoryRequest {
    pub generation: u64,
    pub path: PathBuf,
}

/// Worker result consumed by [`Sidebar::apply_load`].
#[derive(Clone, Debug)]
pub struct DirectoryResult {
    pub generation: u64,
    pub path: PathBuf,
    pub entries: Result<Vec<FileTreeNode>, String>,
}

/// File-sidebar state.
#[derive(Clone, Debug)]
pub struct Sidebar {
    pub current_dir: PathBuf,
    pub root: FileTreeNode,
    generation: u64,
}

impl Sidebar {
    pub fn new() -> Self {
        let current_dir = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"));
        Self {
            root: FileTreeNode::directory(current_dir.clone(), true),
            current_dir,
            generation: 0,
        }
    }

    /// Point the tree at a new root and return the one-level load request.
    pub fn set_current_dir(&mut self, path: PathBuf) -> DirectoryRequest {
        self.generation = self.generation.wrapping_add(1);
        self.current_dir = path.clone();
        self.root = FileTreeNode::directory(path, true);
        self.begin_load_root()
    }

    /// Load the initial root without changing its generation.
    pub fn begin_load_root(&mut self) -> DirectoryRequest {
        self.root.state = DirectoryState::Loading;
        DirectoryRequest {
            generation: self.generation,
            path: self.root.path.clone(),
        }
    }

    /// Toggle a directory and, when necessary, request its first one-level load.
    pub fn toggle_node(&mut self, path: &Path) -> Option<DirectoryRequest> {
        let generation = self.generation;
        let node = find_node_mut(&mut self.root, path)?;
        if !node.is_dir {
            return None;
        }

        match node.state {
            DirectoryState::Unloaded | DirectoryState::Error(_) => {
                node.expanded = true;
                node.state = DirectoryState::Loading;
                Some(DirectoryRequest {
                    generation,
                    path: node.path.clone(),
                })
            }
            DirectoryState::Loading => {
                node.expanded = !node.expanded;
                None
            }
            DirectoryState::Loaded => {
                node.expanded = !node.expanded;
                None
            }
        }
    }

    /// Invalidate outstanding responses and reload the current root.
    pub fn refresh(&mut self) -> DirectoryRequest {
        self.set_current_dir(self.current_dir.clone())
    }

    /// Apply a worker response. Returns `false` for stale or unknown responses.
    pub fn apply_load(&mut self, result: DirectoryResult) -> bool {
        if result.generation != self.generation {
            return false;
        }
        let Some(node) = find_node_mut(&mut self.root, &result.path) else {
            return false;
        };
        match result.entries {
            Ok(entries) => {
                node.children = entries;
                node.state = DirectoryState::Loaded;
            }
            Err(error) => {
                node.children.clear();
                node.state = DirectoryState::Error(error);
            }
        }
        true
    }
}

impl Default for Sidebar {
    fn default() -> Self {
        Self::new()
    }
}

/// Read exactly one directory level. This function is intentionally synchronous;
/// callers run it inside an iced worker task instead of the UI update loop.
pub fn load_directory(request: DirectoryRequest) -> DirectoryResult {
    let entries = read_directory(&request.path);
    DirectoryResult {
        generation: request.generation,
        path: request.path,
        entries,
    }
}

fn read_directory(path: &Path) -> Result<Vec<FileTreeNode>, String> {
    let entries = std::fs::read_dir(path)
        .map_err(|error| format!("Cannot read {}: {error}", path.display()))?;
    let mut nodes = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|error| format!("Cannot read {}: {error}", path.display()))?;
        let name = entry.file_name().to_string_lossy().into_owned();
        // Match the existing sidebar behavior. Hidden files remain available by
        // typing paths in the terminal without overwhelming the visual tree.
        if name.starts_with('.') {
            continue;
        }
        let file_type = entry
            .file_type()
            .map_err(|error| format!("Cannot inspect {}: {error}", entry.path().display()))?;
        nodes.push(FileTreeNode::entry(name, entry.path(), file_type.is_dir()));
    }
    nodes.sort_by(|a, b| {
        b.is_dir
            .cmp(&a.is_dir)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
            .then_with(|| a.name.cmp(&b.name))
    });
    Ok(nodes)
}

fn display_name(path: &Path) -> String {
    path.file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| path.display().to_string())
}

fn find_node_mut<'a>(node: &'a mut FileTreeNode, path: &Path) -> Option<&'a mut FileTreeNode> {
    if node.path == path {
        return Some(node);
    }
    node.children
        .iter_mut()
        .find_map(|child| find_node_mut(child, path))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_tree() -> PathBuf {
        let root = std::env::temp_dir().join(format!("jterm3-sidebar-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(root.join("nested").join("deep")).expect("create test tree");
        for index in 0..32 {
            std::fs::write(root.join(format!("file-{index:02}.txt")), b"x")
                .expect("write test file");
        }
        root
    }

    #[test]
    fn loads_all_entries_and_expands_lazily() {
        let root = temp_tree();
        let mut sidebar = Sidebar::new();
        let request = sidebar.set_current_dir(root.clone());
        assert_eq!(sidebar.root.state, DirectoryState::Loading);
        assert!(sidebar.apply_load(load_directory(request)));
        assert_eq!(sidebar.root.state, DirectoryState::Loaded);
        assert_eq!(sidebar.root.children.len(), 33);

        let nested = root.join("nested");
        let request = sidebar
            .toggle_node(&nested)
            .expect("unloaded directory should request a load");
        assert!(sidebar.apply_load(load_directory(request)));
        let nested_node = find_node_mut(&mut sidebar.root, &nested).expect("nested node");
        assert_eq!(nested_node.state, DirectoryState::Loaded);
        assert_eq!(nested_node.children.len(), 1);

        std::fs::remove_dir_all(root).expect("remove test tree");
    }

    #[test]
    fn stale_response_cannot_replace_new_root() {
        let first = temp_tree();
        let second = temp_tree();
        let mut sidebar = Sidebar::new();
        let stale = sidebar.set_current_dir(first.clone());
        let current = sidebar.set_current_dir(second.clone());

        assert!(!sidebar.apply_load(load_directory(stale)));
        assert!(sidebar.apply_load(load_directory(current)));
        assert_eq!(sidebar.root.path, second);

        std::fs::remove_dir_all(first).expect("remove first tree");
        std::fs::remove_dir_all(second).expect("remove second tree");
    }
}
