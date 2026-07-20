mod char_width;
mod color;
mod command_palette;
mod config;
mod debug;
mod keybindings;
mod kitty_graphics;
mod link;
mod pty;
mod search;
mod search_replace;
mod session_persistence;
mod sidebar;
mod terminal;
mod terminal_view;
mod theme;

use std::hash::Hash;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::sync::{Arc, Mutex};

use config::Config;
use iced::widget::{
    button, checkbox, column, container, mouse_area, pick_list, row, scrollable, slider, stack,
    text, text_input, Space,
};
use iced::{keyboard, Color, Element, Length, Size, Subscription, Task};
use pty::{Pty, ReaderPoll};
use terminal::{TerminalCell, TerminalState};
use terminal_view::{KittyRender, Metrics, MouseButton, MouseInput, TermWidget};
use theme::Theme;

/// Height reserved for the tab bar at the top of the window.
const TAB_BAR_H: f32 = 30.0;
/// Height reserved for the status bar at the bottom of the window.
const STATUS_BAR_H: f32 = 22.0;
/// Default width of the file-tree sidebar when shown.
const SIDEBAR_W: f32 = 220.0;
/// Drag-resize bounds for the sidebar width.
const SIDEBAR_W_MIN: f32 = 120.0;
const SIDEBAR_W_MAX: f32 = 500.0;
/// Thickness of the divider drawn between split panes (also its drag hit area).
const DIVIDER: f32 = 6.0;
/// Maximum panes in a split (all along one axis).
const MAX_PANES: usize = 6;
/// Minimum share of the split axis a single pane may occupy.
const PANE_RATIO_MIN: f32 = 0.1;
const SPLIT_RATIO_KEY_STEP: f32 = 0.05;
/// Dragging a divider within this distance of the even point between its two
/// neighbor panes snaps them to equal size, so tidy layouts are easy to hit.
const SPLIT_SNAP_EPSILON: f32 = 0.02;
/// Two presses on the same divider within this window count as a double-click
/// (equalizes every pane).
const DIVIDER_DOUBLE_CLICK_MS: u64 = 400;
/// Guard against a corrupted or hostile session snapshot spawning unbounded PTYs.
const MAX_RESTORED_SESSIONS: usize = 32;
/// Bound pending user/protocol input while a child is not reading its PTY.
const MAX_PTY_WRITE_QUEUE_BYTES: usize = 8 * 1024 * 1024;
/// Responses are retried separately so a full user-input queue cannot discard
/// terminal protocol replies. The combined per-session backlog remains bounded.
const MAX_PTY_RESPONSE_QUEUE_BYTES: usize = 8 * 1024 * 1024;
const BRACKETED_PASTE_FRAMING_BYTES: usize = 12;
/// Byte caps alone do not cover allocator/Vec metadata for one-byte writes.
const MAX_PTY_QUEUE_ENTRIES: usize = 4096;
const PTY_QUEUE_COALESCE_BYTES: usize = 64 * 1024;
/// Maximum queued input written during one UI update.
const PTY_WRITE_DRAIN_BUDGET: usize = 256 * 1024;
/// Never reflect an unexpectedly huge host clipboard through a terminal escape.
const MAX_CLIPBOARD_RESPONSE_BYTES: usize = 1024 * 1024;

/// Stable widget ids so the overlays' text inputs can be focused on open.
static SEARCH_INPUT_ID: once_cell::sync::Lazy<iced::widget::Id> =
    once_cell::sync::Lazy::new(|| iced::widget::Id::new("jterm-search-input"));
static PALETTE_INPUT_ID: once_cell::sync::Lazy<iced::widget::Id> =
    once_cell::sync::Lazy::new(|| iced::widget::Id::new("jterm-palette-input"));
static TAB_SWITCHER_INPUT_ID: once_cell::sync::Lazy<iced::widget::Id> =
    once_cell::sync::Lazy::new(|| iced::widget::Id::new("jterm-tab-switcher-input"));

/// Toast kind drives the accent color of the floating notification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ToastKind {
    Info,
    Success,
    Warning,
}

/// Transient bottom-right notification. `expires_at` is absolute monotonic time.
#[derive(Debug, Clone)]
struct Toast {
    text: String,
    kind: ToastKind,
    expires_at: std::time::Instant,
}

/// State for the Ctrl+Shift+L quick tab switcher overlay.
#[derive(Debug, Clone, Default)]
struct TabSwitcherState {
    query: String,
    /// Highlighted row in the filtered list.
    selected: usize,
}

/// Which content the left sidebar dock currently shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SidebarPanel {
    /// File-tree browser (doubles as a path picker).
    Files,
    /// Vertical session tab list.
    Tabs,
}

/// How the active view is split into panes: a single pane, or up to
/// [`MAX_PANES`] panes laid out along one axis.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SplitMode {
    /// A single pane filling the terminal area.
    Single,
    /// Panes side by side (left | right).
    Vertical,
    /// Panes stacked (top / bottom).
    Horizontal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PaneDirection {
    Left,
    Right,
    Up,
    Down,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ChromeShortcut {
    CommandPalette,
    Help,
    TabSwitcher,
    Debug,
}

fn chrome_shortcut(key: &keyboard::Key, modifiers: keyboard::Modifiers) -> Option<ChromeShortcut> {
    use keyboard::key::Named;
    use keyboard::Key;

    if matches!(key, Key::Named(Named::F12)) {
        return Some(ChromeShortcut::Debug);
    }
    if !(modifiers.control() && modifiers.shift()) {
        return None;
    }
    let Key::Character(s) = key else {
        return None;
    };
    match s.chars().next()?.to_ascii_lowercase() {
        'p' => Some(ChromeShortcut::CommandPalette),
        '/' | '?' => Some(ChromeShortcut::Help),
        'l' => Some(ChromeShortcut::TabSwitcher),
        _ => None,
    }
}

/// Whether an arrow moves toward higher pane positions along the split axis.
/// `None` for arrows perpendicular to the axis, which deliberately do nothing.
fn direction_along_axis(split_mode: SplitMode, direction: PaneDirection) -> Option<bool> {
    match (split_mode, direction) {
        (SplitMode::Vertical, PaneDirection::Left) | (SplitMode::Horizontal, PaneDirection::Up) => {
            Some(false)
        }
        (SplitMode::Vertical, PaneDirection::Right)
        | (SplitMode::Horizontal, PaneDirection::Down) => Some(true),
        _ => None,
    }
}

/// Return the adjacent pane in a physical direction (no wrap at the edges).
fn directional_pane_target(
    split_mode: SplitMode,
    focused_pane: usize,
    pane_count: usize,
    direction: PaneDirection,
) -> Option<usize> {
    let forward = direction_along_axis(split_mode, direction)?;
    if forward {
        (focused_pane + 1 < pane_count).then(|| focused_pane + 1)
    } else {
        focused_pane.checked_sub(1)
    }
}

/// The divider a resize arrow moves for the focused pane: the one on the
/// arrow's side when it exists, else the pane's other divider — so the arrow
/// always drags a divider in its own direction. Divider `d` sits between panes
/// `d` and `d + 1`.
fn resize_divider_target(
    split_mode: SplitMode,
    focused_pane: usize,
    pane_count: usize,
    direction: PaneDirection,
) -> Option<usize> {
    if pane_count < 2 {
        return None;
    }
    let forward = direction_along_axis(split_mode, direction)?;
    let divider = if forward {
        focused_pane.min(pane_count - 2)
    } else {
        focused_pane.saturating_sub(1)
    };
    Some(divider)
}

/// Set pane `d`'s share of the pair it forms with pane `d + 1`, keeping the
/// pair's total constant and both panes at least `PANE_RATIO_MIN`. With `snap`,
/// shares close to an even pair split settle exactly there. Returns whether
/// anything changed.
fn set_divider_share(ratios: &mut [f32], d: usize, first: f32, snap: bool) -> bool {
    if d + 1 >= ratios.len() {
        return false;
    }
    let pair = ratios[d] + ratios[d + 1];
    let mut first = first;
    if snap && (first - pair / 2.0).abs() < SPLIT_SNAP_EPSILON {
        first = pair / 2.0;
    }
    // A pair squeezed below two minimums degrades to an even split.
    let lo = PANE_RATIO_MIN.min(pair / 2.0);
    let first = first.clamp(lo, pair - lo);
    if (first - ratios[d]).abs() <= f32::EPSILON {
        return false;
    }
    ratios[d] = first;
    ratios[d + 1] = pair - first;
    true
}

/// Make room for a new pane after `at` by halving `at`'s share; when the halves
/// would fall below the minimum, every pane is equalized instead.
fn insert_pane_share(ratios: &mut Vec<f32>, at: usize) {
    let half = ratios[at] / 2.0;
    ratios[at] = half;
    ratios.insert(at + 1, half);
    if half < PANE_RATIO_MIN {
        equalize_shares(ratios);
    }
}

/// Remove pane `at`, folding its share into the preceding pane (or the new
/// first pane when `at` was first) so the other panes keep their sizes.
fn remove_pane_share(ratios: &mut Vec<f32>, at: usize) {
    if at >= ratios.len() {
        return;
    }
    let freed = ratios.remove(at);
    if let Some(neighbor) = ratios.get_mut(at.saturating_sub(1)) {
        *neighbor += freed;
    }
}

fn equalize_shares(ratios: &mut [f32]) {
    let n = ratios.len().max(1) as f32;
    for r in ratios.iter_mut() {
        *r = 1.0 / n;
    }
}

fn last_session_index(session_count: usize) -> Option<usize> {
    session_count.checked_sub(1)
}

/// Linear blend between two colors (t=0 → a, t=1 → b); result is fully opaque.
fn blend(a: Color, b: Color, t: f32) -> Color {
    Color {
        r: a.r + (b.r - a.r) * t,
        g: a.g + (b.g - a.g) * t,
        b: a.b + (b.b - a.b) * t,
        a: 1.0,
    }
}

fn resolve_mono_font(family: &str) -> iced::Font {
    let f = family.trim();
    if f.is_empty() {
        iced::Font::MONOSPACE
    } else {
        // iced stores family names as `&'static str`. Intern each distinct name
        // once so repeatedly applying settings does not leak another allocation.
        static INTERNED_FONTS: once_cell::sync::Lazy<
            Mutex<std::collections::HashMap<String, &'static str>>,
        > = once_cell::sync::Lazy::new(|| Mutex::new(std::collections::HashMap::new()));
        let mut names = INTERNED_FONTS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let name = *names
            .entry(f.to_string())
            .or_insert_with(|| Box::leak(f.to_string().into_boxed_str()));
        iced::Font::with_name(name)
    }
}

fn resolve_optional_font(family: Option<&str>) -> Option<iced::Font> {
    family.map(resolve_mono_font)
}

fn main() -> iced::Result {
    env_logger::init();
    let config = Config::load();
    let win = iced::window::Settings {
        size: Size::new(config.initial_width, config.initial_height),
        // Route window-manager close requests through our foreground-job guard.
        exit_on_close_request: false,
        ..Default::default()
    };
    iced::application(
        move || Jterm::new(config.clone()),
        Jterm::update,
        Jterm::view,
    )
    .title(Jterm::title)
    .subscription(Jterm::subscription)
    .theme(Jterm::iced_theme)
    // MSAA forces wgpu down the multisample path; on Intel/Mesa that triggers
    // the "manual shader clears for srgb textures" path, which flashes the whole
    // surface on heavy redraws (e.g. multi-line `ls` output). Glyph and quad
    // rendering don't benefit from geometry MSAA, so disabling it is free here.
    .antialiasing(false)
    .window(win)
    .run()
}

#[derive(Debug, Clone)]
enum Message {
    PtyOutput(usize, RawFd, Vec<u8>),
    PtyExited(usize, RawFd, i32),
    Key(keyboard::Event),
    /// An input-method (IME) composition event: open/close, pre-edit updates,
    /// and committed text.
    Ime(iced::advanced::input_method::Event),
    ModifiersChanged(keyboard::Modifiers),
    /// A mouse interaction within pane `usize` (index into `panes`).
    MousePane(usize, MouseInput),
    /// Clipboard result scoped to the stable session that requested the paste.
    Pasted(usize, Option<String>),
    /// System clipboard contents read in response to an OSC 52 query from the
    /// app running in the session identified by the file descriptor.
    Osc52Query(usize, RawFd, Option<String>),
    /// System clipboard contents read in response to an OSC 5522 MIME-data read
    /// request. Carries the requesting fd and the MIME type that was requested.
    Osc5522Data(usize, RawFd, String, Option<String>),
    Resized(Size),
    Focus(bool),
    NewSession,
    /// Close the tab with this stable session id.
    CloseTab(usize),
    WindowClose,
    TabHover(Option<usize>),
    /// User pressed the mouse over a tab — start tracking its stable session id.
    TabDragStart(usize),
    /// User released the mouse over a tab. Both endpoints are stable session ids.
    TabDragEnd(usize),
    /// Global mouse-up: clear `dragging_tab` if a drag was started but the
    /// release happened outside any tab.
    TabDragCancel,
    ToggleSidebar,
    SetSidebarPanel(SidebarPanel),
    SetTabPosition(config::TabPosition),
    SidebarDragStart,
    SidebarDragMove(iced::Point),
    SidebarDragEnd,
    SidebarToggleNode(std::path::PathBuf),
    SidebarInsertPath(std::path::PathBuf),
    /// Press on the divider between panes `usize` and `usize + 1`.
    DividerDragStart(usize),
    DividerDragMove(iced::Point),
    DividerDragEnd,
    DividerHover(Option<usize>),
    SearchToggleRegex,
    SearchToggleCase,
    SearchInput(String),
    PaletteInput(String),
    PaletteExecute(usize),
    ToggleConfigPanel,
    SetTheme(String),
    SetFontSize(f32),
    SetLineSpacing(f32),
    SetPadding(f32),
    SetScrollback(u32),
    SetScrollSpeed(u32),
    SetFontFamily(String),
    SetScrollbarAlways(bool),
    SetDisableAltScreen(bool),
    SetAllowClipboardRead(bool),
    ThemeEditOpen,
    ThemeEditClose,
    ThemeEditName(String),
    ThemeEditColor(usize, String),
    ThemeEditSave,
    ThemeDelete(String),
    ConfigSave,
    ConfigReset,
    ConfigTick,
    BlinkTick,
    PtyWriteTick,
    SearchRefreshTick,
    HistoryReflowTick,
    /// Right-click on a tab opened its context menu (close/duplicate/etc).
    TabMenuOpen(usize),
    /// Dismiss the tab context menu without an action.
    TabMenuClose,
    /// Execute a menu action against the target tab.
    TabMenuAction(TabMenuAction),
    /// Toast queue tick (drop expired entries).
    ToastTick,
    /// Dismiss a specific toast by index.
    ToastDismiss(usize),
    /// Filter text changed in the tab switcher.
    TabSwitcherInput(String),
    /// Cancel the tab switcher overlay.
    TabSwitcherClose,
    /// Jump to the given stable session id from the tab switcher (and close it).
    TabSwitcherJump(usize),
    /// User confirmed closing a tab with a running foreground process.
    TabCloseConfirmYes,
    /// User cancelled the close-confirmation overlay.
    TabCloseConfirmNo,
}

/// Context-menu actions that target a stable session id.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TabMenuAction {
    Close(usize),
    CloseOthers(usize),
    CloseToRight(usize),
    Duplicate(usize),
}

/// Subscription identity plus a reader descriptor duplicated synchronously when
/// the session is created. Equality/hash intentionally ignore the descriptor
/// object: the monotonic session id and original fd identify the iced stream.
#[derive(Clone)]
struct PtySubscriptionKey {
    id: usize,
    master_fd: RawFd,
    reader_fd: Arc<OwnedFd>,
}

struct PtyWriteChunk {
    data: Vec<u8>,
    response: bool,
}

impl PartialEq for PtySubscriptionKey {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id && self.master_fd == other.master_fd
    }
}

impl Eq for PtySubscriptionKey {}

impl Hash for PtySubscriptionKey {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.id.hash(state);
        self.master_fd.hash(state);
    }
}

/// In-progress custom theme being edited in the theme editor overlay. UI-chrome
/// colors are inherited from `base`; only the terminal palette is editable here.
struct ThemeEditState {
    base: Theme,
    name: String,
    /// Hex buffers aligned with `Theme::editable_color_labels()` (19 entries).
    hexes: Vec<String>,
    error: Option<String>,
}

/// A single terminal session: its own PTY child and terminal state.
struct Session {
    id: usize,
    terminal: TerminalState,
    pty: Pty,
    master_fd: RawFd,
    reader_fd: Arc<OwnedFd>,
    grid: Arc<Vec<Vec<TerminalCell>>>,
    cursor: (usize, usize),
    cursor_visible: bool,
    /// Cached working directory, refreshed periodically so the status bar can
    /// display it without a `readlink` syscall on every render frame.
    cwd_cache: Option<String>,
    /// Cached foreground process name (via tcgetpgrp + /proc/<pgid>/comm),
    /// refreshed on the same cadence as `cwd_cache`. Empty/None when the
    /// shell itself is in the foreground so tab labels can hide it.
    fg_proc_cache: Option<String>,
    /// Non-blocking PTY writes may be partial. Keep the remainder here and let a
    /// short-lived timer drain it without ever stalling iced's UI thread.
    write_queue: std::collections::VecDeque<PtyWriteChunk>,
    write_queue_offset: usize,
    queued_write_bytes: usize,
    queued_response_bytes: usize,
    /// Host clipboard access is asynchronous. Limit PTY-originated reads to one
    /// per session so a hostile child cannot accumulate work across UI batches.
    clipboard_read_in_flight: bool,
}

impl Session {
    fn spawn(
        config: &Config,
        id: usize,
        cols: usize,
        rows: usize,
        cwd: Option<&str>,
    ) -> Option<Session> {
        let pty = Pty::new_with_cwd(cols, rows, cwd, None, config.shell.as_deref()).ok()?;
        let master_fd = pty.master_fd();
        let reader_fd = unsafe { libc::fcntl(master_fd, libc::F_DUPFD_CLOEXEC, 0) };
        if reader_fd < 0 {
            log::error!(
                "[PTY] failed to duplicate reader fd: {}",
                std::io::Error::last_os_error()
            );
            return None;
        }
        // SAFETY: `fcntl(F_DUPFD_CLOEXEC)` returned a fresh owned descriptor.
        let reader_fd = Arc::new(unsafe { OwnedFd::from_raw_fd(reader_fd) });
        let mut terminal = TerminalState::new(cols, rows);
        terminal.set_max_scrollback(config.scrollback_lines);
        terminal.set_disable_alt_screen(config.disable_alt_screen);
        let grid = terminal.get_visible_cells();
        let cursor = terminal.get_cursor_pos();
        let cursor_visible = terminal.is_cursor_visible();
        Some(Session {
            id,
            terminal,
            pty,
            master_fd,
            reader_fd,
            grid,
            cursor,
            cursor_visible,
            cwd_cache: None,
            fg_proc_cache: None,
            write_queue: std::collections::VecDeque::new(),
            write_queue_offset: 0,
            queued_write_bytes: 0,
            queued_response_bytes: 0,
            clipboard_read_in_flight: false,
        })
    }

    /// Tab label: prefer an OSC-set window title; otherwise show the foreground
    /// process and/or cwd basename so a fresh shell with no title still tells
    /// the user where they are. Falls back to "Session N" only when none of
    /// those are known yet.
    fn label(&self) -> String {
        let t = self.terminal.window_title.trim();
        if !t.is_empty() {
            return t.to_string();
        }
        let cwd_short = self.cwd_cache.as_deref().and_then(Self::cwd_basename);
        match (&self.fg_proc_cache, cwd_short) {
            (Some(p), Some(d)) => format!("{p} · {d}"),
            (Some(p), None) => p.clone(),
            (None, Some(d)) => d,
            (None, None) => format!("Session {}", self.id + 1),
        }
    }

    /// Short, human-friendly form of an absolute cwd: "~" for $HOME, just the
    /// basename otherwise. Returns None for "/" or unparsable paths.
    fn cwd_basename(cwd: &str) -> Option<String> {
        if let Some(home) = std::env::var_os("HOME") {
            let home = home.to_string_lossy();
            if cwd == home {
                return Some("~".to_string());
            }
        }
        let p = std::path::Path::new(cwd);
        p.file_name()
            .and_then(|n| n.to_str())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
    }

    /// Foreground process name on the PTY, or None when it's the shell itself
    /// (so the tab label doesn't redundantly show "bash" / "zsh" / "fish").
    fn fg_proc(&self) -> Option<String> {
        let pgid = unsafe { libc::tcgetpgrp(self.master_fd) };
        if pgid <= 0 {
            return None;
        }
        let comm = std::fs::read_to_string(format!("/proc/{pgid}/comm")).ok()?;
        let comm = comm.trim().to_string();
        if comm.is_empty() {
            return None;
        }
        // Hide when the foreground process *is* the shell — that's the idle case.
        if pgid as i32 == self.pty.get_child_pid() {
            return None;
        }
        const SHELLS: &[&str] = &["bash", "zsh", "fish", "sh", "dash", "ksh", "tcsh"];
        if SHELLS.contains(&comm.as_str()) {
            return None;
        }
        Some(comm)
    }

    fn refresh(&mut self) {
        self.grid = self.terminal.get_visible_cells();
        self.cursor = self.terminal.get_cursor_pos();
        self.cursor_visible = self.terminal.is_cursor_visible();
    }

    fn queue_accepts_entry(
        queue: &std::collections::VecDeque<PtyWriteChunk>,
        len: usize,
        response: bool,
    ) -> bool {
        queue.len() < MAX_PTY_QUEUE_ENTRIES
            || queue.back().is_some_and(|back| {
                back.response == response
                    && len <= PTY_QUEUE_COALESCE_BYTES.saturating_sub(back.data.len())
            })
    }

    fn push_queue_owned(
        queue: &mut std::collections::VecDeque<PtyWriteChunk>,
        data: Vec<u8>,
        response: bool,
    ) {
        let coalesce = queue.back().is_some_and(|back| {
            back.response == response
                && data.len() <= PTY_QUEUE_COALESCE_BYTES.saturating_sub(back.data.len())
        });
        if coalesce {
            if let Some(back) = queue.back_mut() {
                back.data.extend_from_slice(&data);
                return;
            }
        }
        queue.push_back(PtyWriteChunk { data, response });
    }

    fn push_queue_copy(
        queue: &mut std::collections::VecDeque<PtyWriteChunk>,
        data: &[u8],
        response: bool,
    ) {
        let coalesce = queue.back().is_some_and(|back| {
            back.response == response
                && data.len() <= PTY_QUEUE_COALESCE_BYTES.saturating_sub(back.data.len())
        });
        if coalesce {
            if let Some(back) = queue.back_mut() {
                back.data.extend_from_slice(data);
                return;
            }
        }
        queue.push_back(PtyWriteChunk {
            data: data.to_vec(),
            response,
        });
    }

    fn flush_responses(&mut self) {
        let out = self.terminal.get_output();
        if out.is_empty() {
            return;
        }
        if !self.flush_write_queue() {
            return;
        }
        if out.len() > MAX_PTY_RESPONSE_QUEUE_BYTES.saturating_sub(self.queued_response_bytes)
            || !Self::queue_accepts_entry(&self.write_queue, out.len(), true)
        {
            log::warn!(
                "[PTY] response queue limit reached for session {} ({} queued, {} incoming)",
                self.id,
                self.queued_response_bytes,
                out.len()
            );
            return;
        }
        self.queued_response_bytes += out.len();
        Self::push_queue_owned(&mut self.write_queue, out, true);
        let _ = self.flush_write_queue();
    }

    /// Drain prior work and report whether a user payload can be prepared while
    /// staying inside both the byte and allocation-count limits.
    fn can_queue_user_bytes(&mut self, len: usize) -> bool {
        self.flush_write_queue()
            && len <= MAX_PTY_WRITE_QUEUE_BYTES.saturating_sub(self.queued_write_bytes)
            && Self::queue_accepts_entry(&self.write_queue, len, false)
    }

    /// Queue data in-order and make one non-blocking drain attempt. Returns false
    /// if the bounded queue rejected the write or the PTY has failed.
    fn write_pty(&mut self, data: &[u8]) -> bool {
        if data.is_empty() {
            return true;
        }
        if !self.can_queue_user_bytes(data.len()) {
            log::warn!(
                "[PTY] input backpressure for session {} ({} input, {} response, {} incoming)",
                self.id,
                self.queued_write_bytes,
                self.queued_response_bytes,
                data.len()
            );
            return false;
        }
        self.queued_write_bytes += data.len();
        Self::push_queue_copy(&mut self.write_queue, data, false);
        self.flush_write_queue()
    }

    fn flush_write_queue(&mut self) -> bool {
        let mut budget = PTY_WRITE_DRAIN_BUDGET;
        while let Some(front) = self.write_queue.front() {
            if budget == 0 {
                return true;
            }
            let front_len = front.data.len();
            let is_response = front.response;
            let end = (self.write_queue_offset + budget).min(front_len);
            match self.pty.write(&front.data[self.write_queue_offset..end]) {
                Ok(0) => return true,
                Ok(written) => {
                    budget = budget.saturating_sub(written);
                    self.write_queue_offset += written;
                    if is_response {
                        self.queued_response_bytes =
                            self.queued_response_bytes.saturating_sub(written);
                    } else {
                        self.queued_write_bytes = self.queued_write_bytes.saturating_sub(written);
                    }
                    if self.write_queue_offset == front_len {
                        self.write_queue.pop_front();
                        self.write_queue_offset = 0;
                    }
                }
                Err(error) => {
                    log::warn!("[PTY] write failed for session {}: {error}", self.id);
                    self.write_queue.clear();
                    self.write_queue_offset = 0;
                    self.queued_write_bytes = 0;
                    self.queued_response_bytes = 0;
                    return false;
                }
            }
        }
        true
    }

    fn has_pending_write(&self) -> bool {
        self.queued_write_bytes != 0 || self.queued_response_bytes != 0
    }

    /// Working directory of the shell child, used when spawning a sibling.
    fn cwd(&self) -> Option<String> {
        std::fs::read_link(format!("/proc/{}/cwd", self.pty.get_child_pid()))
            .ok()
            .and_then(|p| p.to_str().map(|s| s.to_string()))
    }
}

