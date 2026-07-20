/// 快捷键可配置化系统
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

/// 所有可用的命令
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Command {
    // === 会话管理 ===
    SessionNew,
    SessionClose,
    SessionNext,
    SessionPrev,
    SessionJump(usize), // 跳转到第 N 个会话 (0-8; defaults expose 0-7)
    SessionLast,

    // === 编辑操作 ===
    EditCopy,
    EditPaste,

    // === 搜索操作 ===
    SearchOpen,
    SearchClose,
    SearchNext,
    SearchPrev,
    SearchHistoryPrev,
    SearchHistoryNext,

    // === 终端操作 ===
    TerminalSendSigint, // Ctrl+C
    TerminalSendEof,    // Ctrl+D
    TerminalClear,      // Ctrl+L
    TerminalScrollUp,
    TerminalScrollDown,

    // === 分屏操作 ===
    TerminalSplitVertical,   // Ctrl+Shift+E (left/right)
    TerminalSplitHorizontal, // Ctrl+Shift+D (top/bottom)
    TerminalClosePane,       // Ctrl+Shift+W
    PaneFocusNext,
    PaneFocusPrev,
    PaneFocusLeft,
    PaneFocusRight,
    PaneFocusUp,
    PaneFocusDown,
    PaneResizeLeft,
    PaneResizeRight,
    PaneResizeUp,
    PaneResizeDown,
    PaneZoomToggle,
    PaneSwap,

    // === 窗口操作 ===
    WindowClose,

    // === 配置 ===
    ConfigOpen,
    ConfigClose,
    ConfigToggle,
    SidebarToggle,

    // === 字体缩放 ===
    FontZoomIn,
    FontZoomOut,
    FontZoomReset,
}

impl std::fmt::Display for Command {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Command::SessionNew => write!(f, "session:new"),
            Command::SessionClose => write!(f, "session:close"),
            Command::SessionNext => write!(f, "session:next"),
            Command::SessionPrev => write!(f, "session:prev"),
            Command::SessionJump(n) => write!(f, "session:jump:{}", n),
            Command::SessionLast => write!(f, "session:last"),
            Command::EditCopy => write!(f, "edit:copy"),
            Command::EditPaste => write!(f, "edit:paste"),
            Command::SearchOpen => write!(f, "search:open"),
            Command::SearchClose => write!(f, "search:close"),
            Command::SearchNext => write!(f, "search:next"),
            Command::SearchPrev => write!(f, "search:prev"),
            Command::SearchHistoryPrev => write!(f, "search:history:prev"),
            Command::SearchHistoryNext => write!(f, "search:history:next"),
            Command::TerminalSendSigint => write!(f, "terminal:send_sigint"),
            Command::TerminalSendEof => write!(f, "terminal:send_eof"),
            Command::TerminalClear => write!(f, "terminal:clear"),
            Command::TerminalScrollUp => write!(f, "terminal:scroll_up"),
            Command::TerminalScrollDown => write!(f, "terminal:scroll_down"),
            Command::TerminalSplitVertical => write!(f, "terminal:split_vertical"),
            Command::TerminalSplitHorizontal => write!(f, "terminal:split_horizontal"),
            Command::TerminalClosePane => write!(f, "terminal:close_pane"),
            Command::PaneFocusNext => write!(f, "pane:focus_next"),
            Command::PaneFocusPrev => write!(f, "pane:focus_prev"),
            Command::PaneFocusLeft => write!(f, "pane:focus_left"),
            Command::PaneFocusRight => write!(f, "pane:focus_right"),
            Command::PaneFocusUp => write!(f, "pane:focus_up"),
            Command::PaneFocusDown => write!(f, "pane:focus_down"),
            Command::PaneResizeLeft => write!(f, "pane:resize_left"),
            Command::PaneResizeRight => write!(f, "pane:resize_right"),
            Command::PaneResizeUp => write!(f, "pane:resize_up"),
            Command::PaneResizeDown => write!(f, "pane:resize_down"),
            Command::PaneZoomToggle => write!(f, "pane:zoom_toggle"),
            Command::PaneSwap => write!(f, "pane:swap"),
            Command::WindowClose => write!(f, "window:close"),
            Command::ConfigOpen => write!(f, "config:open"),
            Command::ConfigClose => write!(f, "config:close"),
            Command::ConfigToggle => write!(f, "config:toggle"),
            Command::SidebarToggle => write!(f, "sidebar:toggle"),
            Command::FontZoomIn => write!(f, "font:zoom_in"),
            Command::FontZoomOut => write!(f, "font:zoom_out"),
            Command::FontZoomReset => write!(f, "font:zoom_reset"),
        }
    }
}

