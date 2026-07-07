use crate::color::{resolve_bg_with_palette, resolve_fg_with_palette};
use crate::search::SearchMatch;
use crate::terminal::{CursorShape, DynamicColorPalette, TerminalCell, UnderlineStyle};
use crate::theme::Theme;

use std::time::Instant;

use iced::advanced::input_method::{self, InputMethod};
use iced::advanced::layout::{self, Layout};
use iced::advanced::renderer::{self, Quad};
use iced::advanced::text::{self, Text};
use iced::advanced::widget::{tree, Tree, Widget};
use iced::advanced::{Clipboard, Shell};
use iced::mouse;
use iced::{
    Background, Border, Color, Element, Event, Length, Pixels, Point, Rectangle, Shadow, Size,
};

/// Which mouse button a [`MouseInput`] refers to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseButton {
    Left,
    Middle,
    Right,
}

/// A semantic mouse interaction over the terminal grid, in 0-indexed cell
/// coordinates. Emitted by [`TermWidget`] and handled by the application.
#[derive(Debug, Clone, Copy)]
pub enum MouseInput {
    Press {
        col: usize,
        row: usize,
        button: MouseButton,
        shift: bool,
        alt: bool,
        count: u32,
    },
    Drag {
        col: usize,
        row: usize,
    },
    Release {
        col: usize,
        row: usize,
        button: MouseButton,
    },
    Wheel {
        col: usize,
        row: usize,
        up: bool,
        ctrl: bool,
        /// Number of whole lines this event scrolls (≥1).
        lines: usize,
    },
    /// Drag/jump the scrollbar to an absolute scrollback offset
    /// (0 = bottom/live view).
    ScrollTo {
        offset: usize,
    },
}

/// A Kitty-graphics image to paint: a cached RGBA handle plus its grid-cell
/// placement (col/row origin, cell span) and source pixel dimensions.
#[derive(Clone)]
pub struct KittyRender {
    pub handle: iced::advanced::image::Handle,
    pub col: usize,
    pub row: usize,
    pub cols: usize,
    pub rows: usize,
    pub id: u32,
    pub px_w: u32,
    pub px_h: u32,
}

/// Width of the scrollbar gutter on the right edge, in pixels.
pub const SCROLLBAR_WIDTH: f32 = 10.0;
/// Minimum thumb height so it stays grabbable with deep scrollback.
const SCROLLBAR_MIN_THUMB: f32 = 24.0;

fn hovered_link_color() -> Color {
    Color::from_rgb8(100, 200, 255)
}

/// Per-widget interaction state retained across frames.
#[derive(Default)]
struct State {
    dragging: bool,
    scrollbar_dragging: bool,
    last_click: Option<(Instant, usize, usize)>,
    click_count: u32,
    /// Fractional wheel lines not yet consumed, so sub-line trackpad pixel
    /// deltas accumulate into whole-line scrolls instead of being lost.
    scroll_accum: f32,
}

/// Max gap between presses (ms) for them to count as a multi-click.
const MULTI_CLICK_MS: u128 = 400;

/// Pixel metrics for the terminal grid.
#[derive(Clone, Copy, Debug)]
pub struct Metrics {
    pub font_size: f32,
    pub cell_w: f32,
    pub cell_h: f32,
    pub padding: f32,
}

impl Metrics {
    pub fn new(font_size: f32, line_spacing: f32, padding: f32) -> Self {
        let cell_w = (font_size * 0.6).max(1.0);
        let cell_h = (font_size * 1.2 * line_spacing).max(1.0);
        Metrics {
            font_size,
            cell_w,
            cell_h,
            padding,
        }
    }

    /// Compute (cols, rows) that fit into the given pixel area.
    pub fn grid_size(&self, width: f32, height: f32) -> (usize, usize) {
        let usable_w = (width - self.padding * 2.0).max(0.0);
        let usable_h = (height - self.padding * 2.0).max(0.0);
        let cols = (usable_w / self.cell_w).floor() as usize;
        let rows = (usable_h / self.cell_h).floor() as usize;
        (cols.max(1), rows.max(1))
    }
}

/// A custom widget that renders a terminal grid snapshot using the advanced
/// renderer (quads for backgrounds/cursor, real text shaping for glyphs).
pub struct TermWidget<'a, Message> {
    grid: &'a [Vec<TerminalCell>],
    cursor: (usize, usize),
    cursor_visible: bool,
    cursor_shape: CursorShape,
    focused: bool,
    theme: &'a Theme,
    dynamic_palette: Option<&'a DynamicColorPalette>,
    metrics: Metrics,
    mono: iced::Font,
    cjk_mono: Option<iced::Font>,
    symbol_mono: Option<iced::Font>,
    math_symbol: Option<iced::Font>,
    nerd_symbol: Option<iced::Font>,
    /// Per visible row: the inclusive (start_col, end_col) span to highlight,
    /// or `None` for rows with no selection. `end_col` may exceed the row width.
    selection: Vec<Option<(usize, usize)>>,
    scroll_offset: usize,
    scrollback_len: usize,
    /// Search matches in visible-grid coordinates (line = grid row index).
    search_matches: &'a [SearchMatch],
    /// Identity `(line, col_start)` of the active match, highlighted distinctly.
    current_match: Option<(usize, usize)>,
    shift: bool,
    alt: bool,
    ctrl: bool,
    on_mouse: Option<Box<dyn Fn(MouseInput) -> Message + 'a>>,
    /// Detected clickable links in visible-grid coordinates (line = grid row).
    links: &'a [crate::link::Link],
    /// Kitty-graphics placements to paint over the grid.
    images: Vec<KittyRender>,
    /// When false (Auto), the scrollbar is only drawn while scrolled up; when
    /// true (Always), it is drawn whenever scrollback exists.
    scrollbar_always: bool,
    /// Active IME pre-edit (composition) text plus the byte range the IME marks
    /// as its cursor/selection. Supplied to the runtime each redraw so it can
    /// paint the over-the-spot composition overlay at the terminal cursor.
    preedit: Option<(String, Option<std::ops::Range<usize>>)>,
    /// Current phase of the blink clock: when false, cells with the blink
    /// attribute hide their glyph (drawn as background only).
    blink_on: bool,
}