struct Jterm {
    config: Config,
    theme: Theme,
    metrics: Metrics,
    sessions: Vec<Session>,
    active: usize,
    next_id: usize,
    cols: usize,
    rows: usize,
    focused: bool,
    modifiers: keyboard::Modifiers,
    mono: iced::Font,
    cjk_mono: Option<iced::Font>,
    symbol_mono: Option<iced::Font>,
    math_symbol: Option<iced::Font>,
    nerd_symbol: Option<iced::Font>,
    search: search::SearchState,
    /// PTY output marks active-search results stale; a short timer coalesces
    /// bursts so each chunk does not rescan the entire scrollback.
    search_dirty: bool,
    palette: command_palette::PaletteState,
    keybindings: keybindings::KeyBindings,
    config_panel_open: bool,
    help_open: bool,
    debug_open: bool,
    /// Blink clock phase, toggled by a timer; drives blinking-attribute cells.
    blink_on: bool,
    win_size: Size,
    config_mtime: Option<std::time::SystemTime>,
    /// Font-size changes are live-applied immediately and persisted on the next
    /// config tick so restart restores the latest zoom level.
    config_dirty: bool,
    link_detector: link::LinkDetector,
    links: Vec<link::Link>,
    /// `(stable_session_id, grid_version, scroll_offset)` for cached `links`.
    links_cache_key: Option<(usize, u64, usize)>,
    /// Cached GPU image handles keyed by (stable session id, Kitty image id).
    /// The generation invalidates same-sized retransmissions.
    kitty_handles: std::collections::HashMap<(usize, u32), (iced::advanced::image::Handle, u64)>,
    /// Last persisted session-snapshot JSON, to skip redundant disk writes.
    last_session_save: Option<String>,
    /// Set when session state that feeds the snapshot may have changed (PTY
    /// output can move the cwd, tab switches move the active index). The periodic
    /// save is skipped while this is false, so a fully idle app does no per-tab
    /// `readlink` or JSON serialization on every tick.
    session_dirty: bool,
    /// Diagnostics (F12): wall-clock microseconds spent ingesting the
    /// most recent PTY-output batch (parse + refresh) and its byte count, used
    /// to derive a throughput figure for profiling.
    last_ingest_us: u128,
    last_ingest_bytes: usize,
    /// Current pane layout of the active view.
    split_mode: SplitMode,
    /// Session indices shown as panes (length 1 in `Single`, else 2..=MAX_PANES
    /// along the split axis). Invariant: `panes[focused_pane] == active`.
    panes: Vec<usize>,
    /// Which pane currently has keyboard focus (index into `panes`).
    focused_pane: usize,
    /// Active custom-theme editor overlay, or `None` when closed.
    theme_editor: Option<ThemeEditState>,
    /// File-tree sidebar (left panel) and whether it is currently shown.
    sidebar: sidebar::Sidebar,
    sidebar_open: bool,
    /// Which content the sidebar dock shows (file tree or tab list).
    sidebar_panel: SidebarPanel,
    /// Current dock width in pixels (drag-resizable).
    dock_width: f32,
    /// Whether the sidebar-resize divider is being dragged.
    dragging_sidebar: bool,
    /// Per-pane share of the split axis, aligned with `panes` (sums to ~1.0;
    /// `vec![1.0]` when single). Adjusted by dragging a divider or with the
    /// resize shortcuts.
    pane_ratios: Vec<f32>,
    /// Divider being dragged (between panes `d` and `d + 1`); the layout axis
    /// is implied by `split_mode`.
    dragging_divider: Option<usize>,
    /// Divider under the pointer (drives its hover highlight).
    hovered_divider: Option<usize>,
    /// Last divider press (time + divider index), for double-click detection
    /// (double-click equalizes all panes).
    last_divider_press: Option<(std::time::Instant, usize)>,
    /// Focused pane temporarily expanded to the full terminal area (tmux-style
    /// zoom). Only meaningful while split; cleared when the split collapses.
    pane_zoomed: bool,
    /// Stable id of the tab the pointer is hovering (drives close-button reveal).
    hovered_tab: Option<usize>,
    /// Source-tab id recorded on mouse press over a tab. Cleared on mouse
    /// release (anywhere) by the global mouse-up listener; in between, it
    /// drives tab-drag visual feedback and the reorder-on-release.
    dragging_tab: Option<usize>,
    /// Right-click context menu state: stable id of its target tab, or None.
    /// Rendered as a centered floating panel (Esc / click-outside dismiss).
    tab_menu: Option<usize>,
    /// Transient bottom-right toast queue with absolute expiry timestamps.
    /// Cleared lazily on each render and on ConfigTick.
    toasts: Vec<Toast>,
    /// Tab-switcher overlay (Ctrl+Shift+L): when open, a small fuzzy list of
    /// tab labels lets the user jump by typing. Field holds the typed query
    /// and current selection index.
    tab_switcher: Option<TabSwitcherState>,
    /// Close-confirmation overlay for a tab with a running foreground process.
    /// Holds `(target_id, process_name, activate_after_id)`.
    tab_close_confirm: Option<(usize, String, Option<usize>)>,
    /// Last desktop notification launch. OSC 9/777 originates inside the PTY
    /// (and may be remote over SSH), so process spawning is globally rate-limited.
    last_notification_at: Option<std::time::Instant>,
    /// Sessions whose history needs one width-normalization pass after resize
    /// activity settles, keyed by stable session id.
    history_reflow_sessions: std::collections::HashSet<usize>,
    history_reflow_due: Option<std::time::Instant>,
    /// Held for the process lifetime to enforce single-instance behavior. When
    /// `None`, another instance already holds the lock and this one runs fresh
    /// (no session restore, no snapshot writes) to avoid clobbering its history.
    _instance_lock: Option<std::fs::File>,
    is_first_instance: bool,
}

impl Jterm {
    fn new(config: Config) -> (Self, Task<Message>) {
        let theme = Theme::get_theme(&config.theme).unwrap_or_default();
        let metrics = Metrics::new(config.font_size, config.line_spacing, config.padding);
        let cols = config.cols.max(1);
        let rows = config.rows.max(1);
        let win_size = Size::new(config.initial_width, config.initial_height);
        let config_mtime = Config::config_mtime();

        // Single-instance lock: a second instance starts fresh and never writes
        // the session snapshot, so it cannot clobber the first instance's history.
        let instance_lock = session_persistence::try_acquire_instance_lock();
        let is_first_instance = instance_lock.is_some();
        if !is_first_instance {
            eprintln!("[SessionPersistence] Another instance is running, starting fresh");
        }

        let mono = resolve_mono_font(&config.font_family);
        let cjk_mono = resolve_optional_font(Config::cjk_monospace_font_family());
        let symbol_mono = resolve_optional_font(Config::symbol_monospace_font_family());
        let math_symbol = resolve_optional_font(Config::math_symbol_font_family());
        let nerd_symbol = resolve_optional_font(Config::nerd_symbol_font_family());

        // Restore prior tabs (their cwds + active index) when enabled and we are
        // the first instance; otherwise start with a single default session.
        let (sessions, active, next_id, saved_split) =
            Self::restore_or_spawn(&config, cols, rows, is_first_instance);

        // In Side mode the dock hosts the tab list and starts open (there is no
        // top bar to show tabs otherwise); in Top mode it starts collapsed.
        let side_tabs = config.tab_position == config::TabPosition::Side;
        let sidebar_panel = if side_tabs {
            SidebarPanel::Tabs
        } else {
            SidebarPanel::Files
        };
        let sidebar_open = side_tabs;

        let mut app = Jterm {
            config,
            theme,
            metrics,
            sessions,
            active,
            next_id,
            cols,
            rows,
            focused: true,
            modifiers: keyboard::Modifiers::default(),
            mono,
            cjk_mono,
            symbol_mono,
            math_symbol,
            nerd_symbol,
            search: search::SearchState::new(),
            search_dirty: false,
            palette: command_palette::PaletteState::new(),
            keybindings: load_keybindings(),
            config_panel_open: false,
            help_open: false,
            debug_open: false,
            blink_on: true,
            win_size,
            config_mtime,
            config_dirty: false,
            link_detector: link::LinkDetector::new(link::LinkDetectionConfig::default()),
            links: Vec::new(),
            links_cache_key: None,
            kitty_handles: std::collections::HashMap::new(),
            last_session_save: None,
            session_dirty: true,
            last_ingest_us: 0,
            last_ingest_bytes: 0,
            split_mode: SplitMode::Single,
            panes: vec![active],
            focused_pane: 0,
            theme_editor: None,
            sidebar: sidebar::Sidebar::new(),
            sidebar_open,
            sidebar_panel,
            dock_width: SIDEBAR_W,
            dragging_sidebar: false,
            pane_ratios: vec![1.0],
            dragging_divider: None,
            hovered_divider: None,
            last_divider_press: None,
            pane_zoomed: false,
            hovered_tab: None,
            dragging_tab: None,
            tab_menu: None,
            toasts: Vec::new(),
            tab_switcher: None,
            tab_close_confirm: None,
            last_notification_at: None,
            history_reflow_sessions: std::collections::HashSet::new(),
            history_reflow_due: None,
            _instance_lock: instance_lock,
            is_first_instance,
        };
        // Re-apply a saved split layout once the sessions exist. The snapshot is
        // external input, so every index is validated before use.
        if let Some(split) = saved_split {
            let mode = match split.mode.as_str() {
                "vertical" => Some(SplitMode::Vertical),
                "horizontal" => Some(SplitMode::Horizontal),
                _ => None,
            };
            if let Some(mode) = mode {
                let n = split.panes.len();
                let distinct = split
                    .panes
                    .iter()
                    .collect::<std::collections::HashSet<_>>()
                    .len()
                    == n;
                let valid = (2..=MAX_PANES).contains(&n)
                    && distinct
                    && split.panes.iter().all(|&p| p < app.sessions.len())
                    && split.focused < n;
                if valid {
                    // Saved shares are used when they still line up and are
                    // sane; anything odd falls back to an even split.
                    let sum: f32 = split.ratios.iter().sum();
                    let mut ratios = split.ratios;
                    let usable = ratios.len() == n
                        && ratios.iter().all(|r| r.is_finite() && *r > 0.0)
                        && sum > f32::EPSILON;
                    if usable {
                        for r in ratios.iter_mut() {
                            *r /= sum;
                        }
                    } else {
                        ratios = vec![1.0 / n as f32; n];
                    }
                    app.split_mode = mode;
                    app.pane_ratios = ratios;
                    app.panes = split.panes;
                    app.focused_pane = split.focused;
                    app.active = app.panes[app.focused_pane];
                    app.relayout();
                }
            }
        }
        (app, Task::none())
    }

    fn title(&self) -> String {
        self.sessions
            .get(self.active)
            .map(|s| s.label())
            .unwrap_or_else(|| "jterm3".to_string())
    }

    fn iced_theme(&self) -> iced::Theme {
        iced::Theme::custom(
            "jterm3".to_string(),
            iced::theme::Palette {
                background: self.theme.terminal_background(),
                text: self.theme.terminal_foreground(),
                primary: self.theme.cursor_color(),
                success: self.theme.ansi_color(2),
                warning: self.theme.ansi_color(3),
                danger: self.theme.ansi_color(1),
            },
        )
    }

    /// Single re-apply path for live config changes (Set*, Reset, hot reload):
    /// re-resolve the theme, rebuild metrics, and regrid every session.
    fn apply_config(&mut self) {
        self.theme = Theme::get_theme(&self.config.theme).unwrap_or_default();
        self.mono = resolve_mono_font(&self.config.font_family);
        self.cjk_mono = resolve_optional_font(Config::cjk_monospace_font_family());
        self.symbol_mono = resolve_optional_font(Config::symbol_monospace_font_family());
        self.math_symbol = resolve_optional_font(Config::math_symbol_font_family());
        self.nerd_symbol = resolve_optional_font(Config::nerd_symbol_font_family());
        self.metrics = Metrics::new(
            self.config.font_size,
            self.config.line_spacing,
            self.config.padding,
        );
        let term_h = self.term_height();
        let term_w = (self.term_width() - terminal_view::SCROLLBAR_WIDTH).max(0.0);
        let (cols, rows) = self.metrics.grid_size(term_w, term_h);
        let resized = cols != self.cols || rows != self.rows;
        if resized {
            self.cols = cols;
            self.rows = rows;
        }
        for sess in &mut self.sessions {
            sess.terminal
                .set_max_scrollback(self.config.scrollback_lines);
            sess.terminal
                .set_disable_alt_screen(self.config.disable_alt_screen);
        }
        self.relayout();
        if resized {
            self.refresh_active_context();
        }
    }

    fn sync_tab_position_ui(&mut self) {
        match self.config.tab_position {
            config::TabPosition::Side => {
                self.sidebar_open = true;
                self.sidebar_panel = SidebarPanel::Tabs;
            }
            config::TabPosition::Top => {
                self.sidebar_open = false;
                self.sidebar_panel = SidebarPanel::Files;
            }
        }
    }

    fn adjust_font_size(&mut self, delta: f32) {
        let next = Config::clamp_font_size(self.config.font_size + delta);
        if (next - self.config.font_size).abs() < f32::EPSILON {
            return;
        }
        self.config.font_size = next;
        self.config_dirty = true;
        self.apply_config();
    }

    fn reset_font_size(&mut self) {
        let next = Config::clamp_font_size(14.0);
        if (next - self.config.font_size).abs() < f32::EPSILON {
            return;
        }
        self.config.font_size = next;
        self.config_dirty = true;
        self.apply_config();
    }

    fn persist_live_config(&mut self) {
        if !self.config_dirty {
            return;
        }
        match self.config.save() {
            Ok(()) => {
                self.config_mtime = Config::config_mtime();
                self.config_dirty = false;
            }
            Err(e) => {
                eprintln!("[Config] Live save failed: {}", e);
            }
        }
    }

    /// Whether the left dock is shown. Follows the manual `sidebar_open` toggle
    /// in both tab-position modes, so the dock can always be collapsed.
    fn dock_open(&self) -> bool {
        self.sidebar_open
    }

    /// Whether the terminal itself owns text/IME input. Every overlay with an
    /// editable field or modal action takes ownership until it closes.
    fn terminal_input_active(&self) -> bool {
        !self.search.is_open
            && !self.palette.is_open
            && !self.config_panel_open
            && !self.help_open
            && !self.debug_open
            && self.tab_menu.is_none()
            && self.tab_switcher.is_none()
            && self.tab_close_confirm.is_none()
    }

    /// Search is intentionally non-modal for scrolling/selection. The remaining
    /// overlays block pointer actions from reaching panes underneath them.
    fn terminal_mouse_active(&self) -> bool {
        !self.palette.is_open
            && !self.config_panel_open
            && !self.help_open
            && !self.debug_open
            && self.tab_menu.is_none()
            && self.tab_switcher.is_none()
            && self.tab_close_confirm.is_none()
    }

    /// Toggle the left dock and refresh its file root when it becomes visible.
    /// Keeping this in one place makes the toolbar, shortcut, and command
    /// palette behave identically.
    fn toggle_sidebar(&mut self) {
        self.sidebar_open = !self.sidebar_open;
        if self.sidebar_open {
            if let Some(cwd) = self
                .sessions
                .get(self.active)
                .and_then(|s| s.cwd_cache.clone().or_else(|| s.cwd()))
            {
                self.sidebar.set_current_dir(std::path::PathBuf::from(cwd));
            }
        }
        self.apply_config();
    }

    /// Terminal area height: window minus the tab bar and status bar. The top bar
    /// is always reserved (even in side-tab mode, where it hosts the dock toggle)
    /// so floating chrome never overlaps terminal content.
    fn term_height(&self) -> f32 {
        (self.win_size.height - TAB_BAR_H - STATUS_BAR_H).max(0.0)
    }

    /// Terminal area width: window minus the sidebar (when shown).
    fn term_width(&self) -> f32 {
        (self.win_size.width - self.sidebar_width()).max(0.0)
    }

    /// Current sidebar width (0 when hidden), including the resize divider.
    fn sidebar_width(&self) -> f32 {
        if self.dock_open() {
            self.dock_width + DIVIDER
        } else {
            0.0
        }
    }

    fn session_by_identity(&mut self, id: usize, fd: RawFd) -> Option<&mut Session> {
        self.sessions
            .iter_mut()
            .find(|session| session.id == id && session.master_fd == fd)
    }

    /// Refresh every cache/state object whose coordinates belong to the active
    /// session. All tab/pane activation paths call this before accepting input.
    fn refresh_active_context(&mut self) {
        self.links_cache_key = None;
        if self.search.is_open {
            let reflow_pending = self
                .sessions
                .get(self.active)
                .is_some_and(|session| self.history_reflow_sessions.contains(&session.id));
            if reflow_pending {
                self.search_dirty = true;
            } else {
                self.recompute_search();
                self.reveal_current_search_match();
            }
        }
        self.recompute_links();
        self.refresh_kitty_handles();
    }

    /// Startup session setup: when `restore_session` is enabled and a snapshot
    /// exists, respawn one session per saved tab at its recorded cwd; otherwise
    /// (or on any failure) fall back to a single default session. The fourth
    /// element is the saved split layout (validated and applied by the caller).
    fn restore_or_spawn(
        config: &Config,
        cols: usize,
        rows: usize,
        is_first_instance: bool,
    ) -> (
        Vec<Session>,
        usize,
        usize,
        Option<session_persistence::SplitSnapshot>,
    ) {
        let default = |id_start: usize| {
            let s =
                Session::spawn(config, id_start, cols, rows, None).expect("failed to spawn PTY");
            (vec![s], 0usize, id_start + 1, None)
        };
        if !config.restore_session || !is_first_instance {
            return default(0);
        }
        let Ok(path) = config.session_history_path() else {
            return default(0);
        };
        let snapshot = match session_persistence::SessionsSnapshot::load(&path) {
            Ok(s) if !s.sessions.is_empty() => s,
            _ => return default(0),
        };
        let mut sessions = Vec::new();
        let mut next_id = 0usize;
        if snapshot.sessions.len() > MAX_RESTORED_SESSIONS {
            log::warn!(
                "[SessionPersistence] Snapshot has {} sessions; restoring only the first {}",
                snapshot.sessions.len(),
                MAX_RESTORED_SESSIONS
            );
        }
        for snap in snapshot.sessions.iter().take(MAX_RESTORED_SESSIONS) {
            if let Some(sess) = Session::spawn(config, next_id, cols, rows, snap.cwd.as_deref()) {
                sessions.push(sess);
                next_id += 1;
            }
        }
        if sessions.is_empty() {
            return default(0);
        }
        let active = snapshot.active_index.unwrap_or(0).min(sessions.len() - 1);
        eprintln!(
            "[SessionPersistence] Restored {} session(s) from {}",
            sessions.len(),
            path.display()
        );
        (sessions, active, next_id, snapshot.split)
    }

    /// Persist the current tabs (live cwd of each + active index) when enabled.
    /// De-duplicated against the last write to avoid redundant disk churn.
    fn save_session_snapshot(&mut self) {
        // Reconciling current state now; clear the dirty flag so an idle app does
        // not re-walk every tab's cwd on each periodic tick.
        self.session_dirty = false;
        if !self.config.restore_session || !self.is_first_instance {
            return;
        }
        let snaps: Vec<session_persistence::SessionSnapshot> = self
            .sessions
            .iter()
            .map(|s| session_persistence::SessionSnapshot { cwd: s.cwd() })
            .collect();
        // Persist the split layout so a restart restores the same pane view.
        let split = (self.split_mode != SplitMode::Single && self.panes.len() >= 2).then(|| {
            session_persistence::SplitSnapshot {
                mode: match self.split_mode {
                    SplitMode::Horizontal => "horizontal".to_string(),
                    _ => "vertical".to_string(),
                },
                ratios: self.pane_ratios.clone(),
                panes: self.panes.clone(),
                focused: self.focused_pane,
            }
        });
        let snapshot = session_persistence::SessionsSnapshot::new(snaps, Some(self.active), split);
        let Some(json) = snapshot.to_json() else {
            return;
        };
        if self.last_session_save.as_deref() == Some(json.as_str()) {
            return;
        }
        if let Ok(path) = self.config.session_history_path() {
            if snapshot.save(&path).is_ok() {
                self.last_session_save = Some(json);
            }
        }
    }

    fn new_session(&mut self) {
        let cwd = self.sessions.get(self.active).and_then(|s| s.cwd());
        if let Some(sess) = Session::spawn(
            &self.config,
            self.next_id,
            self.cols,
            self.rows,
            cwd.as_deref(),
        ) {
            self.next_id += 1;
            let insert = (self.active + 1).min(self.sessions.len());
            self.sessions.insert(insert, sess);
            self.active = insert;
            self.unsplit();
            self.refresh_active_context();
            self.save_session_snapshot();
        }
    }

    fn close_session(&mut self, index: usize) -> Task<Message> {
        if index >= self.sessions.len() {
            return Task::none();
        }
        // Closing the last session quits the app.
        if self.sessions.len() == 1 {
            self.save_session_snapshot();
            let _ = self.sessions[0].pty.terminate();
            return iced::exit();
        }
        let mut sess = self.sessions.remove(index);
        let closed_id = sess.id;
        self.history_reflow_sessions.remove(&closed_id);
        if self.hovered_tab == Some(closed_id) {
            self.hovered_tab = None;
        }
        if self.dragging_tab == Some(closed_id) {
            self.dragging_tab = None;
        }
        if self.tab_menu == Some(closed_id) {
            self.tab_menu = None;
        }
        let _ = sess.pty.terminate();
        if self.active >= self.sessions.len() {
            self.active = self.sessions.len() - 1;
        } else if index < self.active {
            self.active -= 1;
        }
        self.prune_closed_pane(index);
        self.refresh_active_context();
        self.save_session_snapshot();
        Task::none()
    }

    /// Reconcile the split after `sessions[index]` was removed: drop its pane
    /// (folding the freed share into a neighbor) and shift the remaining pane
    /// indices. The split survives while two panes remain; closing a session
    /// that is not on screen leaves the layout untouched.
    fn prune_closed_pane(&mut self, index: usize) {
        if self.split_mode == SplitMode::Single {
            self.panes = vec![self.active];
            self.pane_ratios = vec![1.0];
            self.focused_pane = 0;
            return;
        }
        if let Some(pos) = self.panes.iter().position(|&p| p == index) {
            self.panes.remove(pos);
            remove_pane_share(&mut self.pane_ratios, pos);
            if self.focused_pane > pos {
                self.focused_pane -= 1;
            } else if self.focused_pane == pos {
                // Focus follows the freed space into the preceding pane.
                self.focused_pane = pos.saturating_sub(1);
            }
        }
        for p in self.panes.iter_mut() {
            if *p > index {
                *p -= 1;
            }
        }
        if self.panes.len() < 2 {
            // The last surviving pane becomes the plain single view.
            if let Some(&survivor) = self.panes.first() {
                self.active = survivor;
            }
            self.split_mode = SplitMode::Single;
            self.panes = vec![self.active];
            self.pane_ratios = vec![1.0];
            self.focused_pane = 0;
            self.pane_zoomed = false;
            self.hovered_divider = None;
            self.dragging_divider = None;
        } else {
            self.focused_pane = self.focused_pane.min(self.panes.len() - 1);
            self.active = self.panes[self.focused_pane];
        }
        self.relayout();
    }

    fn busy_session_name(&self, index: usize) -> Option<String> {
        self.sessions
            .get(index)
            .and_then(|session| session.fg_proc_cache.clone().or_else(|| session.fg_proc()))
    }

    /// Public entry point for close requests originating from user actions.
    /// Pops a confirmation overlay when the target tab is running a non-shell
    /// foreground process; otherwise closes immediately. Batch close operations
    /// preflight every affected session before reaching the force-close helper.
    fn request_close_session(&mut self, index: usize) -> Task<Message> {
        self.request_close_session_then(index, None)
    }

    fn request_close_session_then(
        &mut self,
        index: usize,
        activate_after: Option<usize>,
    ) -> Task<Message> {
        let busy = self.busy_session_name(index);
        if let Some(name) = busy {
            if let Some(session) = self.sessions.get(index) {
                self.tab_close_confirm = Some((session.id, name, activate_after));
            }
            return Task::none();
        }
        self.close_session_then(index, activate_after)
    }

    fn close_session_then(&mut self, index: usize, activate_after: Option<usize>) -> Task<Message> {
        let task = self.close_session(index);
        if let Some(id) = activate_after {
            if let Some(remaining) = self.sessions.iter().position(|session| session.id == id) {
                if let Some(pos) = self.panes.iter().position(|&p| p == remaining) {
                    // Target is still on screen: focus its pane, keep the split.
                    self.focused_pane = pos;
                    self.active = remaining;
                } else {
                    self.active = remaining;
                    self.unsplit();
                }
                self.refresh_active_context();
                self.save_session_snapshot();
            }
        }
        task
    }

    /// Refuse a whole-window exit while a foreground job is still attached.
    /// The user can inspect and close that tab explicitly, which uses the normal
    /// per-process confirmation flow instead of silently terminating work.
    fn request_window_close(&mut self) -> Task<Message> {
        if let Some((index, process)) = (0..self.sessions.len())
            .find_map(|index| self.busy_session_name(index).map(|name| (index, name)))
        {
            self.active = index;
            self.unsplit();
            self.refresh_active_context();
            self.push_toast(
                format!("{process} is still running — close its tab first"),
                ToastKind::Warning,
            );
            return Task::none();
        }
        self.save_session_snapshot();
        iced::exit()
    }

    fn next_session(&mut self) {
        if !self.sessions.is_empty() {
            self.active = (self.active + 1) % self.sessions.len();
            self.session_dirty = true;
            self.unsplit();
            self.refresh_active_context();
        }
    }

    fn prev_session(&mut self) {
        if !self.sessions.is_empty() {
            self.active = (self.active + self.sessions.len() - 1) % self.sessions.len();
            self.session_dirty = true;
            self.unsplit();
            self.refresh_active_context();
        }
    }

    fn jump_session(&mut self, index: usize) {
        if index < self.sessions.len() {
            self.active = index;
            self.session_dirty = true;
            self.unsplit();
            self.refresh_active_context();
        }
    }

    /// Push a transient bottom-right toast. Auto-expires; dismissable.
    fn push_toast(&mut self, text: impl Into<String>, kind: ToastKind) {
        const TOAST_TTL_MS: u64 = 2400;
        const MAX_TOASTS: usize = 4;
        self.toasts.push(Toast {
            text: text.into(),
            kind,
            expires_at: std::time::Instant::now() + std::time::Duration::from_millis(TOAST_TTL_MS),
        });
        // Drop oldest if we exceed cap so the stack never grows past MAX_TOASTS.
        if self.toasts.len() > MAX_TOASTS {
            let drop = self.toasts.len() - MAX_TOASTS;
            self.toasts.drain(0..drop);
        }
    }

    /// Drop expired toasts. Cheap; called from the periodic tick.
    fn expire_toasts(&mut self) {
        let now = std::time::Instant::now();
        self.toasts.retain(|t| t.expires_at > now);
    }

    /// Apply a tab context-menu action. Close/CloseOthers/CloseToRight close
    /// the matching sessions (terminating their PTYs); Duplicate clones the
    /// target's cwd into a new tab adjacent to it.
    fn execute_tab_menu_action(&mut self, action: TabMenuAction) -> Task<Message> {
        match action {
            TabMenuAction::Close(id) => {
                let Some(index) = self.sessions.iter().position(|session| session.id == id) else {
                    return Task::none();
                };
                self.request_close_session(index)
            }
            TabMenuAction::CloseOthers(keep_id) => {
                let Some(keep) = self
                    .sessions
                    .iter()
                    .position(|session| session.id == keep_id)
                else {
                    return Task::none();
                };
                if let Some((index, process)) = (0..self.sessions.len())
                    .filter(|&index| index != keep)
                    .find_map(|index| self.busy_session_name(index).map(|name| (index, name)))
                {
                    self.active = index;
                    self.unsplit();
                    self.refresh_active_context();
                    self.push_toast(
                        format!("{process} is still running — close that tab explicitly"),
                        ToastKind::Warning,
                    );
                    return Task::none();
                }
                // Close from the back so indices stay valid; skip `keep`.
                let mut tasks: Vec<Task<Message>> = Vec::new();
                let mut i = self.sessions.len();
                while i > 0 {
                    i -= 1;
                    if i != keep {
                        tasks.push(self.close_session(i));
                    }
                }
                self.push_toast("Closed other tabs", ToastKind::Info);
                Task::batch(tasks)
            }
            TabMenuAction::CloseToRight(anchor_id) => {
                let Some(anchor) = self
                    .sessions
                    .iter()
                    .position(|session| session.id == anchor_id)
                else {
                    return Task::none();
                };
                if let Some((index, process)) = ((anchor + 1)..self.sessions.len())
                    .find_map(|index| self.busy_session_name(index).map(|name| (index, name)))
                {
                    self.active = index;
                    self.unsplit();
                    self.refresh_active_context();
                    self.push_toast(
                        format!("{process} is still running — close that tab explicitly"),
                        ToastKind::Warning,
                    );
                    return Task::none();
                }
                let mut tasks: Vec<Task<Message>> = Vec::new();
                while self.sessions.len() > anchor + 1 {
                    let last = self.sessions.len() - 1;
                    tasks.push(self.close_session(last));
                }
                self.push_toast("Closed tabs to the right", ToastKind::Info);
                Task::batch(tasks)
            }
            TabMenuAction::Duplicate(id) => {
                let Some(i) = self.sessions.iter().position(|session| session.id == id) else {
                    return Task::none();
                };
                let cwd = self
                    .sessions
                    .get(i)
                    .and_then(|s| s.cwd_cache.clone().or_else(|| s.cwd()));
                if let Some(sess) = Session::spawn(
                    &self.config,
                    self.next_id,
                    self.cols,
                    self.rows,
                    cwd.as_deref(),
                ) {
                    self.next_id += 1;
                    let insert = (i + 1).min(self.sessions.len());
                    self.sessions.insert(insert, sess);
                    self.active = insert;
                    self.unsplit();
                    self.refresh_active_context();
                    self.save_session_snapshot();
                    self.push_toast("Duplicated tab", ToastKind::Success);
                }
                Task::none()
            }
        }
    }

