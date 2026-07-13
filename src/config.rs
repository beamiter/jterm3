use once_cell::sync::Lazy;
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::PathBuf;
use std::process::Command;

// Nerd Font priority list
const NERD_FONT_CANDIDATES: &[&str] = &[
    "SauceCodePro Nerd Font",
    "SauceCodePro Nerd Font Mono",
    "Monokoi Nerd Font",
    "Monokoi Nerd Font Mono",
    "JetBrains Mono Nerd Font",
    "JetBrains Mono NF",
    "JetBrainsMono Nerd Font",
    "FiraCode Nerd Font",
];

const NERD_FONT_FALLBACK_CANDIDATES: &[&str] = &[
    "SauceCodePro Nerd Font Mono",
    "JetBrainsMono Nerd Font Mono",
    "JetBrains Mono Nerd Font",
    "JetBrainsMono Nerd Font",
    "SauceCodePro Nerd Font",
    "Monokoi Nerd Font Mono",
    "Monokoi Nerd Font",
    "FiraCode Nerd Font",
];

const MATH_SYMBOL_FONT_CANDIDATES: &[&str] =
    &["Noto Sans Math", "Noto Sans Symbols2", "OpenSymbol"];

static MONOSPACE_FONTS: Lazy<Vec<String>> = Lazy::new(|| {
    eprintln!("[Config] Scanning monospace fonts (one-time)...");
    detect_fonts_by_query(&[":spacing=100"])
});

static CJK_MONOSPACE_FONT: Lazy<Option<String>> = Lazy::new(|| {
    eprintln!("[Config] Resolving CJK monospace fallback font...");
    detect_font_by_match(&["monospace:lang=zh-cn"])
});

static SYMBOL_MONOSPACE_FONT: Lazy<Option<String>> = Lazy::new(|| {
    eprintln!("[Config] Resolving terminal symbol fallback font...");
    detect_font_by_match(&["monospace:charset=2303"])
});

static MATH_SYMBOL_FONT: Lazy<Option<String>> = Lazy::new(|| {
    eprintln!("[Config] Resolving math symbol fallback font...");
    detect_preferred_font(MATH_SYMBOL_FONT_CANDIDATES)
        .or_else(|| detect_font_by_match(&["monospace:charset=1D7CF"]))
});