impl<'a, Message> TermWidget<'a, Message> {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        grid: &'a [Vec<TerminalCell>],
        cursor: (usize, usize),
        cursor_visible: bool,
        cursor_shape: CursorShape,
        focused: bool,
        theme: &'a Theme,
        metrics: Metrics,
        mono: iced::Font,
        cjk_mono: Option<iced::Font>,
        symbol_mono: Option<iced::Font>,
        math_symbol: Option<iced::Font>,
        nerd_symbol: Option<iced::Font>,
        selection: Vec<Option<(usize, usize)>>,
        scroll_offset: usize,
        scrollback_len: usize,
    ) -> Self {
        TermWidget {
            grid,
            cursor,
            cursor_visible,
            cursor_shape,
            focused,
            theme,
            dynamic_palette: None,
            metrics,
            mono,
            cjk_mono,
            symbol_mono,
            math_symbol,
            nerd_symbol,
            selection,
            scroll_offset,
            scrollback_len,
            search_matches: &[],
            current_match: None,
            shift: false,
            alt: false,
            ctrl: false,
            on_mouse: None,
            links: &[],
            images: Vec::new(),
            scrollbar_always: true,
            preedit: None,
            blink_on: true,
        }
    }

    /// Set the blink clock phase (true = glyphs visible).
    pub fn blink_on(mut self, on: bool) -> Self {
        self.blink_on = on;
        self
    }

    /// Supply the active IME pre-edit so the runtime can render the composition
    /// overlay and keep the input method enabled while this pane is focused.
    pub fn preedit(mut self, preedit: Option<(String, Option<std::ops::Range<usize>>)>) -> Self {
        self.preedit = preedit;
        self
    }

    /// Set scrollbar visibility: `true` = always shown, `false` = auto (only
    /// while scrolled up).
    pub fn scrollbar_always(mut self, always: bool) -> Self {
        self.scrollbar_always = always;
        self
    }

    /// Supply detected links to color, underline, and make clickable.
    pub fn links(mut self, links: &'a [crate::link::Link]) -> Self {
        self.links = links;
        self
    }

    pub fn dynamic_palette(mut self, palette: &'a DynamicColorPalette) -> Self {
        self.dynamic_palette = Some(palette);
        self
    }

    /// Supply Kitty-graphics placements to paint over the grid.
    pub fn images(mut self, images: Vec<KittyRender>) -> Self {
        self.images = images;
        self
    }

    /// Find the link covering a given (col, row) cell, if any.
    fn link_at(&self, col: usize, row: usize) -> Option<&crate::link::Link> {
        self.links
            .iter()
            .find(|l| l.line == row && col >= l.col_start && col < l.col_end)
    }

    /// Supply search matches (and the active match identity) to highlight.
    pub fn search(mut self, matches: &'a [SearchMatch], current: Option<(usize, usize)>) -> Self {
        self.search_matches = matches;
        self.current_match = current;
        self
    }

    /// Scrollbar track + thumb geometry, or `None` when there is nothing to
    /// scroll. Returns `(track_top, track_h, x, thumb_y, thumb_h)`.
    fn scrollbar_geometry(&self, bounds: Rectangle) -> Option<(f32, f32, f32, f32, f32)> {
        if self.scrollback_len == 0 {
            return None;
        }
        // Auto mode: only reveal the scrollbar while scrolled up into history.
        if !self.scrollbar_always && self.scroll_offset == 0 {
            return None;
        }
        let pad = self.metrics.padding;
        let rows = self.grid.len();
        let total = self.scrollback_len + rows;
        if total == 0 {
            return None;
        }
        let track_top = bounds.y + pad;
        let track_h = (bounds.height - pad * 2.0).max(1.0);
        let x = bounds.x + bounds.width - pad - SCROLLBAR_WIDTH;
        let thumb_h = ((rows as f32 / total as f32) * track_h)
            .clamp(SCROLLBAR_MIN_THUMB.min(track_h), track_h);
        // offset == 0 → thumb at bottom (live view); offset == max → top.
        let frac = self.scroll_offset as f32 / self.scrollback_len as f32;
        let thumb_y = track_top + (track_h - thumb_h) * (1.0 - frac);
        Some((track_top, track_h, x, thumb_y, thumb_h))
    }

    /// Map a pointer y-coordinate (centering the thumb on the cursor) to an
    /// absolute scrollback offset.
    fn offset_from_y(&self, y: f32, bounds: Rectangle) -> usize {
        let Some((track_top, track_h, _, _, thumb_h)) = self.scrollbar_geometry(bounds) else {
            return 0;
        };
        let usable = (track_h - thumb_h).max(1.0);
        let rel = (y - track_top - thumb_h / 2.0).clamp(0.0, usable);
        let frac = 1.0 - rel / usable;
        (frac * self.scrollback_len as f32).round() as usize
    }

    /// Register a callback that maps grid mouse interactions to messages.
    pub fn on_mouse(mut self, f: impl Fn(MouseInput) -> Message + 'a) -> Self {
        self.on_mouse = Some(Box::new(f));
        self
    }

    /// Supply the keyboard modifier state tracked by the application, used to
    /// distinguish selection (shift) and block-selection (alt) from app mouse
    /// reporting.
    pub fn modifiers(mut self, shift: bool, alt: bool, ctrl: bool) -> Self {
        self.shift = shift;
        self.alt = alt;
        self.ctrl = ctrl;
        self
    }

    /// Convert an absolute pixel position into a clamped 0-indexed (col, row).
    fn cell_at(&self, pos: Point, bounds: Rectangle) -> (usize, usize) {
        let pad = self.metrics.padding;
        let cw = self.metrics.cell_w.max(1.0);
        let ch = self.metrics.cell_h.max(1.0);
        let rel_x = (pos.x - bounds.x - pad).max(0.0);
        let rel_y = (pos.y - bounds.y - pad).max(0.0);
        let max_row = self.grid.len().saturating_sub(1);
        let max_col = self
            .grid
            .first()
            .map(|r| r.len())
            .unwrap_or(1)
            .saturating_sub(1);
        let col = ((rel_x / cw) as usize).min(max_col);
        let row = ((rel_y / ch) as usize).min(max_row);
        (col, row)
    }
}