    /// Move `sessions[from]` to position `to`, shifting items between them.
    /// `active` and any indices in `panes` are rewritten so the same tab stays
    /// selected before/after the reorder.
    fn reorder_session(&mut self, from: usize, to: usize) {
        if from >= self.sessions.len() || to >= self.sessions.len() || from == to {
            return;
        }
        let sess = self.sessions.remove(from);
        self.sessions.insert(to, sess);
        let remap = |idx: usize| -> usize {
            if idx == from {
                to
            } else if from < idx && to >= idx {
                idx - 1
            } else if from > idx && to <= idx {
                idx + 1
            } else {
                idx
            }
        };
        self.active = remap(self.active);
        for p in self.panes.iter_mut() {
            *p = remap(*p);
        }
        self.session_dirty = true;
        self.refresh_active_context();
        self.save_session_snapshot();
    }

    /// Per-pane (cols, rows) for the current split mode and window size.
    fn pane_grid(&self, pane_pos: usize) -> (usize, usize) {
        let term_h = self.term_height();
        let term_w = self.term_width();
        let n = self.panes.len().max(1);
        // Fraction of the available space this pane occupies.
        let frac = self
            .pane_ratios
            .get(pane_pos)
            .copied()
            .unwrap_or(1.0 / n as f32);
        let dividers = DIVIDER * n.saturating_sub(1) as f32;
        match self.split_mode {
            SplitMode::Single => (self.cols, self.rows),
            SplitMode::Vertical => {
                let pane_w = ((term_w - dividers) * frac).max(0.0);
                self.metrics
                    .grid_size((pane_w - terminal_view::SCROLLBAR_WIDTH).max(0.0), term_h)
            }
            SplitMode::Horizontal => {
                let pane_h = ((term_h - dividers) * frac).max(0.0);
                self.metrics
                    .grid_size((term_w - terminal_view::SCROLLBAR_WIDTH).max(0.0), pane_h)
            }
        }
    }

    fn grid_pixel_size(&self, cols: usize, rows: usize) -> (u32, u32) {
        let width = (cols as f32 * self.metrics.cell_w).round().max(0.0) as u32;
        let height = (rows as f32 * self.metrics.cell_h).round().max(0.0) as u32;
        (width, height)
    }

    /// Resize one session's terminal + PTY (no-op when already that size).
    fn resize_session(&mut self, index: usize, cols: usize, rows: usize) -> Option<usize> {
        let (pixel_w, pixel_h) = self.grid_pixel_size(cols, rows);
        if let Some(sess) = self.sessions.get_mut(index) {
            sess.terminal.set_viewport_pixel_size(pixel_w, pixel_h);
            let old_dimensions = sess.terminal.get_dimensions();
            if old_dimensions != (cols, rows) {
                sess.terminal.on_resize(cols, rows);
                let _ = sess.pty.resize(cols, rows);
            }
            sess.refresh();
            return (old_dimensions.0 != cols).then_some(sess.id);
        }
        None
    }

    /// Resize every session once for the current layout. Background tabs use the
    /// full terminal area; sessions displayed in a split use their pane size.
    /// While a pane is zoomed every session gets the full area, so the zoomed
    /// pane renders full-size and unzooming is a plain relayout.
    fn relayout(&mut self) {
        let mut targets = vec![(self.cols, self.rows); self.sessions.len()];
        if self.split_mode != SplitMode::Single && !self.pane_zoomed {
            for (position, index) in self.panes.iter().copied().enumerate() {
                if index < targets.len() {
                    targets[index] = self.pane_grid(position);
                }
            }
        }
        let mut width_changed = false;
        for (index, (cols, rows)) in targets.into_iter().enumerate() {
            if let Some(id) = self.resize_session(index, cols, rows) {
                self.history_reflow_sessions.insert(id);
                width_changed = true;
            }
        }
        if width_changed {
            self.history_reflow_due =
                Some(std::time::Instant::now() + std::time::Duration::from_millis(150));
        }
    }

    /// Collapse back to a single pane showing the active session.
    fn unsplit(&mut self) {
        self.pane_zoomed = false;
        self.hovered_divider = None;
        self.dragging_divider = None;
        if self.split_mode == SplitMode::Single {
            self.panes = vec![self.active];
            self.pane_ratios = vec![1.0];
            self.focused_pane = 0;
            return;
        }
        self.split_mode = SplitMode::Single;
        self.focused_pane = 0;
        self.panes = vec![self.active];
        self.pane_ratios = vec![1.0];
        self.relayout();
    }

    /// Add a pane: spawn a fresh session at the focused pane's cwd and insert
    /// it beside that pane, halving its share. When already split the other
    /// orientation re-flows the existing panes along the new axis instead.
    /// Panes are closed individually (`close_focused_pane`), capped at
    /// [`MAX_PANES`].
    fn split(&mut self, mode: SplitMode) {
        if self.split_mode != SplitMode::Single && self.split_mode != mode {
            // Keep every session; just rotate the layout axis.
            self.split_mode = mode;
            self.relayout();
            self.refresh_active_context();
            self.save_session_snapshot();
            return;
        }
        if self.panes.len() >= MAX_PANES {
            self.push_toast(
                format!("Split limit reached ({MAX_PANES} panes)"),
                ToastKind::Warning,
            );
            return;
        }
        let cwd = self.sessions.get(self.active).and_then(|s| s.cwd());
        if let Some(sess) = Session::spawn(
            &self.config,
            self.next_id,
            self.cols,
            self.rows,
            cwd.as_deref(),
        ) {
            self.next_id += 1;
            self.sessions.push(sess);
            let new_idx = self.sessions.len() - 1;
            if self.split_mode == SplitMode::Single {
                self.split_mode = mode;
                self.panes = vec![self.active, new_idx];
                self.pane_ratios = vec![0.5, 0.5];
                self.focused_pane = 1;
            } else {
                insert_pane_share(&mut self.pane_ratios, self.focused_pane);
                self.panes.insert(self.focused_pane + 1, new_idx);
                self.focused_pane += 1;
            }
            self.active = new_idx;
            // Splitting while zoomed lands in the new multi-pane layout.
            self.pane_zoomed = false;
            self.relayout();
            self.refresh_active_context();
            self.save_session_snapshot();
        }
    }

    /// Move keyboard focus to the next pane (wraps). No-op when not split.
    fn focus_next_pane(&mut self) {
        if self.split_mode == SplitMode::Single || self.panes.len() < 2 {
            return;
        }
        self.focused_pane = (self.focused_pane + 1) % self.panes.len();
        self.active = self.panes[self.focused_pane];
        self.refresh_active_context();
    }

    /// Move keyboard focus to the previous pane (wraps). No-op when not split.
    fn focus_prev_pane(&mut self) {
        if self.split_mode == SplitMode::Single || self.panes.len() < 2 {
            return;
        }
        self.focused_pane = (self.focused_pane + self.panes.len() - 1) % self.panes.len();
        self.active = self.panes[self.focused_pane];
        self.refresh_active_context();
    }

    /// Display `sessions[index]` in the focused pane (tab switcher while
    /// split). A session already visible in another pane gets focused there
    /// instead of appearing twice; the swapped-in/out sessions are re-sized for
    /// their new homes.
    fn show_session_in_focused_pane(&mut self, index: usize) {
        if let Some(pos) = self.panes.iter().position(|&p| p == index) {
            self.focused_pane = pos;
        } else {
            self.panes[self.focused_pane] = index;
        }
        self.active = index;
        self.session_dirty = true;
        self.relayout();
        self.refresh_active_context();
    }

    fn focus_pane_direction(&mut self, direction: PaneDirection) {
        let Some(target) = directional_pane_target(
            self.split_mode,
            self.focused_pane,
            self.panes.len(),
            direction,
        ) else {
            return;
        };
        self.focused_pane = target;
        self.active = self.panes[target];
        self.refresh_active_context();
    }

    fn resize_pane_direction(&mut self, direction: PaneDirection) {
        let Some(divider) = resize_divider_target(
            self.split_mode,
            self.focused_pane,
            self.panes.len(),
            direction,
        ) else {
            return;
        };
        // The arrow drags the divider in its own direction: forward arrows grow
        // the pane before the divider, backward arrows shrink it.
        let forward = direction_along_axis(self.split_mode, direction).unwrap_or(true);
        let step = if forward {
            SPLIT_RATIO_KEY_STEP
        } else {
            -SPLIT_RATIO_KEY_STEP
        };
        let first = self.pane_ratios[divider] + step;
        if set_divider_share(&mut self.pane_ratios, divider, first, false) {
            self.relayout();
            self.refresh_active_context();
        }
    }

    /// Toggle tmux-style zoom: the focused pane temporarily takes the whole
    /// terminal area without destroying the split. No-op when not split.
    fn toggle_pane_zoom(&mut self) {
        if self.split_mode == SplitMode::Single {
            return;
        }
        self.pane_zoomed = !self.pane_zoomed;
        self.relayout();
        self.refresh_active_context();
    }

    /// Exchange the focused pane with the next one (wrapping); geometry stays
    /// put and focus follows the moved session, tmux-style.
    fn swap_panes(&mut self) {
        if self.split_mode == SplitMode::Single || self.panes.len() < 2 {
            return;
        }
        let other = (self.focused_pane + 1) % self.panes.len();
        self.panes.swap(self.focused_pane, other);
        self.focused_pane = other;
        self.relayout();
        self.refresh_active_context();
        self.save_session_snapshot();
    }

    /// Close the focused pane's session; the remaining panes keep the split
    /// (which collapses on its own once only one pane is left).
    fn close_focused_pane(&mut self) -> Task<Message> {
        if self.split_mode == SplitMode::Single {
            return self.request_close_session(self.active);
        }
        let victim = self.panes[self.focused_pane];
        // Focus lands on the preceding pane (or the new first when closing the
        // first pane), matching where the freed space goes.
        let keep_pos = self.focused_pane.saturating_sub(1);
        let keep = self
            .panes
            .iter()
            .copied()
            .filter(|&p| p != victim)
            .nth(keep_pos);
        let keep_id = keep.and_then(|idx| self.sessions.get(idx).map(|session| session.id));
        self.request_close_session_then(victim, keep_id)
    }

    /// Look up a key event in the configurable keybindings and run the bound
    /// command. Returns the resulting task when a binding matched and applied,
    /// or `None` to let the key fall through to other handlers / the PTY.
    fn handle_keybinding(
        &mut self,
        key: &keyboard::Key,
        mods: keyboard::Modifiers,
    ) -> Option<Task<Message>> {
        let binding = key_to_binding_string(key, mods)?;
        let cmd = self.keybindings.get_command(&binding)?;
        self.dispatch_command(cmd)
    }

    /// Execute a bound [`keybindings::Command`]. Returns `None` for commands
    /// that don't apply in the current context (e.g. search navigation while
    /// the search bar is closed) so the key can fall through.
    fn dispatch_command(&mut self, cmd: keybindings::Command) -> Option<Task<Message>> {
        use keybindings::Command as C;
        // Write raw bytes to the focused session's PTY (control-key commands).
        let mut send = |bytes: &[u8]| {
            if let Some(sess) = self.sessions.get_mut(self.active) {
                sess.terminal.scroll_to_bottom();
                sess.write_pty(bytes);
                sess.refresh();
            }
        };
        let task = match cmd {
            C::SessionNew => {
                self.new_session();
                Task::none()
            }
            C::SessionClose => return Some(self.request_close_session(self.active)),
            C::WindowClose => return Some(self.request_window_close()),
            C::SessionNext => {
                self.next_session();
                Task::none()
            }
            C::SessionPrev => {
                self.prev_session();
                Task::none()
            }
            C::SessionJump(n) => {
                self.jump_session(n);
                Task::none()
            }
            C::SessionLast => {
                if let Some(last) = last_session_index(self.sessions.len()) {
                    self.jump_session(last);
                }
                Task::none()
            }
            C::EditCopy => {
                let text = self
                    .sessions
                    .get(self.active)
                    .and_then(|s| s.terminal.copy_selection())
                    .filter(|t| !t.is_empty());
                match text {
                    Some(text) => {
                        let n = text.chars().count();
                        self.push_toast(
                            format!("Copied {} char{}", n, if n == 1 { "" } else { "s" }),
                            ToastKind::Success,
                        );
                        iced::clipboard::write(text)
                    }
                    None => Task::none(),
                }
            }
            C::EditPaste => {
                let id = self.sessions.get(self.active)?.id;
                iced::clipboard::read().map(move |text| Message::Pasted(id, text))
            }
            C::SearchOpen => {
                self.search.toggle();
                self.recompute_search();
                self.reveal_current_search_match();
                if self.search.is_open {
                    iced::widget::operation::focus(SEARCH_INPUT_ID.clone())
                } else {
                    Task::none()
                }
            }
            C::SearchClose => {
                if !self.search.is_open {
                    return None;
                }
                self.search.close();
                Task::none()
            }
            C::SearchNext => {
                if !self.search.is_open {
                    return None;
                }
                self.search.next_match();
                self.reveal_current_search_match();
                Task::none()
            }
            C::SearchPrev => {
                if !self.search.is_open {
                    return None;
                }
                self.search.prev_match();
                self.reveal_current_search_match();
                Task::none()
            }
            C::SearchHistoryPrev => {
                if !self.search.is_open {
                    return None;
                }
                self.search.history_prev();
                self.search.current_match_index = 0;
                self.recompute_search();
                self.reveal_current_search_match();
                Task::none()
            }
            C::SearchHistoryNext => {
                if !self.search.is_open {
                    return None;
                }
                self.search.history_next();
                self.search.current_match_index = 0;
                self.recompute_search();
                self.reveal_current_search_match();
                Task::none()
            }
            C::TerminalSendSigint => {
                send(&[0x03]);
                Task::none()
            }
            C::TerminalSendEof => {
                send(&[0x04]);
                Task::none()
            }
            C::TerminalClear => {
                send(&[0x0c]);
                Task::none()
            }
            C::TerminalScrollUp | C::TerminalScrollDown => {
                let speed = self.config.scroll_speed.max(1) as isize;
                let delta = if matches!(cmd, C::TerminalScrollUp) {
                    speed
                } else {
                    -speed
                };
                if let Some(sess) = self.sessions.get_mut(self.active) {
                    sess.terminal.scroll(delta);
                    sess.refresh();
                }
                Task::none()
            }
            C::TerminalSplitVertical => {
                self.split(SplitMode::Vertical);
                Task::none()
            }
            C::TerminalSplitHorizontal => {
                self.split(SplitMode::Horizontal);
                Task::none()
            }
            C::TerminalClosePane => return Some(self.close_focused_pane()),
            C::PaneFocusNext => {
                self.focus_next_pane();
                Task::none()
            }
            C::PaneFocusPrev => {
                self.focus_prev_pane();
                Task::none()
            }
            C::PaneFocusLeft => {
                self.focus_pane_direction(PaneDirection::Left);
                Task::none()
            }
            C::PaneFocusRight => {
                self.focus_pane_direction(PaneDirection::Right);
                Task::none()
            }
            C::PaneFocusUp => {
                self.focus_pane_direction(PaneDirection::Up);
                Task::none()
            }
            C::PaneFocusDown => {
                self.focus_pane_direction(PaneDirection::Down);
                Task::none()
            }
            C::PaneResizeLeft => {
                self.resize_pane_direction(PaneDirection::Left);
                Task::none()
            }
            C::PaneResizeRight => {
                self.resize_pane_direction(PaneDirection::Right);
                Task::none()
            }
            C::PaneResizeUp => {
                self.resize_pane_direction(PaneDirection::Up);
                Task::none()
            }
            C::PaneResizeDown => {
                self.resize_pane_direction(PaneDirection::Down);
                Task::none()
            }
            C::PaneZoomToggle => {
                self.toggle_pane_zoom();
                Task::none()
            }
            C::PaneSwap => {
                self.swap_panes();
                Task::none()
            }
            C::ConfigOpen => {
                self.config_panel_open = true;
                Task::none()
            }
            C::ConfigClose => {
                self.config_panel_open = false;
                Task::none()
            }
            C::ConfigToggle => {
                self.config_panel_open = !self.config_panel_open;
                Task::none()
            }
            C::SidebarToggle => {
                self.toggle_sidebar();
                Task::none()
            }
            C::FontZoomIn => {
                self.adjust_font_size(1.0);
                Task::none()
            }
            C::FontZoomOut => {
                self.adjust_font_size(-1.0);
                Task::none()
            }
            C::FontZoomReset => {
                self.reset_font_size();
                Task::none()
            }
        };
        Some(task)
    }

    /// Non-configurable app-chrome shortcuts that have no [`keybindings::Command`]
    /// (command palette, diagnostics, and help overlays). Returns `Some` when the
    /// keypress was consumed.
    fn handle_tab_shortcut(
        &mut self,
        key: &keyboard::Key,
        mods: keyboard::Modifiers,
    ) -> Option<Task<Message>> {
        match chrome_shortcut(key, mods)? {
            ChromeShortcut::CommandPalette => {
                self.palette.toggle();
                Some(if self.palette.is_open {
                    iced::widget::operation::focus(PALETTE_INPUT_ID.clone())
                } else {
                    Task::none()
                })
            }
            ChromeShortcut::Help => {
                self.help_open = !self.help_open;
                Some(Task::none())
            }
            ChromeShortcut::TabSwitcher => {
                if self.tab_switcher.is_some() {
                    self.tab_switcher = None;
                    return Some(Task::none());
                }
                self.tab_switcher = Some(TabSwitcherState::default());
                Some(iced::widget::operation::focus(
                    TAB_SWITCHER_INPUT_ID.clone(),
                ))
            }
            ChromeShortcut::Debug => {
                self.debug_open = !self.debug_open;
                Some(Task::none())
            }
        }
    }

    /// Tab switcher key handling. Mirrors `handle_palette_key`: filters by
    /// typed text, arrows move selection, Enter jumps, Esc closes.
    fn handle_tab_switcher_key(
        &mut self,
        key: &keyboard::Key,
        mods: keyboard::Modifiers,
        text: Option<&str>,
    ) -> Option<Task<Message>> {
        use keyboard::key::Named;
        use keyboard::Key;
        if chrome_shortcut(key, mods) == Some(ChromeShortcut::TabSwitcher) {
            self.tab_switcher = None;
            return Some(Task::none());
        }
        let state = self.tab_switcher.as_mut()?;
        // Recompute the visible order once so Enter/arrows agree with what's drawn.
        let filtered = tab_switcher_filtered(&self.sessions, &state.query);
        match key {
            Key::Named(Named::Escape) => {
                self.tab_switcher = None;
                return Some(Task::none());
            }
            Key::Named(Named::Enter) => {
                let target = filtered.get(state.selected).map(|&(_, i)| i);
                self.tab_switcher = None;
                if let Some(i) = target {
                    if i < self.sessions.len() && i != self.active {
                        self.show_session_in_focused_pane(i);
                    }
                }
                return Some(Task::none());
            }
            Key::Named(Named::ArrowDown) => {
                if !filtered.is_empty() {
                    state.selected = (state.selected + 1) % filtered.len();
                }
                return Some(Task::none());
            }
            Key::Named(Named::ArrowUp) => {
                if !filtered.is_empty() {
                    state.selected = if state.selected == 0 {
                        filtered.len() - 1
                    } else {
                        state.selected - 1
                    };
                }
                return Some(Task::none());
            }
            Key::Named(Named::Backspace) => {
                state.query.pop();
                state.selected = 0;
                return Some(Task::none());
            }
            _ => {}
        }
        if !mods.control() && !mods.alt() {
            if let Some(t) = text {
                let printable: String = t.chars().filter(|c| !c.is_control()).collect();
                if !printable.is_empty() {
                    state.query.push_str(&printable);
                    state.selected = 0;
                    return Some(Task::none());
                }
            }
        }
        // Swallow all other keys while the overlay owns the keyboard.
        Some(Task::none())
    }

    /// Route a grid mouse interaction either to the running application (when it
    /// has enabled mouse reporting and Shift is not held) or to local selection
    /// and scrollback handling.
    fn handle_mouse(&mut self, input: MouseInput) -> Task<Message> {
        let shift = self.modifiers.shift();
        let speed = self.config.scroll_speed.max(1) as isize;
        // Ctrl+Click opens a detected link, taking precedence over selection
        // and app mouse reporting.
        if let MouseInput::Press {
            col,
            row,
            button: MouseButton::Left,
            ..
        } = input
        {
            if self.modifiers.control() {
                if let Some(link) = self
                    .links
                    .iter()
                    .find(|l| l.line == row && col >= l.col_start && col < l.col_end)
                    .cloned()
                {
                    let cwd = self
                        .sessions
                        .get(self.active)
                        .and_then(|session| session.cwd_cache.clone().or_else(|| session.cwd()));
                    if let Err(error) =
                        link::open_link(&link, cwd.as_deref().map(std::path::Path::new))
                    {
                        self.push_toast(
                            format!("Could not open link: {error}"),
                            ToastKind::Warning,
                        );
                    }
                    return Task::none();
                }
            }
        }
        let Some(sess) = self.sessions.get_mut(self.active) else {
            return Task::none();
        };
        let report_to_app = sess.terminal.is_mouse_enabled() && !shift;

        match input {
            MouseInput::Press {
                col,
                row,
                button,
                alt,
                count,
                ..
            } => {
                if report_to_app {
                    if let Some(report) = sess.terminal.get_mouse_report(btn_code(button), col, row)
                    {
                        sess.write_pty(&report);
                    }
                    return Task::none();
                }
                match button {
                    MouseButton::Left => match count {
                        2 => sess.terminal.select_word_at(row, col),
                        n if n >= 3 => {
                            let (cols, _) = sess.terminal.get_dimensions();
                            sess.terminal.start_selection((row, 0));
                            sess.terminal
                                .update_selection((row, cols.saturating_sub(1)));
                        }
                        _ if alt => sess.terminal.start_block_selection((row, col)),
                        _ => sess.terminal.start_selection((row, col)),
                    },
                    MouseButton::Middle => {
                        let id = sess.id;
                        return iced::clipboard::read_primary()
                            .map(move |text| Message::Pasted(id, text));
                    }
                    MouseButton::Right => {}
                }
            }
            MouseInput::Drag { col, row, count } => {
                if report_to_app {
                    if sess.terminal.is_mouse_motion_enabled() {
                        if let Some(report) = sess.terminal.get_mouse_report(32, col, row) {
                            sess.write_pty(&report);
                        }
                    }
                    return Task::none();
                }
                match count {
                    2 => sess.terminal.extend_word_selection_to(row, col),
                    n if n >= 3 => sess.terminal.extend_line_selection_to(row),
                    _ => sess.terminal.update_selection((row, col)),
                }
            }
            MouseInput::Release { col, row, button } => {
                if report_to_app {
                    if let Some(report) =
                        sess.terminal
                            .get_mouse_release_report(btn_code(button), col, row)
                    {
                        sess.write_pty(&report);
                    }
                    return Task::none();
                }
                if button == MouseButton::Left {
                    if let Some(text) = sess.terminal.copy_selection().filter(|t| !t.is_empty()) {
                        return iced::clipboard::write_primary(text);
                    }
                }
            }
            MouseInput::Wheel {
                col,
                row,
                up,
                ctrl,
                lines,
            } => {
                if ctrl {
                    let delta = if up { 1.0 } else { -1.0 } * lines.max(1) as f32;
                    self.adjust_font_size(delta);
                    return Task::none();
                }
                if report_to_app {
                    let code = if up { 64 } else { 65 };
                    // One wheel report per line so apps see the full magnitude.
                    for _ in 0..lines.max(1) {
                        if let Some(report) = sess.terminal.get_mouse_report(code, col, row) {
                            sess.write_pty(&report);
                        }
                    }
                    return Task::none();
                }
                let step = speed * lines.max(1) as isize;
                sess.terminal.scroll(if up { step } else { -step });
                sess.refresh();
            }
            MouseInput::ScrollTo { offset } => {
                sess.terminal.set_scroll_offset(offset);
                sess.refresh();
            }
        }
        Task::none()
    }

    /// Shift+Page/Home/End scrolls the scrollback viewport. Returns true if the
    /// keypress was consumed.
    fn handle_scroll_shortcut(&mut self, key: &keyboard::Key, mods: keyboard::Modifiers) -> bool {
        use keyboard::key::Named;
        use keyboard::Key;
        if !mods.shift() {
            return false;
        }
        let Some(sess) = self.sessions.get_mut(self.active) else {
            return false;
        };
        // Page by the active pane's own row count, not the whole window — when
        // split, a pane is shorter than `self.rows`.
        let page = sess.terminal.grid.rows().saturating_sub(1).max(1) as isize;
        match key {
            Key::Named(Named::PageUp) => sess.terminal.scroll(page),
            Key::Named(Named::PageDown) => sess.terminal.scroll(-page),
            Key::Named(Named::Home) => {
                let len = sess.terminal.scrollback_len();
                sess.terminal.set_scroll_offset(len);
            }
            Key::Named(Named::End) => sess.terminal.scroll_to_bottom(),
            _ => return false,
        }
        sess.refresh();
        true
    }

    /// Re-run the search over the active session's full scrollback + live grid.
    /// Match rows remain absolute, so scrolling does not invalidate them.
    fn recompute_search(&mut self) {
        self.search_dirty = false;
        if !self.search.is_open {
            return;
        }
        let Some(sess) = self.sessions.get(self.active) else {
            self.search.matches.clear();
            return;
        };
        let (matches, error) = search::SearchEngine::search_lines(
            sess.terminal.search_lines(),
            &self.search.query,
            self.search.use_regex,
            self.search.case_sensitive,
            &mut self.search.regex_cache,
        );
        self.search.matches = matches;
        self.search.error_message = error;
        if self.search.matches.is_empty()
            || self.search.current_match_index >= self.search.matches.len()
        {
            self.search.current_match_index = 0;
        }
    }

    /// Reveal the active full-buffer search result and refresh the session's
    /// visible snapshot. Kept separate from recomputation so streaming PTY
    /// output never steals the user's manually chosen scroll position.
    fn reveal_current_search_match(&mut self) {
        let Some(found) = self.search.current_match() else {
            return;
        };
        if let Some(sess) = self.sessions.get_mut(self.active) {
            sess.terminal.reveal_buffer_row(found.line);
            sess.refresh();
        }
        self.links_cache_key = None;
    }