impl std::str::FromStr for Command {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "session:new" => Ok(Command::SessionNew),
            "session:close" => Ok(Command::SessionClose),
            "session:next" => Ok(Command::SessionNext),
            "session:prev" => Ok(Command::SessionPrev),
            "session:last" => Ok(Command::SessionLast),
            "edit:copy" => Ok(Command::EditCopy),
            "edit:paste" => Ok(Command::EditPaste),
            "search:open" => Ok(Command::SearchOpen),
            "search:close" => Ok(Command::SearchClose),
            "search:next" => Ok(Command::SearchNext),
            "search:prev" => Ok(Command::SearchPrev),
            "search:history:prev" => Ok(Command::SearchHistoryPrev),
            "search:history:next" => Ok(Command::SearchHistoryNext),
            "terminal:send_sigint" => Ok(Command::TerminalSendSigint),
            "terminal:send_eof" => Ok(Command::TerminalSendEof),
            "terminal:clear" => Ok(Command::TerminalClear),
            "terminal:scroll_up" => Ok(Command::TerminalScrollUp),
            "terminal:scroll_down" => Ok(Command::TerminalScrollDown),
            "terminal:split_vertical" => Ok(Command::TerminalSplitVertical),
            "terminal:split_horizontal" => Ok(Command::TerminalSplitHorizontal),
            "terminal:close_pane" => Ok(Command::TerminalClosePane),
            "pane:focus_next" => Ok(Command::PaneFocusNext),
            "pane:focus_prev" => Ok(Command::PaneFocusPrev),
            "pane:focus_left" => Ok(Command::PaneFocusLeft),
            "pane:focus_right" => Ok(Command::PaneFocusRight),
            "pane:focus_up" => Ok(Command::PaneFocusUp),
            "pane:focus_down" => Ok(Command::PaneFocusDown),
            "pane:resize_left" => Ok(Command::PaneResizeLeft),
            "pane:resize_right" => Ok(Command::PaneResizeRight),
            "pane:resize_up" => Ok(Command::PaneResizeUp),
            "pane:resize_down" => Ok(Command::PaneResizeDown),
            "pane:zoom_toggle" => Ok(Command::PaneZoomToggle),
            "pane:swap" => Ok(Command::PaneSwap),
            "window:close" => Ok(Command::WindowClose),
            "config:open" => Ok(Command::ConfigOpen),
            "config:close" => Ok(Command::ConfigClose),
            "config:toggle" => Ok(Command::ConfigToggle),
            "sidebar:toggle" => Ok(Command::SidebarToggle),
            "font:zoom_in" => Ok(Command::FontZoomIn),
            "font:zoom_out" => Ok(Command::FontZoomOut),
            "font:zoom_reset" => Ok(Command::FontZoomReset),
            s if s.starts_with("session:jump:") => {
                let num_str = &s[13..];
                let num = num_str
                    .parse::<usize>()
                    .map_err(|_| format!("Invalid session number: {}", num_str))?;
                if num < 9 {
                    Ok(Command::SessionJump(num))
                } else {
                    Err(format!("Session number out of range: {}", num))
                }
            }
            _ => Err(format!("Unknown command: {}", s)),
        }
    }
}

/// 快捷键修饰符
#[allow(dead_code)]
#[derive(
    Clone, Debug, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, Default,
)]
pub struct Modifiers {
    pub ctrl: bool,
    pub shift: bool,
    pub alt: bool,
    pub super_key: bool,
}

/// 快捷键（可配置）
#[allow(dead_code)]
#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub struct KeyBinding {
    pub key: String, // "a", "Tab", "F1", 等
    pub modifiers: Modifiers,
    pub command: Command,
}

impl KeyBinding {
    #[cfg(test)]
    pub fn new(key: &str, modifiers: Modifiers, command: Command) -> Self {
        Self {
            key: key.to_lowercase(),
            modifiers,
            command,
        }
    }