fn should_use_symbol_fallback_font(ch: char) -> bool {
    matches!(
        ch as u32,
        0x2190..=0x21FF
            | 0x2200..=0x22FF
            | 0x2300..=0x23FF
            | 0x2500..=0x259F
            | 0x25A0..=0x25FF
            | 0x2600..=0x26FF
            | 0x2700..=0x27BF
            | 0x27C0..=0x27FF
            | 0x2800..=0x28FF
            | 0x2900..=0x2AFF
            | 0x2B00..=0x2BFF
    )
}

fn should_use_math_symbol_fallback_font(ch: char) -> bool {
    matches!(ch as u32, 0x1D400..=0x1D7FF)
}

fn should_use_nerd_symbol_fallback_font(ch: char) -> bool {
    matches!(ch as u32, 0xE000..=0xF8FF | 0xF0000..=0xFFFFD | 0x100000..=0x10FFFD)
}

fn should_use_cjk_fallback_font(ch: char) -> bool {
    matches!(
        ch as u32,
        0x2E80..=0x2EFF
            | 0x3000..=0x303F
            | 0x3040..=0x30FF
            | 0x3100..=0x312F
            | 0x3130..=0x318F
            | 0x31A0..=0x31BF
            | 0x31C0..=0x31EF
            | 0x31F0..=0x31FF
            | 0x3400..=0x4DBF
            | 0x4E00..=0x9FFF
            | 0xAC00..=0xD7AF
            | 0xF900..=0xFAFF
            | 0xFF00..=0xFFEF
            | 0x20000..=0x2FA1F
    )
}

fn terminal_glyph_font(
    ch: char,
    primary: iced::Font,
    cjk: Option<iced::Font>,
    symbol: Option<iced::Font>,
    math_symbol: Option<iced::Font>,
    nerd_symbol: Option<iced::Font>,
    italic: bool,
) -> iced::Font {
    if should_use_nerd_symbol_fallback_font(ch) {
        nerd_symbol.unwrap_or_else(|| symbol.unwrap_or(iced::Font::MONOSPACE))
    } else if should_use_math_symbol_fallback_font(ch) {
        math_symbol.unwrap_or_else(|| symbol.unwrap_or(iced::Font::MONOSPACE))
    } else if should_use_symbol_fallback_font(ch) {
        symbol.unwrap_or(iced::Font::MONOSPACE)
    } else if should_use_cjk_fallback_font(ch) {
        cjk.unwrap_or(primary)
    } else if italic {
        iced::Font {
            style: iced::font::Style::Italic,
            ..primary
        }
    } else {
        primary
    }
}