    /// Route a keypress into the search bar while it is open. Returns true if
    /// the key was consumed (and must not reach the PTY).
    fn handle_search_key(
        &mut self,
        key: &keyboard::Key,
        mods: keyboard::Modifiers,
        text: Option<&str>,
    ) -> bool {
        use keyboard::key::Named;
        use keyboard::Key;
        if !self.search.is_open {
            return false;
        }
        if mods.control() && mods.shift() {
            if let Key::Character(c) = key {
                if c.eq_ignore_ascii_case("f") {
                    self.search.close();
                    return true;
                }
            }
        }
        match key {
            Key::Named(Named::Escape) => {
                self.search.close();
                return true;
            }
            Key::Named(Named::Enter) => {
                if mods.shift() {
                    self.search.prev_match();
                } else {
                    self.search.next_match();
                }
                self.reveal_current_search_match();
                return true;
            }
            Key::Named(Named::Backspace) => {
                self.search.query.pop();
                self.search.history_nav_index = None;
                self.recompute_search();
                return true;
            }
            Key::Named(Named::ArrowUp) => {
                self.search.history_prev();
                self.search.current_match_index = 0;
                self.recompute_search();
                self.reveal_current_search_match();
                return true;
            }
            Key::Named(Named::ArrowDown) => {
                self.search.history_next();
                self.search.current_match_index = 0;
                self.recompute_search();
                self.reveal_current_search_match();
                return true;
            }
            // Ctrl+R toggles regex, Ctrl+I toggles case sensitivity (Alt is the
            // JWM window-manager modifier, so it is avoided here).
            Key::Character(c) if mods.control() => {
                match c.chars().next().map(|c| c.to_ascii_lowercase()) {
                    Some('r') => {
                        self.search.toggle_regex();
                        self.recompute_search();
                        self.reveal_current_search_match();
                    }
                    Some('i') => {
                        self.search.toggle_case_sensitive();
                        self.recompute_search();
                        self.reveal_current_search_match();
                    }
                    _ => {}
                }
                return true;
            }
            _ => {}
        }
        // Printable input appends to the query.
        if !mods.control() && !mods.alt() {
            if let Some(t) = text {
                let printable: String = t.chars().filter(|c| !c.is_control()).collect();
                if !printable.is_empty() {
                    self.search.query.push_str(&printable);
                    self.search.history_nav_index = None;
                    self.search.current_match_index = 0;
                    self.recompute_search();
                    self.reveal_current_search_match();
                    return true;
                }
            }
        }
        // Swallow any other key while the search bar owns the keyboard.
        true
    }

    /// While the config panel is open, swallow keys so they don't reach the
    /// PTY; Esc closes it. The panel's own widgets handle their own events.
    fn handle_config_panel_key(
        &mut self,
        key: &keyboard::Key,
        mods: keyboard::Modifiers,
    ) -> Option<Task<Message>> {
        use keyboard::key::Named;
        use keyboard::Key;
        if !self.config_panel_open {
            return None;
        }
        if mods.control() && mods.shift() {
            if let Key::Character(c) = key {
                if c.eq_ignore_ascii_case("o") {
                    self.theme_editor = None;
                    self.config_panel_open = false;
                    return Some(Task::none());
                }
            }
        }
        if let Key::Named(Named::Escape) = key {
            // Esc backs out of the theme editor first, then the panel itself.
            if self.theme_editor.is_some() {
                self.theme_editor = None;
            } else {
                self.config_panel_open = false;
            }
        }
        Some(Task::none())
    }

    /// Route a keypress into the command palette while it is open. Returns
    /// `Some(task)` if consumed (and must not reach the PTY), `None` otherwise.
    fn handle_palette_key(
        &mut self,
        key: &keyboard::Key,
        mods: keyboard::Modifiers,
        text: Option<&str>,
    ) -> Option<Task<Message>> {
        use keyboard::key::Named;
        use keyboard::Key;
        if !self.palette.is_open {
            return None;
        }
        if mods.control() && mods.shift() {
            if let Key::Character(c) = key {
                if c.eq_ignore_ascii_case("p") {
                    self.palette.close();
                    return Some(Task::none());
                }
            }
        }
        match key {
            Key::Named(Named::Escape) => {
                self.palette.close();
                return Some(Task::none());
            }
            Key::Named(Named::Enter) => {
                let action = self.palette.selected_action();
                self.palette.close();
                return Some(match action {
                    Some(a) => self.execute_palette_action(a),
                    None => Task::none(),
                });
            }
            Key::Named(Named::ArrowUp) => {
                self.palette.select_prev();
                return Some(Task::none());
            }
            Key::Named(Named::ArrowDown) => {
                self.palette.select_next();
                return Some(Task::none());
            }
            Key::Named(Named::Backspace) => {
                self.palette.query.pop();
                self.palette.selected = 0;
                return Some(Task::none());
            }
            _ => {}
        }
        // Printable input filters the list.
        if !mods.control() && !mods.alt() {
            if let Some(t) = text {
                let printable: String = t.chars().filter(|c| !c.is_control()).collect();
                if !printable.is_empty() {
                    self.palette.query.push_str(&printable);
                    self.palette.selected = 0;
                    return Some(Task::none());
                }
            }
        }
        // Swallow any other key while the palette owns the keyboard.
        Some(Task::none())
    }

    /// Dispatch a palette action to the matching existing operation.
    fn execute_palette_action(&mut self, action: command_palette::PaletteAction) -> Task<Message> {
        use command_palette::PaletteAction;
        self.palette.record_use(action);
        match action {
            PaletteAction::NewTab => {
                self.new_session();
                Task::none()
            }
            PaletteAction::CloseTab => self.request_close_session(self.active),
            PaletteAction::NextTab => {
                self.next_session();
                Task::none()
            }
            PaletteAction::PrevTab => {
                self.prev_session();
                Task::none()
            }
            PaletteAction::Copy => {
                if let Some(text) = self
                    .sessions
                    .get(self.active)
                    .and_then(|s| s.terminal.copy_selection())
                    .filter(|t| !t.is_empty())
                {
                    let n = text.chars().count();
                    self.push_toast(
                        format!("Copied {} char{}", n, if n == 1 { "" } else { "s" }),
                        ToastKind::Success,
                    );
                    iced::clipboard::write(text)
                } else {
                    Task::none()
                }
            }
            PaletteAction::Paste => {
                let Some(id) = self.sessions.get(self.active).map(|session| session.id) else {
                    return Task::none();
                };
                iced::clipboard::read().map(move |text| Message::Pasted(id, text))
            }
            PaletteAction::OpenSearch => {
                self.search.toggle();
                self.recompute_search();
                if self.search.is_open {
                    iced::widget::operation::focus(SEARCH_INPUT_ID.clone())
                } else {
                    Task::none()
                }
            }
            PaletteAction::SplitVertical => {
                self.split(SplitMode::Vertical);
                Task::none()
            }
            PaletteAction::SplitHorizontal => {
                self.split(SplitMode::Horizontal);
                Task::none()
            }
            PaletteAction::FocusPaneLeft => {
                self.focus_pane_direction(PaneDirection::Left);
                Task::none()
            }
            PaletteAction::FocusPaneRight => {
                self.focus_pane_direction(PaneDirection::Right);
                Task::none()
            }
            PaletteAction::FocusPaneUp => {
                self.focus_pane_direction(PaneDirection::Up);
                Task::none()
            }
            PaletteAction::FocusPaneDown => {
                self.focus_pane_direction(PaneDirection::Down);
                Task::none()
            }
            PaletteAction::ResizePaneLeft => {
                self.resize_pane_direction(PaneDirection::Left);
                Task::none()
            }
            PaletteAction::ResizePaneRight => {
                self.resize_pane_direction(PaneDirection::Right);
                Task::none()
            }
            PaletteAction::ResizePaneUp => {
                self.resize_pane_direction(PaneDirection::Up);
                Task::none()
            }
            PaletteAction::ResizePaneDown => {
                self.resize_pane_direction(PaneDirection::Down);
                Task::none()
            }
            PaletteAction::ZoomPane => {
                self.toggle_pane_zoom();
                Task::none()
            }
            PaletteAction::SwapPanes => {
                self.swap_panes();
                Task::none()
            }
            PaletteAction::ClosePane => self.close_focused_pane(),
            PaletteAction::ToggleSidebar => {
                self.toggle_sidebar();
                Task::none()
            }
            PaletteAction::OpenSettings => {
                self.config_panel_open = true;
                Task::none()
            }
            PaletteAction::QuickTabSwitch => {
                self.tab_switcher = Some(TabSwitcherState::default());
                iced::widget::operation::focus(TAB_SWITCHER_INPUT_ID.clone())
            }
            PaletteAction::OpenHelp => {
                self.help_open = true;
                Task::none()
            }
            PaletteAction::ZoomIn => {
                self.adjust_font_size(1.0);
                Task::none()
            }
            PaletteAction::ZoomOut => {
                self.adjust_font_size(-1.0);
                Task::none()
            }
            PaletteAction::ZoomReset => {
                self.reset_font_size();
                Task::none()
            }
            PaletteAction::ScrollToTop => {
                if let Some(sess) = self.sessions.get_mut(self.active) {
                    let len = sess.terminal.scrollback_len();
                    sess.terminal.set_scroll_offset(len);
                    sess.refresh();
                }
                Task::none()
            }
            PaletteAction::ScrollToBottom => {
                if let Some(sess) = self.sessions.get_mut(self.active) {
                    sess.terminal.scroll_to_bottom();
                    sess.refresh();
                }
                Task::none()
            }
            PaletteAction::ClearScreen => {
                if let Some(sess) = self.sessions.get_mut(self.active) {
                    // Clear screen + scrollback and home the cursor via the
                    // terminal's own parser (shell-agnostic).
                    sess.terminal.process_batch(b"\x1b[3J\x1b[2J\x1b[H");
                    sess.refresh();
                }
                Task::none()
            }
        }
    }

    fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::PtyOutput(id, fd, data) => {
                let t0 = std::time::Instant::now();
                let is_active_output = self
                    .sessions
                    .get(self.active)
                    .is_some_and(|session| session.id == id && session.master_fd == fd);
                let mut clip_set: Option<String> = None;
                let mut clip_query = false;
                let mut clip_requests: Vec<terminal::ClipboardReadKind> = Vec::new();
                let mut notifications: Vec<(String, String)> = Vec::new();
                if let Some(sess) = self.session_by_identity(id, fd) {
                    sess.terminal.process_batch(&data);
                    sess.flush_responses();
                    sess.refresh();
                    clip_set = sess.terminal.take_osc52_clipboard_set();
                    clip_query = sess.terminal.take_osc52_clipboard_query();
                    clip_requests = sess
                        .terminal
                        .take_clipboard_read_requests()
                        .into_iter()
                        .map(|r| r.kind)
                        .collect();
                    notifications = sess.terminal.pending_notifications.drain(..).collect();
                }
                self.last_ingest_us = t0.elapsed().as_micros();
                self.last_ingest_bytes = data.len();
                // Output may have moved the shell's cwd; let the next periodic
                // tick reconcile the session snapshot.
                self.session_dirty = true;
                if is_active_output && self.search.is_open {
                    self.search_dirty = true;
                }

                // Desktop notifications requested via OSC 9 / OSC 777.
                if let Some((title, body)) = notifications.into_iter().next() {
                    let now = std::time::Instant::now();
                    let allowed = self.last_notification_at.is_none_or(|last| {
                        now.duration_since(last) >= std::time::Duration::from_secs(2)
                    });
                    if allowed {
                        self.last_notification_at = Some(now);
                        enqueue_desktop_notification(title, body);
                    }
                }

                // Clipboard set/query via OSC 52. The query path reads the
                // system clipboard asynchronously and writes the base64
                // response back to the originating session's PTY.
                let mut tasks: Vec<Task<Message>> = Vec::new();
                if let Some(text) = clip_set {
                    tasks.push(iced::clipboard::write(text));
                }
                if clip_query && self.config.allow_clipboard_read {
                    let start_read = if let Some(sess) = self.session_by_identity(id, fd) {
                        if sess.clipboard_read_in_flight {
                            false
                        } else {
                            sess.clipboard_read_in_flight = true;
                            true
                        }
                    } else {
                        false
                    };
                    if start_read {
                        tasks.push(
                            iced::clipboard::read().map(move |c| Message::Osc52Query(id, fd, c)),
                        );
                    } else if let Some(sess) = self.session_by_identity(id, fd) {
                        // OSC 52 has no structured busy status; an empty response
                        // is the interoperable refusal while another read runs.
                        sess.terminal.respond_osc52_clipboard("");
                        sess.flush_responses();
                    }
                } else if clip_query {
                    // An empty OSC 52 response reports that clipboard reads are
                    // unavailable without exposing host clipboard contents.
                    if let Some(sess) = self.session_by_identity(id, fd) {
                        sess.terminal.respond_osc52_clipboard("");
                        sess.flush_responses();
                    }
                }

                // OSC 5522 extended-clipboard read requests. iced's clipboard is
                // text-only, so we advertise a text MIME and serve text reads via
                // an async clipboard read; non-text MIME types get ENOSYS.
                for kind in clip_requests {
                    if !self.config.allow_clipboard_read {
                        if let Some(sess) = self.session_by_identity(id, fd) {
                            let resp = osc_5522_packet("type=read:status=EPERM", None);
                            sess.terminal.output_buffer.extend_from_slice(&resp);
                            sess.flush_responses();
                            sess.refresh();
                        }
                        continue;
                    }
                    match kind {
                        terminal::ClipboardReadKind::MimeList => {
                            if let Some(sess) = self.session_by_identity(id, fd) {
                                let resp = sess
                                    .terminal
                                    .build_paste_event(&["text/plain;charset=utf-8".to_string()]);
                                sess.terminal.output_buffer.extend_from_slice(&resp);
                                sess.flush_responses();
                                sess.refresh();
                            }
                        }
                        terminal::ClipboardReadKind::MimeData(mime) => {
                            if mime.starts_with("text") {
                                let start_read =
                                    if let Some(sess) = self.session_by_identity(id, fd) {
                                        if sess.clipboard_read_in_flight {
                                            false
                                        } else {
                                            sess.clipboard_read_in_flight = true;
                                            true
                                        }
                                    } else {
                                        false
                                    };
                                if start_read {
                                    tasks.push(iced::clipboard::read().map(move |c| {
                                        Message::Osc5522Data(id, fd, mime.clone(), c)
                                    }));
                                } else if let Some(sess) = self.session_by_identity(id, fd) {
                                    let resp = osc_5522_packet("type=read:status=EBUSY", None);
                                    sess.terminal.output_buffer.extend_from_slice(&resp);
                                    sess.flush_responses();
                                    sess.refresh();
                                }
                            } else if let Some(sess) = self.session_by_identity(id, fd) {
                                let resp = osc_5522_packet("type=read:status=ENOSYS", None);
                                sess.terminal.output_buffer.extend_from_slice(&resp);
                                sess.flush_responses();
                                sess.refresh();
                            }
                        }
                    }
                }

                if !tasks.is_empty() {
                    return Task::batch(tasks);
                }
            }
            Message::Osc52Query(id, fd, content) => {
                let allow_clipboard_read = self.config.allow_clipboard_read;
                if let Some(sess) = self.session_by_identity(id, fd) {
                    sess.clipboard_read_in_flight = false;
                    let content = content
                        .as_deref()
                        .filter(|value| {
                            allow_clipboard_read && value.len() <= MAX_CLIPBOARD_RESPONSE_BYTES
                        })
                        .unwrap_or("");
                    sess.terminal.respond_osc52_clipboard(content);
                    sess.flush_responses();
                    sess.refresh();
                }
            }
            Message::Osc5522Data(id, fd, mime, content) => {
                let allow_clipboard_read = self.config.allow_clipboard_read;
                if let Some(sess) = self.session_by_identity(id, fd) {
                    sess.clipboard_read_in_flight = false;
                    let data = content.unwrap_or_default();
                    let resp = if !allow_clipboard_read {
                        osc_5522_packet("type=read:status=EPERM", None)
                    } else if data.len() > MAX_CLIPBOARD_RESPONSE_BYTES {
                        osc_5522_packet("type=read:status=EFBIG", None)
                    } else if data.is_empty() {
                        osc_5522_packet("type=read:status=ENOSYS", None)
                    } else {
                        clipboard_5522_response_for_mime(&mime, data.as_bytes())
                    };
                    sess.terminal.output_buffer.extend_from_slice(&resp);
                    sess.flush_responses();
                    sess.refresh();
                }
            }
            Message::PtyExited(id, fd, _code) => {
                if let Some(index) = self
                    .sessions
                    .iter()
                    .position(|session| session.id == id && session.master_fd == fd)
                {
                    return self.close_session(index);
                }
            }
            Message::Key(event) => {
                if let keyboard::Event::KeyPressed {
                    key,
                    location,
                    modifiers,
                    text,
                    ..
                } = event
                {
                    // The close confirmation is the top-most modal. Enter confirms,
                    // Esc cancels, and every other key is swallowed.
                    if self.tab_close_confirm.is_some() {
                        if matches!(key, keyboard::Key::Named(keyboard::key::Named::Enter)) {
                            if let Some((id, _, activate_after)) = self.tab_close_confirm.take() {
                                if let Some(index) =
                                    self.sessions.iter().position(|session| session.id == id)
                                {
                                    return self.close_session_then(index, activate_after);
                                }
                            }
                        } else if matches!(key, keyboard::Key::Named(keyboard::key::Named::Escape))
                        {
                            self.tab_close_confirm = None;
                        }
                        return Task::none();
                    }
                    // The tab menu currently has pointer actions only; keep all
                    // unrelated keypresses out of the PTY while it is visible.
                    if self.tab_menu.is_some() {
                        if matches!(key, keyboard::Key::Named(keyboard::key::Named::Escape)) {
                            self.tab_menu = None;
                        }
                        return Task::none();
                    }
                    // Tab switcher swallows keys while open (Enter to jump,
                    // arrows to move, Esc/Ctrl+Shift+L to dismiss). Handle before
                    // generic keybindings so its toggle shortcut wins.
                    if self.tab_switcher.is_some() {
                        if let Some(task) =
                            self.handle_tab_switcher_key(&key, modifiers, text.as_deref())
                        {
                            return task;
                        }
                    }
                    if self.help_open || self.debug_open {
                        let active_overlay_toggle = (self.help_open
                            && chrome_shortcut(&key, modifiers) == Some(ChromeShortcut::Help))
                            || (self.debug_open
                                && chrome_shortcut(&key, modifiers) == Some(ChromeShortcut::Debug));
                        if active_overlay_toggle
                            || matches!(key, keyboard::Key::Named(keyboard::key::Named::Escape))
                        {
                            self.help_open = false;
                            self.debug_open = false;
                        }
                        return Task::none();
                    }
                    // Input-owning overlays route before global keybindings so a
                    // shortcut or printable key cannot mutate the hidden terminal.
                    if let Some(task) = self.handle_config_panel_key(&key, modifiers) {
                        return task;
                    }
                    if let Some(task) = self.handle_palette_key(&key, modifiers, text.as_deref()) {
                        return task;
                    }
                    if self.handle_search_key(&key, modifiers, text.as_deref()) {
                        return Task::none();
                    }
                    if let Some(task) = self.handle_keybinding(&key, modifiers) {
                        return task;
                    }
                    if let Some(task) = self.handle_tab_shortcut(&key, modifiers) {
                        return task;
                    }
                    if self.handle_scroll_shortcut(&key, modifiers) {
                        return Task::none();
                    }
                    let Some(sess) = self.sessions.get_mut(self.active) else {
                        return Task::none();
                    };
                    let app_cursor = sess.terminal.is_application_cursor_keys();
                    let enh = KeyboardEnhancements {
                        kitty_flags: sess.terminal.keyboard_enhancement_flags(),
                        modify_other_keys: sess.terminal.xterm_modify_other_keys(),
                        format_other_keys: sess.terminal.xterm_format_other_keys(),
                        report_all_keys: sess.terminal.is_report_all_keys_enabled(),
                        application_keypad: sess.terminal.is_application_keypad(),
                    };
                    if let Some(bytes) =
                        encode_key(&key, location, modifiers, text.as_deref(), app_cursor, enh)
                    {
                        sess.terminal.scroll_to_bottom();
                        sess.write_pty(&bytes);
                        sess.refresh();
                    }
                }
            }
            Message::Ime(event) => {
                use iced::advanced::input_method::Event as Ime;
                if !self.terminal_input_active() {
                    return Task::none();
                }
                let Some(sess) = self.sessions.get_mut(self.active) else {
                    return Task::none();
                };
                match event {
                    Ime::Opened => {
                        sess.terminal.ime_enabled = true;
                    }
                    Ime::Closed => {
                        sess.terminal.ime_enabled = false;
                        sess.terminal.clear_preedit();
                        sess.refresh();
                    }
                    Ime::Preedit(content, selection) => {
                        sess.terminal.set_preedit(content, selection);
                        sess.refresh();
                    }
                    Ime::Commit(text) => {
                        sess.terminal.clear_preedit();
                        sess.terminal.scroll_to_bottom();
                        sess.write_pty(text.as_bytes());
                        sess.refresh();
                    }
                }
            }
            Message::ModifiersChanged(mods) => {
                self.modifiers = mods;
            }
            Message::MousePane(pane_pos, input) => {
                if !self.terminal_mouse_active() {
                    return Task::none();
                }
                // Only a press switches the focused pane. Release/Drag aren't
                // bounds-gated in the widget, so every pane emits them — letting
                // those move focus would let the wrong pane steal it on release.
                if matches!(input, MouseInput::Press { .. }) && pane_pos < self.panes.len() {
                    self.focused_pane = pane_pos;
                    self.active = self.panes[pane_pos];
                    self.session_dirty = true;
                    self.refresh_active_context();
                }
                return self.handle_mouse(input);
            }
            Message::Pasted(id, Some(text)) => {
                let mut rejected = false;
                if let Some(sess) = self.sessions.iter_mut().find(|session| session.id == id) {
                    let bracketed = sess.terminal.is_bracketed_paste_enabled();
                    let framing = if bracketed {
                        BRACKETED_PASTE_FRAMING_BYTES
                    } else {
                        0
                    };
                    let required = text.len().saturating_add(framing);
                    if !sess.can_queue_user_bytes(required) {
                        rejected = true;
                    } else {
                        let bytes = if bracketed {
                            wrap_bracketed_paste(text.into_bytes())
                        } else {
                            text.into_bytes()
                        };
                        sess.terminal.scroll_to_bottom();
                        rejected = !sess.write_pty(&bytes);
                        sess.refresh();
                    }
                }
                if rejected {
                    self.push_toast(
                        "Paste rejected: terminal input queue is full",
                        ToastKind::Warning,
                    );
                }
            }
            Message::Pasted(_, None) => {}
            Message::Resized(size) => {
                self.win_size = size;
                let term_h = self.term_height();
                let term_w = (self.term_width() - terminal_view::SCROLLBAR_WIDTH).max(0.0);
                let (cols, rows) = self.metrics.grid_size(term_w, term_h);
                if cols != self.cols || rows != self.rows {
                    self.cols = cols;
                    self.rows = rows;
                    // Apply either full-tab or pane dimensions exactly once.
                    self.relayout();
                    self.refresh_active_context();
                }
            }
            Message::Focus(f) => {
                self.focused = f;
                // The blink tick stops while unfocused; leave the cursor solid so
                // it can't get stuck in the "off" half of a blink.
                if !f {
                    self.blink_on = true;
                }
                if let Some(sess) = self.sessions.get_mut(self.active) {
                    if sess.terminal.is_focus_event_mode() {
                        if f {
                            sess.terminal.emit_focus_in();
                        } else {
                            sess.terminal.emit_focus_out();
                        }
                        sess.flush_responses();
                    }
                }
            }
            Message::NewSession => self.new_session(),
            Message::CloseTab(id) => {
                if let Some(index) = self.sessions.iter().position(|session| session.id == id) {
                    return self.request_close_session(index);
                }
            }
            Message::WindowClose => return self.request_window_close(),
            Message::TabHover(id) => self.hovered_tab = id,
            Message::TabDragStart(id) => {
                if self.sessions.iter().any(|session| session.id == id) {
                    self.dragging_tab = Some(id);
                }
            }
            Message::TabDragEnd(target_id) => {
                if let Some(source_id) = self.dragging_tab.take() {
                    let source = self
                        .sessions
                        .iter()
                        .position(|session| session.id == source_id);
                    let target = self
                        .sessions
                        .iter()
                        .position(|session| session.id == target_id);
                    if let (Some(from), Some(to)) = (source, target) {
                        if from == to {
                            self.jump_session(to);
                        } else {
                            self.reorder_session(from, to);
                        }
                    }
                }
            }
            Message::TabDragCancel => {
                self.dragging_tab = None;
            }
            Message::DividerDragStart(divider) => {
                let now = std::time::Instant::now();
                // Double-click on a divider equalizes every pane.
                let double = self.last_divider_press.is_some_and(|(prev, d)| {
                    d == divider
                        && now.duration_since(prev)
                            < std::time::Duration::from_millis(DIVIDER_DOUBLE_CLICK_MS)
                });
                self.last_divider_press = Some((now, divider));
                if double {
                    equalize_shares(&mut self.pane_ratios);
                    self.relayout();
                    self.refresh_active_context();
                }
                self.dragging_divider = Some(divider);
            }
            Message::DividerDragEnd => self.dragging_divider = None,
            Message::DividerHover(divider) => self.hovered_divider = divider,
            Message::DividerDragMove(pt) => {
                if let Some(divider) = self.dragging_divider {
                    if divider + 1 < self.pane_ratios.len() {
                        // Pointer position as a fraction of the split axis…
                        let frac = match self.split_mode {
                            SplitMode::Vertical => pt.x / self.term_width().max(1.0),
                            SplitMode::Horizontal => pt.y / self.term_height().max(1.0),
                            SplitMode::Single => return Task::none(),
                        };
                        // …minus the panes before this divider gives the
                        // dragged pane's new share of its neighbor pair.
                        let before: f32 = self.pane_ratios[..divider].iter().sum();
                        let first = frac - before;
                        if set_divider_share(&mut self.pane_ratios, divider, first, true) {
                            self.relayout();
                            self.refresh_active_context();
                        }
                    }
                }
            }
            Message::SidebarDragStart => self.dragging_sidebar = true,
            Message::SidebarDragEnd => self.dragging_sidebar = false,
            Message::SidebarDragMove(pt) => {
                if self.dragging_sidebar {
                    // pt.x is relative to the dock+body row, which starts at the
                    // window's left edge, so it is the desired dock width directly.
                    let w = pt.x.clamp(SIDEBAR_W_MIN, SIDEBAR_W_MAX);
                    if (w - self.dock_width).abs() > f32::EPSILON {
                        self.dock_width = w;
                        self.apply_config();
                    }
                }
            }
            Message::ToggleSidebar => {
                self.toggle_sidebar();
            }
            Message::SetSidebarPanel(panel) => {
                self.sidebar_panel = panel;
                // Opening the file tree should reflect the active tab's cwd.
                if panel == SidebarPanel::Files {
                    if let Some(cwd) = self
                        .sessions
                        .get(self.active)
                        .and_then(|s| s.cwd_cache.clone().or_else(|| s.cwd()))
                    {
                        self.sidebar.set_current_dir(std::path::PathBuf::from(cwd));
                    }
                }
            }
            Message::SetTabPosition(pos) => {
                if self.config.tab_position != pos {
                    self.config.tab_position = pos;
                    self.config_dirty = true;
                    self.sync_tab_position_ui();
                    // Layout chrome changed (top bar shown/hidden, dock width):
                    // recompute the grid.
                    self.apply_config();
                }
            }
            Message::SidebarToggleNode(path) => self.sidebar.toggle_node(&path),
            Message::SidebarInsertPath(path) => {
                // Type the (shell-quoted) path into the active terminal so the
                // sidebar doubles as a path picker.
                if let Some(sess) = self.sessions.get_mut(self.active) {
                    let quoted = shell_quote(&path.to_string_lossy());
                    sess.terminal.scroll_to_bottom();
                    sess.write_pty(quoted.as_bytes());
                    sess.refresh();
                }
            }
            Message::SearchToggleRegex => {
                self.search.toggle_regex();
                self.recompute_search();
                self.reveal_current_search_match();
            }
            Message::SearchToggleCase => {
                self.search.toggle_case_sensitive();
                self.recompute_search();
                self.reveal_current_search_match();
            }
            Message::SearchInput(value) => {
                self.search.query = value;
                self.search.history_nav_index = None;
                self.search.current_match_index = 0;
                self.recompute_search();
                self.reveal_current_search_match();
            }
            Message::PaletteInput(value) => {
                self.palette.query = value;
                self.palette.selected = 0;
            }
            Message::PaletteExecute(i) => {
                let action = self.palette.action_at(i);
                self.palette.close();
                if let Some(a) = action {
                    return self.execute_palette_action(a);
                }
            }
            Message::ToggleConfigPanel => {
                self.config_panel_open = !self.config_panel_open;
            }
            Message::BlinkTick => {
                self.blink_on = !self.blink_on;
            }
            Message::PtyWriteTick => {
                let mut failed = false;
                for session in &mut self.sessions {
                    if !session.flush_write_queue() {
                        failed = true;
                    }
                }
                if failed {
                    self.push_toast("Terminal input write failed", ToastKind::Warning);
                }
            }
            Message::SearchRefreshTick => {
                let active_reflow_pending = self
                    .sessions
                    .get(self.active)
                    .is_some_and(|session| self.history_reflow_sessions.contains(&session.id));
                if self.search_dirty && !active_reflow_pending {
                    self.recompute_search();
                }
            }
            Message::HistoryReflowTick => {
                if self
                    .history_reflow_due
                    .is_some_and(|due| std::time::Instant::now() >= due)
                {
                    let pending = std::mem::take(&mut self.history_reflow_sessions);
                    for session in &mut self.sessions {
                        if pending.contains(&session.id) {
                            session.terminal.normalize_scrollback_width();
                            session.refresh();
                        }
                    }
                    self.history_reflow_due = None;
                    if self.search.is_open {
                        self.recompute_search();
                        self.reveal_current_search_match();
                    }
                    self.links_cache_key = None;
                }
            }
            Message::SetTheme(name) => {
                self.config.theme = name;
                self.config_dirty = true;
                self.apply_config();
            }
            Message::SetFontSize(v) => {
                self.config.font_size = Config::clamp_font_size(v);
                self.config_dirty = true;
                self.apply_config();
            }
            Message::SetLineSpacing(v) => {
                self.config.line_spacing = Config::clamp_line_spacing(v);
                self.config_dirty = true;
                self.apply_config();
            }
            Message::SetPadding(v) => {
                self.config.padding = Config::clamp_padding(v);
                self.config_dirty = true;
                self.apply_config();
            }
            Message::SetScrollback(v) => {
                self.config.scrollback_lines = Config::clamp_scrollback_lines(v as usize);
                self.config_dirty = true;
                self.apply_config();
            }
            Message::SetScrollSpeed(v) => {
                self.config.scroll_speed = Config::clamp_scroll_speed(v);
                self.config_dirty = true;
            }
            Message::SetFontFamily(name) => {
                self.config.font_family = name;
                self.config_dirty = true;
                self.apply_config();
            }
            Message::SetScrollbarAlways(always) => {
                self.config.scrollbar_visibility = if always {
                    config::ScrollbarVisibility::Always
                } else {
                    config::ScrollbarVisibility::Auto
                };
                self.config_dirty = true;
            }
            Message::SetDisableAltScreen(disable) => {
                self.config.disable_alt_screen = disable;
                self.config_dirty = true;
                self.apply_config();
            }
            Message::SetAllowClipboardRead(allow) => {
                self.config.allow_clipboard_read = allow;
                self.config_dirty = true;
            }
            Message::ThemeEditOpen => {
                // Seed the editor from the current theme; suggest a fresh name so
                // saving doesn't silently overwrite a builtin.
                let base = self.theme.clone();
                let suggested = if Theme::is_builtin(&base.name) {
                    format!("{}-custom", base.name)
                } else {
                    base.name.clone()
                };
                let hexes = base.editable_color_hexes();
                self.theme_editor = Some(ThemeEditState {
                    base,
                    name: suggested,
                    hexes,
                    error: None,
                });
            }
            Message::ThemeEditClose => {
                self.theme_editor = None;
            }
            Message::ThemeEditName(name) => {
                if let Some(ed) = &mut self.theme_editor {
                    ed.name = name;
                }
            }
            Message::ThemeEditColor(idx, hex) => {
                if let Some(ed) = &mut self.theme_editor {
                    if let Some(slot) = ed.hexes.get_mut(idx) {
                        *slot = hex;
                    }
                }
            }
            Message::ThemeEditSave => {
                let mut save_error: Option<String> = None;
                if let Some(ed) = &mut self.theme_editor {
                    let name = ed.name.trim().to_string();
                    if let Err(message) = Theme::validate_custom_theme_name(&name) {
                        ed.error = Some(message);
                    } else if Theme::is_builtin(&name) {
                        ed.error = Some("Name collides with a builtin theme".to_string());
                    } else if let Some(bad) =
                        ed.hexes.iter().position(|h| Theme::hex_to_rgb(h).is_none())
                    {
                        let labels = Theme::editable_color_labels();
                        ed.error = Some(format!("Invalid hex for {}", labels[bad]));
                    } else {
                        let mut theme = ed.base.clone();
                        theme.name = name.clone();
                        for (i, h) in ed.hexes.iter().enumerate() {
                            theme.set_editable_color(i, h);
                        }
                        match theme.save_custom_theme() {
                            Ok(()) => {
                                self.config.theme = name.clone();
                                self.config_dirty = true;
                                self.theme_editor = None;
                                self.apply_config();
                                self.push_toast(
                                    format!("Saved theme \"{}\"", name),
                                    ToastKind::Success,
                                );
                            }
                            Err(e) => {
                                let msg = format!("Save failed: {}", e);
                                ed.error = Some(msg.clone());
                                save_error = Some(msg);
                            }
                        }
                    }
                }
                if let Some(msg) = save_error {
                    self.push_toast(format!("Theme {}", msg), ToastKind::Warning);
                }
            }
            Message::ThemeDelete(name) => {
                match Theme::delete_custom_theme(&name) {
                    Ok(()) => {
                        self.push_toast(format!("Deleted theme \"{}\"", name), ToastKind::Info)
                    }
                    Err(e) => self.push_toast(format!("Delete failed: {}", e), ToastKind::Warning),
                }
                if self.config.theme == name {
                    self.config.theme = "dark".to_string();
                    self.config_dirty = true;
                    self.apply_config();
                }
            }
            Message::ConfigSave => match self.config.save() {
                Ok(()) => {
                    self.config_mtime = Config::config_mtime();
                    self.config_dirty = false;
                    self.push_toast("Config saved", ToastKind::Success);
                }
                Err(e) => self.push_toast(format!("Save failed: {}", e), ToastKind::Warning),
            },
            Message::ConfigReset => {
                self.config = Config::default();
                self.sync_tab_position_ui();
                self.apply_config();
                match self.config.save() {
                    Ok(()) => {
                        self.config_mtime = Config::config_mtime();
                        self.config_dirty = false;
                        self.push_toast("Config reset to defaults", ToastKind::Info);
                    }
                    Err(error) => {
                        self.config_dirty = true;
                        self.push_toast(
                            format!("Reset applied, save failed: {error}"),
                            ToastKind::Warning,
                        );
                    }
                }
            }
            Message::ConfigTick => {
                self.persist_live_config();
                // Skip while editing so live (unsaved) edits aren't reverted.
                if !self.config_panel_open {
                    let m = Config::config_mtime();
                    if m != self.config_mtime {
                        self.config_mtime = m;
                        if let Ok(path) = Config::config_path() {
                            if let Ok(content) = std::fs::read_to_string(&path) {
                                if let Ok(c) = Config::from_toml(&content) {
                                    self.config = c;
                                    self.config_dirty = false;
                                    self.sync_tab_position_ui();
                                    self.apply_config();
                                }
                            }
                        }
                    }
                }
                // Periodically persist tabs so a recent snapshot (with up-to-date
                // cwds) survives even an abrupt exit. Only when something that
                // feeds the snapshot may have changed since the last save.
                if self.session_dirty {
                    self.save_session_snapshot();
                }
                // Refresh cwd + foreground-process caches for every session so
                // tab labels reflect both. These are cheap /proc reads at 1.5s
                // cadence and let inactive tabs still show "vim · src" etc.
                for sess in self.sessions.iter_mut() {
                    sess.terminal.kitty_graphics.expire_pending_transfer();
                    sess.terminal.check_sync_output_timeout();
                    sess.refresh();
                    sess.cwd_cache = sess.cwd();
                    sess.fg_proc_cache = sess.fg_proc();
                }
                self.expire_toasts();
            }
            Message::TabMenuOpen(id) => {
                if self.sessions.iter().any(|session| session.id == id) {
                    self.tab_menu = Some(id);
                }
            }
            Message::TabMenuClose => self.tab_menu = None,
            Message::TabMenuAction(action) => {
                self.tab_menu = None;
                return self.execute_tab_menu_action(action);
            }
            Message::ToastTick => self.expire_toasts(),
            Message::ToastDismiss(i) => {
                if i < self.toasts.len() {
                    self.toasts.remove(i);
                }
            }
            Message::TabSwitcherClose => self.tab_switcher = None,
            Message::TabSwitcherInput(q) => {
                if let Some(s) = self.tab_switcher.as_mut() {
                    s.query = q;
                    s.selected = 0;
                }
            }
            Message::TabSwitcherJump(id) => {
                self.tab_switcher = None;
                if let Some(index) = self.sessions.iter().position(|session| session.id == id) {
                    if index != self.active {
                        self.show_session_in_focused_pane(index);
                    }
                }
            }
            Message::TabCloseConfirmNo => {
                self.tab_close_confirm = None;
            }
            Message::TabCloseConfirmYes => {
                if let Some((id, _, activate_after)) = self.tab_close_confirm.take() {
                    if let Some(index) = self.sessions.iter().position(|session| session.id == id) {
                        return self.close_session_then(index, activate_after);
                    }
                }
            }
        }
        self.recompute_links();
        self.refresh_kitty_handles();
        Task::none()
    }

    /// Build/refresh cached image handles for the active session's Kitty images.
    /// New or content-changed images get a fresh handle; handles for images no
    /// longer referenced by any placement are dropped.
    fn refresh_kitty_handles(&mut self) {
        type PendingHandle = ((usize, u32), u64, u32, u32, Vec<u8>);
        // Collect, under an immutable borrow, which images need a (re)build and
        // which ids are still live, then release the borrow before mutating.
        let mut needed: Vec<PendingHandle> = Vec::new();
        let mut live_keys = std::collections::HashSet::new();
        {
            let Some(sess) = self.sessions.get(self.active) else {
                self.kitty_handles.clear();
                return;
            };
            let kg = &sess.terminal.kitty_graphics;
            for p in kg.get_placements() {
                let key = (sess.id, p.image_id);
                let Some(img) = kg.get_image(p.image_id) else {
                    continue;
                };
                // Many placements may reference one image. Schedule/cache each
                // texture once so placement fan-out cannot clone and upload the
                // same (potentially large) pixel buffer hundreds of times.
                if !live_keys.insert(key) {
                    continue;
                }
                let stale = self
                    .kitty_handles
                    .get(&key)
                    .map(|(_, generation)| *generation != img.generation)
                    .unwrap_or(true);
                if stale {
                    needed.push((key, img.generation, img.width, img.height, img.data.clone()));
                }
            }
        }
        self.kitty_handles.retain(|key, _| live_keys.contains(key));
        for (key, generation, w, h, data) in needed {
            let handle = iced::advanced::image::Handle::from_rgba(w, h, data);
            self.kitty_handles.insert(key, (handle, generation));
        }
    }

    /// Build the renderable image list for a session from its placements and the
    /// cached handles. Placements are already z-sorted by the graphics state.
    fn kitty_images(&self, sess: &Session) -> Vec<KittyRender> {
        let kg = &sess.terminal.kitty_graphics;
        kg.get_placements()
            .iter()
            .filter_map(|p| {
                let (handle, _) = self.kitty_handles.get(&(sess.id, p.image_id))?;
                let img = kg.get_image(p.image_id)?;
                Some(KittyRender {
                    handle: handle.clone(),
                    col: p.x as usize,
                    row: p.y as usize,
                    cols: (p.width as usize).max(1),
                    rows: (p.height as usize).max(1),
                    id: p.image_id,
                    px_w: img.width,
                    px_h: img.height,
                })
            })
            .collect()
    }

    /// Re-detect links in the active session's visible grid. Version-gated so it
    /// is a no-op when neither the grid, the scroll position, nor the tab changed.
    fn recompute_links(&mut self) {
        let Some(sess) = self.sessions.get(self.active) else {
            self.links.clear();
            return;
        };
        let key = (
            sess.id,
            sess.terminal.get_grid_version(),
            sess.terminal.scroll_offset,
        );
        if self.links_cache_key == Some(key) {
            return;
        }
        self.links_cache_key = Some(key);
        let row_wrapped = sess.terminal.get_visible_row_wrapped();
        self.links = self
            .link_detector
            .detect_links_in_visible_cells_with_wrapping(&sess.grid, &row_wrapped);
    }

    // --- Theme-derived chrome colors and styles ---------------------------
    fn c_panel(&self) -> Color {
        Theme::rgb_to_color32(self.theme.ui.panel_bg)
    }
    fn c_text(&self) -> Color {
        Theme::rgb_to_color32(self.theme.ui.text)
    }
    fn c_text_dim(&self) -> Color {
        Theme::rgb_to_color32(self.theme.ui.text_disabled)
    }
    fn c_border(&self) -> Color {
        Theme::rgb_to_color32(self.theme.ui.border)
    }
    fn c_accent(&self) -> Color {
        Theme::rgb_to_color32(self.theme.tabbar.active_border)
    }

    /// Top tab bar / status bar background, matching the theme's tabbar color.
    fn chrome_bar_style(&self) -> impl Fn(&iced::Theme) -> container::Style {
        let bg = Theme::rgb_to_color32(self.theme.tabbar.bg);
        let text = self.c_text();
        move |_| container::Style {
            text_color: Some(text),
            background: Some(bg.into()),
            ..Default::default()
        }
    }

    /// Sidebar dock background, matching the theme's panel color.
    fn panel_style(&self) -> impl Fn(&iced::Theme) -> container::Style {
        let bg = self.c_panel();
        let text = self.c_text();
        move |_| container::Style {
            text_color: Some(text),
            background: Some(bg.into()),
            ..Default::default()
        }
    }

    /// `active` (hovered or mid-drag) tints the strip with the accent color so
    /// the user can see the divider is grabbable / being dragged.
    fn divider_style(&self, active: bool) -> impl Fn(&iced::Theme) -> container::Style {
        let bg = if active {
            blend(self.c_border(), self.c_accent(), 0.6)
        } else {
            self.c_border()
        };
        move |_| container::Style {
            background: Some(bg.into()),
            ..Default::default()
        }
    }

    /// Container-flavored variant of `tab_btn_style`, used when wrapping a tab
    /// in `mouse_area` (which can't hand the hover status off to a Button).
    /// `hovered`/`dragging` are pushed in by the caller from `self.hovered_tab`
    /// and `self.dragging_tab`.
    fn tab_container_style(
        &self,
        active: bool,
        hovered: bool,
        dragging: bool,
    ) -> impl Fn(&iced::Theme) -> container::Style {
        let base = Theme::rgb_to_color32(self.theme.tabbar.bg);
        let accent = self.c_accent();
        let active_text = Theme::rgb_to_color32(self.theme.tabbar.active_text);
        let inactive_text = Theme::rgb_to_color32(self.theme.tabbar.inactive_text);
        move |_t| {
            let (mut bg, txt, bw) = if active {
                (blend(base, accent, 0.22), active_text, 1.0)
            } else if hovered {
                (blend(base, accent, 0.10), inactive_text, 0.0)
            } else {
                (base, inactive_text, 0.0)
            };
            // Dim the source tab while it is being dragged so the user sees
            // which one will move.
            if dragging {
                bg = Color { a: 0.55, ..bg };
            }
            container::Style {
                text_color: Some(txt),
                background: Some(bg.into()),
                border: iced::Border {
                    color: accent,
                    width: bw,
                    radius: 4.0.into(),
                },
                ..Default::default()
            }
        }
    }

    /// Tab button: accent-tinted + bordered when active, flat otherwise.
    fn tab_btn_style(
        &self,
        active: bool,
    ) -> impl Fn(&iced::Theme, button::Status) -> button::Style {
        let base = Theme::rgb_to_color32(self.theme.tabbar.bg);
        let accent = self.c_accent();
        let active_text = Theme::rgb_to_color32(self.theme.tabbar.active_text);
        let inactive_text = Theme::rgb_to_color32(self.theme.tabbar.inactive_text);
        move |_t, status| {
            let (bg, txt, bw) = if active {
                (blend(base, accent, 0.22), active_text, 1.0)
            } else {
                let bg = match status {
                    button::Status::Hovered => blend(base, accent, 0.10),
                    _ => base,
                };
                (bg, inactive_text, 0.0)
            };
            button::Style {
                background: Some(bg.into()),
                text_color: txt,
                border: iced::Border {
                    color: accent,
                    width: bw,
                    radius: 4.0.into(),
                },
                ..Default::default()
            }
        }
    }

    /// Flat button (toggles, file rows, "+ New"): transparent, accent on hover.
    fn ghost_btn_style(&self) -> impl Fn(&iced::Theme, button::Status) -> button::Style {
        let base = self.c_panel();
        let accent = self.c_accent();
        let text = self.c_text();
        move |_t, status| {
            let bg = match status {
                button::Status::Hovered => Some(blend(base, accent, 0.16).into()),
                _ => None,
            };
            button::Style {
                background: bg,
                text_color: text,
                border: iced::Border {
                    color: Color::TRANSPARENT,
                    width: 0.0,
                    radius: 4.0.into(),
                },
                ..Default::default()
            }
        }
    }

    /// Close (×) button using the theme's close-button colors.
    fn close_btn_style(&self) -> impl Fn(&iced::Theme, button::Status) -> button::Style {
        let normal = Theme::rgb_to_color32(self.theme.tabbar.close_btn_bg);
        let hover = Theme::rgb_to_color32(self.theme.tabbar.close_btn_hover);
        let text = self.c_text();
        move |_t, status| {
            let bg = match status {
                button::Status::Hovered => hover,
                _ => normal,
            };
            button::Style {
                background: Some(bg.into()),
                text_color: text,
                border: iced::Border {
                    color: Color::TRANSPARENT,
                    width: 0.0,
                    radius: 4.0.into(),
                },
                ..Default::default()
            }
        }
    }

    fn tab_bar(&self) -> Element<'_, Message> {
        let mut tabs = row![].spacing(2).padding(2);
        // Sidebar/dock toggle button at the far left of the tab bar.
        tabs = tabs.push(
            button(text("☰").size(13))
                .on_press(Message::ToggleSidebar)
                .padding([3, 8])
                .style(self.tab_btn_style(self.sidebar_open)),
        );
        // In side-tab mode the tab list lives in the dock; the top bar keeps only
        // the dock toggle plus a button to move tabs back to the top.
        if self.config.tab_position == config::TabPosition::Side {
            tabs = tabs.push(
                button(text("▔").size(13))
                    .on_press(Message::SetTabPosition(config::TabPosition::Top))
                    .padding([3, 8])
                    .style(self.ghost_btn_style()),
            );
            return self.top_bar_with_close(tabs.into());
        }
        // Dock the tab strip into the left sidebar (vertical tab list).
        tabs = tabs.push(
            button(text("◧").size(13))
                .on_press(Message::SetTabPosition(config::TabPosition::Side))
                .padding([3, 8])
                .style(self.ghost_btn_style()),
        );
        for (i, sess) in self.sessions.iter().enumerate() {
            let id = sess.id;
            let active = i == self.active;
            let label = sess.label();
            let label = if label.chars().count() > 24 {
                let truncated: String = label.chars().take(23).collect();
                format!("{truncated}…")
            } else {
                label
            };
            // The tab's label area is a styled container wrapped in a
            // mouse_area so we get on_press/on_release/on_enter/on_exit. The
            // styling mirrors `tab_btn_style` so visually it matches the rest
            // of the chrome.
            let hovered = self.hovered_tab == Some(id);
            let dragging_this = self.dragging_tab == Some(id);
            let tab_label = container(text(label).size(13))
                .padding([3, 8])
                .style(self.tab_container_style(active, hovered, dragging_this));
            // Drag press/release lives on the label so a press on the close
            // button never starts a tab drag. Right-click opens the context menu.
            let tab: Element<'_, Message> = mouse_area(tab_label)
                .on_press(Message::TabDragStart(id))
                .on_release(Message::TabDragEnd(id))
                .on_right_press(Message::TabMenuOpen(id))
                .into();
            // Reveal the close button only on the active or hovered tab to cut
            // visual noise; keep its footprint reserved otherwise so tabs don't
            // jump when hovered.
            let show_close = active || hovered;
            let close: Element<'_, Message> = if show_close {
                button(text("×").size(13))
                    .on_press(Message::CloseTab(id))
                    .padding([3, 6])
                    .style(self.close_btn_style())
                    .into()
            } else {
                Space::new().width(Length::Fixed(18.0)).into()
            };
            let cell = row![tab, close].spacing(1).align_y(iced::Alignment::Center);
            // Hover tracking on the whole cell so moving onto the close
            // button does not collapse it out of the layout.
            tabs = tabs.push(
                mouse_area(cell)
                    .on_enter(Message::TabHover(Some(id)))
                    .on_exit(Message::TabHover(None)),
            );
        }
        tabs = tabs.push(
            button(text("+").size(13))
                .on_press(Message::NewSession)
                .padding([3, 8])
                .style(self.ghost_btn_style()),
        );
        let scroller = scrollable(tabs)
            .direction(scrollable::Direction::Horizontal(
                scrollable::Scrollbar::new().width(0).scroller_width(0),
            ))
            .width(Length::Fill);
        self.top_bar_with_close(scroller.into())
    }

    fn top_bar_with_close<'a>(&'a self, content: Element<'a, Message>) -> Element<'a, Message> {
        let close = button(text("×").size(14))
            .on_press(Message::WindowClose)
            .padding([3, 9])
            .style(self.close_btn_style());
        let bar = row![container(content).width(Length::Fill), close]
            .align_y(iced::Alignment::Center)
            .width(Length::Fill);
        container(bar)
            .width(Length::Fill)
            .height(Length::Fixed(TAB_BAR_H))
            .style(self.chrome_bar_style())
            .into()
    }

    /// Floating tab context menu — Close, Close Others, Close to Right, Duplicate.
    /// Background mouse_area dismisses on outside-click; Esc also closes via key handler.
    fn tab_context_menu(&self, id: usize) -> Element<'_, Message> {
        let i = self
            .sessions
            .iter()
            .position(|session| session.id == id)
            .unwrap_or(self.active);
        let label = self
            .sessions
            .get(i)
            .map(|s| s.label())
            .unwrap_or_else(|| format!("Tab {}", i + 1));
        let row_btn = |t: &str, msg: Message| -> Element<'_, Message> {
            button(text(t.to_string()).size(13))
                .on_press(msg)
                .padding([4, 10])
                .width(Length::Fill)
                .style(self.ghost_btn_style())
                .into()
        };
        let only_one = self.sessions.len() <= 1;
        let last_idx = self.sessions.len().saturating_sub(1);

        let mut menu = column![
            text(label).size(12).style(text::secondary),
            row_btn("Close", Message::TabMenuAction(TabMenuAction::Close(id)),),
        ]
        .spacing(2);
        if !only_one {
            menu = menu.push(row_btn(
                "Close Others",
                Message::TabMenuAction(TabMenuAction::CloseOthers(id)),
            ));
        }
        if i < last_idx {
            menu = menu.push(row_btn(
                "Close to Right",
                Message::TabMenuAction(TabMenuAction::CloseToRight(id)),
            ));
        }
        menu = menu.push(row_btn(
            "Duplicate",
            Message::TabMenuAction(TabMenuAction::Duplicate(id)),
        ));

        let panel = container(menu)
            .width(Length::Fixed(200.0))
            .padding(8)
            .style(container::dark);

        // Dismiss-on-outside-click sheet behind the panel.
        let dismiss = mouse_area(
            container(Space::new())
                .width(Length::Fill)
                .height(Length::Fill),
        )
        .on_press(Message::TabMenuClose);
        let top_gap = TAB_BAR_H + 4.0;
        let centered = container(panel)
            .center_x(Length::Fill)
            .align_top(Length::Fill)
            .padding(iced::Padding::from(0).top(top_gap));
        stack![Element::from(dismiss), Element::from(centered)].into()
    }

    /// Centered modal: "Tab is running `<proc>`. Close anyway?". Esc / outside
    /// click cancel; only TabCloseConfirmYes proceeds with the close.
    fn tab_close_confirm_view(&self, id: usize, proc_name: &str) -> Element<'_, Message> {
        let label = self
            .sessions
            .iter()
            .find(|session| session.id == id)
            .map(|s| s.label())
            .unwrap_or_else(|| format!("Session {}", id + 1));
        let body = column![
            text(format!("Close \"{}\"?", label)).size(14),
            text(format!("Foreground process: {}", proc_name))
                .size(12)
                .style(text::secondary),
            row![
                button(text("Cancel").size(13))
                    .on_press(Message::TabCloseConfirmNo)
                    .padding([4, 12])
                    .style(self.ghost_btn_style()),
                Space::new().width(Length::Fill),
                button(text("Close anyway").size(13))
                    .on_press(Message::TabCloseConfirmYes)
                    .padding([4, 12])
                    .style(button::danger),
            ]
            .spacing(8)
            .align_y(iced::Alignment::Center),
        ]
        .spacing(10);
        let panel = container(body)
            .width(Length::Fixed(320.0))
            .padding(14)
            .style(container::dark);
        let dismiss = mouse_area(
            container(Space::new())
                .width(Length::Fill)
                .height(Length::Fill),
        )
        .on_press(Message::TabCloseConfirmNo);
        let centered = container(panel)
            .center_x(Length::Fill)
            .center_y(Length::Fill);
        stack![Element::from(dismiss), Element::from(centered)].into()
    }

    /// Bottom-right toast stack. Each toast is click-dismissable.
    fn toast_overlay(&self) -> Element<'_, Message> {
        let mut col = column![].spacing(6);
        for (idx, t) in self.toasts.iter().enumerate() {
            let accent = match t.kind {
                ToastKind::Info => self.c_accent(),
                ToastKind::Success => self.theme.ansi_color(2),
                ToastKind::Warning => self.theme.ansi_color(3),
            };
            let style_accent = accent;
            let style = move |_t: &iced::Theme| container::Style {
                background: Some(iced::Background::Color(Color {
                    a: 0.96,
                    ..Color::BLACK
                })),
                text_color: Some(Color::WHITE),
                border: iced::Border {
                    color: style_accent,
                    width: 1.0,
                    radius: 6.0.into(),
                },
                ..Default::default()
            };
            let body = container(text(t.text.clone()).size(13))
                .padding([6, 12])
                .style(style);
            let clickable = mouse_area(body).on_press(Message::ToastDismiss(idx));
            col = col.push(clickable);
        }
        container(col)
            .align_right(Length::Fill)
            .align_bottom(Length::Fill)
            .padding(
                iced::Padding::from(0)
                    .right(16.0)
                    .bottom(STATUS_BAR_H + 12.0),
            )
            .into()
    }

    /// Ctrl+Shift+L fuzzy tab switcher overlay (palette-style).
    fn tab_switcher_view(&self, state: &TabSwitcherState) -> Element<'_, Message> {
        let filtered = tab_switcher_filtered(&self.sessions, &state.query);

        let query: Element<'_, Message> = text_input("Jump to tab…", &state.query)
            .id(TAB_SWITCHER_INPUT_ID.clone())
            .on_input(Message::TabSwitcherInput)
            .size(14)
            .into();
        let query_line = row![text("↦").size(16), query]
            .spacing(8)
            .align_y(iced::Alignment::Center);

        let mut list = column![].spacing(2);
        if filtered.is_empty() {
            list = list.push(text("No tabs match").size(13).style(text::secondary));
        } else {
            for &(pos, idx) in filtered.iter() {
                let selected = pos == state.selected;
                let Some(session) = self.sessions.get(idx) else {
                    continue;
                };
                let label = session.label();
                let id = session.id;
                let info = row![
                    text(format!("{:>2}", idx + 1))
                        .size(12)
                        .style(text::secondary),
                    text(label).size(13),
                    Space::new().width(Length::Fill),
                ]
                .spacing(10)
                .align_y(iced::Alignment::Center);
                let accent = self.c_accent();
                let body = container(info).width(Length::Fill).padding([3, 8]).style(
                    move |_t: &iced::Theme| container::Style {
                        background: if selected {
                            Some(iced::Background::Color(Color { a: 0.28, ..accent }))
                        } else {
                            None
                        },
                        border: iced::Border {
                            radius: 4.0.into(),
                            ..Default::default()
                        },
                        ..Default::default()
                    },
                );
                let row_btn = mouse_area(body).on_press(Message::TabSwitcherJump(id));
                list = list.push(row_btn);
            }
        }

        let body = column![query_line, list].spacing(8);
        let panel = container(body)
            .width(Length::Fixed(420.0))
            .max_height(420.0)
            .padding(12)
            .style(container::dark);
        let dismiss = mouse_area(
            container(Space::new())
                .width(Length::Fill)
                .height(Length::Fill),
        )
        .on_press(Message::TabSwitcherClose);
        let centered = container(panel)
            .center_x(Length::Fill)
            .center_y(Length::Fill);
        stack![Element::from(dismiss), Element::from(centered)].into()
    }

    /// Bottom status bar: cwd, grid size, cursor position, and search state.
    fn status_bar(&self) -> Element<'_, Message> {
        let sess = self.sessions.get(self.active);
        let cwd = sess
            .and_then(|s| s.cwd_cache.clone())
            .map(|p| {
                // Abbreviate the home directory to `~` to keep the bar compact.
                if let Some(home) = dirs::home_dir().and_then(|h| h.to_str().map(String::from)) {
                    if let Some(rest) = p.strip_prefix(&home) {
                        return format!("~{rest}");
                    }
                }
                p
            })
            .unwrap_or_default();
        let (cur_row, cur_col) = sess.map(|s| s.cursor).unwrap_or((0, 0));
        // Report the active pane's own grid size; when split it differs from the
        // whole-window `self.cols`×`self.rows`.
        let (grid_cols, grid_rows) = sess
            .map(|s| (s.terminal.grid.cols(), s.terminal.grid.rows()))
            .unwrap_or((self.cols, self.rows));
        let grid = format!("{}×{}", grid_cols, grid_rows);
        let pos = format!("{}:{}", cur_row + 1, cur_col + 1);
        let scroll = sess
            .map(|s| {
                let prefix = if s.terminal.is_alt_buffer_active() {
                    "alt "
                } else {
                    ""
                };
                format!(
                    "{}{}/{}",
                    prefix,
                    s.terminal.scroll_offset,
                    s.terminal.scrollback_len()
                )
            })
            .unwrap_or_else(|| "0/0".to_string());

        let dim = self.c_text_dim();
        let dim_style = move |_t: &iced::Theme| text::Style { color: Some(dim) };

        let mut right = row![
            text(grid).size(11).style(dim_style),
            text(pos).size(11).style(dim_style),
            text(scroll).size(11).style(dim_style),
        ]
        .spacing(14)
        .align_y(iced::Alignment::Center);
        // Split indicator: which pane is focused, and whether it is zoomed.
        if self.split_mode != SplitMode::Single {
            let axis = match self.split_mode {
                SplitMode::Vertical => "│",
                _ => "─",
            };
            let label = if self.pane_zoomed {
                format!("{axis} {}/{} zoom", self.focused_pane + 1, self.panes.len())
            } else {
                format!("{axis} {}/{}", self.focused_pane + 1, self.panes.len())
            };
            let accent = self.c_accent();
            right = right.push(
                text(label)
                    .size(11)
                    .style(move |_t: &iced::Theme| text::Style {
                        color: Some(accent),
                    }),
            );
        }
        if self.search.is_open && !self.search.matches.is_empty() {
            right = right.push(
                text(format!(
                    "{}/{}",
                    self.search.current_match_index + 1,
                    self.search.matches.len()
                ))
                .size(11)
                .style(dim_style),
            );
        }

        let bar = row![
            text(cwd).size(11).style(dim_style),
            Space::new().width(Length::Fill),
            right,
        ]
        .spacing(14)
        .align_y(iced::Alignment::Center);
        container(bar)
            .width(Length::Fill)
            .height(Length::Fixed(STATUS_BAR_H))
            .padding([0, 10])
            .align_y(iced::Alignment::Center)
            .style(self.chrome_bar_style())
            .into()
    }

    /// Build the terminal widget for the session displayed in pane `pane_pos`.
    /// Overlay-style decorations (search, links, Kitty images) are only attached
    /// to the active pane; the other pane renders plain.
    fn pane_view(&self, pane_pos: usize) -> Element<'_, Message> {
        let sess_idx = self.panes[pane_pos];
        let sess = &self.sessions[sess_idx];
        // An open overlay input owns the keyboard and IME, so the terminal pane
        // renders unfocused (no blinking cursor, no competing IME request).
        let focused = self.focused && pane_pos == self.focused_pane && self.terminal_input_active();
        let is_active = sess_idx == self.active;
        // Only walk the grid to build per-row selection spans when a selection
        // actually exists; otherwise hand the widget an empty Vec (no highlight).
        let selection: Vec<Option<(usize, usize)>> = if sess.terminal.selection.is_some() {
            (0..sess.grid.len())
                .map(|r| sess.terminal.row_selection_cols(r))
                .collect()
        } else {
            Vec::new()
        };
        // Only paint match highlights while the search bar is open; otherwise
        // stale matches (whose line indices drift as the grid scrolls) linger.
        let (search_matches, current) = if is_active && self.search.is_open {
            let start = sess.terminal.viewport_absolute_start();
            let end = start.saturating_add(sess.grid.len());
            let visible = self
                .search
                .matches
                .iter()
                .filter(|m| m.line >= start && m.line < end)
                .map(|m| search::SearchMatch {
                    line: m.line - start,
                    col_start: m.col_start,
                    col_end: m.col_end,
                })
                .collect();
            let current = self.search.current_match().and_then(|m| {
                (m.line >= start && m.line < end).then_some((m.line - start, m.col_start))
            });
            (visible, current)
        } else {
            (Vec::new(), None)
        };
        let links: &[link::Link] = if is_active { &self.links } else { &[] };
        let images = if is_active {
            self.kitty_images(sess)
        } else {
            Vec::new()
        };
        TermWidget::new(
            &sess.grid,
            sess.cursor,
            sess.cursor_visible,
            sess.terminal.cursor_shape,
            focused,
            &self.theme,
            self.metrics,
            self.mono,
            self.cjk_mono,
            self.symbol_mono,
            self.math_symbol,
            self.nerd_symbol,
            selection,
            sess.terminal.scroll_offset,
            sess.terminal.scrollback_len(),
        )
        .modifiers(
            self.modifiers.shift(),
            self.modifiers.alt(),
            self.modifiers.control(),
        )
        .scrollbar_always(matches!(
            self.config.scrollbar_visibility,
            config::ScrollbarVisibility::Always
        ))
        .search(search_matches, current)
        .links(links)
        .dynamic_palette(&sess.terminal.dynamic_palette)
        .dynamic_defaults(
            sess.terminal.dynamic_fg,
            sess.terminal.dynamic_bg,
            sess.terminal.dynamic_cursor_color,
        )
        .images(images)
        .preedit(if focused && !sess.terminal.preedit_text.is_empty() {
            Some((
                sess.terminal.preedit_text.clone(),
                sess.terminal.preedit_selection.clone(),
            ))
        } else {
            None
        })
        .blink_on(self.blink_on)
        .on_mouse(move |inp| Message::MousePane(pane_pos, inp))
        .into()
    }

    /// Left dock. A header lets the user switch between the file tree and the
    /// vertical tab list and dock the tab strip back to the top.
    fn sidebar_view(&self) -> Element<'_, Message> {
        // Panel switcher: highlight the active panel.
        let panel_btn = |label: &str, panel: SidebarPanel| {
            let active = self.sidebar_panel == panel;
            button(text(label.to_string()).size(12))
                .on_press(Message::SetSidebarPanel(panel))
                .padding([2, 8])
                .style(self.tab_btn_style(active))
        };
        let header = row![
            panel_btn("Tabs", SidebarPanel::Tabs),
            panel_btn("Files", SidebarPanel::Files),
            Space::new().width(Length::Fill),
        ]
        .spacing(4)
        .align_y(iced::Alignment::Center);
        let header = container(header).padding([4, 6]);

        let panel: Element<'_, Message> = match self.sidebar_panel {
            SidebarPanel::Tabs => self.sidebar_tabs_view(),
            SidebarPanel::Files => self.sidebar_files_view(),
        };

        container(column![header, panel].spacing(2))
            .width(Length::Fixed(self.dock_width))
            .height(Length::Fill)
            .style(self.panel_style())
            .into()
    }

    /// Draggable vertical strip between the dock and the terminal body. Pressing
    /// it starts a width-resize drag (continued via the row's `on_move`).
    fn sidebar_divider(&self) -> Element<'_, Message> {
        let strip = container(Space::new())
            .width(Length::Fixed(DIVIDER))
            .height(Length::Fill);
        mouse_area(strip.style(self.divider_style(self.dragging_sidebar)))
            .on_press(Message::SidebarDragStart)
            .interaction(iced::mouse::Interaction::ResizingHorizontally)
            .into()
    }

    /// File-tree panel body. Directories toggle expand/collapse on click; files
    /// type their (quoted) path into the active terminal.
    fn sidebar_files_view(&self) -> Element<'_, Message> {
        let title = self
            .sidebar
            .current_dir
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("/")
            .to_string();
        let mut rows: Vec<Element<'_, Message>> =
            vec![container(text(title).size(12).font(iced::Font {
                weight: iced::font::Weight::Bold,
                ..iced::Font::DEFAULT
            }))
            .padding([4, 6])
            .into()];
        if let Some(root) = &self.sidebar.root {
            for child in &root.children {
                self.collect_sidebar_nodes(child, 0, &mut rows);
            }
        }
        let list = iced::widget::Column::with_children(rows).spacing(1);
        scrollable(list).height(Length::Fill).into()
    }

    /// Vertical session tab list shown in the dock. Mirrors the top tab strip:
    /// click to select, hover to reveal close, and a trailing "new tab" button.
    fn sidebar_tabs_view(&self) -> Element<'_, Message> {
        let mut list = column![].spacing(2).padding([2, 4]);
        for (i, sess) in self.sessions.iter().enumerate() {
            let id = sess.id;
            let active = i == self.active;
            let label = sess.label();
            let label = if label.chars().count() > 22 {
                let truncated: String = label.chars().take(21).collect();
                format!("{truncated}…")
            } else {
                label
            };
            let hovered = self.hovered_tab == Some(id);
            let dragging_this = self.dragging_tab == Some(id);
            let tab_label = container(text(label).size(13).wrapping(text::Wrapping::None))
                .width(Length::Fill)
                .padding([4, 8])
                .style(self.tab_container_style(active, hovered, dragging_this));
            let tab: Element<'_, Message> = mouse_area(tab_label)
                .on_press(Message::TabDragStart(id))
                .on_release(Message::TabDragEnd(id))
                .into();
            // Reveal the close button on the active or hovered tab only.
            let show_close = active || hovered;
            let close_inner: Element<'_, Message> = if show_close {
                button(text("×").size(13))
                    .on_press(Message::CloseTab(id))
                    .padding([4, 6])
                    .style(self.close_btn_style())
                    .into()
            } else {
                Space::new().into()
            };
            let close = container(close_inner)
                .width(Length::Fixed(24.0))
                .center_x(Length::Fixed(24.0));
            let cell = row![tab, close].spacing(2).align_y(iced::Alignment::Center);
            list = list.push(
                mouse_area(cell)
                    .on_enter(Message::TabHover(Some(id)))
                    .on_exit(Message::TabHover(None)),
            );
        }
        // A compact, flat "+ New" sits apart from the filled tab rows so it does
        // not read as just another tab.
        let new_tab = container(
            button(text("+ New").size(12))
                .on_press(Message::NewSession)
                .padding([2, 10])
                .style(self.ghost_btn_style()),
        )
        .width(Length::Fill)
        .center_x(Length::Fill)
        .padding([4, 0]);
        list = list.push(new_tab);
        scrollable(list).height(Length::Fill).into()
    }

    /// Recursively flatten a file-tree node (and expanded descendants) into rows.
    fn collect_sidebar_nodes<'a>(
        &self,
        node: &'a sidebar::FileTreeNode,
        depth: usize,
        out: &mut Vec<Element<'a, Message>>,
    ) {
        let indent = 6.0 + depth as f32 * 12.0;
        let icon = if node.is_dir {
            if node.expanded {
                "▾"
            } else {
                "▸"
            }
        } else {
            "·"
        };
        let label = row![
            Space::new().width(Length::Fixed(indent)),
            text(icon).size(12).width(Length::Fixed(14.0)),
            text(node.name.clone()).size(12),
        ]
        .align_y(iced::Alignment::Center);
        let msg = if node.is_dir {
            Message::SidebarToggleNode(node.path.clone())
        } else {
            Message::SidebarInsertPath(node.path.clone())
        };
        out.push(
            button(label)
                .on_press(msg)
                .width(Length::Fill)
                .padding([1, 2])
                .style(self.ghost_btn_style())
                .into(),
        );
        if node.is_dir && node.expanded {
            for child in &node.children {
                self.collect_sidebar_nodes(child, depth + 1, out);
            }
        }
    }

    /// The draggable divider strip drawn between panes `index` and `index + 1`.
    /// Pressing it starts a resize drag (continued via the body's `on_move`
    /// while `dragging_divider` is set).
    fn divider(&self, horizontal: bool, index: usize) -> Element<'_, Message> {
        let d = if horizontal {
            container(Space::new())
                .width(Length::Fill)
                .height(Length::Fixed(DIVIDER))
        } else {
            container(Space::new())
                .width(Length::Fixed(DIVIDER))
                .height(Length::Fill)
        };
        let interaction = if horizontal {
            iced::mouse::Interaction::ResizingVertically
        } else {
            iced::mouse::Interaction::ResizingHorizontally
        };
        let active =
            self.hovered_divider == Some(index) || self.dragging_divider == Some(index);
        mouse_area(d.style(self.divider_style(active)))
            .on_press(Message::DividerDragStart(index))
            .on_enter(Message::DividerHover(Some(index)))
            .on_exit(Message::DividerHover(None))
            .interaction(interaction)
            .into()
    }

    /// Thin frame around a split pane: the focused pane gets an accent outline
    /// so keyboard focus is visible at a glance; the other pane stays plain.
    fn pane_frame_style(&self, focused: bool) -> impl Fn(&iced::Theme) -> container::Style {
        let accent = self.c_accent();
        move |_| container::Style {
            border: iced::Border {
                color: if focused { accent } else { Color::TRANSPARENT },
                width: if focused { 1.0 } else { 0.0 },
                radius: 0.0.into(),
            },
            ..Default::default()
        }
    }

    fn view(&self) -> Element<'_, Message> {
        if self.panes.is_empty() || self.sessions.is_empty() {
            return container(text("no session")).into();
        }
        let panes_body: Element<'_, Message> = if self.split_mode != SplitMode::Single
            && self.pane_zoomed
        {
            // Zoomed: the focused pane fills the whole area; the hidden panes
            // keep running in the background exactly like inactive tabs.
            self.pane_view(self.focused_pane)
        } else if self.split_mode == SplitMode::Single {
            self.pane_view(0)
        } else {
            // Panes laid out along the split axis, a draggable divider between
            // each adjacent pair. Integer FillPortions approximate the float
            // shares.
            let horizontal = self.split_mode == SplitMode::Horizontal;
            let n = self.panes.len();
            let mut items: Vec<Element<'_, Message>> = Vec::with_capacity(2 * n - 1);
            for pos in 0..n {
                if pos > 0 {
                    items.push(self.divider(horizontal, pos - 1));
                }
                let share = self.pane_ratios.get(pos).copied().unwrap_or(1.0 / n as f32);
                let portion = (share * 1000.0).round().max(1.0) as u16;
                let pane = container(self.pane_view(pos))
                    .style(self.pane_frame_style(self.focused_pane == pos));
                let pane = if horizontal {
                    pane.width(Length::Fill).height(Length::FillPortion(portion))
                } else {
                    pane.width(Length::FillPortion(portion)).height(Length::Fill)
                };
                items.push(pane.into());
            }
            if horizontal {
                iced::widget::Column::with_children(items)
                    .width(Length::Fill)
                    .height(Length::Fill)
                    .into()
            } else {
                iced::widget::Row::with_children(items)
                    .width(Length::Fill)
                    .height(Length::Fill)
                    .into()
            }
        };
        // While dragging the divider, wrap the panes in a mouse_area so pointer
        // moves drive the resize and release ends it. The handler is attached
        // only during a drag to avoid emitting a message on every idle move.
        let panes_body: Element<'_, Message> = if self.dragging_divider.is_some() {
            mouse_area(panes_body)
                .on_move(Message::DividerDragMove)
                .on_release(Message::DividerDragEnd)
                .on_exit(Message::DividerDragEnd)
                .into()
        } else {
            panes_body
        };
        let body = container(panes_body)
            .width(Length::Fill)
            .height(Length::Fill);
        let body: Element<'_, Message> = if self.config_panel_open {
            let overlay = if self.theme_editor.is_some() {
                self.theme_editor_view()
            } else {
                self.config_panel()
            };
            stack![body, overlay].into()
        } else if self.palette.is_open {
            stack![body, self.command_palette()].into()
        } else if self.search.is_open {
            stack![body, self.search_bar()].into()
        } else {
            body.into()
        };
        // Help and the debug panel float above any other overlay so they can be
        // summoned at any time (and the debug panel can sit alongside others).
        let body: Element<'_, Message> = if self.help_open {
            stack![body, self.help_panel()].into()
        } else if self.debug_open {
            stack![body, self.debug_panel()].into()
        } else {
            body
        };
        // Optional left dock (file tree and/or tab list) beside the terminal,
        // separated by a draggable resize divider.
        let main_area: Element<'_, Message> = if self.dock_open() {
            let dock_row = row![self.sidebar_view(), self.sidebar_divider(), body]
                .width(Length::Fill)
                .height(Length::Fill);
            // While dragging, pointer moves drive the resize and release ends it.
            if self.dragging_sidebar {
                mouse_area(dock_row)
                    .on_move(Message::SidebarDragMove)
                    .on_release(Message::SidebarDragEnd)
                    .on_exit(Message::SidebarDragEnd)
                    .into()
            } else {
                dock_row.into()
            }
        } else {
            body
        };
        // The top bar is always present: in Top mode it holds the tab strip; in
        // Side mode it holds the dock toggle so chrome never overlaps the grid.
        let root: Element<'_, Message> = column![self.tab_bar(), main_area, self.status_bar()]
            .width(Length::Fill)
            .height(Length::Fill)
            .into();
        // Tab context menu, tab switcher, and toasts float above everything
        // so they remain accessible regardless of which other panel is open.
        let root = if let Some(i) = self.tab_menu {
            stack![root, self.tab_context_menu(i)].into()
        } else {
            root
        };
        let root: Element<'_, Message> = if let Some(s) = &self.tab_switcher {
            stack![root, self.tab_switcher_view(s)].into()
        } else {
            root
        };
        let root: Element<'_, Message> = if let Some((id, process, _)) = &self.tab_close_confirm {
            stack![root, self.tab_close_confirm_view(*id, process)].into()
        } else {
            root
        };
        if self.toasts.is_empty() {
            root
        } else {
            stack![root, self.toast_overlay()].into()
        }
    }

    /// Search bar overlaid at the top-right of the terminal. The query is an
    /// editable `text_input`; Enter/Esc/arrows are still handled at the app level
    /// (the input deliberately has no `on_submit` so Shift+Enter can mean "prev").
    fn search_bar(&self) -> Element<'_, Message> {
        let status = if let Some(err) = &self.search.error_message {
            err.clone()
        } else if !self.search.matches.is_empty() {
            format!(
                "{}/{}",
                self.search.current_match_index + 1,
                self.search.matches.len()
            )
        } else if !self.search.query.is_empty() {
            "No matches".to_string()
        } else {
            String::new()
        };

        // Clickable mode toggles (also bound to Ctrl+R / Ctrl+I).
        let regex_btn = button(text(".*").size(12))
            .on_press(Message::SearchToggleRegex)
            .padding([2, 6])
            .style(if self.search.use_regex {
                button::primary
            } else {
                button::secondary
            });
        let case_btn = button(text("Aa").size(12))
            .on_press(Message::SearchToggleCase)
            .padding([2, 6])
            .style(if self.search.case_sensitive {
                button::primary
            } else {
                button::secondary
            });

        let input = text_input("search…", &self.search.query)
            .id(SEARCH_INPUT_ID.clone())
            .on_input(Message::SearchInput)
            .size(13)
            .width(Length::Fixed(220.0));
        let mut bar = row![text("Find:").size(13), input]
            .spacing(8)
            .align_y(iced::Alignment::Center);
        if !status.is_empty() {
            bar = bar.push(text(status).size(13));
        }
        bar = bar.push(regex_btn).push(case_btn);
        let inner = container(bar).padding([4, 10]).style(container::dark);
        container(inner)
            .align_right(Length::Fill)
            .align_top(Length::Fill)
            .padding(8)
            .into()
    }

    /// Centered, fuzzy-filtered command palette overlay. Keys are handled at
    /// the app level (`handle_palette_key`); rows are also mouse-clickable.
    fn command_palette(&self) -> Element<'_, Message> {
        let query = text_input("Type to filter…", &self.palette.query)
            .id(PALETTE_INPUT_ID.clone())
            .on_input(Message::PaletteInput)
            .size(14);
        let query_line = row![text("›").size(16), query]
            .spacing(8)
            .align_y(iced::Alignment::Center);

        let mut list = column![].spacing(2);
        let filtered = self.palette.filtered();
        if filtered.is_empty() {
            list = list.push(text("No commands").size(13).style(text::secondary));
        } else {
            for (pos, (idx, item)) in filtered.iter().enumerate() {
                let mut info = row![
                    text(item.name).size(14),
                    text(item.description).size(11).style(text::secondary),
                    Space::new().width(Length::Fill),
                ]
                .spacing(10)
                .align_y(iced::Alignment::Center);
                if !item.shortcut.is_empty() {
                    info = info.push(text(item.shortcut).size(11).style(text::secondary));
                }
                let row_btn = button(info)
                    .on_press(Message::PaletteExecute(*idx))
                    .width(Length::Fill)
                    .padding([4, 8])
                    .style(if pos == self.palette.selected {
                        button::primary
                    } else {
                        button::text
                    });
                list = list.push(row_btn);
            }
        }

        let footer = text("↑↓ navigate · Enter run · Esc close")
            .size(10)
            .style(text::secondary);
        let inner = container(
            column![query_line, scrollable(list).height(Length::Shrink), footer].spacing(8),
        )
        .width(Length::Fixed(520.0))
        .max_height(420.0)
        .padding(12)
        .style(container::dark);
        container(inner)
            .center_x(Length::Fill)
            .center_y(Length::Fill)
            .into()
    }

    /// Centered settings overlay (Ctrl+Shift+O). Controls live-apply on change;
    /// Save persists to disk, Reset restores defaults.
    fn config_panel(&self) -> Element<'_, Message> {
        let mut themes: Vec<String> = Theme::available_themes()
            .into_iter()
            .map(|s| s.to_string())
            .collect();
        themes.extend(Theme::custom_theme_names());
        let current_theme = Some(self.config.theme.clone());
        let is_custom = !Theme::is_builtin(&self.config.theme);

        // Keep the modal inside the current window and switch to a stacked form
        // before horizontal controls become cramped. The content itself scrolls
        // below, so every setting remains reachable in short windows.
        let panel_width = (self.win_size.width - 24.0).clamp(1.0, 520.0);
        let panel_height = (self.win_size.height - 24.0).clamp(1.0, 560.0);
        let compact = panel_width < 430.0;
        let panel_padding = if compact || panel_height < 360.0 {
            10.0
        } else {
            16.0
        };

        let theme_picker = pick_list(themes, current_theme, Message::SetTheme)
            .text_size(13)
            .width(Length::Fill);
        let mut theme_actions =
            row![button(text("Edit…").size(13)).on_press(Message::ThemeEditOpen),]
                .spacing(8)
                .align_y(iced::Alignment::Center);
        if is_custom {
            theme_actions = theme_actions.push(
                button(text("Delete").size(13))
                    .on_press(Message::ThemeDelete(self.config.theme.clone()))
                    .style(button::danger),
            );
        }
        let theme_row: Element<'_, Message> = if compact {
            column![text("Theme").size(13), theme_picker, theme_actions]
                .spacing(6)
                .into()
        } else {
            row![
                text("Theme").size(13).width(Length::Fixed(120.0)),
                theme_picker,
                theme_actions,
            ]
            .spacing(10)
            .align_y(iced::Alignment::Center)
            .into()
        };

        // Monospace families detected via fc-list (cached, scanned on first open).
        // Ensure the configured family is present so the pick_list shows it.
        let mut fonts: Vec<String> = Config::get_monospace_fonts().clone();
        if !self.config.font_family.trim().is_empty()
            && !fonts.iter().any(|f| f == &self.config.font_family)
        {
            fonts.insert(0, self.config.font_family.clone());
        }
        let font_picker = pick_list(
            fonts,
            Some(self.config.font_family.clone()),
            Message::SetFontFamily,
        )
        .text_size(13)
        .width(Length::Fill);
        let font_family_row: Element<'_, Message> = if compact {
            column![text("Font").size(13), font_picker]
                .spacing(6)
                .into()
        } else {
            row![
                text("Font").size(13).width(Length::Fixed(120.0)),
                font_picker,
            ]
            .spacing(10)
            .align_y(iced::Alignment::Center)
            .into()
        };

        fn responsive_slider_row<'a>(
            compact: bool,
            label: &'static str,
            value: String,
            control: Element<'a, Message>,
        ) -> Element<'a, Message> {
            if compact {
                column![
                    row![
                        text(label).size(13).width(Length::Fill),
                        text(value).size(13),
                    ]
                    .align_y(iced::Alignment::Center),
                    control,
                ]
                .spacing(6)
                .into()
            } else {
                slider_row(label, value, control)
            }
        }

        let font_size = responsive_slider_row(
            compact,
            "Font Size",
            format!("{:.0}", self.config.font_size),
            slider(8.0..=72.0, self.config.font_size, Message::SetFontSize)
                .step(1.0)
                .into(),
        );
        let line_spacing = responsive_slider_row(
            compact,
            "Line Spacing",
            format!("{:.2}", self.config.line_spacing),
            slider(0.8..=3.0, self.config.line_spacing, Message::SetLineSpacing)
                .step(0.05)
                .into(),
        );
        let padding = responsive_slider_row(
            compact,
            "Padding",
            format!("{:.0}", self.config.padding),
            slider(0.0..=20.0, self.config.padding, Message::SetPadding)
                .step(1.0)
                .into(),
        );
        let scrollback = responsive_slider_row(
            compact,
            "Scrollback",
            format!("{}", self.config.scrollback_lines),
            slider(
                100..=100_000u32,
                self.config.scrollback_lines as u32,
                Message::SetScrollback,
            )
            .step(100u32)
            .into(),
        );
        let scroll_speed = responsive_slider_row(
            compact,
            "Scroll Speed",
            format!("{}", self.config.scroll_speed),
            slider(1..=10u32, self.config.scroll_speed, Message::SetScrollSpeed)
                .step(1u32)
                .into(),
        );
        fn responsive_control_row<'a>(
            compact: bool,
            label: &'static str,
            control: Element<'a, Message>,
        ) -> Element<'a, Message> {
            if compact {
                column![text(label).size(13), control].spacing(6).into()
            } else {
                row![text(label).size(13).width(Length::Fixed(120.0)), control,]
                    .spacing(10)
                    .align_y(iced::Alignment::Center)
                    .into()
            }
        }

        let scrollbar_row = responsive_control_row(
            compact,
            "Scrollbar",
            checkbox(matches!(
                self.config.scrollbar_visibility,
                config::ScrollbarVisibility::Always
            ))
            .label("Always show")
            .text_size(13)
            .on_toggle(Message::SetScrollbarAlways)
            .into(),
        );

        let alt_screen_row = responsive_control_row(
            compact,
            "Alt Screen",
            checkbox(self.config.disable_alt_screen)
                .label("Disable")
                .text_size(13)
                .on_toggle(Message::SetDisableAltScreen)
                .into(),
        );

        let clipboard_row = responsive_control_row(
            compact,
            "Clipboard",
            checkbox(self.config.allow_clipboard_read)
                .label("Allow PTY reads (unsafe over SSH)")
                .text_size(13)
                .on_toggle(Message::SetAllowClipboardRead)
                .into(),
        );

        let tab_position_row = responsive_control_row(
            compact,
            "Tabs",
            checkbox(self.config.tab_position == config::TabPosition::Side)
                .label("In sidebar")
                .text_size(13)
                .on_toggle(|side| {
                    Message::SetTabPosition(if side {
                        config::TabPosition::Side
                    } else {
                        config::TabPosition::Top
                    })
                })
                .into(),
        );

        let buttons = row![
            button(text("Save").size(13)).on_press(Message::ConfigSave),
            button(text("Reset").size(13))
                .on_press(Message::ConfigReset)
                .style(button::danger),
            button(text("Close").size(13))
                .on_press(Message::ToggleConfigPanel)
                .style(button::secondary),
        ]
        .spacing(8);

        let footer = text("Changes auto-save · Ctrl+Shift+O toggles · Esc closes")
            .size(10)
            .width(Length::Fill)
            .wrapping(text::Wrapping::Word)
            .style(text::secondary);

        let content = column![
            text("Settings").size(18),
            theme_row,
            font_family_row,
            font_size,
            line_spacing,
            padding,
            scrollback,
            scroll_speed,
            scrollbar_row,
            alt_screen_row,
            clipboard_row,
            tab_position_row,
            buttons,
            footer,
        ]
        .spacing(12)
        .width(Length::Fill);

        let inner = container(scrollable(content).width(Length::Fill).height(Length::Fill))
            .width(Length::Fixed(panel_width))
            .height(Length::Fixed(panel_height))
            .padding(panel_padding)
            .style(container::dark);
        container(inner)
            .center_x(Length::Fill)
            .center_y(Length::Fill)
            .into()
    }

    /// Custom-theme editor overlay: name field plus a hex input per terminal
    /// palette color, with a live swatch. UI-chrome colors are inherited from the
    /// theme the editor was opened on.
    fn theme_editor_view(&self) -> Element<'_, Message> {
        let Some(ed) = &self.theme_editor else {
            return Space::new().into();
        };
        let labels = Theme::editable_color_labels();

        let name_row = row![
            text("Name").size(13).width(Length::Fixed(150.0)),
            text_input("theme name", &ed.name)
                .on_input(Message::ThemeEditName)
                .size(13),
        ]
        .spacing(10)
        .align_y(iced::Alignment::Center);

        let mut list = column![].spacing(6);
        for (i, label) in labels.iter().enumerate() {
            let hex = ed.hexes.get(i).cloned().unwrap_or_default();
            // Live swatch when the hex parses, else a neutral placeholder.
            let swatch_color = Theme::hex_to_rgb(&hex)
                .map(Theme::rgb_to_color32)
                .unwrap_or(iced::Color::from_rgb(0.3, 0.3, 0.3));
            let swatch = container(Space::new())
                .width(Length::Fixed(22.0))
                .height(Length::Fixed(22.0))
                .style(move |_| container::Style {
                    background: Some(swatch_color.into()),
                    border: iced::Border {
                        color: iced::Color::from_rgb(0.5, 0.5, 0.5),
                        width: 1.0,
                        radius: 3.0.into(),
                    },
                    ..Default::default()
                });
            let r = row![
                text(*label).size(12).width(Length::Fixed(150.0)),
                swatch,
                text_input("#RRGGBB", &hex)
                    .on_input(move |s| Message::ThemeEditColor(i, s))
                    .size(12)
                    .width(Length::Fixed(110.0)),
            ]
            .spacing(10)
            .align_y(iced::Alignment::Center);
            list = list.push(r);
        }

        let buttons = row![
            button(text("Save").size(13)).on_press(Message::ThemeEditSave),
            button(text("Cancel").size(13))
                .on_press(Message::ThemeEditClose)
                .style(button::secondary),
        ]
        .spacing(8);

        let mut content = column![
            text("Theme Editor").size(18),
            name_row,
            scrollable(list).height(Length::Fixed(300.0)),
        ]
        .spacing(12);
        if let Some(err) = &ed.error {
            content = content.push(text(err.clone()).size(12).style(text::danger));
        }
        content = content.push(buttons);

        let inner = container(content)
            .width(Length::Fixed(420.0))
            .max_height(560.0)
            .padding(16)
            .style(container::dark);
        container(inner)
            .center_x(Length::Fill)
            .center_y(Length::Fill)
            .into()
    }

    /// Centered keybindings cheat-sheet (Ctrl+Shift+/). Pane direction chords
    /// combine Ctrl with Alt so JWM's bare-Alt shortcuts remain untouched.
    fn help_panel(&self) -> Element<'_, Message> {
        let section = |title: &str| -> Element<'_, Message> {
            text(title.to_string()).size(13).style(text::primary).into()
        };
        let kb = |key: &str, desc: &str| -> Element<'_, Message> {
            row![
                container(text(key.to_string()).size(12).font(iced::Font::MONOSPACE))
                    .width(Length::Fixed(190.0)),
                text(desc.to_string()).size(12).style(text::secondary),
            ]
            .spacing(8)
            .into()
        };

        let body = column![
            text("Keyboard Shortcuts").size(18),
            section("Tabs / Sessions"),
            kb("Ctrl+Shift+T", "New tab"),
            kb("Ctrl+Shift+W", "Close current tab"),
            kb("Ctrl+Tab / Ctrl+PgDn", "Next tab"),
            kb("Ctrl+Shift+Tab / Ctrl+PgUp", "Previous tab"),
            kb("Ctrl+1 .. Ctrl+8", "Jump to tab 1-8"),
            kb("Ctrl+9", "Jump to last tab"),
            kb("Ctrl+Shift+L", "Fuzzy tab switcher"),
            section("Splits / Panes"),
            kb("Ctrl+Shift+E", "Add pane right (re-orients a row split)"),
            kb("Ctrl+Shift+D", "Add pane below (re-orients a column split)"),
            kb("Ctrl+Alt+Arrow", "Focus adjacent pane"),
            kb("Ctrl+Alt+Shift+Arrow", "Resize focused pane"),
            kb("Ctrl+Shift+Z", "Zoom focused pane (toggle)"),
            kb("Ctrl+Shift+X", "Swap pane with the next one"),
            kb("Double-click divider", "Equalize all panes"),
            kb("Ctrl+Shift+W", "Close focused pane / tab"),
            section("Edit / Clipboard"),
            kb("Ctrl+Shift+C", "Copy selection"),
            kb("Ctrl+Shift+V", "Paste"),
            kb("Drag", "Select text"),
            kb("Ctrl+Click", "Open link under cursor"),
            section("Scroll / Search"),
            kb("Shift+Home", "Scroll to top"),
            kb("Shift+End", "Scroll to bottom (live)"),
            kb("Ctrl+Shift+F", "Find"),
            section("Panels"),
            kb("Ctrl+\\", "Toggle tabs / files sidebar"),
            kb("Ctrl+Shift+P", "Command palette"),
            kb("Ctrl+Shift+O", "Settings"),
            kb("F12", "Debug / diagnostics"),
            kb("Ctrl+Shift+/", "This help"),
            kb("Esc", "Close any panel"),
            section("Appearance"),
            kb("Ctrl+= / Ctrl+-", "Increase / decrease font size"),
            kb("Ctrl+0", "Reset font size"),
        ]
        .spacing(6);

        let inner = container(scrollable(body).height(Length::Shrink))
            .width(Length::Fixed(460.0))
            .max_height(560.0)
            .padding(16)
            .style(container::dark);
        container(inner)
            .center_x(Length::Fill)
            .center_y(Length::Fill)
            .into()
    }

    /// Top-right diagnostics overlay (F12): live grid / session /
    /// scrollback / Kitty-image / process-memory stats for the active session.
    fn debug_panel(&self) -> Element<'_, Message> {
        let stat = |label: &str, value: String| -> Element<'_, Message> {
            row![
                container(text(label.to_string()).size(11).style(text::primary))
                    .width(Length::Fixed(110.0)),
                text(value).size(11).font(iced::Font::MONOSPACE),
            ]
            .spacing(8)
            .into()
        };

        let mut lines = column![text("Diagnostics").size(13)].spacing(3);
        lines = lines
            .push(stat("Grid", format!("{}x{}", self.cols, self.rows)))
            .push(stat("Sessions", format!("{}", self.sessions.len())))
            .push(stat("Active", format!("#{}", self.active + 1)))
            .push(stat(
                "Split",
                match self.split_mode {
                    SplitMode::Single => "Single".to_string(),
                    SplitMode::Vertical => {
                        format!("V {}/{}", self.focused_pane + 1, self.panes.len())
                    }
                    SplitMode::Horizontal => {
                        format!("H {}/{}", self.focused_pane + 1, self.panes.len())
                    }
                },
            ));
        if let Some(sess) = self.sessions.get(self.active) {
            lines = lines
                .push(stat(
                    "Scrollback",
                    format!(
                        "{} / {}",
                        sess.terminal.scrollback_len(),
                        self.config.scrollback_lines
                    ),
                ))
                .push(stat(
                    "Scroll Off",
                    format!("{}", sess.terminal.scroll_offset),
                ))
                .push(stat(
                    "Kitty Imgs",
                    format!("{}", sess.terminal.kitty_graphics.image_count()),
                ))
                .push(stat(
                    "Kitty Mem",
                    format!("{} MB", sess.terminal.kitty_graphics.image_memory_mb()),
                ));
        }
        lines = lines.push(stat(
            "Memory",
            match read_rss_mb() {
                Some(mb) => format!("{:.1} MB", mb),
                None => "N/A".to_string(),
            },
        ));
        lines = lines.push(stat("Links", format!("{}", self.links.len())));
        // Ingest cost of the last PTY-output batch. bytes/µs is numerically equal
        // to MB/s, so the throughput needs no extra scaling.
        let ingest = if self.last_ingest_us > 0 {
            format!(
                "{} B / {} µs ({:.0} MB/s)",
                self.last_ingest_bytes,
                self.last_ingest_us,
                self.last_ingest_bytes as f64 / self.last_ingest_us as f64,
            )
        } else {
            format!("{} B / <1 µs", self.last_ingest_bytes)
        };
        lines = lines.push(stat("Ingest", ingest));

        let inner = container(lines)
            .width(Length::Fixed(240.0))
            .padding(10)
            .style(container::dark);
        container(inner)
            .align_right(Length::Fill)
            .align_top(Length::Fill)
            .padding(8)
            .into()
    }

    fn subscription(&self) -> Subscription<Message> {
        let mut subs: Vec<Subscription<Message>> = self
            .sessions
            .iter()
            .map(|s| {
                pty_subscription(PtySubscriptionKey {
                    id: s.id,
                    master_fd: s.master_fd,
                    reader_fd: Arc::clone(&s.reader_fd),
                })
            })
            .collect();
        let events = iced::event::listen_with(|event, status, _id| match event {
            iced::Event::Keyboard(keyboard::Event::ModifiersChanged(m)) => {
                Some(Message::ModifiersChanged(m))
            }
            // When an overlay text input is focused it captures the keys it
            // consumes (typing, Backspace, cursor movement). Dropping captured
            // keyboard events here keeps them from also reaching the terminal,
            // so editing the search/palette query never double-inputs.
            iced::Event::Keyboard(_) if status == iced::event::Status::Captured => None,
            iced::Event::Keyboard(k) => Some(Message::Key(k)),
            iced::Event::InputMethod(_) if status == iced::event::Status::Captured => None,
            iced::Event::InputMethod(ime) => Some(Message::Ime(ime)),
            iced::Event::Window(iced::window::Event::Resized(size)) => Some(Message::Resized(size)),
            iced::Event::Window(iced::window::Event::Focused) => Some(Message::Focus(true)),
            iced::Event::Window(iced::window::Event::Unfocused) => Some(Message::Focus(false)),
            iced::Event::Window(iced::window::Event::CloseRequested) => Some(Message::WindowClose),
            // Catch every left-button release so a tab drag that ends outside
            // any tab still clears `dragging_tab`. When the release lands on a
            // tab, mouse_area's on_release fires Message::TabDragEnd first
            // (which already consumes `dragging_tab`), so this becomes a no-op.
            iced::Event::Mouse(iced::mouse::Event::ButtonReleased(iced::mouse::Button::Left)) => {
                Some(Message::TabDragCancel)
            }
            _ => None,
        });
        subs.push(events);
        subs.push(
            iced::time::every(std::time::Duration::from_millis(1500)).map(|_| Message::ConfigTick),
        );
        // The blink tick redraws and re-shapes the whole grid every 530ms purely
        // to animate blinking cells. Run it only while focused AND when a visible
        // pane actually has blinking text — the common case (no blink, or
        // unfocused) then stays fully idle.
        let has_blink = self.panes.iter().any(|&idx| {
            self.sessions.get(idx).is_some_and(|s| {
                s.terminal
                    .grid
                    .iter()
                    .flatten()
                    .any(|cell| cell.flags.blink())
            })
        });
        if self.focused && has_blink {
            subs.push(
                iced::time::every(std::time::Duration::from_millis(530))
                    .map(|_| Message::BlinkTick),
            );
        }
        if self.sessions.iter().any(Session::has_pending_write) {
            subs.push(
                iced::time::every(std::time::Duration::from_millis(8))
                    .map(|_| Message::PtyWriteTick),
            );
        }
        if self.search.is_open && self.search_dirty {
            subs.push(
                iced::time::every(std::time::Duration::from_millis(50))
                    .map(|_| Message::SearchRefreshTick),
            );
        }
        if self.history_reflow_due.is_some() {
            subs.push(
                iced::time::every(std::time::Duration::from_millis(50))
                    .map(|_| Message::HistoryReflowTick),
            );
        }
        if !self.toasts.is_empty() {
            subs.push(
                iced::time::every(std::time::Duration::from_millis(250))
                    .map(|_| Message::ToastTick),
            );
        }
        Subscription::batch(subs)
    }
}