    /// 从快捷键字符串解析（格式：ctrl+shift+a, alt+F1 等）
    #[cfg(test)]
    pub fn from_string(binding_str: &str, command: Command) -> Result<Self, String> {
        let binding_lower = binding_str.to_lowercase();
        let parts: Vec<&str> = binding_lower.split('+').collect();

        if parts.is_empty() {
            return Err("Empty binding string".to_string());
        }

        let mut modifiers = Modifiers::default();
        let mut key = "";

        for (i, part) in parts.iter().enumerate() {
            match *part {
                "ctrl" => modifiers.ctrl = true,
                "shift" => modifiers.shift = true,
                "alt" => modifiers.alt = true,
                "super" | "cmd" => modifiers.super_key = true,
                _ => {
                    // 最后一部分应该是按键
                    if i == parts.len() - 1 {
                        key = part;
                    } else {
                        return Err(format!("Invalid modifier or key: {}", part));
                    }
                }
            }
        }

        if key.is_empty() {
            return Err("No key specified".to_string());
        }

        Ok(Self::new(key, modifiers, command))
    }

    /// Canonicalize a binding string to the `ctrl+shift+alt+super+key` order
    /// (all lowercase) used internally, accepting any modifier order and the
    /// common aliases (`cmd`/`command`/`win`/`meta` -> super, `option` -> alt,
    /// `control` -> ctrl). Returns `None` if the string isn't a valid binding.
    /// This is what lets a user-written `shift+ctrl+f` or `cmd+c` actually match.
    pub fn canonical(binding_str: &str) -> Option<String> {
        let lower = binding_str.to_lowercase();
        let (mut ctrl, mut shift, mut alt, mut sup) = (false, false, false, false);
        let mut key: Option<String> = None;
        let parts: Vec<&str> = lower.split('+').collect();
        for (i, part) in parts.iter().enumerate() {
            match *part {
                "ctrl" | "control" => ctrl = true,
                "shift" => shift = true,
                "alt" | "option" => alt = true,
                "super" | "cmd" | "command" | "win" | "meta" => sup = true,
                "" => return None,
                other => {
                    if i == parts.len() - 1 {
                        key = Some(other.to_string());
                    } else {
                        return None;
                    }
                }
            }
        }
        let key = match key?.as_str() {
            "\\" | "backslash" => "backslash".to_string(),
            other => other.to_string(),
        };
        let mut out = String::new();
        if ctrl {
            out.push_str("ctrl+");
        }
        if shift {
            out.push_str("shift+");
        }
        if alt {
            out.push_str("alt+");
        }
        if sup {
            out.push_str("super+");
        }
        out.push_str(&key);
        Some(out)
    }
}

#[cfg(test)]
impl std::fmt::Display for KeyBinding {
    /// 转换为快捷键字符串表示。
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut parts = Vec::new();

        if self.modifiers.ctrl {
            parts.push("Ctrl");
        }
        if self.modifiers.shift {
            parts.push("Shift");
        }
        if self.modifiers.alt {
            parts.push("Alt");
        }
        if self.modifiers.super_key {
            parts.push("Super");
        }

        // 按键首字母大写
        let key = if self.key.len() == 1 {
            self.key.to_uppercase()
        } else {
            format!(
                "{}{}",
                self.key.chars().next().unwrap().to_uppercase(),
                &self.key[1..]
            )
        };
        parts.push(&key);

        formatter.write_str(&parts.join("+"))
    }
}

/// 快捷键绑定集合
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct KeyBindings {
    #[serde(flatten)]
    pub bindings: HashMap<String, String>, // "ctrl+shift+a" => "command:name"
}

impl KeyBindings {
    pub fn new() -> Self {
        Self {
            bindings: HashMap::new(),
        }
    }

