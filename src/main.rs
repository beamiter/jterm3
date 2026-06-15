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
mod scripting;
mod session_persistence;
mod sidebar;
mod terminal;
mod terminal_view;
mod theme;

use std::os::unix::io::RawFd;
use std::sync::Arc;

use config::Config;
use iced::widget::{
    button, checkbox, column, container, mouse_area, pick_list, row, scrollable, slider, stack,
    text, text_input, Space,
};
use iced::{keyboard, Element, Length, Size, Subscription, Task};
use pty::Pty;
use terminal::{TerminalCell, TerminalState};
use terminal_view::{KittyRender, Metrics, MouseButton, MouseInput, TermWidget};
use theme::Theme;

/// Height reserved for the tab bar at the top of the window.
const TAB_BAR_H: f32 = 30.0;
/// Height reserved for the status bar at the bottom of the window.
const STATUS_BAR_H: f32 = 22.0;
/// Default width of the file-tree sidebar when shown.
const SIDEBAR_W: f32 = 220.0;
/// Thickness of the divider drawn between split panes (also its drag hit area).
const DIVIDER: f32 = 6.0;

/// Stable widget ids so the overlays' text inputs can be focused on open.
static SEARCH_INPUT_ID: once_cell::sync::Lazy<iced::widget::Id> =
    once_cell::sync::Lazy::new(|| iced::widget::Id::new("jterm-search-input"));
static PALETTE_INPUT_ID: once_cell::sync::Lazy<iced::widget::Id> =
    once_cell::sync::Lazy::new(|| iced::widget::Id::new("jterm-palette-input"));

/// Which content the left sidebar dock currently shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SidebarPanel {
    /// File-tree browser (doubles as a path picker).
    Files,
    /// Vertical session tab list.
    Tabs,
}

/// How the active view is split into panes (MVP: at most two panes).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SplitMode {
    /// A single pane filling the terminal area.
    Single,
    /// Two panes side by side (left | right).
    Vertical,
    /// Two panes stacked (top / bottom).
    Horizontal,
}

/// Resolve the terminal font from the configured family name. iced resolves
/// system-installed families by name (via cosmic-text); an empty name or a
/// missing family falls back to the built-in monospace font. The name is leaked
/// because `iced::Font::with_name` requires `&'static str`; family changes are
/// rare so the leak is negligible.
fn resolve_mono_font(family: &str) -> iced::Font {
    let f = family.trim();
    if f.is_empty() {
        iced::Font::MONOSPACE
    } else {
        iced::Font::with_name(Box::leak(f.to_string().into_boxed_str()))
    }
}

fn main() -> iced::Result {
    env_logger::init();
    let config = Config::load();
    let win = iced::window::Settings {
        size: Size::new(config.initial_width, config.initial_height),
        ..Default::default()
    };
    iced::application(Jterm::new, Jterm::update, Jterm::view)
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
    PtyOutput(RawFd, Vec<u8>),
    PtyExited(RawFd, i32),
    Key(keyboard::Event),
    /// An input-method (IME) composition event: open/close, pre-edit updates,
    /// and committed text.
    Ime(iced::advanced::input_method::Event),
    ModifiersChanged(keyboard::Modifiers),
    /// A mouse interaction within pane `usize` (index into `panes`).
    MousePane(usize, MouseInput),
    Pasted(Option<String>),
    Resized(Size),
    Focus(bool),
    NewSession,
    CloseTab(usize),
    SelectTab(usize),
    TabHover(Option<usize>),
    ToggleSidebar,
    SetSidebarPanel(SidebarPanel),
    SetTabPosition(config::TabPosition),
    SidebarToggleNode(std::path::PathBuf),
    SidebarInsertPath(std::path::PathBuf),
    DividerDragStart,
    DividerDragMove(iced::Point),
    DividerDragEnd,
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
    grid: Arc<Vec<Vec<TerminalCell>>>,
    cursor: (usize, usize),
    cursor_visible: bool,
    /// Cached working directory, refreshed periodically so the status bar can
    /// display it without a `readlink` syscall on every render frame.
    cwd_cache: Option<String>,
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
        let mut terminal = TerminalState::new(cols, rows);
        terminal.set_max_scrollback(config.scrollback_lines);
        let grid = terminal.get_visible_cells();
        let cursor = terminal.get_cursor_pos();
        let cursor_visible = terminal.is_cursor_visible();
        Some(Session {
            id,
            terminal,
            pty,
            master_fd,
            grid,
            cursor,
            cursor_visible,
            cwd_cache: None,
        })
    }

    /// Tab label: OSC-set window title, falling back to a session number.
    fn label(&self) -> String {
        let t = self.terminal.window_title.trim();
        if t.is_empty() {
            format!("Session {}", self.id + 1)
        } else {
            t.to_string()
        }
    }

    fn refresh(&mut self) {
        self.grid = self.terminal.get_visible_cells();
        self.cursor = self.terminal.get_cursor_pos();
        self.cursor_visible = self.terminal.is_cursor_visible();
    }

    fn flush_responses(&mut self) {
        let out = self.terminal.get_output();
        if !out.is_empty() {
            self.write_pty(&out);
        }
    }

    fn write_pty(&mut self, data: &[u8]) {
        let mut written = 0usize;
        while written < data.len() {
            match self.pty.write(&data[written..]) {
                // Kernel write buffer full: wait until the fd drains, then retry
                // so large pastes are not silently truncated.
                Ok(0) => match Pty::wait_fd_writable(self.master_fd, 1000) {
                    Ok(true) => {}
                    // Timed out or fd dead — give up rather than spin forever.
                    Ok(false) | Err(_) => break,
                },
                Ok(n) => written += n,
                Err(_) => break,
            }
        }
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
    search: search::SearchState,
    palette: command_palette::PaletteState,
    keybindings: keybindings::KeyBindings,
    config_panel_open: bool,
    help_open: bool,
    debug_open: bool,
    /// Blink clock phase, toggled by a timer; drives blinking-attribute cells.
    blink_on: bool,
    win_size: Size,
    config_mtime: Option<std::time::SystemTime>,
    link_detector: link::LinkDetector,
    links: Vec<link::Link>,
    /// `(active, grid_version, scroll_offset)` the cached `links` were computed for.
    links_cache_key: Option<(usize, u64, usize)>,
    /// Cached GPU image handles keyed by Kitty image id → (handle, decoded byte len).
    /// Stable ids let the renderer reuse the uploaded texture across frames.
    kitty_handles: std::collections::HashMap<u32, (iced::advanced::image::Handle, usize)>,
    /// Last persisted session-snapshot JSON, to skip redundant disk writes.
    last_session_save: Option<String>,
    /// Set when session state that feeds the snapshot may have changed (PTY
    /// output can move the cwd, tab switches move the active index). The periodic
    /// save is skipped while this is false, so a fully idle app does no per-tab
    /// `readlink` or JSON serialization on every tick.
    session_dirty: bool,
    /// Diagnostics (Ctrl+Shift+G): wall-clock microseconds spent ingesting the
    /// most recent PTY-output batch (parse + refresh) and its byte count, used
    /// to derive a throughput figure for profiling.
    last_ingest_us: u128,
    last_ingest_bytes: usize,
    /// Current pane layout of the active view.
    split_mode: SplitMode,
    /// Session indices shown as panes (length 1 in `Single`, else 2). Invariant:
    /// `panes[focused_pane] == active`.
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
    /// Split ratio for the first pane (0.1..=0.9); adjusted by dragging the
    /// divider. 0.5 is an even split.
    split_ratio: f32,
    /// In-progress divider drag: the layout axis is implied by `split_mode`.
    dragging_divider: bool,
    /// Tab index the pointer is currently hovering (drives close-button reveal).
    hovered_tab: Option<usize>,
    /// Held for the process lifetime to enforce single-instance behavior. When
    /// `None`, another instance already holds the lock and this one runs fresh
    /// (no session restore, no snapshot writes) to avoid clobbering its history.
    _instance_lock: Option<std::fs::File>,
    is_first_instance: bool,
}