fn solid_quad(bounds: Rectangle) -> Quad {
    Quad {
        bounds,
        border: Border::default(),
        shadow: Shadow::default(),
        snap: true,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        should_use_cjk_fallback_font, should_use_math_symbol_fallback_font,
        should_use_nerd_symbol_fallback_font, should_use_symbol_fallback_font, terminal_glyph_font,
    };

    #[test]
    fn terminal_symbols_use_fallback_font() {
        assert!(should_use_symbol_fallback_font('⌃'));
        assert!(should_use_symbol_fallback_font('⌅'));
        assert!(should_use_symbol_fallback_font('⋮'));
        assert!(should_use_symbol_fallback_font('─'));
        assert!(should_use_symbol_fallback_font('☰'));
        assert!(should_use_symbol_fallback_font('✓'));
        assert!(should_use_symbol_fallback_font('⟂'));
        assert!(should_use_symbol_fallback_font('⮕'));
        assert!(should_use_symbol_fallback_font('⣿'));
        assert!(!should_use_symbol_fallback_font('𝟏'));
        assert!(!should_use_symbol_fallback_font('中'));
        assert!(!should_use_symbol_fallback_font('A'));
    }

    #[test]
    fn math_alphanumeric_symbols_use_math_fallback_font() {
        assert!(should_use_math_symbol_fallback_font('𝟏'));
        assert!(should_use_math_symbol_fallback_font('𝟘'));
        assert!(should_use_math_symbol_fallback_font('𝐀'));
        assert!(!should_use_math_symbol_fallback_font('1'));
        assert!(!should_use_math_symbol_fallback_font('中'));
    }

    #[test]
    fn private_use_symbols_use_nerd_fallback_font() {
        assert!(should_use_nerd_symbol_fallback_font('\u{e0b0}'));
        assert!(should_use_nerd_symbol_fallback_font('\u{f0131}'));
        assert!(!should_use_nerd_symbol_fallback_font('𝟏'));
        assert!(!should_use_nerd_symbol_fallback_font('中'));
        assert!(!should_use_nerd_symbol_fallback_font('A'));
    }

    #[test]
    fn cjk_uses_cjk_fallback_font() {
        assert!(should_use_cjk_fallback_font('中'));
        assert!(should_use_cjk_fallback_font('あ'));
        assert!(!should_use_cjk_fallback_font('⌃'));
        assert!(!should_use_cjk_fallback_font('A'));
    }

    #[test]
    fn symbol_font_is_preferred_for_terminal_symbols() {
        let primary = iced::Font::with_name("Primary");
        let cjk = iced::Font::with_name("Cjk");
        let symbol = iced::Font::with_name("Symbol");
        let math = iced::Font::with_name("Math");
        let nerd = iced::Font::with_name("Nerd");
        let font_for = |ch, italic| {
            terminal_glyph_font(
                ch,
                primary,
                Some(cjk),
                Some(symbol),
                Some(math),
                Some(nerd),
                italic,
            )
        };

        assert_eq!(font_for('⌃', false), symbol);
        assert_eq!(font_for('⋮', false), symbol);
        assert_eq!(font_for('✓', false), symbol);
        assert_eq!(font_for('⣿', false), symbol);
        assert_eq!(font_for('\u{e0b0}', false), nerd);
        assert_eq!(font_for('𝟏', false), math);
        assert_eq!(font_for('中', false), cjk);
        assert_eq!(font_for('A', false), primary);
    }

    #[test]
    fn italic_style_does_not_override_fallback_fonts() {
        let primary = iced::Font::with_name("Primary");
        let cjk = iced::Font::with_name("Cjk");
        let symbol = iced::Font::with_name("Symbol");
        let math = iced::Font::with_name("Math");
        let nerd = iced::Font::with_name("Nerd");
        let italic_primary = iced::Font {
            style: iced::font::Style::Italic,
            ..primary
        };
        let font_for = |ch, italic| {
            terminal_glyph_font(
                ch,
                primary,
                Some(cjk),
                Some(symbol),
                Some(math),
                Some(nerd),
                italic,
            )
        };

        assert_eq!(font_for('中', true), cjk);
        assert_eq!(font_for('⌃', true), symbol);
        assert_eq!(font_for('𝟏', true), math);
        assert_eq!(font_for('\u{e0b0}', true), nerd);
        assert_eq!(font_for('A', true), italic_primary);
    }
}