/// A labeled settings row: fixed-width label, the control, then its value.
fn slider_row<'a>(
    label: &'static str,
    value: String,
    control: Element<'a, Message>,
) -> Element<'a, Message> {
    row![
        text(label).size(13).width(Length::Fixed(120.0)),
        control,
        text(value).size(13).width(Length::Fixed(64.0)),
    ]
    .spacing(10)
    .align_y(iced::Alignment::Center)
    .into()
}

/// Score and sort tabs against the switcher query. Empty query returns all in
/// declaration order; otherwise returns matches highest score first as
/// `(filtered_position, session_index)` tuples. Used by both the renderer and
/// the key handler so navigation matches the visible list.
fn tab_switcher_filtered(sessions: &[Session], query: &str) -> Vec<(usize, usize)> {
    use fuzzy_matcher::skim::SkimMatcherV2;
    use fuzzy_matcher::FuzzyMatcher;
    if query.is_empty() {
        return sessions.iter().enumerate().map(|(i, _)| (i, i)).collect();
    }
    let matcher = SkimMatcherV2::default();
    let mut scored: Vec<(i64, usize)> = sessions
        .iter()
        .enumerate()
        .filter_map(|(i, s)| matcher.fuzzy_match(&s.label(), query).map(|sc| (sc, i)))
        .collect();
    scored.sort_by_key(|item| std::cmp::Reverse(item.0));
    scored
        .into_iter()
        .enumerate()
        .map(|(pos, (_, idx))| (pos, idx))
        .collect()
}