static NERD_SYMBOL_FONT: Lazy<Option<String>> = Lazy::new(|| {
    eprintln!("[Config] Resolving Nerd Font symbol fallback...");
    detect_preferred_font(NERD_FONT_FALLBACK_CANDIDATES)
});

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum FontBackendType {
    #[default]
    Fontdue,
    AbGlyph,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum AppRendererType {
    #[default]
    Glow,
    Wgpu,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum ScrollbarVisibility {
    Auto,
    #[default]
    Always,
}

/// Where the session tab strip is rendered.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum TabPosition {
    /// Horizontal tab strip across the top of the window.
    #[default]
    Top,
    /// Vertical tab list docked in the left sidebar.
    Side,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    #[serde(default = "default_font_size")]
    pub font_size: f32,

    #[serde(default = "default_font_family")]
    pub font_family: String,

    #[serde(default = "default_font_weight")]
    pub font_weight: f32,

    #[serde(default = "default_font_sharpness")]
    pub font_sharpness: f32,

    #[serde(default)]
    pub font_backend: FontBackendType,

    #[serde(default = "default_padding")]
    pub padding: f32,

    #[serde(default = "default_line_spacing")]
    pub line_spacing: f32,

    #[serde(default)]
    pub scrollbar_visibility: ScrollbarVisibility,

    #[serde(default)]
    pub tab_position: TabPosition,

    #[serde(default = "default_scrollback_lines")]
    pub scrollback_lines: usize,

    #[serde(default = "default_initial_width")]
    pub initial_width: f32,

    #[serde(default = "default_initial_height")]
    pub initial_height: f32,

    #[serde(default = "default_cols")]
    pub cols: usize,

    #[serde(default = "default_rows")]
    pub rows: usize,

    #[serde(default = "default_theme")]
    pub theme: String,

    #[serde(default = "default_restore_session")]
    pub restore_session: bool,

    #[serde(default)]
    pub session_history_file: Option<PathBuf>,

    #[serde(default = "default_opacity")]
    pub opacity: f32,

    #[serde(default = "default_gpu_rendering")]
    pub gpu_rendering: bool,

    #[serde(default)]
    pub app_renderer: AppRendererType,

    #[serde(default = "default_scroll_speed")]
    pub scroll_speed: u32,

    #[serde(default)]
    pub disable_alt_screen: bool,

    #[serde(default)]
    pub ui_scale: Option<f32>,

    #[serde(default = "default_subpixel_rendering")]
    pub subpixel_rendering: bool,

    /// Explicit shell path (overrides auto-detection). Useful when PATH is stripped by launchers like wofi.
    #[serde(default)]
    pub shell: Option<String>,

    /// Permit applications running in the PTY to read the host clipboard via
    /// OSC 52 / OSC 5522. Disabled by default because this crosses the local /
    /// remote-shell trust boundary.
    #[serde(default)]
    pub allow_clipboard_read: bool,
}

fn default_font_size() -> f32 {
    14.0
}

fn default_font_weight() -> f32 {
    1.0
}

fn default_font_sharpness() -> f32 {
    1.0
}

fn default_line_spacing() -> f32 {
    1.0
}

fn detect_fonts_by_query(extra_args: &[&str]) -> Vec<String> {
    let mut args = Vec::from(extra_args);
    args.push("family");
    if let Ok(output) = Command::new("fc-list").args(&args).output() {
        if let Ok(stdout) = String::from_utf8(output.stdout) {
            let mut seen = std::collections::HashSet::new();
            let mut families: Vec<String> = stdout
                .lines()
                .filter_map(|line| {
                    let family = line.split(',').next()?.trim();
                    if family.is_empty() {
                        return None;
                    }
                    if seen.insert(family.to_lowercase()) {
                        Some(family.to_string())
                    } else {
                        None
                    }
                })
                .collect();
            families.sort_by_key(|a| a.to_lowercase());
            return families;
        }
    }
    Vec::new()
}

fn detect_font_by_match(args: &[&str]) -> Option<String> {
    let output = Command::new("fc-match")
        .args(args)
        .args(["family"])
        .output()
        .ok()?;
    let stdout = String::from_utf8(output.stdout).ok()?;
    stdout
        .lines()
        .find_map(|line| {
            line.split(',')
                .next()
                .map(str::trim)
                .filter(|f| !f.is_empty())
        })
        .map(ToOwned::to_owned)
}

fn detect_preferred_font(candidates: &[&str]) -> Option<String> {
    for candidate in candidates {
        let output = Command::new("fc-match")
            .arg("-f")
            .arg("%{family}\n")
            .arg(candidate)
            .output()
            .ok()?;
        let stdout = String::from_utf8(output.stdout).ok()?;
        let line = stdout.lines().next()?.trim();
        let line_lower = line.to_lowercase();
        if line_lower
            .split(',')
            .map(str::trim)
            .any(|family| family == candidate.to_lowercase())
        {
            return line.split(',').next().map(str::trim).map(ToOwned::to_owned);
        }
    }
    None
}

fn detect_monospace_fonts() -> &'static Vec<String> {
    &MONOSPACE_FONTS
}

fn default_font_family() -> String {
    // 快速路径：直接使用第一个候选字体，不检测系统字体
    // 这避免了启动时的 fc-list 调用，加快启动速度
    // 字体检测会在用户打开配置面板时延迟进行
    eprintln!(
        "[Config] Using default font (no scan): {}",
        NERD_FONT_CANDIDATES[0]
    );
    NERD_FONT_CANDIDATES[0].to_string()

    // 原有的检测逻辑已移除，避免启动时阻塞
    // 如需验证字体存在性，可在配置面板中按需检测
}

fn default_padding() -> f32 {
    2.0
}

fn default_scrollback_lines() -> usize {
    10000
}

fn default_initial_width() -> f32 {
    1200.0
}

fn default_initial_height() -> f32 {
    600.0
}

fn default_cols() -> usize {
    100
}

fn default_rows() -> usize {
    30
}

fn default_theme() -> String {
    "dark".to_string()
}

fn default_restore_session() -> bool {
    true
}

fn default_opacity() -> f32 {
    1.0
}

fn default_gpu_rendering() -> bool {
    true
}

fn default_scroll_speed() -> u32 {
    3
}

fn default_subpixel_rendering() -> bool {
    true
}