impl<Message, Renderer> Widget<Message, iced::Theme, Renderer> for TermWidget<'_, Message>
where
    Renderer: text::Renderer<Font = iced::Font>
        + iced::advanced::image::Renderer<Handle = iced::advanced::image::Handle>,
{
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<State>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(State::default())
    }

    fn size(&self) -> Size<Length> {
        Size::new(Length::Fill, Length::Fill)
    }

    fn layout(
        &mut self,
        _tree: &mut Tree,
        _renderer: &Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        layout::Node::new(limits.max())
    }

    fn update(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        _renderer: &Renderer,
        _clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
        _viewport: &Rectangle,
    ) {
        let bounds = layout.bounds();

        // Keep the input method enabled and positioned at the text cursor while
        // this pane is focused. The runtime only honors the request during a
        // RedrawRequested, and renders any supplied pre-edit as an over-the-spot
        // overlay anchored to `cursor`.
        if self.focused {
            if let Event::Window(iced::window::Event::RedrawRequested(_)) = event {
                let pad = self.metrics.padding;
                let (row, col) = self.cursor;
                let cursor_rect = Rectangle::new(
                    Point::new(
                        bounds.x + pad + col as f32 * self.metrics.cell_w,
                        bounds.y + pad + row as f32 * self.metrics.cell_h,
                    ),
                    Size::new(self.metrics.cell_w, self.metrics.cell_h),
                );
                let preedit =
                    self.preedit
                        .as_ref()
                        .map(|(content, selection)| input_method::Preedit {
                            content: content.as_str(),
                            selection: selection.clone(),
                            text_size: Some(Pixels(self.metrics.font_size)),
                        });
                shell.request_input_method(&InputMethod::Enabled {
                    cursor: cursor_rect,
                    purpose: input_method::Purpose::Terminal,
                    preedit,
                });
            }
        }

        let Some(on_mouse) = self.on_mouse.as_ref() else {
            return;
        };
        let state = tree.state.downcast_mut::<State>();

        match event {
            Event::Mouse(mouse::Event::ButtonPressed(btn)) => {
                let Some(pos) = cursor.position_over(bounds) else {
                    return;
                };
                let (col, row) = self.cell_at(pos, bounds);
                let button = match btn {
                    mouse::Button::Left => MouseButton::Left,
                    mouse::Button::Middle => MouseButton::Middle,
                    mouse::Button::Right => MouseButton::Right,
                    _ => return,
                };
                let (shift, alt) = (self.shift, self.alt);
                // Grabbing the scrollbar gutter starts a scroll drag, not a
                // text selection.
                if button == MouseButton::Left {
                    if let Some((_, _, sb_x, _, _)) = self.scrollbar_geometry(bounds) {
                        if pos.x >= sb_x {
                            state.scrollbar_dragging = true;
                            let offset = self.offset_from_y(pos.y, bounds);
                            shell.publish(on_mouse(MouseInput::ScrollTo { offset }));
                            shell.capture_event();
                            return;
                        }
                    }
                }
                if button == MouseButton::Left {
                    state.dragging = true;
                    let now = Instant::now();
                    let same_cell = state
                        .last_click
                        .map(|(t, c, r)| {
                            c == col
                                && r == row
                                && now.duration_since(t).as_millis() <= MULTI_CLICK_MS
                        })
                        .unwrap_or(false);
                    state.click_count = if same_cell { state.click_count + 1 } else { 1 };
                    state.last_click = Some((now, col, row));
                } else {
                    state.click_count = 1;
                }
                shell.publish(on_mouse(MouseInput::Press {
                    col,
                    row,
                    button,
                    shift,
                    alt,
                    count: state.click_count,
                }));
                shell.capture_event();
            }
            Event::Mouse(mouse::Event::CursorMoved { .. }) => {
                let pos = cursor.position().unwrap_or(Point::new(bounds.x, bounds.y));
                if state.scrollbar_dragging {
                    let offset = self.offset_from_y(pos.y, bounds);
                    shell.publish(on_mouse(MouseInput::ScrollTo { offset }));
                    return;
                }
                if !state.dragging {
                    return;
                }
                let (col, row) = self.cell_at(pos, bounds);
                shell.publish(on_mouse(MouseInput::Drag { col, row }));
            }
            Event::Mouse(mouse::Event::ButtonReleased(btn)) => {
                // Only the pane that owns the interaction (was dragging) or has
                // the cursor over it should process the release; otherwise every
                // split pane emits a release for the same physical click.
                if !state.dragging
                    && !state.scrollbar_dragging
                    && cursor.position_over(bounds).is_none()
                {
                    return;
                }
                let button = match btn {
                    mouse::Button::Left => MouseButton::Left,
                    mouse::Button::Middle => MouseButton::Middle,
                    mouse::Button::Right => MouseButton::Right,
                    _ => return,
                };
                if button == MouseButton::Left {
                    if state.scrollbar_dragging {
                        state.scrollbar_dragging = false;
                        return;
                    }
                    state.dragging = false;
                }
                let pos = cursor.position().unwrap_or(Point::new(bounds.x, bounds.y));
                let (col, row) = self.cell_at(pos, bounds);
                shell.publish(on_mouse(MouseInput::Release { col, row, button }));
            }
            Event::Mouse(mouse::Event::WheelScrolled { delta }) => {
                let Some(pos) = cursor.position_over(bounds) else {
                    return;
                };
                // Normalize both delta kinds to lines: Lines is already in lines;
                // Pixels is divided by the cell height. Fractions accumulate so a
                // trackpad's stream of sub-line pixel deltas still scrolls.
                let dy = match delta {
                    mouse::ScrollDelta::Lines { y, .. } => *y,
                    mouse::ScrollDelta::Pixels { y, .. } => *y / self.metrics.cell_h.max(1.0),
                };
                if dy == 0.0 {
                    return;
                }
                let state = tree.state.downcast_mut::<State>();
                // Drop any leftover fraction of the opposite sign on a direction
                // reversal, otherwise it cancels part of the new delta and the
                // first reversed tick gets swallowed.
                if state.scroll_accum != 0.0 && (dy > 0.0) != (state.scroll_accum > 0.0) {
                    state.scroll_accum = 0.0;
                }
                state.scroll_accum += dy;
                let whole = state.scroll_accum.trunc();
                if whole == 0.0 {
                    shell.capture_event();
                    return;
                }
                state.scroll_accum -= whole;
                let (col, row) = self.cell_at(pos, bounds);
                shell.publish(on_mouse(MouseInput::Wheel {
                    col,
                    row,
                    up: whole > 0.0,
                    ctrl: self.ctrl,
                    lines: whole.abs() as usize,
                }));
                shell.capture_event();
            }
            _ => {}
        }
    }

    fn draw(
        &self,
        _tree: &Tree,
        renderer: &mut Renderer,
        _theme: &iced::Theme,
        _style: &renderer::Style,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        let bounds = layout.bounds();
        let clip = bounds.intersection(viewport).unwrap_or(bounds);

        // The link currently under the pointer (brightened on hover).
        let hovered: Option<&crate::link::Link> = cursor
            .position_over(bounds)
            .map(|p| self.cell_at(p, bounds))
            .and_then(|(hc, hr)| self.link_at(hc, hr));
        let pad = self.metrics.padding;
        let cw = self.metrics.cell_w;
        let ch = self.metrics.cell_h;
        let ox = bounds.x + pad;
        let oy = bounds.y + pad;
        let default_bg = self.theme.terminal_background();

        // Whole-widget background.
        renderer.fill_quad(solid_quad(bounds), Background::Color(default_bg));

        // Bucket links by visible row so the per-cell hit test scans only the
        // links on that row instead of the whole list. Skipped entirely (no
        // allocation) in the common case where no links are present.
        let links_by_row: Vec<Vec<&crate::link::Link>> = if self.links.is_empty() {
            Vec::new()
        } else {
            let mut buckets: Vec<Vec<&crate::link::Link>> = vec![Vec::new(); self.grid.len()];
            for l in self.links {
                if l.line < buckets.len() {
                    buckets[l.line].push(l);
                }
            }
            buckets
        };

        for (row_idx, row) in self.grid.iter().enumerate() {
            let y = oy + row_idx as f32 * ch;

            // Backgrounds.
            for (col_idx, cell) in row.iter().enumerate() {
                if cell.flags.wide_continuation() {
                    continue;
                }
                let mut bg =
                    resolve_bg_with_palette(cell.background, self.theme, self.dynamic_palette);
                let mut fg = resolve_fg_with_palette(
                    cell.foreground,
                    self.theme,
                    self.dynamic_palette,
                    cell.flags.bold(),
                    cell.flags.dim(),
                );
                if cell.flags.inverse() {
                    std::mem::swap(&mut bg, &mut fg);
                }
                let span = if cell.flags.wide() { 2.0 } else { 1.0 };
                if bg != default_bg {
                    let x = ox + col_idx as f32 * cw;
                    renderer.fill_quad(
                        solid_quad(Rectangle {
                            x,
                            y,
                            width: cw * span,
                            height: ch,
                        }),
                        Background::Color(bg),
                    );
                }
            }

            // Selection highlight (semi-transparent overlay).
            if let Some(Some((sc, ec))) = self.selection.get(row_idx) {
                let last = row.len().saturating_sub(1);
                let start = (*sc).min(last);
                let end = (*ec).min(last);
                if end >= start {
                    let x = ox + start as f32 * cw;
                    let width = (end - start + 1) as f32 * cw;
                    renderer.fill_quad(
                        solid_quad(Rectangle {
                            x,
                            y,
                            width,
                            height: ch,
                        }),
                        Background::Color(self.theme.selection_color()),
                    );
                }
            }

            // Search match highlights (semi-transparent overlay).
            if !self.search_matches.is_empty() {
                let last = row.len().saturating_sub(1);
                for m in self.search_matches.iter().filter(|m| m.line == row_idx) {
                    let start = m.col_start.min(last);
                    let end = m.col_end.saturating_sub(1).min(last);
                    if end >= start {
                        let color = if self.current_match == Some((m.line, m.col_start)) {
                            self.theme.search_current_color()
                        } else {
                            self.theme.search_match_color()
                        };
                        let x = ox + start as f32 * cw;
                        let width = (end - start + 1) as f32 * cw;
                        renderer.fill_quad(
                            solid_quad(Rectangle {
                                x,
                                y,
                                width,
                                height: ch,
                            }),
                            Background::Color(color),
                        );
                    }
                }
            }

            // Glyphs + decorations. Consecutive narrow cells sharing a foreground
            // color are coalesced into a single shaped text run, which slashes the
            // number of per-frame String allocations and text-shaping calls. Runs
            // break on color changes, spaces, wide glyphs, and links so each run
            // starts at an exact cell origin — drift from approximate cell widths
            // can never accumulate across a break.
            let font = self.mono;
            let font_size = self.metrics.font_size;
            // Cells covered by the active selection draw their glyphs in the
            // selection foreground color so text stays legible over the overlay.
            let sel_range = self.selection.get(row_idx).copied().flatten();
            let mut run_text = String::new();
            let mut run_len: usize = 0;
            let mut run_fg = Color::TRANSPARENT;
            let mut run_start = 0usize;
            let mut run_font = font;
            let emit_run = |renderer: &mut Renderer,
                            text: &mut String,
                            len: &mut usize,
                            start: usize,
                            fg: Color,
                            run_font: iced::Font| {
                if *len == 0 {
                    return;
                }
                let rx = ox + start as f32 * cw;
                renderer.fill_text(
                    Text {
                        content: std::mem::take(text),
                        bounds: Size::new(cw * *len as f32, ch),
                        size: Pixels(font_size),
                        line_height: text::LineHeight::Absolute(Pixels(ch)),
                        font: run_font,
                        align_x: text::Alignment::Left,
                        align_y: iced::alignment::Vertical::Center,
                        shaping: text::Shaping::Basic,
                        wrapping: text::Wrapping::None,
                    },
                    Point::new(rx, y + ch / 2.0),
                    fg,
                    clip,
                );
                *len = 0;
            };

            for (col_idx, cell) in row.iter().enumerate() {
                if cell.flags.wide_continuation() {
                    continue;
                }
                let glyph = cell.character;
                let is_wide = cell.flags.wide();
                let span = if is_wide { 2.0 } else { 1.0 };
                let x = ox + col_idx as f32 * cw;
                let mut fg = resolve_fg_with_palette(
                    cell.foreground,
                    self.theme,
                    self.dynamic_palette,
                    cell.flags.bold(),
                    cell.flags.dim(),
                );
                if cell.flags.inverse() {
                    fg = resolve_bg_with_palette(cell.background, self.theme, self.dynamic_palette);
                }
                let selected = sel_range.is_some_and(|(sc, ec)| col_idx >= sc && col_idx <= ec);
                if selected {
                    fg = self.theme.selection_fg_color();
                }
                let glyph_font = terminal_glyph_font(
                    glyph,
                    font,
                    self.cjk_mono,
                    self.symbol_mono,
                    self.math_symbol,
                    self.nerd_symbol,
                    cell.flags.italic(),
                );
                // Blink: during the off phase, blinking cells show no glyph.
                let blink_hidden = cell.flags.blink() && !self.blink_on;

                // Clickable links keep their terminal color unless hovered.
                let row_links: &[&crate::link::Link] =
                    links_by_row.get(row_idx).map(Vec::as_slice).unwrap_or(&[]);
                let is_link = row_links
                    .iter()
                    .any(|l| col_idx >= l.col_start && col_idx < l.col_end);
                if is_link {
                    let is_hovered = hovered.is_some_and(|h| {
                        h.line == row_idx && col_idx >= h.col_start && col_idx < h.col_end
                    });
                    if is_hovered {
                        fg = hovered_link_color();
                    }
                }

                let printable = glyph != ' ' && glyph != '\0' && !blink_hidden;

                if printable && !is_wide {
                    // Extend the current run, or flush and start a new one when the
                    // color, font (italic), or contiguity changes.
                    if run_len != 0
                        && (fg != run_fg
                            || glyph_font != run_font
                            || col_idx != run_start + run_len)
                    {
                        emit_run(
                            renderer,
                            &mut run_text,
                            &mut run_len,
                            run_start,
                            run_fg,
                            run_font,
                        );
                    }
                    if run_len == 0 {
                        run_start = col_idx;
                        run_fg = fg;
                        run_font = glyph_font;
                    }
                    run_text.push(glyph);
                    run_len += 1;
                    emit_run(
                        renderer,
                        &mut run_text,
                        &mut run_len,
                        run_start,
                        run_fg,
                        run_font,
                    );
                } else {
                    // Spaces and wide glyphs end any pending run; wide glyphs are
                    // drawn individually, centered over their two-cell span.
                    emit_run(
                        renderer,
                        &mut run_text,
                        &mut run_len,
                        run_start,
                        run_fg,
                        run_font,
                    );
                    if printable {
                        renderer.fill_text(
                            Text {
                                content: glyph.to_string(),
                                bounds: Size::new(cw * span, ch),
                                size: Pixels(font_size),
                                line_height: text::LineHeight::Absolute(Pixels(ch)),
                                font: glyph_font,
                                align_x: text::Alignment::Center,
                                align_y: iced::alignment::Vertical::Center,
                                shaping: text::Shaping::Basic,
                                wrapping: text::Wrapping::None,
                            },
                            Point::new(x + cw * span / 2.0, y + ch / 2.0),
                            fg,
                            clip,
                        );
                    }
                }

                if is_link || cell.flags.underline() != UnderlineStyle::None {
                    renderer.fill_quad(
                        solid_quad(Rectangle {
                            x,
                            y: y + ch - 2.0,
                            width: cw * span,
                            height: 1.0,
                        }),
                        Background::Color(fg),
                    );
                }
                if cell.flags.strikethrough() {
                    renderer.fill_quad(
                        solid_quad(Rectangle {
                            x,
                            y: y + ch * 0.5,
                            width: cw * span,
                            height: 1.0,
                        }),
                        Background::Color(fg),
                    );
                }
            }
            // Flush any run that reached the end of the row.
            emit_run(
                renderer,
                &mut run_text,
                &mut run_len,
                run_start,
                run_fg,
                run_font,
            );
        }

        // Cursor.
        if self.cursor_visible {
            let (cr, cc) = self.cursor;
            let x = ox + cc as f32 * cw;
            let y = oy + cr as f32 * ch;
            let cur = self.theme.cursor_color();
            let cursor_cell = self.grid.get(cr).and_then(|r| r.get(cc));
            // A wide (CJK) glyph occupies two cells; the cursor must cover both.
            let cursor_w = if cursor_cell.is_some_and(|c| c.flags.wide()) {
                cw * 2.0
            } else {
                cw
            };
            let shape_bounds = match self.cursor_shape {
                CursorShape::Block => Rectangle {
                    x,
                    y,
                    width: cursor_w,
                    height: ch,
                },
                CursorShape::Underline => {
                    let h = (ch * 0.12).clamp(1.0, 3.0);
                    Rectangle {
                        x,
                        y: y + ch - h,
                        width: cursor_w,
                        height: h,
                    }
                }
                CursorShape::Beam => {
                    let w = (cw * 0.12).clamp(1.0, 3.0);
                    Rectangle {
                        x,
                        y,
                        width: w,
                        height: ch,
                    }
                }
            };
            if self.focused {
                renderer.fill_quad(solid_quad(shape_bounds), Background::Color(cur));
                if self.cursor_shape == CursorShape::Block {
                    if let Some(cell) = cursor_cell {
                        let glyph = cell.character;
                        if glyph != ' ' && glyph != '\0' {
                            renderer.fill_text(
                                Text {
                                    content: glyph.to_string(),
                                    bounds: Size::new(cursor_w, ch),
                                    size: Pixels(self.metrics.font_size),
                                    line_height: text::LineHeight::Absolute(Pixels(ch)),
                                    font: terminal_glyph_font(
                                        glyph,
                                        self.mono,
                                        self.cjk_mono,
                                        self.symbol_mono,
                                        self.math_symbol,
                                        self.nerd_symbol,
                                        cell.flags.italic(),
                                    ),
                                    align_x: text::Alignment::Center,
                                    align_y: iced::alignment::Vertical::Center,
                                    shaping: text::Shaping::Basic,
                                    wrapping: text::Wrapping::None,
                                },
                                Point::new(x + cursor_w / 2.0, y + ch / 2.0),
                                default_bg,
                                clip,
                            );
                        }
                    }
                }
            } else if self.cursor_shape == CursorShape::Block {
                let cursor_border = Quad {
                    bounds: shape_bounds,
                    border: Border {
                        color: cur,
                        width: 1.0,
                        radius: 0.0.into(),
                    },
                    shadow: Shadow::default(),
                    snap: true,
                };
                renderer.fill_quad(cursor_border, Background::Color(Color::TRANSPARENT));
            } else {
                renderer.fill_quad(solid_quad(shape_bounds), Background::Color(cur));
            }
        }

        // Kitty graphics: paint each placement (already z-sorted) as a texture
        // stretched into its cell span.
        for im in &self.images {
            let x = ox + im.col as f32 * cw;
            let y = oy + im.row as f32 * ch;
            let w = im.cols as f32 * cw;
            let h = im.rows as f32 * ch;
            let rect = Rectangle {
                x,
                y,
                width: w,
                height: h,
            };
            renderer.draw_image(
                iced::advanced::image::Image::new(im.handle.clone()),
                rect,
                clip,
            );
        }

        // Scrollbar (only when scrollback exists).
        if let Some((track_top, track_h, sb_x, thumb_y, thumb_h)) = self.scrollbar_geometry(bounds)
        {
            let fg = self.theme.terminal_foreground();
            let track = Color { a: 0.10, ..fg };
            let thumb = Color { a: 0.45, ..fg };
            renderer.fill_quad(
                solid_quad(Rectangle {
                    x: sb_x,
                    y: track_top,
                    width: SCROLLBAR_WIDTH,
                    height: track_h,
                }),
                Background::Color(track),
            );
            renderer.fill_quad(
                Quad {
                    bounds: Rectangle {
                        x: sb_x + 1.0,
                        y: thumb_y,
                        width: SCROLLBAR_WIDTH - 2.0,
                        height: thumb_h,
                    },
                    border: Border {
                        color: Color::TRANSPARENT,
                        width: 0.0,
                        radius: ((SCROLLBAR_WIDTH - 2.0) / 2.0).into(),
                    },
                    shadow: Shadow::default(),
                    snap: true,
                },
                Background::Color(thumb),
            );
        }
    }

    fn mouse_interaction(
        &self,
        _tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        _viewport: &Rectangle,
        _renderer: &Renderer,
    ) -> mouse::Interaction {
        if let Some(p) = cursor.position_over(layout.bounds()) {
            let (c, r) = self.cell_at(p, layout.bounds());
            if self.link_at(c, r).is_some() {
                return mouse::Interaction::Pointer;
            }
        }
        mouse::Interaction::default()
    }
}

impl<'a, Message, Renderer> From<TermWidget<'a, Message>>
    for Element<'a, Message, iced::Theme, Renderer>
where
    Renderer: text::Renderer<Font = iced::Font>
        + iced::advanced::image::Renderer<Handle = iced::advanced::image::Handle>
        + 'a,
    Message: 'a,
{
    fn from(w: TermWidget<'a, Message>) -> Self {
        Element::new(w)
    }
}