/// Resident set size of this process in MB (Linux /proc), for the debug panel.
fn read_rss_mb() -> Option<f64> {
    let content = std::fs::read_to_string("/proc/self/status").ok()?;
    for line in content.lines() {
        if let Some(rest) = line.strip_prefix("VmRSS:") {
            let kb: u64 = rest.split_whitespace().next()?.parse().ok()?;
            return Some(kb as f64 / 1024.0);
        }
    }
    None
}

/// xterm button code for press/motion reports.
fn btn_code(button: MouseButton) -> u8 {
    match button {
        MouseButton::Left => 0,
        MouseButton::Middle => 1,
        MouseButton::Right => 2,
    }
}

/// Shell-quote a path for typing into the terminal, with a trailing space.
fn shell_quote(s: &str) -> String {
    if s.is_empty() {
        return "''".to_string();
    }
    let safe = s
        .chars()
        .all(|c| c.is_alphanumeric() || "._-/~".contains(c));
    if safe {
        format!("{s} ")
    } else {
        format!("'{}' ", s.replace('\'', "'\\''"))
    }
}

/// Submit an OSC 9/777 notification to one bounded worker. The worker owns and
/// waits for every `notify-send` child, preventing zombies; a stuck notifier can
/// fill at most this small queue instead of spawning unbounded processes/threads.
fn enqueue_desktop_notification(title: String, body: String) {
    type Notification = (String, String);
    static SENDER: std::sync::OnceLock<std::sync::mpsc::SyncSender<Notification>> =
        std::sync::OnceLock::new();

    let sender = SENDER.get_or_init(|| {
        let (sender, receiver) = std::sync::mpsc::sync_channel::<Notification>(8);
        let _ = std::thread::Builder::new()
            .name("jterm3-notifications".to_string())
            .spawn(move || {
                while let Ok((title, body)) = receiver.recv() {
                    let _ = std::process::Command::new("notify-send")
                        .arg(title)
                        .arg(body)
                        .status();
                }
            });
        sender
    });
    let _ = sender.try_send((title, body));
}

/// Wrap a paste payload in bracketed-paste delimiters.
fn wrap_bracketed_paste(mut payload: Vec<u8>) -> Vec<u8> {
    const PREFIX: &[u8] = b"\x1b[200~";
    const SUFFIX: &[u8] = b"\x1b[201~";
    let payload_len = payload.len();
    payload.reserve(BRACKETED_PASTE_FRAMING_BYTES);
    payload.resize(payload_len + BRACKETED_PASTE_FRAMING_BYTES, 0);
    payload.copy_within(0..payload_len, PREFIX.len());
    payload[..PREFIX.len()].copy_from_slice(PREFIX);
    payload[PREFIX.len() + payload_len..].copy_from_slice(SUFFIX);
    payload
}

/// Build a single OSC 5522 packet: `ESC ] 5522 ; <metadata> [; <payload>] ESC \`.
fn osc_5522_packet(metadata: &str, payload: Option<&str>) -> Vec<u8> {
    let mut packet = Vec::new();
    packet.extend_from_slice(b"\x1b]5522;");
    packet.extend_from_slice(metadata.as_bytes());
    if let Some(payload) = payload {
        packet.extend_from_slice(b";");
        packet.extend_from_slice(payload.as_bytes());
    }
    packet.extend_from_slice(b"\x1b\\");
    packet
}

/// Build the OK/DATA/DONE sequence answering an OSC 5522 MIME-data read.
fn clipboard_5522_response_for_mime(mime_type: &str, data: &[u8]) -> Vec<u8> {
    use base64::Engine;
    let engine = base64::engine::general_purpose::STANDARD;
    let encoded_mime = engine.encode(mime_type.as_bytes());
    let encoded_data = engine.encode(data);
    let mut output = Vec::new();
    output.extend_from_slice(&osc_5522_packet("type=read:status=OK", None));
    output.extend_from_slice(&osc_5522_packet(
        &format!("type=read:status=DATA:mime={encoded_mime}"),
        Some(&encoded_data),
    ));
    output.extend_from_slice(&osc_5522_packet("type=read:status=DONE", None));
    output
}

fn pty_subscription(key: PtySubscriptionKey) -> Subscription<Message> {
    // Key on the stable session id (not the raw fd): a closed session's fd
    // number can be reused by a new session, and keying on fd alone would let
    // iced confuse the two and reuse the old reader thread on the reused fd.
    Subscription::run_with(key, |key: &PtySubscriptionKey| pty_stream(key.clone()))
}

fn pty_stream(key: PtySubscriptionKey) -> impl iced::futures::Stream<Item = Message> {
    use iced::futures::{SinkExt, StreamExt};
    iced::stream::channel(
        2,
        move |mut output: iced::futures::channel::mpsc::Sender<Message>| async move {
            let id = key.id;
            let fd = key.master_fd;
            // Each message is capped at 1 MiB below. Two shallow handoff queues
            // keep only a few MiB resident per session while backpressuring read(2).
            let (mut tx, mut rx) = iced::futures::channel::mpsc::channel::<Message>(2);
            // Self-pipe so dropping this subscription (session/tab closed) wakes the
            // reader thread and stops it BEFORE it can read from a PTY fd whose
            // number may have been reused by a freshly spawned session.
            let (shutdown_r, shutdown_w) = match Pty::make_shutdown_pipe() {
                Ok((read_fd, write_fd)) => {
                    // SAFETY: make_shutdown_pipe returns two fresh owned fds.
                    unsafe {
                        (
                            OwnedFd::from_raw_fd(read_fd),
                            OwnedFd::from_raw_fd(write_fd),
                        )
                    }
                }
                Err(error) => {
                    log::error!("[PTY] failed to create reader shutdown pipe: {error}");
                    let _ = output.send(Message::PtyExited(id, fd, -1)).await;
                    return;
                }
            };
            let reader_fd = key.reader_fd;
            let spawn_result = std::thread::Builder::new()
                .name(format!("jterm3-pty-{id}"))
                .spawn(move || {
                    let reader_raw = reader_fd.as_raw_fd();
                    let shutdown_raw = shutdown_r.as_raw_fd();
                    // Drain everything currently readable into one message instead of
                    // emitting a separate message per 64 KiB read. Bursty output (e.g.
                    // `cat bigfile`) then triggers far fewer process/refresh/render
                    // cycles, while a lone keystroke still hits WouldBlock immediately
                    // and is delivered with no added latency. Capped so the UI gets a
                    // chance to repaint between very large bursts.
                    const COALESCE_CAP: usize = 1 << 20; // 1 MiB per message
                    let mut buf = vec![0u8; 65536];
                    loop {
                        match Pty::wait_fd_or_shutdown(reader_raw, shutdown_raw, 200) {
                            Ok(ReaderPoll::Shutdown) => break,
                            Ok(ReaderPoll::Timeout) => continue,
                            Ok(ReaderPoll::Data) => {
                                let mut acc: Vec<u8> = Vec::new();
                                let mut exited = false;
                                let mut errored = false;
                                loop {
                                    let n = unsafe {
                                        libc::read(
                                            reader_raw,
                                            buf.as_mut_ptr() as *mut libc::c_void,
                                            buf.len(),
                                        )
                                    };
                                    if n > 0 {
                                        acc.extend_from_slice(&buf[..n as usize]);
                                        if acc.len() >= COALESCE_CAP {
                                            break;
                                        }
                                    } else if n == 0 {
                                        exited = true;
                                        break;
                                    } else {
                                        let err = std::io::Error::last_os_error();
                                        if err.kind() == std::io::ErrorKind::WouldBlock {
                                            break;
                                        }
                                        if err.raw_os_error() == Some(libc::EINTR) {
                                            continue;
                                        }
                                        errored = true;
                                        break;
                                    }
                                }
                                if !acc.is_empty()
                                    && iced::futures::executor::block_on(
                                        tx.send(Message::PtyOutput(id, fd, acc)),
                                    )
                                    .is_err()
                                {
                                    break;
                                }
                                if exited {
                                    let _ = iced::futures::executor::block_on(
                                        tx.send(Message::PtyExited(id, fd, 0)),
                                    );
                                    break;
                                }
                                if errored {
                                    let _ = iced::futures::executor::block_on(
                                        tx.send(Message::PtyExited(id, fd, -1)),
                                    );
                                    break;
                                }
                            }
                            Err(_) => {
                                let _ = iced::futures::executor::block_on(
                                    tx.send(Message::PtyExited(id, fd, -1)),
                                );
                                break;
                            }
                        }
                    }
                });
            if let Err(error) = spawn_result {
                log::error!("[PTY] failed to spawn reader thread: {error}");
                let _ = output.send(Message::PtyExited(id, fd, -1)).await;
                return;
            }
            // Dropping this owned write end (subscription removed) signals the reader.
            let _shutdown_guard = shutdown_w;
            while let Some(msg) = rx.next().await {
                if output.send(msg).await.is_err() {
                    break;
                }
            }
        },
    )
}

/// Load keybindings from disk (merged onto defaults), logging any load error or
/// invalid binding so a malformed config degrades gracefully to the defaults.
fn load_keybindings() -> keybindings::KeyBindings {
    match keybindings::KeyBindings::load() {
        Ok(kb) => {
            for issue in kb.check_conflicts() {
                log::warn!("[keybindings] {issue}");
            }
            kb
        }
        Err(e) => {
            log::warn!("[keybindings] failed to load, using defaults: {e}");
            keybindings::KeyBindings::default()
        }
    }
}

