use crate::terminal::Color;
use crate::theme::Theme;
use iced::Color as IColor;

/// Map a Color enum variant to an index into the theme's 16-color ANSI palette.
fn ansi_index(color: Color) -> Option<usize> {
    match color {
        Color::Black => Some(0),
        Color::Red => Some(1),
        Color::Green => Some(2),
        Color::Yellow => Some(3),
        Color::Blue => Some(4),
        Color::Magenta => Some(5),
        Color::Cyan => Some(6),
        Color::White => Some(7),
        Color::BrightBlack => Some(8),
        Color::BrightRed => Some(9),
        Color::BrightGreen => Some(10),
        Color::BrightYellow => Some(11),
        Color::BrightBlue => Some(12),
        Color::BrightMagenta => Some(13),
        Color::BrightCyan => Some(14),
        Color::BrightWhite => Some(15),
        _ => None,
    }
}

/// Resolve a foreground color using the theme palette, with VTE4-compatible
/// bold-brightening and dim attenuation.
pub fn resolve_fg(color: Color, theme: &Theme, bold: bool, dim: bool) -> IColor {
    let base = match color {
        Color::Default => theme.terminal_foreground(),
        Color::Indexed(idx) => color_256(idx, theme),
        Color::Rgb(r, g, b) => IColor::from_rgb8(r, g, b),
        _ => {
            let idx = ansi_index(color).unwrap();
            // VTE4: bold + standard color (0-7) promotes to bright variant (8-15)
            let idx = if bold && idx < 8 { idx + 8 } else { idx };
            theme.ansi_color(idx)
        }
    };
    if dim {
        IColor {
            r: base.r * 2.0 / 3.0,
            g: base.g * 2.0 / 3.0,
            b: base.b * 2.0 / 3.0,
            a: base.a,
        }
    } else {
        base
    }
}

/// Resolve a background color using the theme palette.
pub fn resolve_bg(color: Color, theme: &Theme) -> IColor {
    match color {
        Color::Default => theme.terminal_background(),
        Color::Indexed(idx) => color_256(idx, theme),
        Color::Rgb(r, g, b) => IColor::from_rgb8(r, g, b),
        _ => {
            let idx = ansi_index(color).unwrap();
            theme.ansi_color(idx)
        }
    }
}

/// 256-color palette resolution using theme colors for indices 0-15.
pub fn color_256(idx: u8, theme: &Theme) -> IColor {
    match idx {
        0..=15 => theme.ansi_color(idx as usize),
        16..=231 => {
            let idx = idx - 16;
            let r_idx = idx / 36;
            let g_idx = (idx % 36) / 6;
            let b_idx = idx % 6;
            let r = if r_idx == 0 { 0 } else { 55 + r_idx * 40 };
            let g = if g_idx == 0 { 0 } else { 55 + g_idx * 40 };
            let b = if b_idx == 0 { 0 } else { 55 + b_idx * 40 };
            IColor::from_rgb8(r, g, b)
        }
        232..=255 => {
            let gray = 8 + (idx - 232) * 10;
            IColor::from_rgb8(gray, gray, gray)
        }
    }
}

#[allow(dead_code)]
pub mod defaults {
    use iced::Color;

    pub const FOREGROUND: Color = Color::from_rgb(229.0 / 255.0, 229.0 / 255.0, 229.0 / 255.0);
    pub const BACKGROUND: Color = Color::from_rgb(29.0 / 255.0, 29.0 / 255.0, 29.0 / 255.0);
    pub const CURSOR: Color = Color::from_rgb(127.0 / 255.0, 127.0 / 255.0, 127.0 / 255.0);
    pub fn selection() -> Color {
        Color::from_rgba(200.0 / 255.0, 200.0 / 255.0, 200.0 / 255.0, 100.0 / 255.0)
    }
}