impl Default for Config {
    fn default() -> Self {
        Config {
            font_size: default_font_size(),
            font_family: default_font_family(),
            font_weight: default_font_weight(),
            font_sharpness: default_font_sharpness(),
            font_backend: FontBackendType::default(),
            padding: default_padding(),
            line_spacing: default_line_spacing(),
            scrollbar_visibility: ScrollbarVisibility::default(),
            tab_position: TabPosition::default(),
            scrollback_lines: default_scrollback_lines(),
            initial_width: default_initial_width(),
            initial_height: default_initial_height(),
            cols: default_cols(),
            rows: default_rows(),
            theme: default_theme(),
            restore_session: default_restore_session(),
            session_history_file: None,
            opacity: default_opacity(),
            gpu_rendering: default_gpu_rendering(),
            app_renderer: AppRendererType::default(),
            scroll_speed: default_scroll_speed(),
            disable_alt_screen: false,
            subpixel_rendering: default_subpixel_rendering(),
            ui_scale: None,
            shell: None,
            allow_clipboard_read: false,
        }
    }
}

impl Config {
    /// Parse and normalize configuration from TOML. Keeping this as the single
    /// path ensures startup and live reload enforce identical bounds.
    pub fn from_toml(content: &str) -> Result<Self, toml::de::Error> {
        toml::from_str::<Config>(content).map(Self::normalized)
    }

    fn normalized(mut self) -> Self {
        self.font_size = Self::clamp_font_size(self.font_size);
        self.line_spacing = Self::clamp_line_spacing(self.line_spacing);
        self.padding = Self::clamp_padding(self.padding);
        self.scrollback_lines = Self::clamp_scrollback_lines(self.scrollback_lines);
        self.scroll_speed = Self::clamp_scroll_speed(self.scroll_speed);
        self.opacity = Self::clamp_opacity(self.opacity);
        self.font_weight = finite_clamp(self.font_weight, default_font_weight(), 0.1, 2.0);
        self.font_sharpness = finite_clamp(self.font_sharpness, default_font_sharpness(), 0.1, 2.0);
        self.initial_width =
            finite_clamp(self.initial_width, default_initial_width(), 320.0, 16_384.0);
        self.initial_height = finite_clamp(
            self.initial_height,
            default_initial_height(),
            200.0,
            16_384.0,
        );
        self.cols = self.cols.clamp(1, crate::terminal::MAX_TERMINAL_COLS);
        self.rows = self.rows.clamp(1, crate::terminal::MAX_TERMINAL_ROWS);
        self.ui_scale = self
            .ui_scale
            .filter(|value| value.is_finite())
            .map(|value| value.clamp(0.5, 4.0));
        self.font_family = self.font_family.trim().to_string();
        self.theme = self.theme.trim().to_string();
        if self.theme.is_empty() {
            self.theme = default_theme();
        }
        self.shell = self
            .shell
            .map(|shell| shell.trim().to_string())
            .filter(|shell| !shell.is_empty());
        self
    }

    pub fn load() -> Self {
        if let Ok(config_path) = Self::config_path() {
            if config_path.exists() {
                if let Ok(content) = std::fs::read_to_string(&config_path) {
                    match Self::from_toml(&content) {
                        Ok(config) => {
                            eprintln!("[Config] Loaded from {}", config_path.display());
                            eprintln!("[Config] Font: {}", config.font_family);
                            return config;
                        }
                        Err(error) => {
                            eprintln!(
                                "[Config] Failed to parse {}: {error}",
                                config_path.display()
                            );
                        }
                    }
                }
            }
        }
        eprintln!("[Config] Using default configuration");
        let config = Self::default();
        eprintln!("[Config] Font: {}", config.font_family);
        config
    }

    pub fn save(&self) -> Result<(), Box<dyn std::error::Error>> {
        let config_path = Self::config_path()?;
        let config_dir = config_path.parent().ok_or("Config path has no parent")?;

        // Create config directory if it doesn't exist
        std::fs::create_dir_all(config_dir)?;

        // Write and fsync a sibling temporary file before the atomic rename so
        // a crash cannot leave a half-written TOML file behind.
        let content = toml::to_string_pretty(self)?;
        let tmp = config_path.with_extension(format!("toml.tmp.{}", std::process::id()));
        let write_result = (|| -> Result<(), Box<dyn std::error::Error>> {
            let mut file = std::fs::OpenOptions::new()
                .create(true)
                .truncate(true)
                .write(true)
                .open(&tmp)?;
            file.write_all(content.as_bytes())?;
            file.sync_all()?;
            std::fs::rename(&tmp, &config_path)?;
            Ok(())
        })();
        if write_result.is_err() {
            let _ = std::fs::remove_file(&tmp);
        }
        write_result?;
        eprintln!("[Config] Saved to {}", config_path.display());
        Ok(())
    }