/// Build the normalized binding string (e.g. `"ctrl+shift+t"`) for a key event,
/// matching the lowercase `modifier+...+key` format stored in keybindings.toml.
/// Returns `None` for keys that should never be treated as shortcuts — plain
/// character input (no Ctrl/Alt/Super) and unmappable named keys — so ordinary
/// typing is never swallowed by the keybinding layer.
fn key_to_binding_string(key: &keyboard::Key, mods: keyboard::Modifiers) -> Option<String> {
    use keyboard::key::Named;
    use keyboard::Key;
    let name: String = match key {
        Key::Character(s) => {
            // Shift alone just changes case; require a "real" modifier so typing
            // an uppercase letter can't trigger a command.
            if !(mods.control() || mods.alt() || mods.logo()) {
                return None;
            }
            match s.chars().next()?.to_ascii_lowercase() {
                '\\' => "backslash".to_string(),
                c => c.to_string(),
            }
        }
        Key::Named(named) => match named {
            Named::Tab => "tab",
            Named::Enter => "enter",
            Named::Escape => "escape",
            Named::Backspace => "backspace",
            Named::Delete => "delete",
            Named::Insert => "insert",
            Named::Home => "home",
            Named::End => "end",
            Named::PageUp => "pageup",
            Named::PageDown => "pagedown",
            Named::ArrowUp => "up",
            Named::ArrowDown => "down",
            Named::ArrowLeft => "left",
            Named::ArrowRight => "right",
            Named::Space => "space",
            Named::F1 => "f1",
            Named::F2 => "f2",
            Named::F3 => "f3",
            Named::F4 => "f4",
            Named::F5 => "f5",
            Named::F6 => "f6",
            Named::F7 => "f7",
            Named::F8 => "f8",
            Named::F9 => "f9",
            Named::F10 => "f10",
            Named::F11 => "f11",
            Named::F12 => "f12",
            _ => return None,
        }
        .to_string(),
        _ => return None,
    };
    let mut binding = String::new();
    if mods.control() {
        binding.push_str("ctrl+");
    }
    if mods.shift() {
        binding.push_str("shift+");
    }
    if mods.alt() {
        binding.push_str("alt+");
    }
    if mods.logo() {
        binding.push_str("super+");
    }
    binding.push_str(&name);
    Some(binding)
}

/// Flags describing which enhanced-keyboard protocols an application has
/// enabled, sampled from the focused terminal before encoding a key press.
#[derive(Clone, Copy, Default)]
struct KeyboardEnhancements {
    kitty_flags: u16,
    modify_other_keys: u16,
    format_other_keys: u16,
    report_all_keys: bool,
    application_keypad: bool,
}

/// Translate an iced key press into the bytes to send to the PTY.
fn encode_key(
    key: &keyboard::Key,
    location: keyboard::Location,
    mods: keyboard::Modifiers,
    text: Option<&str>,
    app_cursor: bool,
    enh: KeyboardEnhancements,
) -> Option<Vec<u8>> {
    use keyboard::key::Named;
    use keyboard::Key;

    let ctrl = mods.control();
    let alt = mods.alt();

    // Enhanced keyboard protocols (Kitty / xterm modifyOtherKeys) take
    // precedence when an app has enabled them. Unlike jterm2/egui, iced puts
    // committed text on this same key event; there is no second text event to
    // suppress. Skipping an alphanumeric key here would therefore violate
    // Kitty's report-all-keys mode and send plain text instead.
    if let Some(enc) = kitty_encode_key(key, mods, enh.kitty_flags) {
        return Some(enc);
    }
    if let Some(enc) = xterm_modify_other_keys_encode(
        key,
        mods,
        text,
        enh.modify_other_keys,
        enh.format_other_keys,
        enh.report_all_keys,
    ) {
        return Some(enc);
    }

    let csi = |c: &str| -> Vec<u8> { format!("\x1b[{c}").into_bytes() };
    let ss3 = |c: &str| -> Vec<u8> { format!("\x1bO{c}").into_bytes() };

    match key {
        Key::Named(named) => {
            let mut bytes = match named {
                Named::Enter => {
                    if enh.application_keypad && location == keyboard::Location::Numpad {
                        ss3("M")
                    } else {
                        vec![b'\r']
                    }
                }
                Named::Backspace => vec![if ctrl { 0x08 } else { 0x7f }],
                Named::Tab => {
                    if mods.shift() {
                        csi("Z")
                    } else {
                        vec![b'\t']
                    }
                }
                Named::Escape => vec![0x1b],
                Named::Space => vec![if ctrl { 0x00 } else { b' ' }],
                _ => {
                    return legacy_function_key_sequence(
                        named,
                        mods,
                        app_cursor,
                        enh.report_all_keys,
                    )
                }
            };
            if alt {
                bytes.insert(0, 0x1b);
            }
            Some(bytes)
        }
        Key::Character(s) => {
            let c = s.chars().next()?;
            if ctrl {
                // Map Ctrl+key to the corresponding control byte.
                let b = c.to_ascii_lowercase() as u8;
                let ctrl_byte = match b {
                    b'a'..=b'z' => b & 0x1f,
                    b'@' => 0,
                    b'[' => 0x1b,
                    b'\\' => 0x1c,
                    b']' => 0x1d,
                    b'^' => 0x1e,
                    b'_' => 0x1f,
                    b' ' => 0,
                    _ => return text.map(|t| t.as_bytes().to_vec()),
                };
                let mut v = Vec::new();
                if alt {
                    v.push(0x1b);
                }
                v.push(ctrl_byte);
                Some(v)
            } else if let Some(t) = text {
                let mut v = Vec::new();
                if alt {
                    v.push(0x1b);
                }
                v.extend_from_slice(t.as_bytes());
                Some(v)
            } else {
                let mut v = Vec::new();
                if alt {
                    v.push(0x1b);
                }
                v.extend_from_slice(s.as_bytes());
                Some(v)
            }
        }
        Key::Unidentified => text.map(|t| t.as_bytes().to_vec()),
    }
}

/// Encode the legacy xterm/terminfo functional-key family. Modified cursor,
/// editing, and function keys carry a parameter instead of losing Ctrl/Shift
/// or being represented as an ambiguous ESC prefix.
fn legacy_function_key_sequence(
    named: &keyboard::key::Named,
    mods: keyboard::Modifiers,
    app_cursor: bool,
    force_modifier: bool,
) -> Option<Vec<u8>> {
    use keyboard::key::Named;

    let csi = |body: &str| format!("\x1b[{body}").into_bytes();
    let ss3 = |final_byte: char| format!("\x1bO{final_byte}").into_bytes();
    let has_modifier = mods.shift() || mods.alt() || mods.control() || mods.logo();
    let modified = force_modifier || has_modifier;
    let modifier = keyboard_modifier_value(mods);
    let cursor = |final_byte: char| {
        if modified {
            csi(&format!("1;{modifier}{final_byte}"))
        } else if app_cursor {
            ss3(final_byte)
        } else {
            csi(&final_byte.to_string())
        }
    };
    let tilde = |code: u8| {
        if modified {
            csi(&format!("{code};{modifier}~"))
        } else {
            csi(&format!("{code}~"))
        }
    };
    let function = |final_byte: char| {
        if modified {
            csi(&format!("1;{modifier}{final_byte}"))
        } else {
            ss3(final_byte)
        }
    };

    Some(match named {
        Named::ArrowUp => cursor('A'),
        Named::ArrowDown => cursor('B'),
        Named::ArrowRight => cursor('C'),
        Named::ArrowLeft => cursor('D'),
        Named::Home => cursor('H'),
        Named::End => cursor('F'),
        Named::PageUp => tilde(5),
        Named::PageDown => tilde(6),
        Named::Delete => tilde(3),
        Named::Insert => tilde(2),
        Named::F1 => function('P'),
        Named::F2 => function('Q'),
        Named::F3 => function('R'),
        Named::F4 => function('S'),
        Named::F5 => tilde(15),
        Named::F6 => tilde(17),
        Named::F7 => tilde(18),
        Named::F8 => tilde(19),
        Named::F9 => tilde(20),
        Named::F10 => tilde(21),
        Named::F11 => tilde(23),
        Named::F12 => tilde(24),
        _ => return None,
    })
}

/// The base Unicode codepoint a key reports under the Kitty keyboard protocol.
/// Kitty uses the unshifted/lowercase form for text keys and C0 values for the
/// handful of named keys that have legacy control-byte encodings.
fn kitty_text_key_code(key: &keyboard::Key) -> Option<u32> {
    use keyboard::key::Named;
    use keyboard::Key;

    match key {
        Key::Character(s) => s.chars().next()?.to_lowercase().next().map(u32::from),
        Key::Named(Named::Escape) => Some(27),
        Key::Named(Named::Enter) => Some(13),
        Key::Named(Named::Tab) => Some(9),
        Key::Named(Named::Backspace) => Some(127),
        Key::Named(Named::Space) => Some(32),
        _ => None,
    }
}

/// Codepoint for the xterm modifyOtherKeys report; like [`kitty_text_key_code`]
/// but prefers iced's committed text when modifiers changed the character.
fn text_key_code(
    key: &keyboard::Key,
    mods: keyboard::Modifiers,
    text: Option<&str>,
) -> Option<u32> {
    let codepoint = kitty_text_key_code(key)?;
    if mods.shift() {
        if let Some(character) = text.and_then(|value| value.chars().find(|c| !c.is_control())) {
            return Some(character as u32);
        }
        if let keyboard::Key::Character(s) = key {
            return s.chars().next()?.to_uppercase().next().map(u32::from);
        }
    }
    Some(codepoint)
}

/// The CSI-u / modifyOtherKeys modifier value: a bitfield + 1.
fn keyboard_modifier_value(mods: keyboard::Modifiers) -> u8 {
    let mut bits = 0u8;
    if mods.shift() {
        bits |= 0b1;
    }
    if mods.alt() {
        bits |= 0b10;
    }
    if mods.control() {
        bits |= 0b100;
    }
    if mods.logo() {
        bits |= 0b1000;
    }
    bits + 1
}

/// Encode a key press as a Kitty keyboard protocol report (`CSI codepoint;mod u`)
/// when the app has enabled disambiguation or report-all-keys. Returns `None`
/// when the protocol is inactive or the key needs no special report.
fn kitty_encode_key(
    key: &keyboard::Key,
    mods: keyboard::Modifiers,
    kitty_flags: u16,
) -> Option<Vec<u8>> {
    let disambiguate = (kitty_flags & 0b1) != 0;
    let report_all_keys = (kitty_flags & 0b1000) != 0;
    if !disambiguate && !report_all_keys {
        return None;
    }
    let codepoint = kitty_text_key_code(key)?;
    let legacy_c0_exception = matches!(
        key,
        keyboard::Key::Named(
            keyboard::key::Named::Enter
                | keyboard::key::Named::Tab
                | keyboard::key::Named::Backspace
        )
    );
    if legacy_c0_exception && !report_all_keys {
        return None;
    }
    let is_escape = matches!(key, keyboard::Key::Named(keyboard::key::Named::Escape));
    let should_encode = report_all_keys || is_escape || mods.control() || mods.alt() || mods.logo();
    if !should_encode {
        return None;
    }
    Some(format!("\x1b[{};{}u", codepoint, keyboard_modifier_value(mods)).into_bytes())
}

/// Encode a key press under xterm's modifyOtherKeys/formatOtherKeys regime.
fn xterm_modify_other_keys_encode(
    key: &keyboard::Key,
    mods: keyboard::Modifiers,
    text: Option<&str>,
    modify_other_keys: u16,
    format_other_keys: u16,
    report_all_keys: bool,
) -> Option<Vec<u8>> {
    let codepoint = text_key_code(key, mods, text)?;
    let modifier_value = keyboard_modifier_value(mods);
    let has_non_shift_modifier = mods.control() || mods.alt() || mods.logo();
    let should_encode = if report_all_keys {
        true
    } else {
        match modify_other_keys {
            0 => false,
            1 => mods.alt() || mods.logo(),
            2 => has_non_shift_modifier || mods.shift(),
            _ => true,
        }
    };
    if !should_encode {
        return None;
    }
    if format_other_keys == 1 || report_all_keys {
        Some(format!("\x1b[{};{}u", codepoint, modifier_value).into_bytes())
    } else {
        Some(format!("\x1b[27;{};{}~", modifier_value, codepoint).into_bytes())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use iced::keyboard::key::Named;

    #[test]
    fn app_chrome_shortcuts_keep_palette_help_switcher_and_f12_contract() {
        let ctrl_shift = keyboard::Modifiers::CTRL | keyboard::Modifiers::SHIFT;
        let character = |s: &str| keyboard::Key::Character(s.into());

        assert_eq!(
            chrome_shortcut(&character("p"), ctrl_shift),
            Some(ChromeShortcut::CommandPalette)
        );
        assert_eq!(
            chrome_shortcut(&character("/"), ctrl_shift),
            Some(ChromeShortcut::Help)
        );
        assert_eq!(
            chrome_shortcut(&character("l"), ctrl_shift),
            Some(ChromeShortcut::TabSwitcher)
        );
        assert_eq!(
            chrome_shortcut(&keyboard::Key::Named(Named::F12), keyboard::Modifiers::NONE),
            Some(ChromeShortcut::Debug)
        );

        assert_eq!(chrome_shortcut(&character("g"), ctrl_shift), None);
        assert_eq!(chrome_shortcut(&character("k"), ctrl_shift), None);
        assert_eq!(
            chrome_shortcut(&character("p"), keyboard::Modifiers::CTRL),
            None
        );
    }

    #[test]
    fn physical_key_events_match_sidebar_focus_and_resize_binding_names() {
        assert_eq!(
            key_to_binding_string(
                &keyboard::Key::Character("\\".into()),
                keyboard::Modifiers::CTRL
            )
            .as_deref(),
            Some("ctrl+backslash")
        );

        let focus_mods = keyboard::Modifiers::CTRL | keyboard::Modifiers::ALT;
        let resize_mods = focus_mods | keyboard::Modifiers::SHIFT;
        let cases = [
            (Named::ArrowLeft, focus_mods, "ctrl+alt+left"),
            (Named::ArrowRight, focus_mods, "ctrl+alt+right"),
            (Named::ArrowUp, focus_mods, "ctrl+alt+up"),
            (Named::ArrowDown, focus_mods, "ctrl+alt+down"),
            (Named::ArrowLeft, resize_mods, "ctrl+shift+alt+left"),
            (Named::ArrowRight, resize_mods, "ctrl+shift+alt+right"),
            (Named::ArrowUp, resize_mods, "ctrl+shift+alt+up"),
            (Named::ArrowDown, resize_mods, "ctrl+shift+alt+down"),
        ];
        for (named, modifiers, expected) in cases {
            assert_eq!(
                key_to_binding_string(&keyboard::Key::Named(named), modifiers).as_deref(),
                Some(expected),
                "{named:?}"
            );
        }
    }

    #[test]
    fn pane_focus_is_axis_aware_and_never_wraps_at_an_edge() {
        assert_eq!(
            directional_pane_target(SplitMode::Vertical, 0, 3, PaneDirection::Right),
            Some(1)
        );
        assert_eq!(
            directional_pane_target(SplitMode::Vertical, 2, 3, PaneDirection::Left),
            Some(1)
        );
        assert_eq!(
            directional_pane_target(SplitMode::Horizontal, 0, 2, PaneDirection::Down),
            Some(1)
        );
        assert_eq!(
            directional_pane_target(SplitMode::Horizontal, 1, 2, PaneDirection::Up),
            Some(0)
        );

        assert_eq!(
            directional_pane_target(SplitMode::Vertical, 0, 3, PaneDirection::Left),
            None,
            "left edge must not wrap"
        );
        assert_eq!(
            directional_pane_target(SplitMode::Horizontal, 2, 3, PaneDirection::Down),
            None,
            "bottom edge must not wrap"
        );
        assert_eq!(
            directional_pane_target(SplitMode::Vertical, 0, 2, PaneDirection::Down),
            None,
            "perpendicular direction must not change focus"
        );
        assert_eq!(
            directional_pane_target(SplitMode::Single, 0, 1, PaneDirection::Right),
            None
        );
    }

    #[test]
    fn resize_arrows_pick_the_divider_on_their_own_side() {
        // Middle pane of three: right arrow moves its right divider (1), left
        // arrow moves its left divider (0).
        assert_eq!(
            resize_divider_target(SplitMode::Vertical, 1, 3, PaneDirection::Right),
            Some(1)
        );
        assert_eq!(
            resize_divider_target(SplitMode::Vertical, 1, 3, PaneDirection::Left),
            Some(0)
        );
        // Edge panes fall back to their only divider so the arrow still works.
        assert_eq!(
            resize_divider_target(SplitMode::Vertical, 2, 3, PaneDirection::Right),
            Some(1)
        );
        assert_eq!(
            resize_divider_target(SplitMode::Vertical, 0, 3, PaneDirection::Left),
            Some(0)
        );
        // Perpendicular arrows and single panes do nothing.
        assert_eq!(
            resize_divider_target(SplitMode::Vertical, 0, 3, PaneDirection::Up),
            None
        );
        assert_eq!(
            resize_divider_target(SplitMode::Horizontal, 0, 1, PaneDirection::Down),
            None
        );
    }

    #[test]
    fn divider_shares_clamp_to_the_pane_minimum_and_snap_when_close() {
        let mut ratios = vec![0.5, 0.5];
        // Snap: near an even pair split settles exactly at half the pair.
        assert!(set_divider_share(
            &mut ratios,
            0,
            0.5 + SPLIT_SNAP_EPSILON * 2.0,
            true
        ));
        assert!(set_divider_share(
            &mut ratios,
            0,
            0.5 + SPLIT_SNAP_EPSILON * 0.5,
            true
        ));
        assert_eq!(ratios, vec![0.5, 0.5]);
        // Clamping: neither pane of the pair may go below the minimum.
        assert!(set_divider_share(&mut ratios, 0, 0.0, false));
        assert!((ratios[0] - PANE_RATIO_MIN).abs() < 1e-6);
        assert!((ratios[1] - (1.0 - PANE_RATIO_MIN)).abs() < 1e-6);
        // Only the pair around the divider moves; other panes are untouched.
        let mut three = vec![0.25, 0.25, 0.5];
        assert!(set_divider_share(&mut three, 0, 0.3, false));
        assert!((three[0] - 0.3).abs() < 1e-6);
        assert!((three[1] - 0.2).abs() < 1e-6);
        assert!((three[2] - 0.5).abs() < 1e-6);
        // Out-of-range divider index is rejected.
        assert!(!set_divider_share(&mut three, 2, 0.4, false));
    }

    #[test]
    fn pane_shares_split_in_half_on_insert_and_refold_on_remove() {
        let mut ratios = vec![0.5, 0.5];
        insert_pane_share(&mut ratios, 1);
        assert_eq!(ratios, vec![0.5, 0.25, 0.25]);
        // Removing the middle pane folds its share into the previous one.
        remove_pane_share(&mut ratios, 1);
        assert_eq!(ratios, vec![0.75, 0.25]);
        // Removing the first pane folds forward into the new first.
        remove_pane_share(&mut ratios, 0);
        assert_eq!(ratios, vec![1.0]);
        // Splitting a pane too thin to halve equalizes the whole row instead.
        let mut thin = vec![1.0 - PANE_RATIO_MIN, PANE_RATIO_MIN];
        insert_pane_share(&mut thin, 1);
        assert_eq!(thin.len(), 3);
        assert!(thin.iter().all(|r| (r - 1.0 / 3.0).abs() < 1e-6));
    }

    #[test]
    fn session_last_targets_the_final_index_without_underflow() {
        assert_eq!(last_session_index(0), None);
        assert_eq!(last_session_index(1), Some(0));
        assert_eq!(last_session_index(12), Some(11));
    }

    #[test]
    fn bracketed_paste_framing_preserves_payload() {
        assert_eq!(
            wrap_bracketed_paste(b"hello\nworld".to_vec()),
            b"\x1b[200~hello\nworld\x1b[201~"
        );
        assert_eq!(wrap_bracketed_paste(Vec::new()), b"\x1b[200~\x1b[201~");
    }

    #[test]
    fn tiny_pty_writes_are_coalesced_and_entry_bounded() {
        let mut queue = std::collections::VecDeque::new();
        for _ in 0..1000 {
            Session::push_queue_copy(&mut queue, b"x", false);
        }
        assert_eq!(queue.len(), 1);
        assert_eq!(queue[0].data.len(), 1000);

        Session::push_queue_copy(&mut queue, b"response", true);
        Session::push_queue_copy(&mut queue, b"later-input", false);
        assert_eq!(queue.len(), 3, "different classes must preserve FIFO order");
        assert!(!queue[0].response);
        assert!(queue[1].response);
        assert!(!queue[2].response);

        queue.resize_with(MAX_PTY_QUEUE_ENTRIES, || PtyWriteChunk {
            data: Vec::new(),
            response: false,
        });
        queue.back_mut().expect("queue is populated").data = vec![0; PTY_QUEUE_COALESCE_BYTES];
        assert!(!Session::queue_accepts_entry(&queue, 1, false));
    }

    #[test]
    fn modified_function_keys_keep_their_xterm_modifier_parameters() {
        let cases = [
            (
                Named::ArrowLeft,
                keyboard::Modifiers::CTRL,
                false,
                b"\x1b[1;5D".as_slice(),
            ),
            (
                Named::F5,
                keyboard::Modifiers::SHIFT,
                false,
                b"\x1b[15;2~".as_slice(),
            ),
            (
                Named::PageDown,
                keyboard::Modifiers::ALT,
                false,
                b"\x1b[6;3~".as_slice(),
            ),
            (
                Named::F1,
                keyboard::Modifiers::CTRL | keyboard::Modifiers::SHIFT,
                false,
                b"\x1b[1;6P".as_slice(),
            ),
            (
                Named::ArrowUp,
                keyboard::Modifiers::NONE,
                true,
                b"\x1bOA".as_slice(),
            ),
        ];

        for (named, modifiers, app_cursor, expected) in cases {
            let encoded = encode_key(
                &keyboard::Key::Named(named),
                keyboard::Location::Standard,
                modifiers,
                None,
                app_cursor,
                KeyboardEnhancements::default(),
            );
            assert_eq!(encoded.as_deref(), Some(expected), "{named:?}");
        }

        let report_all_arrow = encode_key(
            &keyboard::Key::Named(Named::ArrowUp),
            keyboard::Location::Standard,
            keyboard::Modifiers::NONE,
            None,
            true,
            KeyboardEnhancements {
                report_all_keys: true,
                ..Default::default()
            },
        );
        assert_eq!(report_all_arrow.as_deref(), Some(&b"\x1b[1;1A"[..]));
    }

    #[test]
    fn legacy_control_keys_preserve_ctrl_and_alt_semantics() {
        let ctrl_backspace = encode_key(
            &keyboard::Key::Named(Named::Backspace),
            keyboard::Location::Standard,
            keyboard::Modifiers::CTRL,
            None,
            false,
            KeyboardEnhancements::default(),
        );
        assert_eq!(ctrl_backspace.as_deref(), Some(&b"\x08"[..]));

        let ctrl_alt_backspace = encode_key(
            &keyboard::Key::Named(Named::Backspace),
            keyboard::Location::Standard,
            keyboard::Modifiers::CTRL | keyboard::Modifiers::ALT,
            None,
            false,
            KeyboardEnhancements::default(),
        );
        assert_eq!(ctrl_alt_backspace.as_deref(), Some(&b"\x1b\x08"[..]));

        let ctrl_space = encode_key(
            &keyboard::Key::Named(Named::Space),
            keyboard::Location::Standard,
            keyboard::Modifiers::CTRL,
            Some(" "),
            false,
            KeyboardEnhancements::default(),
        );
        assert_eq!(ctrl_space.as_deref(), Some(&b"\0"[..]));
    }

    #[test]
    fn kitty_report_all_and_disambiguation_do_not_fall_back_to_plain_text() {
        let report_all = KeyboardEnhancements {
            kitty_flags: 0b1000,
            report_all_keys: true,
            ..Default::default()
        };
        let letter = encode_key(
            &keyboard::Key::Character("a".into()),
            keyboard::Location::Standard,
            keyboard::Modifiers::NONE,
            Some("a"),
            false,
            report_all,
        );
        assert_eq!(letter.as_deref(), Some(&b"\x1b[97;1u"[..]));

        let enter = encode_key(
            &keyboard::Key::Named(Named::Enter),
            keyboard::Location::Standard,
            keyboard::Modifiers::NONE,
            None,
            false,
            report_all,
        );
        assert_eq!(enter.as_deref(), Some(&b"\x1b[13;1u"[..]));

        let disambiguate = KeyboardEnhancements {
            kitty_flags: 0b1,
            ..Default::default()
        };
        let escape = encode_key(
            &keyboard::Key::Named(Named::Escape),
            keyboard::Location::Standard,
            keyboard::Modifiers::NONE,
            None,
            false,
            disambiguate,
        );
        assert_eq!(escape.as_deref(), Some(&b"\x1b[27;1u"[..]));

        let legacy_enter = encode_key(
            &keyboard::Key::Named(Named::Enter),
            keyboard::Location::Standard,
            keyboard::Modifiers::NONE,
            None,
            false,
            disambiguate,
        );
        assert_eq!(legacy_enter.as_deref(), Some(&b"\r"[..]));

        let ctrl_super = encode_key(
            &keyboard::Key::Character("a".into()),
            keyboard::Location::Standard,
            keyboard::Modifiers::CTRL | keyboard::Modifiers::LOGO,
            None,
            false,
            disambiguate,
        );
        assert_eq!(ctrl_super.as_deref(), Some(&b"\x1b[97;13u"[..]));
    }

    #[test]
    fn modify_other_keys_handles_shifted_text_and_level_three() {
        let shifted_symbol = encode_key(
            &keyboard::Key::Character("1".into()),
            keyboard::Location::Standard,
            keyboard::Modifiers::SHIFT,
            Some("!"),
            false,
            KeyboardEnhancements {
                modify_other_keys: 2,
                ..Default::default()
            },
        );
        assert_eq!(shifted_symbol.as_deref(), Some(&b"\x1b[27;2;33~"[..]));

        let shifted_tab = encode_key(
            &keyboard::Key::Named(Named::Tab),
            keyboard::Location::Standard,
            keyboard::Modifiers::SHIFT,
            None,
            false,
            KeyboardEnhancements {
                modify_other_keys: 2,
                ..Default::default()
            },
        );
        assert_eq!(shifted_tab.as_deref(), Some(&b"\x1b[27;2;9~"[..]));

        let unmodified_level_three = encode_key(
            &keyboard::Key::Character("x".into()),
            keyboard::Location::Standard,
            keyboard::Modifiers::NONE,
            Some("x"),
            false,
            KeyboardEnhancements {
                modify_other_keys: 3,
                ..Default::default()
            },
        );
        assert_eq!(
            unmodified_level_three.as_deref(),
            Some(&b"\x1b[27;1;120~"[..])
        );
    }

    #[test]
    fn enter_honors_key_location_in_application_keypad_mode() {
        let plain = encode_key(
            &keyboard::Key::Named(Named::Enter),
            keyboard::Location::Standard,
            keyboard::Modifiers::default(),
            None,
            false,
            KeyboardEnhancements::default(),
        );
        assert_eq!(plain.as_deref(), Some(&b"\r"[..]));

        let standard_in_keypad_mode = encode_key(
            &keyboard::Key::Named(Named::Enter),
            keyboard::Location::Standard,
            keyboard::Modifiers::default(),
            None,
            false,
            KeyboardEnhancements {
                application_keypad: true,
                ..Default::default()
            },
        );
        assert_eq!(standard_in_keypad_mode.as_deref(), Some(&b"\r"[..]));

        let numpad_in_keypad_mode = encode_key(
            &keyboard::Key::Named(Named::Enter),
            keyboard::Location::Numpad,
            keyboard::Modifiers::default(),
            None,
            false,
            KeyboardEnhancements {
                application_keypad: true,
                ..Default::default()
            },
        );
        assert_eq!(numpad_in_keypad_mode.as_deref(), Some(&b"\x1bOM"[..]));
    }
}