    /// 加载默认快捷键
    pub fn default_bindings() -> Self {
        let mut bindings = Self::new();

        // 会话管理
        bindings
            .bindings
            .insert("ctrl+shift+t".to_string(), "session:new".to_string());
        bindings
            .bindings
            .insert("ctrl+d".to_string(), "terminal:send_eof".to_string());
        bindings
            .bindings
            .insert("ctrl+tab".to_string(), "session:next".to_string());
        bindings
            .bindings
            .insert("ctrl+shift+tab".to_string(), "session:prev".to_string());
        bindings
            .bindings
            .insert("ctrl+pagedown".to_string(), "session:next".to_string());
        bindings
            .bindings
            .insert("ctrl+pageup".to_string(), "session:prev".to_string());

        // Browser-style tab switching: Ctrl+1..8 address the first eight tabs,
        // Ctrl+9 always selects the last tab, and Ctrl+0 resets font zoom.
        for i in 0..8 {
            bindings
                .bindings
                .insert(format!("ctrl+{}", i + 1), format!("session:jump:{}", i));
        }
        bindings
            .bindings
            .insert("ctrl+9".to_string(), "session:last".to_string());

        // 编辑操作
        bindings
            .bindings
            .insert("ctrl+shift+c".to_string(), "edit:copy".to_string());
        bindings
            .bindings
            .insert("ctrl+shift+v".to_string(), "edit:paste".to_string());

        // 分屏操作
        bindings.bindings.insert(
            "ctrl+shift+e".to_string(),
            "terminal:split_vertical".to_string(),
        );
        bindings.bindings.insert(
            "ctrl+shift+d".to_string(),
            "terminal:split_horizontal".to_string(),
        );
        bindings.bindings.insert(
            "ctrl+shift+w".to_string(),
            "terminal:close_pane".to_string(),
        );
        for (key, command) in [
            ("left", "pane:focus_left"),
            ("right", "pane:focus_right"),
            ("up", "pane:focus_up"),
            ("down", "pane:focus_down"),
        ] {
            bindings
                .bindings
                .insert(format!("ctrl+alt+{key}"), command.to_string());
        }
        for (key, command) in [
            ("left", "pane:resize_left"),
            ("right", "pane:resize_right"),
            ("up", "pane:resize_up"),
            ("down", "pane:resize_down"),
        ] {
            bindings
                .bindings
                .insert(format!("ctrl+shift+alt+{key}"), command.to_string());
        }
        bindings
            .bindings
            .insert("ctrl+shift+z".to_string(), "pane:zoom_toggle".to_string());
        bindings
            .bindings
            .insert("ctrl+shift+x".to_string(), "pane:swap".to_string());

        // 搜索操作
        bindings
            .bindings
            .insert("ctrl+shift+f".to_string(), "search:open".to_string());

        // 配置操作
        bindings
            .bindings
            .insert("ctrl+shift+o".to_string(), "config:toggle".to_string());
        bindings
            .bindings
            .insert("ctrl+backslash".to_string(), "sidebar:toggle".to_string());

        // 终端操作
        bindings
            .bindings
            .insert("ctrl+up".to_string(), "terminal:scroll_up".to_string());
        bindings
            .bindings
            .insert("ctrl+down".to_string(), "terminal:scroll_down".to_string());

        // 字体缩放
        bindings
            .bindings
            .insert("ctrl+=".to_string(), "font:zoom_in".to_string());
        bindings
            .bindings
            .insert("ctrl+-".to_string(), "font:zoom_out".to_string());
        bindings
            .bindings
            .insert("ctrl+0".to_string(), "font:zoom_reset".to_string());

        bindings
    }

    /// 获取快捷键对应的命令
    pub fn get_command(&self, key_str: &str) -> Option<Command> {
        let normalized = KeyBinding::canonical(key_str)?;
        self.bindings
            .get(&normalized)
            .and_then(|cmd_str| cmd_str.parse::<Command>().ok())
    }

    /// 检测快捷键冲突
    pub fn check_conflicts(&self) -> Vec<String> {
        let mut conflicts = Vec::new();

        // 如果两个不同的快捷键映射到同一个命令，不算冲突
        // 如果一个快捷键映射到多个命令，这在 HashMap 中不会发生

        for (binding, command_str) in &self.bindings {
            if let Err(e) = command_str.parse::<Command>() {
                conflicts.push(format!("Invalid command in binding '{}': {}", binding, e));
            }
        }

        conflicts
    }

    /// 加载配置文件，与默认配置合并
    pub fn load() -> Result<Self, Box<dyn std::error::Error>> {
        let mut bindings = Self::default_bindings();

        let path = Self::config_path()?;
        if path.exists() {
            let content = std::fs::read_to_string(&path)?;
            let user_bindings: KeyBindings = toml::from_str(&content)?;
            // 合并用户配置到默认配置，用户配置会覆盖默认值。
            // Canonicalize each user key so a different modifier order or an
            // alias (cmd/option/…) still overrides the matching default.
            for (key, value) in user_bindings.bindings {
                let key = KeyBinding::canonical(&key).unwrap_or(key);
                bindings.bindings.insert(key, value);
            }
        }

        Ok(bindings)
    }

    /// 获取配置文件路径
    pub fn config_path() -> Result<PathBuf, Box<dyn std::error::Error>> {
        let config_dir = dirs::config_dir().ok_or("Could not determine config directory")?;
        Ok(config_dir.join("jterm3/keybindings.toml"))
    }
}