    pub fn session_history_path(&self) -> Result<PathBuf, Box<dyn std::error::Error>> {
        let config_dir = dirs::config_dir().ok_or("Failed to determine config directory")?;
        let default = config_dir.join("jterm3").join("session_history.json");
        let Some(path) = self.session_history_file.as_ref() else {
            return Ok(default);
        };
        if path.is_absolute() {
            return Ok(path.clone());
        }
        if let Ok(rest) = path.strip_prefix("~") {
            if let Some(home) = dirs::home_dir() {
                return Ok(home.join(rest));
            }
        }
        Ok(config_dir.join("jterm3").join(path))
    }

    pub fn config_path() -> Result<PathBuf, Box<dyn std::error::Error>> {
        let config_dir = dirs::config_dir().ok_or("Failed to determine config directory")?;
        Ok(config_dir.join("jterm3").join("config.toml"))
    }

    pub fn config_mtime() -> Option<std::time::SystemTime> {
        Self::config_path()
            .ok()
            .and_then(|p| std::fs::metadata(p).ok())
            .and_then(|m| m.modified().ok())
    }

    // 配置值约束方法
    #[allow(dead_code)]
    pub fn clamp_font_size(size: f32) -> f32 {
        finite_clamp(size, default_font_size(), 8.0, 72.0)
    }

    #[allow(dead_code)]
    pub fn clamp_line_spacing(spacing: f32) -> f32 {
        finite_clamp(spacing, default_line_spacing(), 0.8, 3.0)
    }

    #[allow(dead_code)]
    pub fn clamp_padding(padding: f32) -> f32 {
        finite_clamp(padding, default_padding(), 0.0, 20.0)
    }

    #[allow(dead_code)]
    pub fn clamp_scrollback_lines(lines: usize) -> usize {
        lines.clamp(100, 100_000)
    }

    #[allow(dead_code)]
    pub fn clamp_opacity(opacity: f32) -> f32 {
        finite_clamp(opacity, default_opacity(), 0.05, 1.0)
    }

    #[allow(dead_code)]
    pub fn clamp_scroll_speed(speed: u32) -> u32 {
        speed.clamp(1, 10)
    }

    pub fn get_monospace_fonts() -> &'static Vec<String> {
        detect_monospace_fonts()
    }

    pub fn cjk_monospace_font_family() -> Option<&'static str> {
        CJK_MONOSPACE_FONT.as_deref()
    }

    pub fn symbol_monospace_font_family() -> Option<&'static str> {
        SYMBOL_MONOSPACE_FONT.as_deref()
    }

    pub fn math_symbol_font_family() -> Option<&'static str> {
        MATH_SYMBOL_FONT.as_deref()
    }

    pub fn nerd_symbol_font_family() -> Option<&'static str> {
        NERD_SYMBOL_FONT.as_deref()
    }
}

fn finite_clamp(value: f32, fallback: f32, min: f32, max: f32) -> f32 {
    if value.is_finite() {
        value.clamp(min, max)
    } else {
        fallback
    }
}

#[allow(dead_code)]
pub fn create_default_config() {
    let config = Config::default();
    if let Err(e) = config.save() {
        eprintln!("[Config] Warning: Could not save default config: {}", e);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalization_bounds_untrusted_numeric_values() {
        let config = Config {
            font_size: f32::NAN,
            line_spacing: f32::INFINITY,
            initial_width: -1.0,
            cols: usize::MAX,
            rows: 0,
            ui_scale: Some(f32::NAN),
            ..Config::default()
        };

        let normalized = config.normalized();

        assert_eq!(normalized.font_size, default_font_size());
        assert_eq!(normalized.line_spacing, default_line_spacing());
        assert_eq!(normalized.initial_width, 320.0);
        assert_eq!(normalized.cols, crate::terminal::MAX_TERMINAL_COLS);
        assert_eq!(normalized.rows, 1);
        assert_eq!(normalized.ui_scale, None);
    }

    #[test]
    fn empty_optional_strings_are_normalized() {
        let config = Config {
            theme: "   ".to_string(),
            shell: Some("  ".to_string()),
            ..Config::default()
        };

        let normalized = config.normalized();

        assert_eq!(normalized.theme, default_theme());
        assert_eq!(normalized.shell, None);
    }
}