impl Jterm {
    fn new() -> (Self, Task<Message>) {
        let config = Config::load();
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

        // Restore prior tabs (their cwds + active index) when enabled and we are
        // the first instance; otherwise start with a single default session.
        let (sessions, active, next_id) =
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

        let app = Jterm {
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
            search: search::SearchState::new(),
            palette: command_palette::PaletteState::new(),
            keybindings: load_keybindings(),
            config_panel_open: false,
            help_open: false,
            debug_open: false,
            blink_on: true,
            win_size,
            config_mtime,
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
            split_ratio: 0.5,
            dragging_divider: false,
            hovered_tab: None,
            _instance_lock: instance_lock,
            is_first_instance,
        };
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
            sess.terminal.set_max_scrollback(self.config.scrollback_lines);
            if resized {
                sess.terminal.on_resize(cols, rows);
                let _ = sess.pty.resize(cols, rows);
            }
            sess.refresh();
        }
        self.relayout();
    }

    /// Whether the left dock is shown. Follows the manual `sidebar_open` toggle
    /// in both tab-position modes, so the dock can always be collapsed.
    fn dock_open(&self) -> bool {
        self.sidebar_open
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

    /// Current sidebar width (0 when hidden).
    fn sidebar_width(&self) -> f32 {
        if self.dock_open() {
            SIDEBAR_W
        } else {
            0.0
        }
    }

    fn session_by_fd(&mut self, fd: RawFd) -> Option<&mut Session> {
        self.sessions.iter_mut().find(|s| s.master_fd == fd)
    }

    /// Startup session setup: when `restore_session` is enabled and a snapshot
    /// exists, respawn one session per saved tab at its recorded cwd; otherwise
    /// (or on any failure) fall back to a single default session.
    fn restore_or_spawn(
        config: &Config,
        cols: usize,
        rows: usize,
        is_first_instance: bool,
    ) -> (Vec<Session>, usize, usize) {
        let default = |id_start: usize| {
            let s = Session::spawn(config, id_start, cols, rows, None)
                .expect("failed to spawn PTY");
            (vec![s], 0usize, id_start + 1)
        };
        if !config.restore_session || !is_first_instance {
            return default(0);
        }
        let Ok(path) = Config::session_history_path() else {
            return default(0);
        };
        let snapshot = match session_persistence::SessionsSnapshot::load(&path) {
            Ok(s) if !s.sessions.is_empty() => s,
            _ => return default(0),
        };
        let mut sessions = Vec::new();
        let mut next_id = 0usize;
        for snap in &snapshot.sessions {
            if let Some(sess) =
                Session::spawn(config, next_id, cols, rows, snap.cwd.as_deref())
            {
                sessions.push(sess);
                next_id += 1;
            }
        }
        if sessions.is_empty() {
            return default(0);
        }
        let active = snapshot
            .active_index
            .unwrap_or(0)
            .min(sessions.len() - 1);
        eprintln!(
            "[SessionPersistence] Restored {} session(s) from {}",
            sessions.len(),
            path.display()
        );
        (sessions, active, next_id)
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
        let snapshot = session_persistence::SessionsSnapshot::new(snaps, Some(self.active));
        let Some(json) = snapshot.to_json() else {
            return;
        };
        if self.last_session_save.as_deref() == Some(json.as_str()) {
            return;
        }
        if let Ok(path) = Config::session_history_path() {
            if snapshot.save(&path).is_ok() {
                self.last_session_save = Some(json);
            }
        }
    }

    fn new_session(&mut self) {
        let cwd = self.sessions.get(self.active).and_then(|s| s.cwd());
        if let Some(sess) =
            Session::spawn(&self.config, self.next_id, self.cols, self.rows, cwd.as_deref())
        {
            self.next_id += 1;
            let insert = (self.active + 1).min(self.sessions.len());
            self.sessions.insert(insert, sess);
            self.active = insert;
            self.unsplit();
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
        let _ = sess.pty.terminate();
        if self.active >= self.sessions.len() {
            self.active = self.sessions.len() - 1;
        } else if index < self.active {
            self.active -= 1;
        }
        self.unsplit();
        self.save_session_snapshot();
        Task::none()
    }

    fn next_session(&mut self) {
        if !self.sessions.is_empty() {
            self.active = (self.active + 1) % self.sessions.len();
            self.session_dirty = true;
            self.unsplit();
        }
    }

    fn prev_session(&mut self) {
        if !self.sessions.is_empty() {
            self.active = (self.active + self.sessions.len() - 1) % self.sessions.len();
            self.session_dirty = true;
            self.unsplit();
        }
    }

    fn jump_session(&mut self, index: usize) {
        if index < self.sessions.len() {
            self.active = index;
            self.session_dirty = true;
            self.unsplit();
        }
    }

    /// Per-pane (cols, rows) for the current split mode and window size.
    fn pane_grid(&self, pane_pos: usize) -> (usize, usize) {
        let term_h = self.term_height();
        let term_w = self.term_width();
        // Fraction of the available space this pane occupies.
        let frac = if pane_pos == 0 {
            self.split_ratio
        } else {
            1.0 - self.split_ratio
        };
        match self.split_mode {
            SplitMode::Single => (self.cols, self.rows),
            SplitMode::Vertical => {
                let pane_w = ((term_w - DIVIDER) * frac).max(0.0);
                self.metrics
                    .grid_size((pane_w - terminal_view::SCROLLBAR_WIDTH).max(0.0), term_h)
            }
            SplitMode::Horizontal => {
                let pane_h = ((term_h - DIVIDER) * frac).max(0.0);
                self.metrics
                    .grid_size((term_w - terminal_view::SCROLLBAR_WIDTH).max(0.0), pane_h)
            }
        }
    }

    /// Resize one session's terminal + PTY (no-op when already that size).
    fn resize_session(&mut self, index: usize, cols: usize, rows: usize) {
        if let Some(sess) = self.sessions.get_mut(index) {
            if sess.terminal.get_dimensions() != (cols, rows) {
                sess.terminal.on_resize(cols, rows);
                let _ = sess.pty.resize(cols, rows);
            }
            sess.refresh();
        }
    }

    /// Resize the currently displayed pane session(s) to fit the layout.
    fn relayout(&mut self) {
        match self.split_mode {
            SplitMode::Single => {
                let (c, r) = (self.cols, self.rows);
                self.resize_session(self.active, c, r);
            }
            _ => {
                for (pos, idx) in self.panes.clone().into_iter().enumerate() {
                    let (c, r) = self.pane_grid(pos);
                    self.resize_session(idx, c, r);
                }
            }
        }
    }

    /// Collapse back to a single pane showing the active session.
    fn unsplit(&mut self) {
        if self.split_mode == SplitMode::Single {
            self.panes = vec![self.active];
            self.focused_pane = 0;
            return;
        }
        self.split_mode = SplitMode::Single;
        self.focused_pane = 0;
        self.panes = vec![self.active];
        self.relayout();
    }

    /// Split the active view in two, spawning a fresh sibling session at the
    /// active session's cwd as the second pane. No-op if already split (max 2).
    fn split(&mut self, mode: SplitMode) {
        if self.split_mode != SplitMode::Single {
            // Same key toggles the split off, terminating the spawned secondary
            // pane so repeated split/unsplit cycles don't leak orphan sessions.
            self.focused_pane = 1;
            let _ = self.close_focused_pane();
            return;
        }
        let cwd = self.sessions.get(self.active).and_then(|s| s.cwd());
        if let Some(sess) =
            Session::spawn(&self.config, self.next_id, self.cols, self.rows, cwd.as_deref())
        {
            self.next_id += 1;
            self.sessions.push(sess);
            let new_idx = self.sessions.len() - 1;
            self.panes = vec![self.active, new_idx];
            self.focused_pane = 1;
            self.active = new_idx;
            self.split_mode = mode;
            self.relayout();
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
    }

    /// Close the focused pane's session and collapse to the remaining one.
    fn close_focused_pane(&mut self) -> Task<Message> {
        if self.split_mode == SplitMode::Single {
            return self.close_session(self.active);
        }
        let victim = self.panes[self.focused_pane];
        let keep = self.panes[1 - self.focused_pane];
        if let Some(mut sess) = (victim < self.sessions.len()).then(|| self.sessions.remove(victim))
        {
            let _ = sess.pty.terminate();
        }
        // Removing `victim` shifts later indices down by one.
        self.active = if keep > victim { keep - 1 } else { keep };
        self.split_mode = SplitMode::Single;
        self.focused_pane = 0;
        self.panes = vec![self.active];
        self.relayout();
        self.save_session_snapshot();
        Task::none()
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
            C::SessionClose | C::WindowClose => return Some(self.close_session(self.active)),
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
            C::EditCopy => {
                let text = self
                    .sessions
                    .get(self.active)
                    .and_then(|s| s.terminal.copy_selection())
                    .filter(|t| !t.is_empty());
                match text {
                    Some(text) => iced::clipboard::write(text),
                    None => Task::none(),
                }
            }
            C::EditPaste => iced::clipboard::read().map(Message::Pasted),
            C::SearchOpen => {
                self.search.toggle();
                self.recompute_search();
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
                Task::none()
            }
            C::SearchPrev => {
                if !self.search.is_open {
                    return None;
                }
                self.search.prev_match();
                Task::none()
            }
            C::SearchHistoryPrev => {
                if !self.search.is_open {
                    return None;
                }
                self.search.history_prev();
                self.recompute_search();
                Task::none()
            }
            C::SearchHistoryNext => {
                if !self.search.is_open {
                    return None;
                }
                self.search.history_next();
                self.recompute_search();
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
            // Only two panes exist, so next and prev are the same toggle.
            C::PaneFocusNext | C::PaneFocusPrev => {
                self.focus_next_pane();
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
        use keyboard::key::Named;
        use keyboard::Key;
        // F12 toggles the diagnostics overlay (also reachable via Ctrl+Shift+G),
        // checked before the modifier gate since it takes no modifier.
        if matches!(key, Key::Named(Named::F12)) {
            self.debug_open = !self.debug_open;
            return Some(Task::none());
        }
        if !(mods.control() && mods.shift()) {
            return None;
        }
        if let Key::Character(s) = key {
            match s.chars().next()?.to_ascii_lowercase() {
                'p' => {
                    self.palette.toggle();
                    return Some(if self.palette.is_open {
                        iced::widget::operation::focus(PALETTE_INPUT_ID.clone())
                    } else {
                        Task::none()
                    });
                }
                'g' => {
                    self.debug_open = !self.debug_open;
                    return Some(Task::none());
                }
                '/' | '?' => {
                    self.help_open = !self.help_open;
                    return Some(Task::none());
                }
                _ => {}
            }
        }
        None
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
                {
                    let _ = link::open_link(link);
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
                        sess.write_pty(report.as_bytes());
                    }
                    return Task::none();
                }
                match button {
                    MouseButton::Left => match count {
                        2 => sess.terminal.select_word_at(row, col),
                        n if n >= 3 => {
                            let (cols, _) = sess.terminal.get_dimensions();
                            sess.terminal.start_selection((row, 0));
                            sess.terminal.update_selection((row, cols.saturating_sub(1)));
                        }
                        _ if alt => sess.terminal.start_block_selection((row, col)),
                        _ => sess.terminal.start_selection((row, col)),
                    },
                    MouseButton::Middle => {
                        return iced::clipboard::read_primary().map(Message::Pasted);
                    }
                    MouseButton::Right => {}
                }
            }
            MouseInput::Drag { col, row } => {
                if report_to_app {
                    if sess.terminal.is_mouse_motion_enabled() {
                        if let Some(report) = sess.terminal.get_mouse_report(32, col, row) {
                            sess.write_pty(report.as_bytes());
                        }
                    }
                    return Task::none();
                }
                sess.terminal.update_selection((row, col));
            }
            MouseInput::Release { col, row, button } => {
                if report_to_app {
                    if let Some(report) =
                        sess.terminal.get_mouse_release_report(btn_code(button), col, row)
                    {
                        sess.write_pty(report.as_bytes());
                    }
                    return Task::none();
                }
                if button == MouseButton::Left {
                    if let Some(text) =
                        sess.terminal.copy_selection().filter(|t| !t.is_empty())
                    {
                        return iced::clipboard::write_primary(text);
                    }
                }
            }
            MouseInput::Wheel {
                col,
                row,
                up,
                lines,
            } => {
                if report_to_app {
                    let code = if up { 64 } else { 65 };
                    // One wheel report per line so apps see the full magnitude.
                    for _ in 0..lines.max(1) {
                        if let Some(report) = sess.terminal.get_mouse_report(code, col, row) {
                            sess.write_pty(report.as_bytes());
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
    fn handle_scroll_shortcut(
        &mut self,
        key: &keyboard::Key,
        mods: keyboard::Modifiers,
    ) -> bool {
        use keyboard::key::Named;
        use keyboard::Key;
        if !mods.shift() {
            return false;
        }
        let page = self.rows.saturating_sub(1).max(1) as isize;
        let Some(sess) = self.sessions.get_mut(self.active) else {
            return false;
        };
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

    /// Re-run the search over the active session's visible grid and refresh
    /// match state. No-op when the search bar is closed.
    fn recompute_search(&mut self) {
        if !self.search.is_open {
            return;
        }
        let Some(sess) = self.sessions.get(self.active) else {
            self.search.matches.clear();
            return;
        };
        let (matches, error) = search::SearchEngine::search(
            &sess.grid,
            &self.search.query,
            self.search.use_regex,
            self.search.case_sensitive,
        );
        self.search.matches = matches;
        self.search.error_message = error;
        if self.search.matches.is_empty()
            || self.search.current_match_index >= self.search.matches.len()
        {
            self.search.current_match_index = 0;
        }
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
                self.recompute_search();
                return true;
            }
            Key::Named(Named::ArrowDown) => {
                self.search.history_next();
                self.recompute_search();
                return true;
            }
            // Ctrl+R toggles regex, Ctrl+I toggles case sensitivity (Alt is the
            // JWM window-manager modifier, so it is avoided here).
            Key::Character(c) if mods.control() => {
                match c.chars().next().map(|c| c.to_ascii_lowercase()) {
                    Some('r') => {
                        self.search.toggle_regex();
                        self.recompute_search();
                    }
                    Some('i') => {
                        self.search.toggle_case_sensitive();
                        self.recompute_search();
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
                    self.recompute_search();
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
        _mods: keyboard::Modifiers,
    ) -> Option<Task<Message>> {
        use keyboard::key::Named;
        use keyboard::Key;
        if !self.config_panel_open {
            return None;
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
        match action {
            PaletteAction::NewTab => {
                self.new_session();
                Task::none()
            }
            PaletteAction::CloseTab => self.close_session(self.active),
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
                    iced::clipboard::write(text)
                } else {
                    Task::none()
                }
            }
            PaletteAction::Paste => iced::clipboard::read().map(Message::Pasted),
            PaletteAction::OpenSearch => {
                self.search.toggle();
                self.recompute_search();
                if self.search.is_open {
                    iced::widget::operation::focus(SEARCH_INPUT_ID.clone())
                } else {
                    Task::none()
                }
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
            Message::PtyOutput(fd, data) => {
                let t0 = std::time::Instant::now();
                if let Some(sess) = self.session_by_fd(fd) {
                    sess.terminal.process_batch(&data);
                    sess.flush_responses();
                    sess.refresh();
                }
                self.last_ingest_us = t0.elapsed().as_micros();
                self.last_ingest_bytes = data.len();
                // Output may have moved the shell's cwd; let the next periodic
                // tick reconcile the session snapshot.
                self.session_dirty = true;
                self.recompute_search();
            }
            Message::PtyExited(fd, _code) => {
                if let Some(index) = self.sessions.iter().position(|s| s.master_fd == fd) {
                    return self.close_session(index);
                }
            }
            Message::Key(event) => {
                if let keyboard::Event::KeyPressed {
                    key,
                    modifiers,
                    text,
                    ..
                } = event
                {
                    if let Some(task) = self.handle_keybinding(&key, modifiers) {
                        return task;
                    }
                    if let Some(task) = self.handle_tab_shortcut(&key, modifiers) {
                        return task;
                    }
                    // Esc closes the help / debug overlays (handled before they
                    // would otherwise fall through to the PTY).
                    if (self.help_open || self.debug_open)
                        && matches!(key, keyboard::Key::Named(keyboard::key::Named::Escape))
                    {
                        self.help_open = false;
                        self.debug_open = false;
                        return Task::none();
                    }
                    if let Some(task) = self.handle_config_panel_key(&key, modifiers) {
                        return task;
                    }
                    if let Some(task) = self.handle_palette_key(&key, modifiers, text.as_deref()) {
                        return task;
                    }
                    if self.handle_search_key(&key, modifiers, text.as_deref()) {
                        return Task::none();
                    }
                    if self.handle_scroll_shortcut(&key, modifiers) {
                        return Task::none();
                    }
                    let Some(sess) = self.sessions.get_mut(self.active) else {
                        return Task::none();
                    };
                    let app_cursor = sess.terminal.is_application_cursor_keys();
                    if let Some(bytes) = encode_key(&key, modifiers, text.as_deref(), app_cursor) {
                        sess.terminal.scroll_to_bottom();
                        sess.write_pty(&bytes);
                        sess.refresh();
                    }
                }
            }
            Message::Ime(event) => {
                use iced::advanced::input_method::Event as Ime;
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
                // Only a press switches the focused pane. Release/Drag aren't
                // bounds-gated in the widget, so every pane emits them — letting
                // those move focus would let the wrong pane steal it on release.
                if matches!(input, MouseInput::Press { .. }) && pane_pos < self.panes.len() {
                    self.focused_pane = pane_pos;
                    self.active = self.panes[pane_pos];
                    self.session_dirty = true;
                }
                return self.handle_mouse(input);
            }
            Message::Pasted(Some(text)) => {
                if let Some(sess) = self.sessions.get_mut(self.active) {
                    let bracketed = sess.terminal.is_bracketed_paste_enabled();
                    let bytes = if bracketed {
                        wrap_bracketed_paste(text.into_bytes())
                    } else {
                        text.into_bytes()
                    };
                    sess.terminal.scroll_to_bottom();
                    sess.write_pty(&bytes);
                    sess.refresh();
                }
            }
            Message::Pasted(None) => {}
            Message::Resized(size) => {
                self.win_size = size;
                let term_h = self.term_height();
                let term_w = (self.term_width() - terminal_view::SCROLLBAR_WIDTH).max(0.0);
                let (cols, rows) = self.metrics.grid_size(term_w, term_h);
                if cols != self.cols || rows != self.rows {
                    self.cols = cols;
                    self.rows = rows;
                    for sess in &mut self.sessions {
                        sess.terminal.on_resize(cols, rows);
                        let _ = sess.pty.resize(cols, rows);
                        sess.refresh();
                    }
                    // Re-apply pane sizing for the displayed split panes.
                    self.relayout();
                }
            }
            Message::Focus(f) => {
                self.focused = f;
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
            Message::CloseTab(i) => return self.close_session(i),
            Message::SelectTab(i) => self.jump_session(i),
            Message::TabHover(i) => self.hovered_tab = i,
            Message::DividerDragStart => self.dragging_divider = true,
            Message::DividerDragEnd => self.dragging_divider = false,
            Message::DividerDragMove(pt) => {
                if self.dragging_divider {
                    let ratio = match self.split_mode {
                        SplitMode::Vertical => pt.x / self.term_width().max(1.0),
                        SplitMode::Horizontal => pt.y / self.term_height().max(1.0),
                        SplitMode::Single => self.split_ratio,
                    };
                    let ratio = ratio.clamp(0.15, 0.85);
                    if (ratio - self.split_ratio).abs() > f32::EPSILON {
                        self.split_ratio = ratio;
                        self.relayout();
                    }
                }
            }
            Message::ToggleSidebar => {
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
                    match pos {
                        // Docking tabs to the side: open the dock and surface the
                        // tab list (there is no top bar to show tabs otherwise).
                        config::TabPosition::Side => {
                            self.sidebar_open = true;
                            self.sidebar_panel = SidebarPanel::Tabs;
                        }
                        // Returning tabs to the top bar: collapse the dock back to
                        // the classic top-only layout.
                        config::TabPosition::Top => {
                            self.sidebar_open = false;
                            self.sidebar_panel = SidebarPanel::Files;
                        }
                    }
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
            }
            Message::SearchToggleCase => {
                self.search.toggle_case_sensitive();
                self.recompute_search();
            }
            Message::SearchInput(value) => {
                self.search.query = value;
                self.search.history_nav_index = None;
                self.recompute_search();
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
            Message::SetTheme(name) => {
                self.config.theme = name;
                self.apply_config();
            }
            Message::SetFontSize(v) => {
                self.config.font_size = Config::clamp_font_size(v);
                self.apply_config();
            }
            Message::SetLineSpacing(v) => {
                self.config.line_spacing = Config::clamp_line_spacing(v);
                self.apply_config();
            }
            Message::SetPadding(v) => {
                self.config.padding = Config::clamp_padding(v);
                self.apply_config();
            }
            Message::SetScrollback(v) => {
                self.config.scrollback_lines = Config::clamp_scrollback_lines(v as usize);
                self.apply_config();
            }
            Message::SetScrollSpeed(v) => {
                self.config.scroll_speed = Config::clamp_scroll_speed(v);
            }
            Message::SetFontFamily(name) => {
                self.config.font_family = name;
                self.apply_config();
            }
            Message::SetScrollbarAlways(always) => {
                self.config.scrollbar_visibility = if always {
                    config::ScrollbarVisibility::Always
                } else {
                    config::ScrollbarVisibility::Auto
                };
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
                if let Some(ed) = &mut self.theme_editor {
                    let name = ed.name.trim().to_string();
                    if name.is_empty() {
                        ed.error = Some("Name cannot be empty".to_string());
                    } else if Theme::is_builtin(&name) {
                        ed.error = Some("Name collides with a builtin theme".to_string());
                    } else if let Some(bad) =
                        ed.hexes.iter().position(|h| Theme::hex_to_rgb(h).is_none())
                    {
                        let labels = Theme::editable_color_labels();
                        ed.error =
                            Some(format!("Invalid hex for {}", labels[bad]));
                    } else {
                        let mut theme = ed.base.clone();
                        theme.name = name.clone();
                        for (i, h) in ed.hexes.iter().enumerate() {
                            theme.set_editable_color(i, h);
                        }
                        match theme.save_custom_theme() {
                            Ok(()) => {
                                self.config.theme = name;
                                self.theme_editor = None;
                                self.apply_config();
                            }
                            Err(e) => {
                                ed.error = Some(format!("Save failed: {}", e));
                            }
                        }
                    }
                }
            }
            Message::ThemeDelete(name) => {
                let _ = Theme::delete_custom_theme(&name);
                if self.config.theme == name {
                    self.config.theme = "dark".to_string();
                    self.apply_config();
                }
            }
            Message::ConfigSave => {
                let _ = self.config.save();
                self.config_mtime = Config::config_mtime();
            }
            Message::ConfigReset => {
                self.config = Config::default();
                self.apply_config();
                let _ = self.config.save();
                self.config_mtime = Config::config_mtime();
            }
            Message::ConfigTick => {
                // Skip while editing so live (unsaved) edits aren't reverted.
                if !self.config_panel_open {
                    let m = Config::config_mtime();
                    if m != self.config_mtime {
                        self.config_mtime = m;
                        if let Ok(path) = Config::config_path() {
                            if let Ok(content) = std::fs::read_to_string(&path) {
                                if let Ok(c) = toml::from_str::<Config>(&content) {
                                    self.config = c;
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
                // Refresh the active session's cwd cache for the status bar.
                if let Some(sess) = self.sessions.get_mut(self.active) {
                    sess.cwd_cache = sess.cwd();
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
        // Collect, under an immutable borrow, which images need a (re)build and
        // which ids are still live, then release the borrow before mutating.
        let mut needed: Vec<(u32, u32, u32, Vec<u8>)> = Vec::new();
        let mut live_ids = std::collections::HashSet::new();
        {
            let Some(sess) = self.sessions.get(self.active) else {
                self.kitty_handles.clear();
                return;
            };
            let kg = &sess.terminal.kitty_graphics;
            for p in kg.get_placements() {
                live_ids.insert(p.image_id);
                if let Some(img) = kg.get_image(p.image_id) {
                    let stale = self
                        .kitty_handles
                        .get(&p.image_id)
                        .map(|(_, len)| *len != img.data.len())
                        .unwrap_or(true);
                    if stale {
                        needed.push((img.id, img.width, img.height, img.data.clone()));
                    }
                }
            }
        }
        self.kitty_handles.retain(|id, _| live_ids.contains(id));
        for (id, w, h, data) in needed {
            let len = data.len();
            let handle = iced::advanced::image::Handle::from_rgba(w, h, data);
            self.kitty_handles.insert(id, (handle, len));
        }
    }

    /// Build the renderable image list for a session from its placements and the
    /// cached handles. Placements are already z-sorted by the graphics state.
    fn kitty_images(&self, sess: &Session) -> Vec<KittyRender> {
        let kg = &sess.terminal.kitty_graphics;
        kg.get_placements()
            .iter()
            .filter_map(|p| {
                let (handle, _) = self.kitty_handles.get(&p.image_id)?;
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
            self.active,
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

    fn tab_bar(&self) -> Element<'_, Message> {
        let mut tabs = row![].spacing(2).padding(2);
        // Sidebar/dock toggle button at the far left of the tab bar.
        tabs = tabs.push(
            button(text("☰").size(13))
                .on_press(Message::ToggleSidebar)
                .padding([3, 8])
                .style(if self.sidebar_open {
                    button::primary
                } else {
                    button::secondary
                }),
        );
        // In side-tab mode the tab list lives in the dock; the top bar keeps only
        // the dock toggle plus a button to move tabs back to the top.
        if self.config.tab_position == config::TabPosition::Side {
            tabs = tabs.push(
                button(text("▔ Tabs to top").size(13))
                    .on_press(Message::SetTabPosition(config::TabPosition::Top))
                    .padding([3, 8])
                    .style(button::secondary),
            );
            return container(tabs)
                .width(Length::Fill)
                .height(Length::Fixed(TAB_BAR_H))
                .into();
        }
        // Dock the tab strip into the left sidebar (vertical tab list).
        tabs = tabs.push(
            button(text("◧").size(13))
                .on_press(Message::SetTabPosition(config::TabPosition::Side))
                .padding([3, 8])
                .style(button::secondary),
        );
        for (i, sess) in self.sessions.iter().enumerate() {
            let active = i == self.active;
            let label = sess.label();
            let label = if label.chars().count() > 24 {
                let truncated: String = label.chars().take(23).collect();
                format!("{truncated}…")
            } else {
                label
            };
            let tab = button(text(label).size(13))
                .on_press(Message::SelectTab(i))
                .padding([3, 8]);
            let tab = if active {
                tab.style(button::primary)
            } else {
                tab.style(button::secondary)
            };
            // Reveal the close button only on the active or hovered tab to cut
            // visual noise; keep its footprint reserved otherwise so tabs don't
            // jump when hovered.
            let show_close = active || self.hovered_tab == Some(i);
            let close: Element<'_, Message> = if show_close {
                button(text("×").size(13))
                    .on_press(Message::CloseTab(i))
                    .padding([3, 6])
                    .style(button::secondary)
                    .into()
            } else {
                Space::new().width(Length::Fixed(18.0)).into()
            };
            let cell = row![tab, close]
                .spacing(1)
                .align_y(iced::Alignment::Center);
            tabs = tabs.push(
                mouse_area(cell)
                    .on_enter(Message::TabHover(Some(i)))
                    .on_exit(Message::TabHover(None)),
            );
        }
        tabs = tabs.push(
            button(text("+").size(13))
                .on_press(Message::NewSession)
                .padding([3, 8])
                .style(button::secondary),
        );
        let scroller = scrollable(tabs)
            .direction(scrollable::Direction::Horizontal(
                scrollable::Scrollbar::new().width(0).scroller_width(0),
            ))
            .width(Length::Fill);
        container(scroller)
            .width(Length::Fill)
            .height(Length::Fixed(TAB_BAR_H))
            .into()
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
        let grid = format!("{}×{}", self.cols, self.rows);
        let pos = format!("{}:{}", cur_row + 1, cur_col + 1);

        let mut right = row![
            text(grid).size(11).style(text::secondary),
            text(pos).size(11).style(text::secondary),
        ]
        .spacing(14)
        .align_y(iced::Alignment::Center);
        if self.search.is_open && !self.search.matches.is_empty() {
            right = right.push(
                text(format!(
                    "{}/{}",
                    self.search.current_match_index + 1,
                    self.search.matches.len()
                ))
                .size(11)
                .style(text::secondary),
            );
        }

        let bar = row![
            text(cwd).size(11).style(text::secondary),
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
            .style(container::dark)
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
        let overlay_input_active = self.search.is_open || self.palette.is_open;
        let focused =
            self.focused && pane_pos == self.focused_pane && !overlay_input_active;
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
        let (search_matches, current): (&[search::SearchMatch], _) =
            if is_active && self.search.is_open {
                (
                    &self.search.matches,
                    self.search.current_match().map(|m| (m.line, m.col_start)),
                )
            } else {
                (&[], None)
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
            focused,
            &self.theme,
            self.metrics,
            self.mono,
            selection,
            sess.terminal.scroll_offset,
            sess.terminal.scrollback_len(),
        )
        .modifiers(self.modifiers.shift(), self.modifiers.alt())
        .scrollbar_always(matches!(
            self.config.scrollbar_visibility,
            config::ScrollbarVisibility::Always
        ))
        .search(search_matches, current)
        .links(links)
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
                .style(if active {
                    button::primary
                } else {
                    button::secondary
                })
        };
        let header = row![
            // Collapse the dock; the top-bar ☰ reopens it.
            button(text("☰").size(12))
                .on_press(Message::ToggleSidebar)
                .padding([2, 8])
                .style(button::secondary),
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
            .width(Length::Fixed(SIDEBAR_W))
            .height(Length::Fill)
            .style(container::dark)
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
        let mut rows: Vec<Element<'_, Message>> = vec![container(
            text(title).size(12).font(iced::Font {
                weight: iced::font::Weight::Bold,
                ..iced::Font::DEFAULT
            }),
        )
        .padding([4, 6])
        .into()];
        if let Some(root) = &self.sidebar.root {
            for child in &root.children {
                Self::collect_sidebar_nodes(child, 0, &mut rows);
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
            let active = i == self.active;
            let label = sess.label();
            let label = if label.chars().count() > 22 {
                let truncated: String = label.chars().take(21).collect();
                format!("{truncated}…")
            } else {
                label
            };
            let tab = button(text(label).size(13))
                .on_press(Message::SelectTab(i))
                .width(Length::Fill)
                .padding([4, 8])
                .style(if active {
                    button::primary
                } else {
                    button::secondary
                });
            // Reveal the close button on the active or hovered tab only.
            let show_close = active || self.hovered_tab == Some(i);
            let close: Element<'_, Message> = if show_close {
                button(text("×").size(13))
                    .on_press(Message::CloseTab(i))
                    .padding([4, 6])
                    .style(button::secondary)
                    .into()
            } else {
                Space::new().width(Length::Fixed(20.0)).into()
            };
            let cell = row![tab, close]
                .spacing(2)
                .align_y(iced::Alignment::Center);
            list = list.push(
                mouse_area(cell)
                    .on_enter(Message::TabHover(Some(i)))
                    .on_exit(Message::TabHover(None)),
            );
        }
        list = list.push(
            button(text("+ New tab").size(13))
                .on_press(Message::NewSession)
                .width(Length::Fill)
                .padding([4, 8])
                .style(button::secondary),
        );
        scrollable(list).height(Length::Fill).into()
    }

    /// Recursively flatten a file-tree node (and expanded descendants) into rows.
    fn collect_sidebar_nodes<'a>(
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
                .style(button::text)
                .into(),
        );
        if node.is_dir && node.expanded {
            for child in &node.children {
                Self::collect_sidebar_nodes(child, depth + 1, out);
            }
        }
    }

    /// A draggable divider strip drawn between split panes. Pressing it starts a
    /// resize drag (continued via the body's `on_move` while `dragging_divider`).
    fn divider(&self, horizontal: bool) -> Element<'_, Message> {
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
        mouse_area(d.style(container::dark))
            .on_press(Message::DividerDragStart)
            .interaction(interaction)
            .into()
    }

    fn view(&self) -> Element<'_, Message> {
        if self.panes.is_empty() || self.sessions.is_empty() {
            return container(text("no session")).into();
        }
        // Integer FillPortions approximating the float split ratio.
        let p0 = (self.split_ratio * 1000.0).round().clamp(1.0, 999.0) as u16;
        let p1 = 1000 - p0;
        let panes_body: Element<'_, Message> = match self.split_mode {
            SplitMode::Single => self.pane_view(0),
            SplitMode::Vertical => row![
                container(self.pane_view(0))
                    .width(Length::FillPortion(p0))
                    .height(Length::Fill),
                self.divider(false),
                container(self.pane_view(1))
                    .width(Length::FillPortion(p1))
                    .height(Length::Fill),
            ]
            .width(Length::Fill)
            .height(Length::Fill)
            .into(),
            SplitMode::Horizontal => column![
                container(self.pane_view(0))
                    .width(Length::Fill)
                    .height(Length::FillPortion(p0)),
                self.divider(true),
                container(self.pane_view(1))
                    .width(Length::Fill)
                    .height(Length::FillPortion(p1)),
            ]
            .width(Length::Fill)
            .height(Length::Fill)
            .into(),
        };
        // While dragging the divider, wrap the panes in a mouse_area so pointer
        // moves drive the resize and release ends it. The handler is attached
        // only during a drag to avoid emitting a message on every idle move.
        let panes_body: Element<'_, Message> = if self.dragging_divider {
            mouse_area(panes_body)
                .on_move(Message::DividerDragMove)
                .on_release(Message::DividerDragEnd)
                .on_exit(Message::DividerDragEnd)
                .into()
        } else {
            panes_body
        };
        let body = container(panes_body).width(Length::Fill).height(Length::Fill);
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
        // Optional left dock (file tree and/or tab list) beside the terminal.
        let main_area: Element<'_, Message> = if self.dock_open() {
            row![self.sidebar_view(), body]
                .width(Length::Fill)
                .height(Length::Fill)
                .into()
        } else {
            body
        };
        // The top bar is always present: in Top mode it holds the tab strip; in
        // Side mode it holds the dock toggle so chrome never overlaps the grid.
        column![self.tab_bar(), main_area, self.status_bar()]
            .width(Length::Fill)
            .height(Length::Fill)
            .into()
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
        let inner = container(bar)
            .padding([4, 10])
            .style(container::dark);
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

        let mut theme_row = row![
            text("Theme").size(13).width(Length::Fixed(120.0)),
            pick_list(themes, current_theme, Message::SetTheme).text_size(13),
            button(text("Edit…").size(13)).on_press(Message::ThemeEditOpen),
        ]
        .spacing(10)
        .align_y(iced::Alignment::Center);
        if is_custom {
            theme_row = theme_row.push(
                button(text("Delete").size(13))
                    .on_press(Message::ThemeDelete(self.config.theme.clone()))
                    .style(button::danger),
            );
        }

        // Monospace families detected via fc-list (cached, scanned on first open).
        // Ensure the configured family is present so the pick_list shows it.
        let mut fonts: Vec<String> = Config::get_monospace_fonts().clone();
        if !self.config.font_family.trim().is_empty()
            && !fonts.iter().any(|f| f == &self.config.font_family)
        {
            fonts.insert(0, self.config.font_family.clone());
        }
        let font_family_row = row![
            text("Font").size(13).width(Length::Fixed(120.0)),
            pick_list(
                fonts,
                Some(self.config.font_family.clone()),
                Message::SetFontFamily
            )
            .text_size(13),
        ]
        .spacing(10)
        .align_y(iced::Alignment::Center);

        let font_size = slider_row(
            "Font Size",
            format!("{:.0}", self.config.font_size),
            slider(8.0..=72.0, self.config.font_size, Message::SetFontSize)
                .step(1.0)
                .into(),
        );
        let line_spacing = slider_row(
            "Line Spacing",
            format!("{:.2}", self.config.line_spacing),
            slider(0.8..=3.0, self.config.line_spacing, Message::SetLineSpacing)
                .step(0.05)
                .into(),
        );
        let padding = slider_row(
            "Padding",
            format!("{:.0}", self.config.padding),
            slider(0.0..=20.0, self.config.padding, Message::SetPadding)
                .step(1.0)
                .into(),
        );
        let scrollback = slider_row(
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
        let scroll_speed = slider_row(
            "Scroll Speed",
            format!("{}", self.config.scroll_speed),
            slider(1..=10u32, self.config.scroll_speed, Message::SetScrollSpeed)
                .step(1u32)
                .into(),
        );
        let scrollbar_row = row![
            text("Scrollbar").size(13).width(Length::Fixed(120.0)),
            checkbox(matches!(
                self.config.scrollbar_visibility,
                config::ScrollbarVisibility::Always
            ))
            .label("Always show")
            .text_size(13)
            .on_toggle(Message::SetScrollbarAlways),
        ]
        .spacing(10)
        .align_y(iced::Alignment::Center);

        let tab_position_row = row![
            text("Tabs").size(13).width(Length::Fixed(120.0)),
            checkbox(self.config.tab_position == config::TabPosition::Side)
                .label("In sidebar")
                .text_size(13)
                .on_toggle(|side| Message::SetTabPosition(if side {
                    config::TabPosition::Side
                } else {
                    config::TabPosition::Top
                })),
        ]
        .spacing(10)
        .align_y(iced::Alignment::Center);

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

        let footer = text("Ctrl+Shift+O toggles · Esc closes")
            .size(10)
            .style(text::secondary);

        let inner = container(
            column![
                text("Settings").size(18),
                theme_row,
                font_family_row,
                font_size,
                line_spacing,
                padding,
                scrollback,
                scroll_speed,
                scrollbar_row,
                tab_position_row,
                buttons,
                footer,
            ]
            .spacing(12),
        )
        .width(Length::Fixed(460.0))
        .max_height(480.0)
        .padding(16)
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

    /// Centered keybindings cheat-sheet (Ctrl+Shift+/). All listed bindings are
    /// Ctrl-based — jterm3 never binds Alt (reserved by the JWM window manager).
    fn help_panel(&self) -> Element<'_, Message> {
        let section = |title: &str| -> Element<'_, Message> {
            text(title.to_string())
                .size(13)
                .style(text::primary)
                .into()
        };
        let kb = |key: &str, desc: &str| -> Element<'_, Message> {
            row![
                container(text(key.to_string()).size(12).font(iced::Font::MONOSPACE))
                    .width(Length::Fixed(150.0)),
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
            kb("Ctrl+Shift+Tab / PgUp", "Previous tab"),
            kb("Ctrl+1 .. Ctrl+9", "Jump to tab 1-9"),
            section("Splits / Panes"),
            kb("Ctrl+Shift+D", "Split left/right (toggle)"),
            kb("Ctrl+Shift+E", "Split top/bottom (toggle)"),
            kb("Ctrl+Shift+J", "Focus next pane"),
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
            kb("Ctrl+Shift+P", "Command palette"),
            kb("Ctrl+Shift+O", "Settings"),
            kb("Ctrl+Shift+G", "Debug / diagnostics"),
            kb("Ctrl+Shift+/", "This help"),
            kb("Esc", "Close any panel"),
        ]
        .spacing(6);

        let inner = container(scrollable(body).height(Length::Shrink))
            .width(Length::Fixed(420.0))
            .max_height(560.0)
            .padding(16)
            .style(container::dark);
        container(inner)
            .center_x(Length::Fill)
            .center_y(Length::Fill)
            .into()
    }

    /// Top-right diagnostics overlay (Ctrl+Shift+G): live grid / session /
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
                    SplitMode::Vertical => format!("V {}/{}", self.focused_pane + 1, self.panes.len()),
                    SplitMode::Horizontal => format!("H {}/{}", self.focused_pane + 1, self.panes.len()),
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
            .map(|s| pty_subscription(s.id, s.master_fd))
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
            iced::Event::InputMethod(ime) => Some(Message::Ime(ime)),
            iced::Event::Window(iced::window::Event::Resized(size)) => {
                Some(Message::Resized(size))
            }
            iced::Event::Window(iced::window::Event::Focused) => Some(Message::Focus(true)),
            iced::Event::Window(iced::window::Event::Unfocused) => Some(Message::Focus(false)),
            _ => None,
        });
        subs.push(events);
        subs.push(
            iced::time::every(std::time::Duration::from_millis(1500))
                .map(|_| Message::ConfigTick),
        );
        subs.push(
            iced::time::every(std::time::Duration::from_millis(530))
                .map(|_| Message::BlinkTick),
        );
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

/// Wrap a paste payload in bracketed-paste delimiters.
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

fn wrap_bracketed_paste(mut payload: Vec<u8>) -> Vec<u8> {
    let mut wrapped = Vec::with_capacity(payload.len() + 12);
    wrapped.extend_from_slice(b"\x1b[200~");
    wrapped.append(&mut payload);
    wrapped.extend_from_slice(b"\x1b[201~");
    wrapped
}

fn pty_subscription(id: usize, fd: RawFd) -> Subscription<Message> {
    // Key on the stable session id (not the raw fd): a closed session's fd
    // number can be reused by a new session, and keying on fd alone would let
    // iced confuse the two and reuse the old reader thread on the reused fd.
    Subscription::run_with((id, fd), |&(_, fd): &(usize, RawFd)| pty_stream(fd))
}

fn pty_stream(fd: RawFd) -> impl iced::futures::Stream<Item = Message> {
    use iced::futures::{SinkExt, StreamExt};
    iced::stream::channel(256, move |mut output: iced::futures::channel::mpsc::Sender<Message>| async move {
        let (tx, mut rx) = iced::futures::channel::mpsc::unbounded::<Message>();
        std::thread::spawn(move || {
            // Drain everything currently readable into one message instead of
            // emitting a separate message per 64 KiB read. Bursty output (e.g.
            // `cat bigfile`) then triggers far fewer process/refresh/render
            // cycles, while a lone keystroke still hits WouldBlock immediately
            // and is delivered with no added latency. Capped so the UI gets a
            // chance to repaint between very large bursts.
            const COALESCE_CAP: usize = 1 << 20; // 1 MiB per message
            let mut buf = vec![0u8; 65536];
            loop {
                match Pty::wait_fd_readable(fd, 200) {
                    Ok(true) => {
                        let mut acc: Vec<u8> = Vec::new();
                        let mut exited = false;
                        let mut errored = false;
                        loop {
                            let n = unsafe {
                                libc::read(fd, buf.as_mut_ptr() as *mut libc::c_void, buf.len())
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
                                errored = true;
                                break;
                            }
                        }
                        if !acc.is_empty()
                            && tx.unbounded_send(Message::PtyOutput(fd, acc)).is_err()
                        {
                            break;
                        }
                        if exited {
                            let _ = tx.unbounded_send(Message::PtyExited(fd, 0));
                            break;
                        }
                        if errored {
                            let _ = tx.unbounded_send(Message::PtyExited(fd, -1));
                            break;
                        }
                    }
                    Ok(false) => continue,
                    Err(_) => {
                        let _ = tx.unbounded_send(Message::PtyExited(fd, -1));
                        break;
                    }
                }
            }
        });
        while let Some(msg) = rx.next().await {
            if output.send(msg).await.is_err() {
                break;
            }
        }
    })
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
            s.chars().next()?.to_ascii_lowercase().to_string()
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

/// Translate an iced key press into the bytes to send to the PTY.
fn encode_key(
    key: &keyboard::Key,
    mods: keyboard::Modifiers,
    text: Option<&str>,
    app_cursor: bool,
) -> Option<Vec<u8>> {
    use keyboard::key::Named;
    use keyboard::Key;

    let ctrl = mods.control();
    let alt = mods.alt();

    let csi = |c: &str| -> Vec<u8> { format!("\x1b[{c}").into_bytes() };
    let ss3 = |c: &str| -> Vec<u8> { format!("\x1bO{c}").into_bytes() };
    let cursor = |c: &str| if app_cursor { ss3(c) } else { csi(c) };

    match key {
        Key::Named(named) => {
            let bytes = match named {
                Named::Enter => vec![b'\r'],
                Named::Backspace => vec![0x7f],
                Named::Tab => {
                    if mods.shift() {
                        csi("Z")
                    } else {
                        vec![b'\t']
                    }
                }
                Named::Escape => vec![0x1b],
                Named::Space => vec![b' '],
                Named::ArrowUp => cursor("A"),
                Named::ArrowDown => cursor("B"),
                Named::ArrowRight => cursor("C"),
                Named::ArrowLeft => cursor("D"),
                Named::Home => cursor("H"),
                Named::End => cursor("F"),
                Named::PageUp => csi("5~"),
                Named::PageDown => csi("6~"),
                Named::Delete => csi("3~"),
                Named::Insert => csi("2~"),
                _ => return None,
            };
            if alt {
                let mut v = vec![0x1b];
                v.extend_from_slice(&bytes);
                Some(v)
            } else {
                Some(bytes)
            }
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
