//! 会话持久化：记录每个标签页的工作目录与活动索引，在重启后恢复。
//! 端口自 jterm2 `session_persistence.rs`，精简为 jterm3 实际需要的字段。
use serde::{Deserialize, Serialize};
use std::path::Path;

/// 单个会话快照（jterm3 仅需要 cwd 来重新 spawn）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionSnapshot {
    #[serde(default)]
    pub cwd: Option<String>,
}

/// 分屏布局快照:重启后恢复分屏方向、各 pane 占比与对应的会话。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SplitSnapshot {
    /// "vertical"(左右)或 "horizontal"(上下)。
    pub mode: String,
    /// 每个 pane 的占比(与 `panes` 一一对应,总和约为 1)。
    /// 缺失或长度不符时恢复端回退为均分。
    #[serde(default)]
    pub ratios: Vec<f32>,
    /// 各 pane 对应的会话索引(指向 `sessions`)。
    pub panes: Vec<usize>,
    /// 拥有键盘焦点的 pane(索引进 `panes`)。
    pub focused: usize,
}

/// tmux 风格的递归分屏布局快照。`Leaf` 显示一个会话;`Split` 沿某轴划分若干
/// 子节点。旧的扁平 `SplitSnapshot` 仍可读取(见 `split` 字段),但新布局写入
/// 此字段以支持任意嵌套。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum PaneTreeSnapshot {
    /// 叶子:指向 `sessions` 的会话索引。
    Leaf { session: usize },
    /// 分裂:`axis` 为 "vertical"(左右)或 "horizontal"(上下)。
    Split {
        axis: String,
        #[serde(default)]
        ratios: Vec<f32>,
        children: Vec<PaneTreeSnapshot>,
    },
}

/// 会话列表快照。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionsSnapshot {
    pub version: u32,
    pub sessions: Vec<SessionSnapshot>,
    #[serde(default)]
    pub active_index: Option<usize>,
    /// 旧的扁平分屏布局(单轴)。仅用于读取旧快照,新快照不再写入。
    #[serde(default)]
    pub split: Option<SplitSnapshot>,
    /// 递归分屏布局树;`None` 表示单 pane。旧快照缺省为 `None`,向后兼容。
    #[serde(default)]
    pub tree: Option<PaneTreeSnapshot>,
}

impl SessionsSnapshot {
    pub fn new(
        sessions: Vec<SessionSnapshot>,
        active_index: Option<usize>,
        tree: Option<PaneTreeSnapshot>,
    ) -> Self {
        SessionsSnapshot {
            version: 1,
            sessions,
            active_index,
            split: None,
            tree,
        }
    }

    /// 序列化为 JSON 字符串（也用于变更去重）。
    pub fn to_json(&self) -> Option<String> {
        serde_json::to_string_pretty(self).ok()
    }

    /// 原子写入到文件（先写 .tmp 再 rename）。
    pub fn save(&self, path: &Path) -> Result<(), Box<dyn std::error::Error>> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let json = serde_json::to_string_pretty(self)?;
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, &json)?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    }

    /// 从文件加载；文件不存在时返回空快照。
    pub fn load(path: &Path) -> Result<Self, Box<dyn std::error::Error>> {
        if !path.exists() {
            return Ok(SessionsSnapshot::new(Vec::new(), None, None));
        }
        let content = std::fs::read_to_string(path)?;
        Ok(serde_json::from_str(&content)?)
    }
}

/// 尝试获取单实例锁。成功返回持锁的 `File`（需在进程生命周期内持有），
/// 失败（已有实例运行）返回 `None`。端口自 jterm2 `try_acquire_instance_lock`。
pub fn try_acquire_instance_lock() -> Option<std::fs::File> {
    let lock_path = dirs::config_dir()?.join("jterm3").join("instance.lock");
    if let Some(parent) = lock_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }

    use std::os::unix::fs::OpenOptionsExt;
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        // PTY children are fork/exec'd from this process. Without CLOEXEC they
        // inherit the flock and can make jterm3 look permanently running after
        // the UI exits.
        .custom_flags(libc::O_CLOEXEC)
        .open(&lock_path)
        .ok()?;

    use std::os::unix::io::AsRawFd;
    let fd = file.as_raw_fd();
    // LOCK_EX | LOCK_NB: 非阻塞排他锁。fd 来自有效的 File，生命周期覆盖本次调用。
    let ret = unsafe { libc::flock(fd, libc::LOCK_EX | libc::LOCK_NB) };
    if ret == 0 {
        use std::io::{Seek, Write};
        // Truncate only after owning the lock. Opening with truncate(true)
        // allowed a second instance to erase the first instance's PID even
        // though its flock attempt subsequently failed.
        let _ = file.set_len(0);
        let mut f = &file;
        let _ = f.seek(std::io::SeekFrom::Start(0));
        let _ = write!(f, "{}", std::process::id());
        Some(file)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshots_without_split_field_still_deserialize() {
        let legacy = r#"{"version":1,"sessions":[{"cwd":"/tmp"}],"active_index":0}"#;
        let snap: SessionsSnapshot = serde_json::from_str(legacy).unwrap();
        assert!(snap.split.is_none());
        assert_eq!(snap.sessions.len(), 1);
    }

    #[test]
    fn tree_layout_round_trips_through_json() {
        // V[ 0, H[2, 1] ] — a genuinely nested tmux-style layout.
        let snap = SessionsSnapshot::new(
            vec![
                SessionSnapshot { cwd: None },
                SessionSnapshot { cwd: None },
                SessionSnapshot { cwd: None },
            ],
            Some(1),
            Some(PaneTreeSnapshot::Split {
                axis: "vertical".to_string(),
                ratios: vec![0.6, 0.4],
                children: vec![
                    PaneTreeSnapshot::Leaf { session: 0 },
                    PaneTreeSnapshot::Split {
                        axis: "horizontal".to_string(),
                        ratios: vec![0.5, 0.5],
                        children: vec![
                            PaneTreeSnapshot::Leaf { session: 2 },
                            PaneTreeSnapshot::Leaf { session: 1 },
                        ],
                    },
                ],
            }),
        );
        let json = snap.to_json().unwrap();
        let back: SessionsSnapshot = serde_json::from_str(&json).unwrap();
        let PaneTreeSnapshot::Split { axis, children, .. } = back.tree.unwrap() else {
            panic!("expected a split at the root");
        };
        assert_eq!(axis, "vertical");
        assert_eq!(children.len(), 2);
        assert!(matches!(children[0], PaneTreeSnapshot::Leaf { session: 0 }));
        assert!(matches!(children[1], PaneTreeSnapshot::Split { .. }));
    }

    #[test]
    fn legacy_flat_split_field_still_deserializes() {
        // Old jterm3 snapshots stored a single-axis `split` and no `tree`. Both
        // fields must round-trip so the restore path can fall back to `split`.
        let legacy = r#"{"version":1,"sessions":[{"cwd":null},{"cwd":null}],
            "active_index":0,
            "split":{"mode":"vertical","ratios":[0.35,0.65],"panes":[0,1],"focused":0}}"#;
        let snap: SessionsSnapshot = serde_json::from_str(legacy).unwrap();
        assert!(snap.tree.is_none());
        let split = snap.split.unwrap();
        assert_eq!(split.panes, vec![0, 1]);
        assert_eq!(split.mode, "vertical");
    }
}