impl Default for KeyBindings {
    fn default() -> Self {
        Self::default_bindings()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_command_parse() {
        let cmd: Command = "session:new".parse().unwrap();
        assert_eq!(cmd, Command::SessionNew);

        let cmd: Command = "session:jump:5".parse().unwrap();
        assert_eq!(cmd, Command::SessionJump(5));

        let cmd: Command = "sidebar:toggle".parse().unwrap();
        assert_eq!(cmd, Command::SidebarToggle);
    }

    #[test]
    fn test_keybinding_from_string() {
        let binding = KeyBinding::from_string("ctrl+shift+a", Command::EditCopy).unwrap();
        assert!(binding.modifiers.ctrl);
        assert!(binding.modifiers.shift);
        assert_eq!(binding.key, "a");
    }

    #[test]
    fn test_keybinding_display() {
        let binding = KeyBinding::new(
            "a",
            Modifiers {
                ctrl: true,
                shift: true,
                alt: false,
                super_key: false,
            },
            Command::EditCopy,
        );

        let display = binding.to_string();
        assert!(display.contains("Ctrl"));
        assert!(display.contains("Shift"));
        assert!(display.contains("A"));
    }

    #[test]
    fn common_default_chord_matrix() {
        let bindings = KeyBindings::default_bindings();
        let cases = [
            ("ctrl+shift+t", Command::SessionNew),
            ("ctrl+shift+w", Command::TerminalClosePane),
            ("ctrl+shift+c", Command::EditCopy),
            ("ctrl+shift+v", Command::EditPaste),
            ("ctrl+shift+f", Command::SearchOpen),
            ("ctrl+shift+o", Command::ConfigToggle),
            ("ctrl+backslash", Command::SidebarToggle),
            ("ctrl+shift+e", Command::TerminalSplitVertical),
            ("ctrl+shift+d", Command::TerminalSplitHorizontal),
            ("ctrl+shift+z", Command::PaneZoomToggle),
            ("ctrl+shift+x", Command::PaneSwap),
            ("ctrl+=", Command::FontZoomIn),
            ("ctrl+-", Command::FontZoomOut),
            ("ctrl+0", Command::FontZoomReset),
        ];
        for (chord, expected) in cases {
            assert_eq!(bindings.get_command(chord), Some(expected), "{chord}");
        }

        assert_eq!(
            bindings.get_command("ctrl+\\"),
            Some(Command::SidebarToggle),
            "literal and named backslash forms must canonicalize identically"
        );
        assert_eq!(bindings.get_command("ctrl+shift+j"), None);
    }

    #[test]
    fn browser_style_digit_bindings_use_nine_for_last_and_zero_for_zoom() {
        let bindings = KeyBindings::default_bindings();
        for digit in 1usize..=8 {
            let chord = format!("ctrl+{digit}");
            assert_eq!(
                bindings.get_command(&chord),
                Some(Command::SessionJump(digit - 1)),
                "{chord}"
            );
        }
        assert_eq!(bindings.get_command("ctrl+9"), Some(Command::SessionLast));
        assert_eq!(bindings.get_command("ctrl+0"), Some(Command::FontZoomReset));
    }

    #[test]
    fn pane_direction_and_resize_chords_are_complete() {
        let bindings = KeyBindings::default_bindings();
        let cases = [
            ("ctrl+alt+left", Command::PaneFocusLeft),
            ("ctrl+alt+right", Command::PaneFocusRight),
            ("ctrl+alt+up", Command::PaneFocusUp),
            ("ctrl+alt+down", Command::PaneFocusDown),
            ("ctrl+alt+shift+left", Command::PaneResizeLeft),
            ("ctrl+alt+shift+right", Command::PaneResizeRight),
            ("ctrl+alt+shift+up", Command::PaneResizeUp),
            ("ctrl+alt+shift+down", Command::PaneResizeDown),
        ];
        for (chord, expected) in cases {
            assert_eq!(bindings.get_command(chord), Some(expected), "{chord}");
        }
    }

    #[test]
    fn test_conflict_detection() {
        let bindings = KeyBindings::default_bindings();
        let conflicts = bindings.check_conflicts();
        assert!(
            conflicts.is_empty(),
            "Default bindings should have no conflicts"
        );
    }

    #[test]
    fn test_command_display() {
        assert_eq!(Command::SessionNew.to_string(), "session:new");
        assert_eq!(Command::SessionJump(3).to_string(), "session:jump:3");
        assert_eq!(Command::SessionLast.to_string(), "session:last");
        assert_eq!(Command::PaneFocusLeft.to_string(), "pane:focus_left");
        assert_eq!(Command::PaneResizeDown.to_string(), "pane:resize_down");
    }
}
