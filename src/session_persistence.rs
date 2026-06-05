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

/// 会话列表快照。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionsSnapshot {
    pub version: u32,
    pub sessions: Vec<SessionSnapshot>,
    #[serde(default)]
    pub active_index: Option<usize>,
}

impl SessionsSnapshot {
    pub fn new(sessions: Vec<SessionSnapshot>, active_index: Option<usize>) -> Self {
        SessionsSnapshot {
            version: 1,
            sessions,
            active_index,
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
            return Ok(SessionsSnapshot::new(Vec::new(), None));
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

    let file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(&lock_path)
        .ok()?;

    use std::os::unix::io::AsRawFd;
    let fd = file.as_raw_fd();
    // LOCK_EX | LOCK_NB: 非阻塞排他锁。fd 来自有效的 File，生命周期覆盖本次调用。
    let ret = unsafe { libc::flock(fd, libc::LOCK_EX | libc::LOCK_NB) };
    if ret == 0 {
        use std::io::Write;
        let mut f = &file;
        let _ = write!(f, "{}", std::process::id());
        Some(file)
    } else {
        None
    }
}
