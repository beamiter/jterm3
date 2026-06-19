use crate::kitty_graphics::KittyGraphicsState;
use base64::Engine;
use smallvec::SmallVec;
use std::collections::VecDeque;
use unicode_normalization::UnicodeNormalization;

/// Character class for word selection boundaries.
#[derive(PartialEq)]
enum CharClass {
    Word,
    Whitespace,
    Symbol,
}

fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

fn is_whitespace_char(c: char) -> bool {
    c == ' ' || c == '\t' || c == '\0'
}

fn char_class(c: char) -> CharClass {
    if is_word_char(c) {
        CharClass::Word
    } else if is_whitespace_char(c) {
        CharClass::Whitespace
    } else {
        CharClass::Symbol
    }
}

fn is_extended_token_separator(c: char) -> bool {
    matches!(
        c,
        '/' | '\\' | '.' | ':' | '-' | '~' | '?' | '&' | '=' | '#' | '%' | '+' | '@'
    )
}

fn is_extended_token_char(c: char) -> bool {
    is_word_char(c) || is_extended_token_separator(c)
}

fn is_token_prefix_wrapper(c: char) -> bool {
    matches!(c, '"' | '\'' | '`' | '(' | '[' | '{' | '<')
}

fn is_token_suffix_wrapper(c: char) -> bool {
    matches!(
        c,
        '"' | '\'' | '`' | ')' | ']' | '}' | '>' | ',' | ';' | '!'
    )
}

const PRIMARY_DEVICE_ATTRIBUTES_RESPONSE: &[u8] = b"\x1b[?65;1;9c";
const SECONDARY_DEVICE_ATTRIBUTES_RESPONSE: &[u8] = b"\x1b[>1;7802;0c";
const XTERM_VERSION_RESPONSE: &[u8] = b"\x1bP>|VTE(7802)\x1b\\";
pub const MAX_TERMINAL_COLS: usize = 1024;
pub const MAX_TERMINAL_ROWS: usize = 512;

pub fn clamp_terminal_dimensions(cols: usize, rows: usize) -> (usize, usize) {
    (
        cols.clamp(1, MAX_TERMINAL_COLS),
        rows.clamp(1, MAX_TERMINAL_ROWS),
    )
}

/// 连续内存网格存储 - 优化内存局部性和缓存命中率
/// 内存布局: cells[row * cols + col] 对应 grid[row][col]
#[derive(Clone)]
pub struct TerminalGrid {
    cells: Vec<TerminalCell>,
    rows: usize,
    cols: usize,
    pub row_wrapped: Vec<bool>,
}

impl TerminalGrid {
    pub fn new(rows: usize, cols: usize) -> Self {
        TerminalGrid {
            cells: vec![TerminalCell::default(); rows * cols],
            rows,
            cols,
            row_wrapped: vec![false; rows],
        }
    }

    #[inline]
    pub fn get(&self, row: usize, col: usize) -> &TerminalCell {
        &self.cells[row * self.cols + col]
    }

    #[inline]
    pub fn get_mut(&mut self, row: usize, col: usize) -> &mut TerminalCell {
        &mut self.cells[row * self.cols + col]
    }

    #[inline]
    pub fn rows(&self) -> usize {
        self.rows
    }

    #[inline]
    pub fn cols(&self) -> usize {
        self.cols
    }

    /// 获取行作Vec引用（用于兼容旧代码）
    pub fn get_row(&self, row: usize) -> Vec<TerminalCell> {
        let start = row * self.cols;
        let end = start + self.cols;
        self.cells[start..end].to_vec()
    }

    /// 返回行数（兼容 grid.len()）
    #[inline]
    #[allow(dead_code)]
    pub fn len(&self) -> usize {
        self.rows
    }

    /// 返回行数（兼容 grid[i].len()）
    #[inline]
    pub fn row_len(&self) -> usize {
        self.cols
    }

    /// 设置整行
    #[allow(dead_code)]
    pub fn set_row(&mut self, row: usize, cells: Vec<TerminalCell>) {
        let start = row * self.cols;
        let copy_len = cells.len().min(self.cols);
        self.cells[start..start + copy_len].copy_from_slice(&cells[..copy_len]);
    }

    /// 获取所有行为Vec<Vec> (用于兼容旧代码)
    pub fn to_vec(&self) -> Vec<Vec<TerminalCell>> {
        self.cells
            .chunks_exact(self.cols)
            .map(|chunk| chunk.to_vec())
            .collect()
    }


    /// 在行内指定列插入一个cell，右侧cell右移，末尾cell被丢弃
    pub fn insert_cell_in_row(&mut self, row: usize, col: usize, cell: TerminalCell) {
        if row >= self.rows || col >= self.cols {
            return;
        }
        let start = row * self.cols;
        self.cells.copy_within(start + col..start + self.cols - 1, start + col + 1);
        self.cells[start + col] = cell;
    }

    /// 删除行内指定列的cell，右侧cell左移，末尾补blank
    pub fn remove_cell_from_row(&mut self, row: usize, col: usize) {
        if row >= self.rows || col >= self.cols {
            return;
        }
        let start = row * self.cols;
        self.cells.copy_within(start + col + 1..start + self.cols, start + col);
        // Fill last cell with default
        self.cells[start + self.cols - 1] = TerminalCell::default();
    }

    /// 删除第一行，向上移动所有行，末尾补新行
    #[allow(dead_code)]
    pub fn remove_first_row(&mut self) -> (Vec<TerminalCell>, bool) {
        let removed = self.get_row(0);
        let was_wrapped = self.row_wrapped[0];
        self.shift_rows_up();
        (removed, was_wrapped)
    }

    /// Shift all rows up by one (discard first row, blank last row).
    /// Does not return the removed row - use get_row(0) before calling if needed.
    #[inline]
    pub fn shift_rows_up(&mut self) {
        self.cells.copy_within(self.cols.., 0);
        let last_start = (self.rows - 1) * self.cols;
        self.cells[last_start..].fill(TerminalCell::default());
        self.row_wrapped.copy_within(1.., 0);
        self.row_wrapped[self.rows - 1] = false;
    }

    /// 用blank_cell填充末尾一行
    pub fn fill_last_row(&mut self, cell: TerminalCell) {
        let last_start = (self.rows - 1) * self.cols;
        self.cells[last_start..].fill(cell);
    }

    /// 是否为空
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.rows == 0
    }

    /// 调整网格大小
    pub fn resize(&mut self, new_rows: usize, new_cols: usize, default_cell: TerminalCell) {
        let mut new_cells = vec![default_cell; new_rows * new_cols];
        let copy_rows = self.rows.min(new_rows);
        let copy_cols = self.cols.min(new_cols);
        for row in 0..copy_rows {
            let src_start = row * self.cols;
            let dst_start = row * new_cols;
            new_cells[dst_start..dst_start + copy_cols]
                .copy_from_slice(&self.cells[src_start..src_start + copy_cols]);
        }
        self.cells = new_cells;
        let mut new_wrapped = vec![false; new_rows];
        new_wrapped[..copy_rows].copy_from_slice(&self.row_wrapped[..copy_rows]);
        self.row_wrapped = new_wrapped;
        self.rows = new_rows;
        self.cols = new_cols;
    }

    /// 获取mut访问所有行
    pub fn iter_mut(&mut self) -> std::slice::ChunksMut<'_, TerminalCell> {
        self.cells.chunks_mut(self.cols)
    }

    /// 获取只读访问所有行
    pub fn iter(&self) -> std::slice::Chunks<'_, TerminalCell> {
        self.cells.chunks(self.cols)
    }
}

impl std::ops::Index<usize> for TerminalGrid {
    type Output = [TerminalCell];
    fn index(&self, row: usize) -> &[TerminalCell] {
        let start = row * self.cols;
        &self.cells[start..start + self.cols]
    }
}

impl std::ops::IndexMut<usize> for TerminalGrid {
    fn index_mut(&mut self, row: usize) -> &mut [TerminalCell] {
        let start = row * self.cols;
        &mut self.cells[start..start + self.cols]
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Color {
    Black,
    Red,
    Green,
    Yellow,
    Blue,
    Magenta,
    Cyan,
    White,
    BrightBlack,
    BrightRed,
    BrightGreen,
    BrightYellow,
    BrightBlue,
    BrightMagenta,
    BrightCyan,
    BrightWhite,
    Indexed(u8),
    Rgb(u8, u8, u8),
    Default,
}

#[derive(Clone, Debug)]
#[derive(Default)]
pub enum CursorShape {
    #[default]
    Block,     // 0 or 1 - block cursor (default)
    Underline, // 2 - underline cursor
    Beam,      // 3 - beam/vertical line cursor
}


#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[derive(Default)]
pub enum UnderlineStyle {
    #[default]
    None,
    Single,
    Double,
    #[allow(dead_code)]
    Curly,   // SGR 4:3
    #[allow(dead_code)]
    Dotted,  // SGR 4:4
    #[allow(dead_code)]
    Dashed,  // SGR 4:5
}

/// Packed style flags in a u16 bitfield (includes wide character bits).
/// Layout:
///   bit 0: bold
///   bit 1: italic
///   bit 2-4: underline style (3 bits, 0-5)
///   bit 5: inverse
///   bit 6: dim
///   bit 7: blink
///   bit 8: strikethrough
///   bit 9: wide
///   bit 10: wide_continuation
#[derive(Clone, Copy, PartialEq, Eq, Default)]
#[repr(transparent)]
pub struct StyleFlags(u16);

impl std::fmt::Debug for StyleFlags {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StyleFlags")
            .field("bold", &self.bold())
            .field("italic", &self.italic())
            .field("underline", &self.underline())
            .field("inverse", &self.inverse())
            .field("dim", &self.dim())
            .field("blink", &self.blink())
            .field("strikethrough", &self.strikethrough())
            .finish()
    }
}

const BOLD_BIT: u16 = 1 << 0;
const ITALIC_BIT: u16 = 1 << 1;
const UNDERLINE_SHIFT: u32 = 2;
const UNDERLINE_MASK: u16 = 0b111 << 2;
const INVERSE_BIT: u16 = 1 << 5;
const DIM_BIT: u16 = 1 << 6;
const BLINK_BIT: u16 = 1 << 7;
const STRIKETHROUGH_BIT: u16 = 1 << 8;
const WIDE_BIT: u16 = 1 << 9;
const WIDE_CONT_BIT: u16 = 1 << 10;

impl StyleFlags {
    #[inline(always)]
    pub fn new() -> Self { Self(0) }

    #[inline(always)]
    pub fn bold(&self) -> bool { self.0 & BOLD_BIT != 0 }
    #[inline(always)]
    pub fn italic(&self) -> bool { self.0 & ITALIC_BIT != 0 }
    #[inline(always)]
    pub fn underline(&self) -> UnderlineStyle {
        match (self.0 & UNDERLINE_MASK) >> UNDERLINE_SHIFT {
            1 => UnderlineStyle::Single,
            2 => UnderlineStyle::Double,
            3 => UnderlineStyle::Curly,
            4 => UnderlineStyle::Dotted,
            5 => UnderlineStyle::Dashed,
            _ => UnderlineStyle::None,
        }
    }
    #[inline(always)]
    pub fn inverse(&self) -> bool { self.0 & INVERSE_BIT != 0 }
    #[inline(always)]
    pub fn dim(&self) -> bool { self.0 & DIM_BIT != 0 }
    #[inline(always)]
    pub fn blink(&self) -> bool { self.0 & BLINK_BIT != 0 }
    #[inline(always)]
    pub fn strikethrough(&self) -> bool { self.0 & STRIKETHROUGH_BIT != 0 }
    #[inline(always)]
    pub fn wide(&self) -> bool { self.0 & WIDE_BIT != 0 }
    #[inline(always)]
    pub fn wide_continuation(&self) -> bool { self.0 & WIDE_CONT_BIT != 0 }

    #[inline(always)]
    pub fn set_bold(&mut self, v: bool) { if v { self.0 |= BOLD_BIT; } else { self.0 &= !BOLD_BIT; } }
    #[inline(always)]
    pub fn set_italic(&mut self, v: bool) { if v { self.0 |= ITALIC_BIT; } else { self.0 &= !ITALIC_BIT; } }
    #[inline(always)]
    pub fn set_underline(&mut self, v: UnderlineStyle) {
        self.0 = (self.0 & !UNDERLINE_MASK) | ((v as u16) << UNDERLINE_SHIFT);
    }
    #[inline(always)]
    pub fn set_inverse(&mut self, v: bool) { if v { self.0 |= INVERSE_BIT; } else { self.0 &= !INVERSE_BIT; } }
    #[inline(always)]
    pub fn set_dim(&mut self, v: bool) { if v { self.0 |= DIM_BIT; } else { self.0 &= !DIM_BIT; } }
    #[inline(always)]
    pub fn set_blink(&mut self, v: bool) { if v { self.0 |= BLINK_BIT; } else { self.0 &= !BLINK_BIT; } }
    #[inline(always)]
    pub fn set_strikethrough(&mut self, v: bool) { if v { self.0 |= STRIKETHROUGH_BIT; } else { self.0 &= !STRIKETHROUGH_BIT; } }
    #[inline(always)]
    pub fn set_wide(&mut self, v: bool) { if v { self.0 |= WIDE_BIT; } else { self.0 &= !WIDE_BIT; } }
    #[inline(always)]
    pub fn set_wide_continuation(&mut self, v: bool) { if v { self.0 |= WIDE_CONT_BIT; } else { self.0 &= !WIDE_CONT_BIT; } }

    #[inline(always)]
    pub fn is_default_style(&self) -> bool { self.0 & 0x1FF == 0 }
}

#[derive(Clone, Debug)]
pub struct ScrollbackLine {
    data: CompressedLineData,
    pub is_wrapped: bool,
    cols: u16,
}

#[derive(Clone, Debug)]
enum CompressedLineData {
    Plain(String, u16),
    Encoded(Vec<u8>),
}

impl ScrollbackLine {
    pub fn compress(cells: &[TerminalCell], is_wrapped: bool) -> Self {
        let cols = cells.len() as u16;
        let trailing_blanks = cells.iter().rev()
            .take_while(|c| c.character == ' ' && c.foreground == Color::Default
                && c.background == Color::Default && c.flags.is_default_style()
                && !c.flags.wide() && !c.flags.wide_continuation())
            .count();

        let active_len = cells.len() - trailing_blanks;
        let all_default_attrs = cells[..active_len].iter().all(|c|
            c.foreground == Color::Default && c.background == Color::Default
            && c.flags.is_default_style()
            && !c.flags.wide() && !c.flags.wide_continuation()
        );

        if all_default_attrs {
            let text: String = cells[..active_len].iter().map(|c| c.character).collect();
            ScrollbackLine {
                data: CompressedLineData::Plain(text, trailing_blanks as u16),
                is_wrapped,
                cols,
            }
        } else {
            let encoded = Self::encode_cells(&cells[..active_len]);
            ScrollbackLine {
                data: CompressedLineData::Encoded(encoded),
                is_wrapped,
                cols,
            }
        }
    }

    pub fn decompress(&self) -> Vec<TerminalCell> {
        match &self.data {
            CompressedLineData::Plain(text, trailing) => {
                let mut cells: Vec<TerminalCell> = text.chars()
                    .map(|ch| TerminalCell { character: ch, ..Default::default() })
                    .collect();
                cells.resize(cells.len() + *trailing as usize, TerminalCell::default());
                cells
            }
            CompressedLineData::Encoded(data) => {
                Self::decode_cells(data, self.cols as usize)
            }
        }
    }

    #[allow(dead_code)]
    pub fn cells(&self) -> Vec<TerminalCell> {
        self.decompress()
    }

    fn encode_color(color: &Color, buf: &mut Vec<u8>) {
        match color {
            Color::Default => buf.push(0),
            Color::Black => buf.push(1),
            Color::Red => buf.push(2),
            Color::Green => buf.push(3),
            Color::Yellow => buf.push(4),
            Color::Blue => buf.push(5),
            Color::Magenta => buf.push(6),
            Color::Cyan => buf.push(7),
            Color::White => buf.push(8),
            Color::BrightBlack => buf.push(9),
            Color::BrightRed => buf.push(10),
            Color::BrightGreen => buf.push(11),
            Color::BrightYellow => buf.push(12),
            Color::BrightBlue => buf.push(13),
            Color::BrightMagenta => buf.push(14),
            Color::BrightCyan => buf.push(15),
            Color::BrightWhite => buf.push(16),
            Color::Indexed(i) => { buf.push(17); buf.push(*i); }
            Color::Rgb(r, g, b) => { buf.push(18); buf.push(*r); buf.push(*g); buf.push(*b); }
        }
    }

    fn decode_color(data: &[u8], pos: &mut usize) -> Color {
        if *pos >= data.len() { return Color::Default; }
        let tag = data[*pos];
        *pos += 1;
        match tag {
            0 => Color::Default,
            1 => Color::Black,
            2 => Color::Red,
            3 => Color::Green,
            4 => Color::Yellow,
            5 => Color::Blue,
            6 => Color::Magenta,
            7 => Color::Cyan,
            8 => Color::White,
            9 => Color::BrightBlack,
            10 => Color::BrightRed,
            11 => Color::BrightGreen,
            12 => Color::BrightYellow,
            13 => Color::BrightBlue,
            14 => Color::BrightMagenta,
            15 => Color::BrightCyan,
            16 => Color::BrightWhite,
            17 => {
                let i = data.get(*pos).copied().unwrap_or(0);
                *pos += 1;
                Color::Indexed(i)
            }
            18 => {
                let r = data.get(*pos).copied().unwrap_or(0);
                let g = data.get(*pos + 1).copied().unwrap_or(0);
                let b = data.get(*pos + 2).copied().unwrap_or(0);
                *pos += 3;
                Color::Rgb(r, g, b)
            }
            _ => Color::Default,
        }
    }

    fn encode_flags(flags: &StyleFlags) -> u8 {
        let mut f = 0u8;
        if flags.bold() { f |= 1; }
        if flags.italic() { f |= 2; }
        match flags.underline() {
            UnderlineStyle::None => {}
            UnderlineStyle::Single => f |= 4,
            UnderlineStyle::Double => f |= 8,
            UnderlineStyle::Curly => f |= 12,
            UnderlineStyle::Dotted => f |= 16,
            UnderlineStyle::Dashed => f |= 20,
        }
        if flags.inverse() { f |= 32; }
        if flags.dim() { f |= 64; }
        if flags.strikethrough() { f |= 128; }
        f
    }

    fn decode_flags(f: u8) -> StyleFlags {
        let underline = match (f >> 2) & 0x7 {
            0 => UnderlineStyle::None,
            1 => UnderlineStyle::Single,
            2 => UnderlineStyle::Double,
            3 => UnderlineStyle::Curly,
            4 => UnderlineStyle::Dotted,
            5 => UnderlineStyle::Dashed,
            _ => UnderlineStyle::None,
        };
        let mut flags = StyleFlags::new();
        flags.set_bold(f & 1 != 0);
        flags.set_italic(f & 2 != 0);
        flags.set_underline(underline);
        flags.set_inverse(f & 32 != 0);
        flags.set_dim(f & 64 != 0);
        flags.set_strikethrough(f & 128 != 0);
        flags
    }

    fn encode_cells(cells: &[TerminalCell]) -> Vec<u8> {
        let mut buf = Vec::with_capacity(cells.len() * 3);
        let mut i = 0;
        while i < cells.len() {
            let cell = &cells[i];
            let ch_str = cell.character.to_string();
            let ch_bytes = ch_str.as_bytes();

            // RLE: count consecutive identical cells (packed flags comparison is a single u16 ==)
            let mut run = 1u8;
            while (run as u16) < 255 && (i + run as usize) < cells.len() {
                let next = &cells[i + run as usize];
                if next.character == cell.character && next.foreground == cell.foreground
                    && next.background == cell.background && next.flags == cell.flags
                {
                    run += 1;
                } else {
                    break;
                }
            }

            // Format: [char_len:1][char_bytes][fg][bg][flags:1][wide_bits:1][run:1]
            buf.push(ch_bytes.len() as u8);
            buf.extend_from_slice(ch_bytes);
            Self::encode_color(&cell.foreground, &mut buf);
            Self::encode_color(&cell.background, &mut buf);
            let f = Self::encode_flags(&cell.flags);
            buf.push(f);
            let wide_bits = if cell.flags.wide() { 1u8 } else { 0 } | if cell.flags.wide_continuation() { 2 } else { 0 };
            buf.push(wide_bits);
            buf.push(run);

            i += run as usize;
        }
        buf
    }

    fn decode_cells(data: &[u8], cols: usize) -> Vec<TerminalCell> {
        let mut cells = Vec::with_capacity(cols);
        let mut pos = 0;
        while pos < data.len() {
            let ch_len = data[pos] as usize;
            pos += 1;
            if pos + ch_len > data.len() { break; }
            let ch = std::str::from_utf8(&data[pos..pos + ch_len])
                .ok()
                .and_then(|s| s.chars().next())
                .unwrap_or(' ');
            pos += ch_len;

            let fg = Self::decode_color(data, &mut pos);
            let bg = Self::decode_color(data, &mut pos);
            let f = data.get(pos).copied().unwrap_or(0);
            pos += 1;
            let wide_bits = data.get(pos).copied().unwrap_or(0);
            pos += 1;
            let run = data.get(pos).copied().unwrap_or(1).max(1);
            pos += 1;

            let mut flags = Self::decode_flags(f);
            flags.set_wide(wide_bits & 1 != 0);
            flags.set_wide_continuation(wide_bits & 2 != 0);

            let cell = TerminalCell {
                character: ch,
                foreground: fg,
                background: bg,
                flags,
            };
            for _ in 0..run {
                cells.push(cell);
            }
        }
        // Pad to cols
        cells.resize(cols, TerminalCell::default());
        cells
    }
}

#[derive(Clone, Copy, Debug)]
pub struct TerminalCell {
    pub character: char,
    pub foreground: Color,
    pub background: Color,
    pub flags: StyleFlags,
}

impl Default for TerminalCell {
    fn default() -> Self {
        TerminalCell {
            character: ' ',
            foreground: Color::Default,
            background: Color::Default,
            flags: StyleFlags::new(),
        }
    }
}

const _: () = assert!(std::mem::size_of::<TerminalCell>() == 16);

/// 追踪改变的行和列区间（脏矩形）
#[derive(Clone, Debug)]
pub struct DirtyRegion {
    pub rows: Vec<(usize, usize)>, // (row_start, row_end)，包含端点
    #[allow(dead_code)]
    pub col_start: usize,
    #[allow(dead_code)]
    pub col_end: usize,
}

impl DirtyRegion {
    pub fn new(cols: usize) -> Self {
        DirtyRegion {
            rows: Vec::new(),
            col_start: 0,
            col_end: cols,
        }
    }

    /// 标记某一行为脏
    pub fn mark_row(&mut self, row: usize) {
        if let Some(last) = self.rows.last_mut() {
            if row > 0 && last.1 == row - 1 {
                // 合并相邻的行
                last.1 = row;
                return;
            }
        }
        self.rows.push((row, row));
    }

    /// 标记行范围为脏
    pub fn mark_rows(&mut self, start: usize, end: usize) {
        for row in start..=end {
            self.mark_row(row);
        }
    }

    /// 标记整个网格为脏
    pub fn mark_all(&mut self, rows: usize) {
        self.rows.clear();
        self.rows.push((0, rows.saturating_sub(1)));
    }

    /// 清除脏标记
    pub fn clear(&mut self) {
        self.rows.clear();
    }

}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SelectionMode {
    Normal,
    Block,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Selection {
    pub anchor: (usize, usize),
    pub active: (usize, usize),
    pub mode: SelectionMode,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Charset {
    #[default]
    Ascii,
    DecSpecialGraphics,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ClipboardReadKind {
    MimeList,
    MimeData(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClipboardReadRequest {
    pub kind: ClipboardReadKind,
}

#[allow(dead_code)]
#[derive(Clone, Debug)]
pub struct CommandZone {
    pub prompt_start: usize,
    pub command_start: Option<usize>,
    pub output_start: Option<usize>,
    pub output_end: Option<usize>,
    pub exit_code: Option<i32>,
}

#[derive(Clone, Debug, Default)]
enum ZoneState {
    #[default]
    Idle,
    PromptStarted(usize),
    CommandStarted(usize, usize),
    OutputStarted(usize, usize, usize),
}

#[derive(Clone, Debug, Default)]
struct TerminalModes {
    bits: u32,
}

impl TerminalModes {
    const fn bit_index(mode: u16) -> Option<u32> {
        match mode {
            7 => Some(0),
            25 => Some(1),
            1000 => Some(2),
            1001 => Some(3),
            1002 => Some(4),
            1003 => Some(5),
            1004 => Some(6),
            1006 => Some(7),
            1049 => Some(8),
            2004 => Some(9),
            2026 => Some(10),
            2031 => Some(11),
            5522 => Some(12),
            1 => Some(13),    // DECCKM application cursor keys
            4 => Some(14),    // IRM insert/replace mode
            6 => Some(15),    // DECOM origin mode
            1005 => Some(16), // UTF-8 mouse encoding
            1015 => Some(17), // urxvt mouse encoding
            _ => None,
        }
    }

    #[inline]
    fn contains(&self, mode: &u16) -> bool {
        match Self::bit_index(*mode) {
            Some(bit) => self.bits & (1 << bit) != 0,
            None => false,
        }
    }

    #[inline]
    fn insert(&mut self, mode: u16) {
        if let Some(bit) = Self::bit_index(mode) {
            self.bits |= 1 << bit;
        }
    }

    #[inline]
    fn remove(&mut self, mode: &u16) {
        if let Some(bit) = Self::bit_index(*mode) {
            self.bits &= !(1 << bit);
        }
    }
}

/// Full cursor state saved by DECSC (ESC 7) / CSI s and restored by DECRC (ESC 8) / CSI u.
/// Per the VT spec this captures more than position: SGR attributes, the active charsets,
/// and origin mode.
#[derive(Clone, Copy)]
struct SavedCursor {
    row: usize,
    col: usize,
    fg: Color,
    bg: Color,
    flags: StyleFlags,
    g0: Charset,
    g1: Charset,
    active: Charset,
    origin_mode: bool,
}

pub struct TerminalState {
    pub grid: TerminalGrid,
    alt_grid: TerminalGrid,
    pub scrollback: VecDeque<ScrollbackLine>,
    pub selection: Option<Selection>,
    pub scroll_offset: usize,
    max_scrollback: usize,
    use_alt_buffer: bool,

    pub cursor_row: usize,
    pub cursor_col: usize,
    // Cursor position saved when switching to the alternate screen (mode 1049).
    // Kept separate from the DECSC slot so the two don't clobber each other.
    saved_cursor_row: usize,
    saved_cursor_col: usize,
    // DECSC/DECRC (and CSI s/u) saved full cursor state.
    saved_cursor: Option<SavedCursor>,
    // Per-column horizontal tab stops (HTS/TBC); index = column.
    tab_stops: Vec<bool>,
    // Last printed character, for REP (CSI b).
    last_printed_char: Option<char>,
    alt_cursor_row: usize,
    alt_cursor_col: usize,
    pub cursor_shape: CursorShape,

    current_fg: Color,
    current_bg: Color,
    current_flags: StyleFlags,
    pub window_title: String,

    // Global background color set by vim (CSI ... m)
    pub global_bg: Color,

    // Scrolling region (DECSTBM)
    scroll_region_top: usize,
    scroll_region_bottom: usize,

    // UTF-8 decoding buffer
    utf8_buf: [u8; 4],
    utf8_len: u8,
    utf8_expected: u8,

    // Incomplete escape sequence buffer across PTY reads
    pending_escape: Vec<u8>,

    g0_charset: Charset,
    g1_charset: Charset,
    active_charset: Charset,

    // IME support
    pub ime_enabled: bool,
    pub preedit_text: String,
    /// Byte range within `preedit_text` the IME marks as the active cursor /
    /// selection, used to highlight it in the over-the-spot overlay.
    pub preedit_selection: Option<std::ops::Range<usize>>,

    modes: TerminalModes,

    // Output buffer for DSR/CPR responses to be sent back to PTY
    pub output_buffer: Vec<u8>,

    keyboard_enhancement_flags: u16,
    keyboard_enhancement_stack: Vec<u16>,
    alt_keyboard_enhancement_flags: u16,
    alt_keyboard_enhancement_stack: Vec<u16>,
    xterm_modify_other_keys: u16,
    xterm_format_other_keys: u16,
    pending_clipboard_requests: Vec<ClipboardReadRequest>,
    pending_paste_password: Option<String>,

    // Kitty graphics protocol support
    pub kitty_graphics: KittyGraphicsState,

    // Dirty rectangle tracking for optimized rendering
    pub dirty_region: DirtyRegion,

    // P4 优化：行版本化追踪
    pub grid_version: u64,      // 全局网格版本号
    pub row_versions: Vec<u64>, // 每行的修改版本号

    // Cached visible cells to avoid per-frame cloning
    visible_cells_cache: Option<(u64, usize, std::sync::Arc<Vec<Vec<TerminalCell>>>)>,

    // OSC 8 hyperlink tracking
    current_hyperlink: Option<(String, Option<String>)>, // (url, id)
    #[allow(dead_code)]
    osc8_hyperlinks: Vec<crate::link::Link>, // Stored hyperlinks from OSC 8

    // Synchronized output (mode 2026): suppress rendering until mode is cleared
    pub sync_output_active: bool,
    sync_output_start: Option<std::time::Instant>,

    // OSC 52 clipboard set requests (selection_param, decoded_text)
    pub pending_osc52_clipboard_set: Option<String>,
    // OSC 52 clipboard query pending (needs clipboard read + response)
    pub pending_osc52_clipboard_query: bool,

    // OSC 133 shell integration: command zones for prompt navigation
    pub command_zones: VecDeque<CommandZone>,
    current_zone_state: ZoneState,

    // OSC 10/11/12 dynamic colors
    pub dynamic_fg: Option<(u8, u8, u8)>,
    pub dynamic_bg: Option<(u8, u8, u8)>,
    pub dynamic_cursor_color: Option<(u8, u8, u8)>,

    // OSC 9/777 pending notifications
    pub pending_notifications: Vec<(String, String)>,
}

impl TerminalState {
    fn parse_csi_params(param_bytes: &[u8]) -> SmallVec<[u16; 8]> {
        let mut params = SmallVec::new();
        let mut current: u16 = 0;
        let mut has_digits = false;

        for &byte in param_bytes {
            match byte {
                b'0'..=b'9' => {
                    current = current.saturating_mul(10).saturating_add((byte - b'0') as u16);
                    has_digits = true;
                }
                b';' | b':' => {
                    if has_digits {
                        params.push(current);
                    }
                    current = 0;
                    has_digits = false;
                }
                _ => {}
            }
        }

        if has_digits {
            params.push(current);
        }

        params
    }

    /// Parse SGR parameters into groups. Top-level parameters are separated by ';';
    /// within a group, ':' introduces sub-parameters (ISO 8613-6 / curly underline,
    /// e.g. `4:3` or `38:2:r:g:b`). Empty fields parse as 0 so positions are preserved.
    fn parse_sgr_groups(param_bytes: &[u8]) -> SmallVec<[SmallVec<[u16; 6]>; 8]> {
        let mut groups: SmallVec<[SmallVec<[u16; 6]>; 8]> = SmallVec::new();
        let mut group: SmallVec<[u16; 6]> = SmallVec::new();
        let mut current: u16 = 0;

        for &byte in param_bytes {
            match byte {
                b'0'..=b'9' => {
                    current = current.saturating_mul(10).saturating_add((byte - b'0') as u16);
                }
                b':' => {
                    group.push(current);
                    current = 0;
                }
                b';' => {
                    group.push(current);
                    groups.push(std::mem::take(&mut group));
                    current = 0;
                }
                _ => {}
            }
        }
        group.push(current);
        groups.push(group);
        groups
    }

    pub fn new(cols: usize, rows: usize) -> Self {
        let (cols, rows) = clamp_terminal_dimensions(cols, rows);
        let grid = TerminalGrid::new(rows, cols);
        let alt_grid = TerminalGrid::new(rows, cols);

        let mut modes = TerminalModes::default();
        modes.insert(25);
        modes.insert(7);

        let mut dirty_region = DirtyRegion::new(cols);
        // Mark all rows as dirty on initialization to ensure first frame renders correctly
        dirty_region.mark_all(rows);

        TerminalState {
            grid,
            alt_grid,
            scrollback: VecDeque::new(),
            selection: None,
            scroll_offset: 0,
            max_scrollback: 10000,
            use_alt_buffer: false,
            cursor_row: 0,
            cursor_col: 0,
            saved_cursor_row: 0,
            saved_cursor_col: 0,
            saved_cursor: None,
            tab_stops: Self::default_tab_stops(cols),
            last_printed_char: None,
            alt_cursor_row: 0,
            alt_cursor_col: 0,
            cursor_shape: CursorShape::default(),
            current_fg: Color::Default,
            current_bg: Color::Default,
            current_flags: StyleFlags::default(),
            window_title: String::new(),
            global_bg: Color::Default,
            utf8_buf: [0; 4],
            utf8_len: 0,
            utf8_expected: 0,
            pending_escape: Vec::new(),
            g0_charset: Charset::Ascii,
            g1_charset: Charset::Ascii,
            active_charset: Charset::Ascii,
            ime_enabled: false,
            preedit_text: String::new(),
            preedit_selection: None,
            scroll_region_top: 0,
            scroll_region_bottom: rows.saturating_sub(1),
            modes,
            output_buffer: Vec::new(),
            keyboard_enhancement_flags: 0,
            keyboard_enhancement_stack: Vec::new(),
            alt_keyboard_enhancement_flags: 0,
            alt_keyboard_enhancement_stack: Vec::new(),
            xterm_modify_other_keys: 0,
            xterm_format_other_keys: 0,
            pending_clipboard_requests: Vec::new(),
            pending_paste_password: None,
            kitty_graphics: KittyGraphicsState::new(),
            dirty_region,
            grid_version: 1,
            // IMPORTANT: row_versions must match grid.rows(), not the parameter 'rows'
            // This ensures dirty tracking works correctly even with scrollback
            row_versions: vec![1; rows],  // Use 'rows' here since grid.rows() == rows at init
            visible_cells_cache: None,
            current_hyperlink: None,
            osc8_hyperlinks: Vec::new(),
            sync_output_active: false,
            sync_output_start: None,
            pending_osc52_clipboard_set: None,
            pending_osc52_clipboard_query: false,
            command_zones: VecDeque::new(),
            current_zone_state: ZoneState::default(),
            dynamic_fg: None,
            dynamic_bg: None,
            dynamic_cursor_color: None,
            pending_notifications: Vec::new(),
        }
    }

    fn decode_base64(value: &str) -> Option<String> {
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(value)
            .ok()?;
        String::from_utf8(bytes).ok()
    }

    fn osc_terminator() -> &'static [u8] {
        b"\x1b\\"
    }

    fn append_osc_5522_status(&mut self, metadata: &str, payload: Option<&str>) {
        self.output_buffer.extend_from_slice(b"\x1b]5522;");
        self.output_buffer.extend_from_slice(metadata.as_bytes());
        if let Some(payload) = payload {
            self.output_buffer.extend_from_slice(b";");
            self.output_buffer.extend_from_slice(payload.as_bytes());
        }
        self.output_buffer.extend_from_slice(Self::osc_terminator());
    }

    fn handle_osc_color(&mut self, command: &str, value: &str) {
        if value == "?" {
            // Query: respond with current color
            let color = match command {
                "10" => self.dynamic_fg.unwrap_or((255, 255, 255)),
                "11" => self.dynamic_bg.unwrap_or((0, 0, 0)),
                "12" => self.dynamic_cursor_color.unwrap_or((255, 255, 255)),
                _ => return,
            };
            let response = format!(
                "\x1b]{};rgb:{:04x}/{:04x}/{:04x}\x1b\\",
                command,
                (color.0 as u16) * 257,
                (color.1 as u16) * 257,
                (color.2 as u16) * 257,
            );
            self.output_buffer.extend_from_slice(response.as_bytes());
        } else if let Some(rgb) = Self::parse_color_spec(value) {
            match command {
                "10" => self.dynamic_fg = Some(rgb),
                "11" => self.dynamic_bg = Some(rgb),
                "12" => self.dynamic_cursor_color = Some(rgb),
                _ => {}
            }
        }
    }

    fn parse_color_spec(spec: &str) -> Option<(u8, u8, u8)> {
        // Parse rgb:RR/GG/BB or rgb:RRRR/GGGG/BBBB or #RRGGBB
        if let Some(hex) = spec.strip_prefix('#') {
            if hex.len() == 6 {
                let r = u8::from_str_radix(&hex[0..2], 16).ok()?;
                let g = u8::from_str_radix(&hex[2..4], 16).ok()?;
                let b = u8::from_str_radix(&hex[4..6], 16).ok()?;
                return Some((r, g, b));
            }
        } else if let Some(rgb) = spec.strip_prefix("rgb:") {
            let parts: Vec<&str> = rgb.split('/').collect();
            if parts.len() == 3 {
                let r = u16::from_str_radix(parts[0], 16).ok()?;
                let g = u16::from_str_radix(parts[1], 16).ok()?;
                let b = u16::from_str_radix(parts[2], 16).ok()?;
                // Normalize to 8-bit
                let scale = if parts[0].len() == 4 { 257 } else { 1 };
                return Some(((r / scale) as u8, (g / scale) as u8, (b / scale) as u8));
            }
        }
        None
    }

    fn handle_osc_133(&mut self, value: &str) {
        let absolute_row = self.scrollback.len() + self.cursor_row;
        let mark = value.chars().next().unwrap_or('\0');
        match mark {
            'A' => {
                // Prompt start
                self.current_zone_state = ZoneState::PromptStarted(absolute_row);
            }
            'B' => {
                // Command start (user is typing the command)
                if let ZoneState::PromptStarted(prompt_start) = self.current_zone_state {
                    self.current_zone_state = ZoneState::CommandStarted(prompt_start, absolute_row);
                }
            }
            'C' => {
                // Command executed (output begins)
                if let ZoneState::CommandStarted(prompt_start, cmd_start) = self.current_zone_state {
                    self.current_zone_state =
                        ZoneState::OutputStarted(prompt_start, cmd_start, absolute_row);
                }
            }
            'D' => {
                // Command finished
                let exit_code = value.get(2..).and_then(|s| s.parse::<i32>().ok());
                match self.current_zone_state {
                    ZoneState::OutputStarted(prompt_start, cmd_start, out_start) => {
                        let zone = CommandZone {
                            prompt_start,
                            command_start: Some(cmd_start),
                            output_start: Some(out_start),
                            output_end: Some(absolute_row),
                            exit_code,
                        };
                        self.command_zones.push_back(zone);
                        if self.command_zones.len() > 256 {
                            self.command_zones.pop_front();
                        }
                    }
                    ZoneState::CommandStarted(prompt_start, cmd_start) => {
                        let zone = CommandZone {
                            prompt_start,
                            command_start: Some(cmd_start),
                            output_start: None,
                            output_end: Some(absolute_row),
                            exit_code,
                        };
                        self.command_zones.push_back(zone);
                        if self.command_zones.len() > 256 {
                            self.command_zones.pop_front();
                        }
                    }
                    _ => {}
                }
                self.current_zone_state = ZoneState::Idle;
            }
            _ => {}
        }
    }

    fn handle_osc_52(&mut self, value: &str) {
        // OSC 52 format: <selection>;<base64-data>
        // selection: c=clipboard, p=primary, s=select (we treat all as clipboard)
        // data: ? means query, base64 means set
        if let Some((_sel, data)) = value.split_once(';') {
            if data == "?" {
                // Query: signal main loop to read clipboard and respond
                self.pending_osc52_clipboard_query = true;
            } else if !data.is_empty() {
                // Set: decode base64 and store for main loop to apply
                if let Some(decoded) = Self::decode_base64(data) {
                    self.pending_osc52_clipboard_set = Some(decoded);
                }
            }
        }
    }

    fn handle_osc_5522(&mut self, metadata: &str, _payload: Option<&str>) {
        crate::debug_log!("[OSC5522] metadata={} payload={:?}", metadata, _payload);

        let mut message_type = None;
        let mut mime = None;
        let mut password = None;

        for part in metadata.split(':') {
            if let Some(value) = part.strip_prefix("type=") {
                message_type = Some(value);
            } else if let Some(value) = part.strip_prefix("mime=") {
                mime = Self::decode_base64(value);
            } else if let Some(value) = part.strip_prefix("password=") {
                password = Self::decode_base64(value);
            } else if let Some(value) = part.strip_prefix("pw=") {
                password = Self::decode_base64(value);
            }
        }

        if message_type != Some("read") {
            return;
        }

        let kind = if let Some(mime_type) = mime {
            if let Some(expected) = &self.pending_paste_password {
                if password.as_deref() != Some(expected.as_str()) {
                    self.append_osc_5522_status("type=read:status=EPERM", None);
                    return;
                }
            }
            self.pending_paste_password = None;
            ClipboardReadKind::MimeData(mime_type)
        } else {
            ClipboardReadKind::MimeList
        };

        self.pending_clipboard_requests
            .push(ClipboardReadRequest { kind });
    }

    fn set_keyboard_enhancement_flags(&mut self, flags: u16, mode: u16) {
        match mode {
            1 => self.keyboard_enhancement_flags = flags,
            2 => self.keyboard_enhancement_flags |= flags,
            3 => self.keyboard_enhancement_flags &= !flags,
            _ => {}
        }
    }

    fn push_keyboard_enhancement_flags(&mut self, flags: u16) {
        if self.keyboard_enhancement_stack.len() >= 32 {
            self.keyboard_enhancement_stack.remove(0);
        }
        self.keyboard_enhancement_stack
            .push(self.keyboard_enhancement_flags);
        self.keyboard_enhancement_flags = flags;
    }

    fn pop_keyboard_enhancement_flags(&mut self, count: usize) {
        for _ in 0..count.max(1) {
            match self.keyboard_enhancement_stack.pop() {
                Some(flags) => self.keyboard_enhancement_flags = flags,
                None => {
                    self.keyboard_enhancement_flags = 0;
                    break;
                }
            }
        }
    }

    /// Advance to the start of the next line, honoring the DECSTBM scroll region.
    /// When the cursor sits on the region's bottom row this scrolls the region up
    /// (pushing to scrollback only for a full-screen region); otherwise it just
    /// moves down. Used by autowrap and linefeed so both stay region-aware.
    fn wrap_to_next_line(&mut self) {
        self.grid.row_wrapped[self.cursor_row] = true;
        self.cursor_col = 0;
        if self.cursor_row == self.scroll_region_bottom {
            self.scroll_region_up(self.scroll_region_top, self.scroll_region_bottom);
        } else if self.cursor_row + 1 < self.grid.rows() {
            self.cursor_row += 1;
        }
    }

    /// IRM (insert mode): shift cells at/after `col` right by `count`, dropping the
    /// rightmost `count` cells off the end of the row.
    fn shift_cells_right(&mut self, row: usize, col: usize, count: usize) {
        let cols = self.grid.row_len();
        if count == 0 || col >= cols {
            return;
        }
        let blank = self.create_blank_cell();
        let line = &mut self.grid[row];
        if col + count < cols {
            line.copy_within(col..cols - count, col + count);
        }
        for cell in &mut line[col..(col + count).min(cols)] {
            *cell = blank.clone();
        }
        self.dirty_region.mark_row(row);
        self.mark_row_dirty(row);
    }

    /// Merge a zero-width combining mark into the preceding base cell when the
    /// pair has a single precomposed form (NFC); otherwise drop it.
    fn combine_with_previous(&mut self, mark: char) {
        if self.cursor_col == 0 {
            return;
        }
        let mut base_col = self.cursor_col - 1;
        if base_col > 0
            && self
                .grid
                .get(self.cursor_row, base_col)
                .flags
                .wide_continuation()
        {
            base_col -= 1;
        }
        let cell = self.grid.get_mut(self.cursor_row, base_col);
        let mut combined = String::with_capacity(8);
        combined.push(cell.character);
        combined.push(mark);
        let nfc: String = combined.nfc().collect();
        let mut chars = nfc.chars();
        if let (Some(c0), None) = (chars.next(), chars.next()) {
            cell.character = c0;
            self.dirty_region.mark_row(self.cursor_row);
            self.mark_row_dirty(self.cursor_row);
        }
    }

    fn default_tab_stops(cols: usize) -> Vec<bool> {
        (0..cols).map(|c| c % 8 == 0 && c != 0).collect()
    }

    /// Next tab stop strictly right of `col`, or the last column if none.
    fn next_tab_stop(&self, col: usize) -> usize {
        let cols = self.grid.row_len();
        let mut c = col + 1;
        while c < cols {
            if self.tab_stops.get(c).copied().unwrap_or(false) {
                return c;
            }
            c += 1;
        }
        cols.saturating_sub(1)
    }

    /// Previous tab stop strictly left of `col`, or column 0 if none.
    fn prev_tab_stop(&self, col: usize) -> usize {
        let mut c = col;
        while c > 0 {
            c -= 1;
            if self.tab_stops.get(c).copied().unwrap_or(false) {
                return c;
            }
        }
        0
    }

    fn save_cursor(&mut self) {
        self.saved_cursor = Some(SavedCursor {
            row: self.cursor_row,
            col: self.cursor_col,
            fg: self.current_fg,
            bg: self.current_bg,
            flags: self.current_flags,
            g0: self.g0_charset,
            g1: self.g1_charset,
            active: self.active_charset,
            origin_mode: self.modes.contains(&6),
        });
    }

    fn restore_cursor(&mut self) {
        if let Some(s) = self.saved_cursor {
            self.cursor_row = s.row.min(self.grid.rows().saturating_sub(1));
            self.cursor_col = s.col.min(self.grid.row_len().saturating_sub(1));
            self.current_fg = s.fg;
            self.current_bg = s.bg;
            self.current_flags = s.flags;
            self.g0_charset = s.g0;
            self.g1_charset = s.g1;
            self.active_charset = s.active;
            if s.origin_mode {
                self.modes.insert(6);
            } else {
                self.modes.remove(&6);
            }
        } else {
            self.cursor_row = 0;
            self.cursor_col = 0;
        }
    }

    /// Place the cursor for CUP/HVP (CSI H / f). Honors DECOM origin mode: when set,
    /// the row is relative to the scroll region and clamped within it.
    fn place_cursor(&mut self, row_1based: usize, col_1based: usize) {
        let row0 = row_1based.saturating_sub(1);
        let col0 = col_1based.saturating_sub(1);
        if self.modes.contains(&6) {
            self.cursor_row =
                (self.scroll_region_top + row0).min(self.scroll_region_bottom);
        } else {
            self.cursor_row = row0.min(self.grid.rows().saturating_sub(1));
        }
        self.cursor_col = col0.min(self.grid.row_len().saturating_sub(1));
    }

    /// VPA (CSI d): move to an absolute row, honoring origin mode, keeping the column.
    fn set_cursor_row_abs(&mut self, row_1based: usize) {
        let row0 = row_1based.saturating_sub(1);
        if self.modes.contains(&6) {
            self.cursor_row =
                (self.scroll_region_top + row0).min(self.scroll_region_bottom);
        } else {
            self.cursor_row = row0.min(self.grid.rows().saturating_sub(1));
        }
    }

    fn put_char(&mut self, ch: char) {
        let ch = self.translate_char(ch);
        let width = crate::char_width::cached_char_width(ch);
        if width == 0 {
            self.combine_with_previous(ch);
            return;
        }

        let cols = self.grid.row_len();
        let blank_cell = self.create_blank_cell();

        // If character doesn't fit at end of line, handle based on autowrap mode
        if self.cursor_col + width > cols {
            // Only wrap to next line if autowrap mode (mode 7) is enabled
            if self.modes.contains(&7) {
                self.wrap_to_next_line();
            } else {
                // Autowrap disabled: clamp cursor to last column instead of wrapping
                self.cursor_col = cols.saturating_sub(width);
            }
        }

        // IRM insert mode (mode 4): make room by shifting the row right.
        if self.modes.contains(&4) {
            self.shift_cells_right(self.cursor_row, self.cursor_col, width);
        }

        // If current position has a continuation cell to its left, clear the wide character
        if self.cursor_col > 0
            && self
                .grid
                .get(self.cursor_row, self.cursor_col)
                .flags.wide_continuation()
        {
            *self.grid.get_mut(self.cursor_row, self.cursor_col - 1) = blank_cell.clone();
        }

        // If current position has a wide character, clear its continuation cell
        if self.grid.get(self.cursor_row, self.cursor_col).flags.wide() && self.cursor_col + 1 < cols {
            *self.grid.get_mut(self.cursor_row, self.cursor_col + 1) = blank_cell.clone();
        }

        // Write character
        let cell = self.grid.get_mut(self.cursor_row, self.cursor_col);
        cell.character = ch;
        cell.foreground = self.current_fg;
        cell.background = self.current_bg;
        cell.flags = self.current_flags;
        cell.flags.set_wide(width == 2);
        cell.flags.set_wide_continuation(false);

        // Set up wide character continuation cell if needed
        if width == 2 && self.cursor_col + 1 < cols {
            let cont_cell = self.grid.get_mut(self.cursor_row, self.cursor_col + 1);
            *cont_cell = blank_cell;
            cont_cell.flags.set_wide_continuation(true);
        }

        self.cursor_col += width;
        self.last_printed_char = Some(ch);
        // Mark the row as dirty after writing character
        self.dirty_region.mark_row(self.cursor_row);
        self.mark_row_dirty(self.cursor_row);
    }

    fn put_ascii_run(&mut self, bytes: &[u8]) {
        let cols = self.grid.row_len();
        let autowrap = self.modes.contains(&7);
        let mut pos = 0;
        if let Some(&last) = bytes.last() {
            self.last_printed_char = Some(last as char);
        }

        while pos < bytes.len() {
            let remaining = cols - self.cursor_col;
            let chunk_len = (bytes.len() - pos).min(remaining);

            // Write chunk to grid directly through a single row slice
            // (avoids recomputing row*cols + bounds-check on every cell)
            let fg = self.current_fg;
            let bg = self.current_bg;
            let mut flags = self.current_flags;
            flags.set_wide(false);
            flags.set_wide_continuation(false);
            let col = self.cursor_col;
            let row = &mut self.grid[self.cursor_row][col..col + chunk_len];
            for (cell, &byte) in row.iter_mut().zip(&bytes[pos..pos + chunk_len]) {
                cell.character = byte as char;
                cell.foreground = fg;
                cell.background = bg;
                cell.flags = flags;
            }

            self.cursor_col += chunk_len;
            pos += chunk_len;

            self.dirty_region.mark_row(self.cursor_row);
            self.mark_row_dirty(self.cursor_row);

            // Handle wrap if there's more data
            if pos < bytes.len() && self.cursor_col >= cols {
                if autowrap {
                    self.wrap_to_next_line();
                } else {
                    self.cursor_col = cols - 1;
                    break;
                }
            }
        }
    }

    fn create_blank_cell(&self) -> TerminalCell {
        TerminalCell {
            character: ' ',
            foreground: Color::Default,
            background: self.current_bg, // Preserve current background color
            flags: StyleFlags::default(),
        }
    }

    fn blank_line(&self, cols: usize) -> Vec<TerminalCell> {
        vec![self.create_blank_cell(); cols]
    }

    fn normalize_line_width(&self, mut line: Vec<TerminalCell>, cols: usize) -> Vec<TerminalCell> {
        match line.len().cmp(&cols) {
            std::cmp::Ordering::Equal => line,
            std::cmp::Ordering::Greater => {
                line.truncate(cols);
                line
            }
            std::cmp::Ordering::Less => {
                line.resize(cols, self.create_blank_cell());
                line
            }
        }
    }

    fn push_scrollback_compressed(&mut self, line: ScrollbackLine) {
        if self.use_alt_buffer {
            return;
        }
        if self.scrollback.len() >= self.max_scrollback {
            self.scrollback.pop_front();
        }
        self.scrollback.push_back(line);
        // Pin the viewport when the user is reading history: without this,
        // start_idx = scrollback.len() - scroll_offset - rows drifts by +1
        // for every new line, sliding the visible region toward the bottom.
        if self.scroll_offset > 0 {
            self.scroll_offset = (self.scroll_offset + 1).min(self.scrollback.len());
            self.visible_cells_cache = None;
        }
    }

    fn scroll_region_down(&mut self, top: usize, bottom: usize) {
        if top >= self.grid.rows() || bottom >= self.grid.rows() || top > bottom {
            return;
        }
        let cols = self.grid.row_len();
        // Shift rows down: move [top..bottom) to [top+1..=bottom]
        let src_start = top * cols;
        let src_end = bottom * cols;
        let dst = (top + 1) * cols;
        self.grid.cells.copy_within(src_start..src_end, dst);
        // Clear top row
        self.grid.cells[src_start..src_start + cols].fill(TerminalCell::default());
        self.grid.row_wrapped.copy_within(top..bottom, top + 1);
        self.grid.row_wrapped[top] = false;
        self.dirty_region.mark_rows(top, bottom);
        self.mark_rows_dirty(top, bottom);
    }

    fn scroll_region_up(&mut self, top: usize, bottom: usize) {
        if top >= self.grid.rows() || bottom >= self.grid.rows() || top > bottom {
            return;
        }

        let cols = self.grid.row_len();
        let is_full_screen_region = top == 0 && bottom + 1 == self.grid.rows();

        // Compress the removed line directly from the grid slice before mutating,
        // avoiding a per-line Vec allocation from get_row.
        let scrollback_line = if is_full_screen_region && !self.use_alt_buffer {
            Some(ScrollbackLine::compress(&self.grid[top], self.grid.row_wrapped[top]))
        } else {
            None
        };

        let src_start = (top + 1) * cols;
        let src_end = (bottom + 1) * cols;
        let dst_start = top * cols;
        self.grid.cells.copy_within(src_start..src_end, dst_start);
        let blank_start = bottom * cols;
        self.grid.cells[blank_start..blank_start + cols].fill(TerminalCell::default());
        self.grid.row_wrapped.copy_within(top + 1..=bottom, top);
        self.grid.row_wrapped[bottom] = false;

        self.dirty_region.mark_rows(top, bottom);
        self.mark_rows_dirty(top, bottom);

        if let Some(line) = scrollback_line {
            self.push_scrollback_compressed(line);
        }
    }

    fn charset_from_designator(byte: u8) -> Charset {
        match byte {
            b'0' => Charset::DecSpecialGraphics,
            _ => Charset::Ascii,
        }
    }

    fn translate_char(&self, ch: char) -> char {
        match self.active_charset {
            Charset::Ascii => ch,
            Charset::DecSpecialGraphics => match ch {
                '`' => '◆',
                'a' => '▒',
                'f' => '°',
                'g' => '±',
                'j' => '┘',
                'k' => '┐',
                'l' => '┌',
                'm' => '└',
                'n' => '┼',
                'o' => '⎺',
                'p' => '⎻',
                'q' => '─',
                'r' => '⎼',
                's' => '⎽',
                't' => '├',
                'u' => '┤',
                'v' => '┴',
                'w' => '┬',
                'x' => '│',
                'y' => '≤',
                'z' => '≥',
                '{' => 'π',
                '|' => '≠',
                '}' => '£',
                '~' => '·',
                _ => ch,
            },
        }
    }

    fn clear_cell(&mut self, row: usize, col: usize) {
        let cols = self.grid.row_len();
        let bg_color = self.current_bg;
        let blank_cell = TerminalCell {
            character: ' ',
            foreground: Color::Default,
            background: bg_color,
            flags: StyleFlags::default(),
        };
        // If clearing a continuation cell, also clear the wide character body
        if self.grid.get(row, col).flags.wide_continuation() && col > 0 {
            *self.grid.get_mut(row, col - 1) = blank_cell.clone();
        }
        // If clearing a wide character body, also clear the continuation cell
        if self.grid.get(row, col).flags.wide() && col + 1 < cols {
            *self.grid.get_mut(row, col + 1) = blank_cell.clone();
        }
        *self.grid.get_mut(row, col) = blank_cell;
    }

    /// P3 优化：批量处理输入数据，只在处理完成后触发一次网格版本更新
    /// 相比多次 process_input，这个方法避免了多次网格版本递增
    pub fn process_batch(&mut self, input: &[u8]) {
        self.grid_version = self.grid_version.wrapping_add(1);
        self.process_input(input);
    }

    #[inline]
    fn mark_row_dirty(&mut self, row: usize) {
        if row < self.row_versions.len() {
            self.row_versions[row] = self.grid_version;
        }
    }

    #[inline]
    fn mark_rows_dirty(&mut self, start: usize, end: usize) {
        for row in start..=end.min(self.row_versions.len().saturating_sub(1)) {
            self.row_versions[row] = self.grid_version;
        }
    }

    /// P4：获取上次渲染后修改过的行索引
    pub fn get_dirty_rows(&self, last_rendered_version: u64, out: &mut Vec<usize>) {
        out.clear();
        for (i, &v) in self.row_versions.iter().enumerate() {
            if v > last_rendered_version {
                out.push(i);
            }
        }
    }

    /// P4：获取网格版本号（用于缓存比较）
    pub fn get_grid_version(&self) -> u64 {
        self.grid_version
    }

    pub fn take_osc52_clipboard_set(&mut self) -> Option<String> {
        self.pending_osc52_clipboard_set.take()
    }

    pub fn take_osc52_clipboard_query(&mut self) -> bool {
        let q = self.pending_osc52_clipboard_query;
        self.pending_osc52_clipboard_query = false;
        q
    }

    pub fn respond_osc52_clipboard(&mut self, content: &str) {
        use base64::Engine;
        let encoded = base64::engine::general_purpose::STANDARD.encode(content.as_bytes());
        self.output_buffer.extend_from_slice(b"\x1b]52;c;");
        self.output_buffer.extend_from_slice(encoded.as_bytes());
        self.output_buffer.extend_from_slice(Self::osc_terminator());
    }

    /// Check if sync output timed out (>1s) and auto-clear if so
    pub fn check_sync_output_timeout(&mut self) {
        if self.sync_output_active {
            if let Some(start) = self.sync_output_start {
                if start.elapsed() > std::time::Duration::from_secs(1) {
                    self.sync_output_active = false;
                    self.sync_output_start = None;
                    self.modes.remove(&2026);
                    self.dirty_region.mark_all(self.grid.rows());
                    self.mark_rows_dirty(0, self.grid.rows().saturating_sub(1));
                }
            }
        }
    }

    #[allow(dead_code)]
    pub fn is_focus_event_mode(&self) -> bool {
        self.modes.contains(&1004)
    }

    #[allow(dead_code)]
    pub fn is_bracketed_paste_mode(&self) -> bool {
        self.modes.contains(&2004)
    }

    #[allow(dead_code)]
    pub fn emit_focus_in(&mut self) {
        if self.modes.contains(&1004) {
            self.output_buffer.extend_from_slice(b"\x1b[I");
        }
    }

    #[allow(dead_code)]
    pub fn emit_focus_out(&mut self) {
        if self.modes.contains(&1004) {
            self.output_buffer.extend_from_slice(b"\x1b[O");
        }
    }

    pub fn process_input(&mut self, input: &[u8]) {
        // Guard against an unterminated OSC/DCS/escape sequence. Such a sequence
        // is buffered into `pending_escape` and re-scanned from its start on every
        // read, which is both O(n^2) in CPU and unbounded in memory. Once the
        // buffered prefix exceeds this cap, abandon the partial sequence. The cap
        // is generous enough for legitimate large payloads (e.g. OSC 52 clipboard).
        const MAX_PENDING_ESCAPE: usize = 1 << 20; // 1 MiB
        if self.pending_escape.len() > MAX_PENDING_ESCAPE {
            self.pending_escape.clear();
        }

        // Fast path: if no pending escape, process input directly without allocation
        let data;
        let data_slice: &[u8] = if self.pending_escape.is_empty() {
            input
        } else {
            // Slow path: merge pending escape with new input
            let mut combined = std::mem::take(&mut self.pending_escape);
            combined.extend_from_slice(input);
            data = combined;
            &data
        };

        let mut i = 0;

        while i < data_slice.len() {
            let byte = data_slice[i];

            match byte {
                b'\x08' => {
                    // Backspace (0x08) - just move cursor left.
                    // Shell handles actual deletion and sends back updated display.
                    if self.cursor_col > 0 {
                        self.cursor_col -= 1;
                    }
                    i += 1;
                }
                b'\x7f' => {
                    // DEL (0x7f) is a fill/padding character; xterm ignores it.
                    i += 1;
                }
                b'\n' => {
                    // Linefeed - move cursor down or scroll the region.
                    // Only scroll when exactly on the region's bottom row; when the
                    // cursor is below the region just move down (don't scroll).
                    if self.cursor_row == self.scroll_region_bottom {
                        self.scroll_region_up(self.scroll_region_top, self.scroll_region_bottom);
                    } else if self.cursor_row + 1 < self.grid.rows() {
                        self.cursor_row += 1;
                    }
                    i += 1;
                }
                b'\r' => {
                    self.cursor_col = 0;
                    i += 1;
                }
                b'\x0e' => {
                    self.active_charset = self.g1_charset;
                    i += 1;
                }
                b'\x0f' => {
                    self.active_charset = self.g0_charset;
                    i += 1;
                }
                b'\x07' => {
                    // Bell - ignore
                    i += 1;
                }
                b'\t' => {
                    // Tab - advance to the next tab stop.
                    self.cursor_col = self.next_tab_stop(self.cursor_col);
                    i += 1;
                }
                b'\x1b' => {
                    let esc_start = i;

                    if i + 1 >= data_slice.len() {
                        self.pending_escape.extend_from_slice(&data_slice[esc_start..]);
                        break;
                    }

                    match data_slice[i + 1] {
                        b'7' => {
                            // DECSC - Save cursor (position + SGR + charset + origin)
                            self.save_cursor();
                            i += 2;
                        }
                        b'8' => {
                            // DECRC - Restore cursor
                            self.restore_cursor();
                            i += 2;
                        }
                        b'E' => {
                            // NEL - Next Line (linefeed + carriage return)
                            self.cursor_col = 0;
                            if self.cursor_row == self.scroll_region_bottom {
                                self.scroll_region_up(
                                    self.scroll_region_top,
                                    self.scroll_region_bottom,
                                );
                            } else if self.cursor_row + 1 < self.grid.rows() {
                                self.cursor_row += 1;
                            }
                            i += 2;
                        }
                        b'H' => {
                            // HTS - set a horizontal tab stop at the current column
                            if let Some(stop) = self.tab_stops.get_mut(self.cursor_col) {
                                *stop = true;
                            }
                            i += 2;
                        }
                        b'c' => {
                            // RIS - Reset to Initial State
                            self.full_reset();
                            i += 2;
                        }
                        b'#' => {
                            // DEC private: ESC # 8 = DECALN (fill screen with 'E')
                            if i + 2 < data_slice.len() && data_slice[i + 2] == b'8' {
                                self.decaln();
                                i += 3;
                            } else {
                                i += 2;
                            }
                        }
                        b']' => {
                            i += 2;

                            let payload_start = i;

                            let mut terminated = false;
                            while i < data_slice.len() {
                                if data_slice[i] == 0x07 {
                                    i += 1;
                                    terminated = true;
                                    break;
                                } else if i + 1 < data_slice.len()
                                    && data_slice[i] == 0x1b
                                    && data_slice[i + 1] == 0x5c
                                {
                                    i += 2;
                                    terminated = true;
                                    break;
                                } else {
                                    i += 1;
                                }
                            }

                            if !terminated {
                                self.pending_escape.extend_from_slice(&data_slice[esc_start..]);
                                break;
                            }

                            let payload_end = if data_slice[i - 1] == 0x07 { i - 1 } else { i - 2 };
                            if payload_end >= payload_start {
                                if let Ok(payload) =
                                    std::str::from_utf8(&data_slice[payload_start..payload_end])
                                {
                                    if let Some((command, value)) = payload.split_once(';') {
                                        if command == "0" || command == "2" {
                                            self.window_title.clear();
                                            self.window_title.push_str(value);
                                        } else if command == "8" {
                                            // OSC 8 - Hyperlinks
                                            // Format: ESC ] 8 ; params ; URI ST
                                            // params can include id=<identifier>
                                            // Empty URI = close hyperlink
                                            if let Some((params, uri)) = value.split_once(';') {
                                                if uri.is_empty() {
                                                    // Close hyperlink
                                                    self.current_hyperlink = None;
                                                } else {
                                                    // Open hyperlink
                                                    let id = params
                                                        .split(':')
                                                        .find_map(|p| p.strip_prefix("id="))
                                                        .map(|s| s.to_string());
                                                    self.current_hyperlink = Some((uri.to_string(), id));
                                                }
                                            } else if value.is_empty() {
                                                // OSC 8 ; ; (close hyperlink)
                                                self.current_hyperlink = None;
                                            }
                                        } else if command == "10" || command == "11" || command == "12" {
                                            self.handle_osc_color(command, value);
                                        } else if command == "9" {
                                            // Desktop notification (iTerm2/ConEmu)
                                            if self.pending_notifications.len() < 8 {
                                                let title = "jterm2".to_string();
                                                let body = value.chars().take(256).collect();
                                                self.pending_notifications.push((title, body));
                                            }
                                        } else if command == "777" {
                                            // rxvt notification: 777;notify;title;body
                                            let parts: Vec<&str> = value.splitn(3, ';').collect();
                                            if parts.len() >= 2 && parts[0] == "notify" {
                                                let title = parts.get(1).unwrap_or(&"").chars().take(256).collect();
                                                let body = parts.get(2).unwrap_or(&"").chars().take(256).collect();
                                                if self.pending_notifications.len() < 8 {
                                                    self.pending_notifications.push((title, body));
                                                }
                                            }
                                        } else if command == "133" {
                                            self.handle_osc_133(value);
                                        } else if command == "52" {
                                            self.handle_osc_52(value);
                                        } else if command == "5522" {
                                            let (metadata, osc_payload) =
                                                if let Some((metadata, osc_payload)) =
                                                    value.split_once(';')
                                                {
                                                    (metadata, Some(osc_payload))
                                                } else {
                                                    (value, None)
                                                };
                                            self.handle_osc_5522(metadata, osc_payload);
                                        }
                                    }
                                }
                            }
                        }
                        b'P' | b'X' | b'^' | b'_' => {
                            i += 2;

                            let mut terminated = false;
                            let dcs_start = i;
                            while i < data_slice.len() {
                                if i + 1 < data_slice.len() && data_slice[i] == 0x1b && data_slice[i + 1] == 0x5c {
                                    // Extract DCS payload
                                    let payload = &data_slice[dcs_start..i];

                                    // Check if this is a Kitty graphics protocol DCS
                                    if let Ok(payload_str) = std::str::from_utf8(payload) {
                                        // Kitty graphics protocol starts with @ or other specific markers
                                        if payload_str.starts_with('@')
                                            || payload_str.contains("a=")
                                            || payload_str.starts_with("kitty")
                                        {
                                            if let Err(_e) = self
                                                .kitty_graphics
                                                .parse_graphics_payload(payload_str)
                                            {
                                                crate::debug_log!(
                                                    "[DCS] Kitty graphics error: {}",
                                                    _e
                                                );
                                            }
                                        }
                                    }

                                    i += 2;
                                    terminated = true;
                                    break;
                                }
                                i += 1;
                            }

                            if !terminated {
                                self.pending_escape.extend_from_slice(&data_slice[esc_start..]);
                                break;
                            }
                        }
                        b'>' => {
                            // ESC > - DECKPNM (Keypad Numeric Mode) or other private sequence
                            // Just skip it and any following bytes that are part of it
                            i += 2;
                        }
                        b'<' => {
                            // ESC < - DECKPM (Keypad Application Mode) or other private sequence
                            // Just skip it
                            i += 2;
                        }
                        b'=' => {
                            // ESC = - DECKPAM (Keypad Application Mode)
                            // Just skip it
                            i += 2;
                        }
                        b'(' | b')' => {
                            if i + 2 >= data_slice.len() {
                                self.pending_escape.extend_from_slice(&data_slice[esc_start..]);
                                break;
                            }

                            // Character set selection: ESC ( X or ESC ) X
                            // data_slice[i] = ESC, data_slice[i+1] = '(' or ')', data_slice[i+2] = designator
                            let is_g0 = data_slice[i + 1] == b'(';
                            let designator = data_slice[i + 2];
                            let charset = Self::charset_from_designator(designator);

                            crate::debug_log!(
                                "[CHARSET] ESC {} designator={} (0x{:02x}) charset={:?}",
                                if is_g0 { '(' } else { ')' },
                                designator as char,
                                designator,
                                charset
                            );

                            if is_g0 {
                                self.g0_charset = charset;
                                self.active_charset = self.g0_charset;
                            } else {
                                self.g1_charset = charset;
                            }

                            i += 3;
                        }
                        b'M' => {
                            i += 2;

                            if self.cursor_row > self.scroll_region_top {
                                self.cursor_row -= 1;
                            } else if self.scroll_region_top < self.grid.rows()
                                && self.scroll_region_bottom < self.grid.rows()
                                && self.scroll_region_top <= self.scroll_region_bottom
                            {
                                self.scroll_region_down(
                                    self.scroll_region_top,
                                    self.scroll_region_bottom,
                                );
                            }
                        }
                        b'D' => {
                            i += 2;

                            if self.cursor_row < self.scroll_region_bottom {
                                self.cursor_row += 1;
                            } else {
                                self.scroll_region_up(
                                    self.scroll_region_top,
                                    self.scroll_region_bottom,
                                );
                            }
                        }
                        b'[' => {
                            i += 2;

                            // Use stack arrays for CSI params (typical CSI sequences are short)
                            let mut param_bytes = [0u8; 32];
                            let mut param_len = 0;
                            let mut intermediates = [0u8; 8];
                            let mut inter_len = 0;
                            let mut final_byte = None;

                            while i < data_slice.len() {
                                match data_slice[i] {
                                    0x30..=0x3f => {
                                        if param_len < param_bytes.len() {
                                            param_bytes[param_len] = data_slice[i];
                                            param_len += 1;
                                        }
                                    }
                                    0x20..=0x2f => {
                                        if inter_len < intermediates.len() {
                                            intermediates[inter_len] = data_slice[i];
                                            inter_len += 1;
                                        }
                                    }
                                    0x40..=0x7e => {
                                        final_byte = Some(data_slice[i]);
                                        break;
                                    }
                                    _ => break,
                                }
                                i += 1;
                            }

                            let Some(final_byte) = final_byte else {
                                self.pending_escape.extend_from_slice(&data_slice[esc_start..]);
                                break;
                            };

                            let private_prefix = match param_bytes.first().copied() {
                                Some(prefix @ (b'<' | b'=' | b'>' | b'?')) => {
                                    // Shift remaining params left
                                    for j in 0..param_len - 1 {
                                        param_bytes[j] = param_bytes[j + 1];
                                    }
                                    param_len -= 1;
                                    Some(prefix)
                                }
                                _ => None,
                            };
                            let params = Self::parse_csi_params(&param_bytes[..param_len]);
                            let cmd = final_byte as char;

                            self.handle_escape_sequence(
                                &params,
                                &param_bytes[..param_len],
                                cmd,
                                private_prefix,
                                &intermediates[..inter_len],
                            );
                            i += 1;
                        }
                        _ => {
                            // Unknown 2-byte escape (e.g. SS2 `ESC N`, SS3 `ESC O`).
                            // Consume BOTH bytes so the trailing letter isn't printed
                            // as literal text.
                            i += 2;
                        }
                    }
                }
                32..=126 => {
                    // ASCII fast path: scan for run of printable ASCII and process in bulk.
                    // Insert mode (IRM) needs per-cell shifting, so fall back to put_char.
                    if self.utf8_len == 0
                        && self.active_charset == Charset::Ascii
                        && !self.modes.contains(&4)
                    {
                        let run_start = i;
                        i += 1;
                        while i < data_slice.len() {
                            let b = data_slice[i];
                            if b < 32 || b > 126 {
                                break;
                            }
                            i += 1;
                        }
                        self.put_ascii_run(&data_slice[run_start..i]);
                    } else {
                        self.put_char(byte as char);
                        i += 1;
                    }
                }
                // UTF-8 multi-byte sequences: try to consume all bytes eagerly
                0xC2..=0xDF => {
                    let expected: u8 = 2;
                    if i + 1 < data_slice.len() && (data_slice[i + 1] & 0xC0) == 0x80 {
                        let buf = [byte, data_slice[i + 1], 0, 0];
                        if let Ok(s) = std::str::from_utf8(&buf[..2]) {
                            if let Some(ch) = s.chars().next() {
                                self.put_char(ch);
                            }
                        }
                        i += 2;
                    } else {
                        self.utf8_buf[0] = byte;
                        self.utf8_len = 1;
                        self.utf8_expected = expected;
                        i += 1;
                    }
                }
                0xE0..=0xEF => {
                    let expected: u8 = 3;
                    if i + 2 < data_slice.len()
                        && (data_slice[i + 1] & 0xC0) == 0x80
                        && (data_slice[i + 2] & 0xC0) == 0x80
                    {
                        let buf = [byte, data_slice[i + 1], data_slice[i + 2], 0];
                        if let Ok(s) = std::str::from_utf8(&buf[..3]) {
                            if let Some(ch) = s.chars().next() {
                                self.put_char(ch);
                            }
                        }
                        i += 3;
                    } else {
                        self.utf8_buf[0] = byte;
                        self.utf8_len = 1;
                        self.utf8_expected = expected;
                        i += 1;
                    }
                }
                0xF0..=0xF4 => {
                    let expected: u8 = 4;
                    if i + 3 < data_slice.len()
                        && (data_slice[i + 1] & 0xC0) == 0x80
                        && (data_slice[i + 2] & 0xC0) == 0x80
                        && (data_slice[i + 3] & 0xC0) == 0x80
                    {
                        let buf = [byte, data_slice[i + 1], data_slice[i + 2], data_slice[i + 3]];
                        if let Ok(s) = std::str::from_utf8(&buf[..4]) {
                            if let Some(ch) = s.chars().next() {
                                self.put_char(ch);
                            }
                        }
                        i += 4;
                    } else {
                        self.utf8_buf[0] = byte;
                        self.utf8_len = 1;
                        self.utf8_expected = expected;
                        i += 1;
                    }
                }
                _ => {
                    if self.utf8_len > 0 && (byte & 0xC0) == 0x80 {
                        self.utf8_buf[self.utf8_len as usize] = byte;
                        self.utf8_len += 1;
                        if self.utf8_len == self.utf8_expected {
                            if let Ok(s) =
                                std::str::from_utf8(&self.utf8_buf[..self.utf8_len as usize])
                            {
                                if let Some(ch) = s.chars().next() {
                                    self.put_char(ch);
                                }
                            }
                            self.utf8_len = 0;
                        }
                    } else {
                        self.utf8_len = 0;
                    }
                    i += 1;
                }
            }
        }
    }

    fn handle_escape_sequence(
        &mut self,
        params: &[u16],
        raw_params: &[u8],
        cmd: char,
        private_prefix: Option<u8>,
        intermediates: &[u8],
    ) {
        match cmd {
            'A' => {
                // CUU - cursor up. Stops at the top margin (or row 0 if the
                // cursor starts above the region); never scrolls.
                let n = params.first().copied().unwrap_or(1).max(1) as usize;
                let limit = if self.cursor_row >= self.scroll_region_top {
                    self.scroll_region_top
                } else {
                    0
                };
                self.cursor_row = self.cursor_row.saturating_sub(n).max(limit);
            }
            'B' => {
                // CUD - cursor down. Stops at the bottom margin (or last row if
                // the cursor starts below the region); never scrolls.
                let n = params.first().copied().unwrap_or(1).max(1) as usize;
                let limit = if self.cursor_row <= self.scroll_region_bottom {
                    self.scroll_region_bottom
                } else {
                    self.grid.rows() - 1
                };
                self.cursor_row = (self.cursor_row + n).min(limit);
            }
            'C' => {
                let n = params.first().copied().unwrap_or(1).max(1) as usize;
                self.cursor_col = (self.cursor_col + n).min(self.grid.row_len() - 1);
            }
            'D' => {
                let n = params.first().copied().unwrap_or(1).max(1) as usize;
                self.cursor_col = self.cursor_col.saturating_sub(n);
            }
            'E' => {
                // CNL - cursor next line. Down n, to column 0, bounded by the
                // bottom margin (matching CUD); never scrolls.
                let n = params.first().copied().unwrap_or(1).max(1) as usize;
                let limit = if self.cursor_row <= self.scroll_region_bottom {
                    self.scroll_region_bottom
                } else {
                    self.grid.rows() - 1
                };
                self.cursor_row = (self.cursor_row + n).min(limit);
                self.cursor_col = 0;
            }
            'F' => {
                // CPL - cursor previous line. Up n, to column 0, bounded by the
                // top margin (matching CUU); never scrolls.
                let n = params.first().copied().unwrap_or(1).max(1) as usize;
                let limit = if self.cursor_row >= self.scroll_region_top {
                    self.scroll_region_top
                } else {
                    0
                };
                self.cursor_row = self.cursor_row.saturating_sub(n).max(limit);
                self.cursor_col = 0;
            }
            'G' | '`' => {
                // CHA / HPA - move cursor to absolute column (1-based)
                let col = params.first().copied().unwrap_or(1) as usize;
                self.cursor_col = col.saturating_sub(1).min(self.grid.row_len() - 1);
            }
            'd' => {
                // VPA - move cursor to absolute row (1-based), honoring origin mode
                let row = params.first().copied().unwrap_or(1) as usize;
                self.set_cursor_row_abs(row);
            }
            'I' => {
                // CHT - cursor forward tabulation (n tab stops)
                let n = params.first().copied().unwrap_or(1).max(1) as usize;
                for _ in 0..n {
                    self.cursor_col = self.next_tab_stop(self.cursor_col);
                }
            }
            'Z' => {
                // CBT - cursor backward tabulation (n tab stops)
                let n = params.first().copied().unwrap_or(1).max(1) as usize;
                for _ in 0..n {
                    self.cursor_col = self.prev_tab_stop(self.cursor_col);
                }
            }
            'b' => {
                // REP - repeat the last printed character n times
                let n = params.first().copied().unwrap_or(1).max(1) as usize;
                if let Some(ch) = self.last_printed_char {
                    for _ in 0..n {
                        self.put_char(ch);
                    }
                }
            }
            'g' => {
                // TBC - tab clear (0 = at cursor, 3 = all)
                match params.first().copied().unwrap_or(0) {
                    0 => {
                        if let Some(stop) = self.tab_stops.get_mut(self.cursor_col) {
                            *stop = false;
                        }
                    }
                    3 => {
                        for stop in self.tab_stops.iter_mut() {
                            *stop = false;
                        }
                    }
                    _ => {}
                }
            }
            'H' => {
                let row = params.first().copied().unwrap_or(1) as usize;
                let col = params.get(1).copied().unwrap_or(1) as usize;
                self.place_cursor(row, col);
            }
            'f' => {
                if private_prefix == Some(b'>') && intermediates.is_empty() {
                    let resource = params.first().copied().unwrap_or(0);
                    let value = params.get(1).copied().unwrap_or(0);
                    if resource == 4 {
                        crate::debug_log!(
                            "[XTFMTKEYS] formatOtherKeys={} previous={}",
                            value,
                            self.xterm_format_other_keys
                        );
                        self.xterm_format_other_keys = value;
                    }
                } else {
                    let row = params.first().copied().unwrap_or(1) as usize;
                    let col = params.get(1).copied().unwrap_or(1) as usize;
                    self.place_cursor(row, col);
                }
            }
            'J' => {
                match params.first().copied().unwrap_or(0) {
                    0 => {
                        // Clear from cursor to end of display
                        for col in self.cursor_col..self.grid.row_len() {
                            self.clear_cell(self.cursor_row, col);
                        }
                        for row in (self.cursor_row + 1)..self.grid.rows() {
                            for col in 0..self.grid.row_len() {
                                self.clear_cell(row, col);
                            }
                        }
                        // Mark affected rows as dirty
                        self.dirty_region
                            .mark_rows(self.cursor_row, self.grid.rows().saturating_sub(1));
                        self.mark_rows_dirty(self.cursor_row, self.grid.rows().saturating_sub(1));
                    }
                    1 => {
                        // Clear from start to cursor
                        for row in 0..=self.cursor_row {
                            let end_col = if row == self.cursor_row {
                                self.cursor_col + 1
                            } else {
                                self.grid.row_len()
                            };
                            for col in 0..end_col {
                                self.clear_cell(row, col);
                            }
                        }
                        // Mark affected rows as dirty
                        self.dirty_region.mark_rows(0, self.cursor_row);
                        self.mark_rows_dirty(0, self.cursor_row);
                    }
                    2 => {
                        // ED 2: erase the whole screen but leave the cursor in place.
                        self.clear_screen_no_home();
                    }
                    3 => {
                        // Clear scrollback (xterm extension)
                        self.scrollback.clear();
                        self.scroll_offset = 0;
                    }
                    _ => {}
                }
            }
            'K' => {
                // Clear line
                match params.first().copied().unwrap_or(0) {
                    0 => {
                        // Clear from cursor to end of line
                        for col in self.cursor_col..self.grid.row_len() {
                            self.clear_cell(self.cursor_row, col);
                        }
                        // Mark the line as dirty
                        self.dirty_region.mark_row(self.cursor_row);
                        self.mark_row_dirty(self.cursor_row);
                    }
                    1 => {
                        // Clear from start of line to cursor
                        for col in 0..=self.cursor_col {
                            self.clear_cell(self.cursor_row, col);
                        }
                        // Mark the line as dirty
                        self.dirty_region.mark_row(self.cursor_row);
                        self.mark_row_dirty(self.cursor_row);
                    }
                    2 => {
                        // Clear entire line
                        for col in 0..self.grid.row_len() {
                            self.clear_cell(self.cursor_row, col);
                        }
                        // Mark the line as dirty
                        self.dirty_region.mark_row(self.cursor_row);
                        self.mark_row_dirty(self.cursor_row);
                    }
                    _ => {}
                }
            }
            'L' => {
                let n = params.first().copied().unwrap_or(1).max(1) as usize;
                for _ in 0..n {
                    if self.cursor_row >= self.scroll_region_top
                        && self.cursor_row <= self.scroll_region_bottom
                    {
                        let cols = self.grid.row_len();
                        let src_start = self.cursor_row * cols;
                        let src_end = self.scroll_region_bottom * cols;
                        let dst = (self.cursor_row + 1) * cols;
                        self.grid.cells.copy_within(src_start..src_end, dst);
                        self.grid.cells[src_start..src_start + cols].fill(TerminalCell::default());
                    }
                }
                self.mark_rows_dirty(self.cursor_row, self.scroll_region_bottom);
            }
            'M' => {
                let n = params.first().copied().unwrap_or(1).max(1) as usize;
                for _ in 0..n {
                    if self.cursor_row >= self.scroll_region_top
                        && self.cursor_row <= self.scroll_region_bottom
                    {
                        let cols = self.grid.row_len();
                        let src_start = (self.cursor_row + 1) * cols;
                        let src_end = (self.scroll_region_bottom + 1) * cols;
                        let dst = self.cursor_row * cols;
                        self.grid.cells.copy_within(src_start..src_end, dst);
                        let blank_start = self.scroll_region_bottom * cols;
                        self.grid.cells[blank_start..blank_start + cols].fill(TerminalCell::default());
                    }
                }
                self.mark_rows_dirty(self.cursor_row, self.scroll_region_bottom);
            }
            'm' => {
                if private_prefix == Some(b'>') && intermediates.is_empty() {
                    let resource = params.first().copied().unwrap_or(0);
                    let value = params.get(1).copied().unwrap_or(0);
                    if resource == 4 {
                        crate::debug_log!(
                            "[XTMODKEYS] modifyOtherKeys={} previous={}",
                            value,
                            self.xterm_modify_other_keys
                        );
                        self.xterm_modify_other_keys = value;
                    }
                } else {
                    // SGR - Select Graphic Rendition
                    self.handle_sgr(&Self::parse_sgr_groups(raw_params));
                }
            }
            's' => {
                if private_prefix.is_none() && intermediates.is_empty() {
                    self.save_cursor();
                }
            }
            'u' => {
                if intermediates.is_empty() {
                    match private_prefix {
                        None => {
                            self.restore_cursor();
                        }
                        Some(b'?') => {
                            crate::debug_log!(
                                "[KEYBOARD_PROTO] query current kitty flags -> {}",
                                self.keyboard_enhancement_flags
                            );
                            let response = format!("\x1b[?{}u", self.keyboard_enhancement_flags);
                            self.output_buffer.extend_from_slice(response.as_bytes());
                        }
                        Some(b'=') => {
                            let flags = params.first().copied().unwrap_or(0);
                            let mode = params.get(1).copied().unwrap_or(1);
                            crate::debug_log!(
                                "[KEYBOARD_PROTO] set kitty flags flags={} mode={} previous={}",
                                flags,
                                mode,
                                self.keyboard_enhancement_flags
                            );
                            self.set_keyboard_enhancement_flags(flags, mode);
                            crate::debug_log!(
                                "[KEYBOARD_PROTO] new kitty flags={}",
                                self.keyboard_enhancement_flags
                            );
                        }
                        Some(b'>') => {
                            let flags = params.first().copied().unwrap_or(0);
                            crate::debug_log!(
                                "[KEYBOARD_PROTO] push kitty flags current={} new={}",
                                self.keyboard_enhancement_flags,
                                flags
                            );
                            self.push_keyboard_enhancement_flags(flags);
                        }
                        Some(b'<') => {
                            let count = params.first().copied().unwrap_or(1) as usize;
                            crate::debug_log!(
                                "[KEYBOARD_PROTO] pop kitty flags count={} current={} stack_depth={}",
                                count,
                                self.keyboard_enhancement_flags,
                                self.keyboard_enhancement_stack.len()
                            );
                            self.pop_keyboard_enhancement_flags(count);
                            crate::debug_log!(
                                "[KEYBOARD_PROTO] new kitty flags={}",
                                self.keyboard_enhancement_flags
                            );
                        }
                        _ => {}
                    }
                }
            }
            'S' => {
                // Scroll up (Scroll Up, SU) - content moves up, new lines appear at bottom
                let n = params.first().copied().unwrap_or(1).max(1) as usize;
                // Scroll within the scroll region by moving lines
                for _ in 0..n {
                    self.scroll_region_up(self.scroll_region_top, self.scroll_region_bottom);
                }
            }
            'T' => {
                // Scroll down (Scroll Down, SD) - content moves down, new lines appear at top
                let n = params.first().copied().unwrap_or(1).max(1) as usize;
                for _ in 0..n {
                    self.scroll_region_down(self.scroll_region_top, self.scroll_region_bottom);
                }
            }
            'n' => {
                // DSR - Device Status Report
                match params.first().copied().unwrap_or(0) {
                    5 => {
                        // Report device OK: CSI 0 n
                        self.output_buffer.extend_from_slice(b"\x1b[0n");
                    }
                    6 => {
                        // CPR - Cursor Position Report: CSI row ; col R (1-indexed)
                        let row = (self.cursor_row + 1) as u16;
                        let col = (self.cursor_col + 1) as u16;
                        let response = format!("\x1b[{};{}R", row, col);
                        self.output_buffer.extend(response.as_bytes());
                    }
                    _ => {}
                }
            }
            'c' => {
                if intermediates.is_empty() {
                    match private_prefix {
                        None => {
                            crate::debug_log!("[DA] primary device attributes request");
                            self.output_buffer
                                .extend_from_slice(PRIMARY_DEVICE_ATTRIBUTES_RESPONSE);
                        }
                        Some(b'>') => {
                            crate::debug_log!("[DA] secondary device attributes request");
                            self.output_buffer
                                .extend_from_slice(SECONDARY_DEVICE_ATTRIBUTES_RESPONSE);
                        }
                        _ => {}
                    }
                }
            }
            'p' => {
                if intermediates == [b'!'] && private_prefix.is_none() {
                    // DECSTR (CSI ! p) - soft terminal reset.
                    self.soft_reset();
                } else if private_prefix == Some(b'?') && intermediates == [b'$']
                    && params.first().copied() == Some(5522) {
                        let state = if self.modes.contains(&5522) { 1 } else { 2 };
                        let response = format!("\x1b[?5522;{}$y", state);
                        crate::debug_log!("[OSC5522] DECRQM query -> {}", response);
                        self.output_buffer.extend_from_slice(response.as_bytes());
                    }
            }
            'h' => {
                // Set mode: DECSET (CSI ? Pn h) vs ANSI SM (CSI Pn h). The two
                // share parameter numbers (e.g. 4 = DECSCLM private vs IRM ANSI),
                // so the private prefix must be threaded through.
                let private = private_prefix == Some(b'?');
                for &mode in params {
                    self.set_mode(mode, private);
                }
            }
            'l' => {
                // Reset mode: DECRST (CSI ? Pn l) vs ANSI RM (CSI Pn l).
                let private = private_prefix == Some(b'?');
                for &mode in params {
                    self.reset_mode(mode, private);
                }
            }
            'r' => {
                // Set scroll region (DECSTBM)
                let top = params.first().copied().unwrap_or(1) as usize;
                let bottom = params.get(1).copied().unwrap_or(self.grid.rows() as u16) as usize;

                // Convert from 1-indexed to 0-indexed, and clamp to valid range
                self.scroll_region_top = top
                    .saturating_sub(1)
                    .min(self.grid.rows().saturating_sub(1));
                self.scroll_region_bottom = bottom
                    .saturating_sub(1)
                    .min(self.grid.rows().saturating_sub(1));

                // If range is invalid, reset to full screen
                if self.scroll_region_top > self.scroll_region_bottom {
                    self.scroll_region_top = 0;
                    self.scroll_region_bottom = self.grid.rows().saturating_sub(1);
                }

                // Move cursor to home position when setting scroll region
                self.cursor_row = 0;
                self.cursor_col = 0;
            }
            '@' => {
                // ICH - Insert Character(s)
                let n = params.first().copied().unwrap_or(1).max(1) as usize;
                let cols = self.grid.row_len();
                let blank_cell = self.create_blank_cell();
                if self.cursor_col < cols {
                    // Insert n blank cells at cursor position, shifting content right
                    // insert_cell_in_row shifts cells right and discards the last cell
                    for _ in 0..n {
                        if self.cursor_col < cols {
                            self.grid.insert_cell_in_row(
                                self.cursor_row,
                                self.cursor_col,
                                blank_cell.clone(),
                            );
                        }
                    }
                    // Mark row as dirty after modification
                    self.mark_row_dirty(self.cursor_row);
                }
            }
            'P' => {
                // DCH - Delete Character(s)
                let n = params.first().copied().unwrap_or(1).max(1) as usize;
                let blank_cell = self.create_blank_cell();
                for _ in 0..n {
                    if self.cursor_col < self.grid.row_len() {
                        self.grid
                            .remove_cell_from_row(self.cursor_row, self.cursor_col);
                        // Fill the last cell with proper blank (remove_cell_from_row uses default)
                        let last_col = self.grid.row_len() - 1;
                        *self.grid.get_mut(self.cursor_row, last_col) = blank_cell.clone();
                    }
                }
                // Mark row as dirty after modification
                self.mark_row_dirty(self.cursor_row);
            }
            'X' => {
                // ECH - Erase Character(s)
                let n = params.first().copied().unwrap_or(1).max(1) as usize;
                for i in 0..n {
                    let col = self.cursor_col + i;
                    if col < self.grid.row_len() {
                        self.clear_cell(self.cursor_row, col);
                    } else {
                        break;
                    }
                }
                // Mark row as dirty after modification
                self.mark_row_dirty(self.cursor_row);
            }
            'q' => {
                if private_prefix == Some(b'>') && intermediates.is_empty()
                    && params.first().copied().unwrap_or(0) == 0 {
                        crate::debug_log!("[XTVERSION] report terminal version request");
                        self.output_buffer.extend_from_slice(XTERM_VERSION_RESPONSE);
                    }

                // DECSCUSR - Set cursor style
                if private_prefix.is_none() && intermediates == [b' '] {
                    let shape = params.first().copied().unwrap_or(0) as u8;
                    self.cursor_shape = match shape {
                        0 | 1 => CursorShape::Block,
                        2 => CursorShape::Underline,
                        3 => CursorShape::Beam,
                        _ => CursorShape::Block,
                    };
                }
            }
            _ => {}
        }
    }

    /// Resolve an extended color (SGR 38/48/58) from either the colon sub-parameter
    /// form (within a single group, e.g. `38:2:r:g:b` or `38:2:cs:r:g:b`) or the
    /// legacy semicolon form (`38;2;r;g;b`), advancing `gi` past consumed groups.
    fn parse_ext_color(groups: &[SmallVec<[u16; 6]>], gi: &mut usize) -> Option<Color> {
        let g = &groups[*gi];
        if g.len() >= 2 {
            // Colon sub-parameter form: everything lives in this group.
            match g[1] {
                5 => g.get(2).map(|&n| Color::Indexed(n as u8)),
                2 => {
                    // 38:2:r:g:b (len 5) or 38:2:colorspace:r:g:b (len >= 6)
                    if g.len() >= 6 {
                        Some(Color::Rgb(g[3] as u8, g[4] as u8, g[5] as u8))
                    } else if g.len() >= 5 {
                        Some(Color::Rgb(g[2] as u8, g[3] as u8, g[4] as u8))
                    } else {
                        None
                    }
                }
                _ => None,
            }
        } else {
            // Legacy semicolon form: the kind and components are separate groups.
            let first = |idx: usize| groups.get(idx).and_then(|x| x.first().copied());
            match first(*gi + 1) {
                Some(5) => {
                    let n = first(*gi + 2);
                    *gi += 2;
                    n.map(|n| Color::Indexed(n as u8))
                }
                Some(2) => {
                    let (r, gg, b) = (first(*gi + 2), first(*gi + 3), first(*gi + 4));
                    *gi += 4;
                    match (r, gg, b) {
                        (Some(r), Some(gg), Some(b)) => {
                            Some(Color::Rgb(r as u8, gg as u8, b as u8))
                        }
                        _ => None,
                    }
                }
                _ => None,
            }
        }
    }

    fn handle_sgr(&mut self, groups: &[SmallVec<[u16; 6]>]) {
        // CSI m with no parameters is a full reset.
        if groups.len() == 1 && groups[0].len() == 1 && groups[0][0] == 0 {
            self.current_flags = StyleFlags::default();
            self.current_fg = Color::Default;
            self.current_bg = Color::Default;
            return;
        }

        let mut gi = 0;
        while gi < groups.len() {
            let g = &groups[gi];
            let param = g.first().copied().unwrap_or(0);
            match param {
                0 => {
                    self.current_flags = StyleFlags::default();
                    self.current_fg = Color::Default;
                    self.current_bg = Color::Default;
                }
                1 => self.current_flags.set_bold(true),
                2 => self.current_flags.set_dim(true),
                3 => self.current_flags.set_italic(true),
                4 => {
                    // Colon sub-parameter (4:n) selects the underline style; plain 4 is
                    // a single underline. Semicolon-separated values are NOT consumed here.
                    let style = if g.len() >= 2 { g[1] } else { 1 };
                    self.current_flags.set_underline(match style {
                        0 => UnderlineStyle::None,
                        1 => UnderlineStyle::Single,
                        2 => UnderlineStyle::Double,
                        3 => UnderlineStyle::Curly,
                        4 => UnderlineStyle::Dotted,
                        5 => UnderlineStyle::Dashed,
                        _ => UnderlineStyle::Single,
                    });
                }
                5 => self.current_flags.set_blink(true),
                7 => self.current_flags.set_inverse(true),
                9 => self.current_flags.set_strikethrough(true),
                21 => self.current_flags.set_underline(UnderlineStyle::Double),
                22 => {
                    self.current_flags.set_bold(false);
                    self.current_flags.set_dim(false);
                }
                23 => self.current_flags.set_italic(false),
                24 => self.current_flags.set_underline(UnderlineStyle::None),
                25 => self.current_flags.set_blink(false),
                27 => self.current_flags.set_inverse(false),
                29 => self.current_flags.set_strikethrough(false),
                39 => self.current_fg = Color::Default,
                30..=37 => {
                    self.current_fg = match param {
                        30 => Color::Black,
                        31 => Color::Red,
                        32 => Color::Green,
                        33 => Color::Yellow,
                        34 => Color::Blue,
                        35 => Color::Magenta,
                        36 => Color::Cyan,
                        37 => Color::White,
                        _ => Color::Default,
                    };
                }
                49 => self.current_bg = Color::Default,
                40..=47 => {
                    self.current_bg = match param {
                        40 => Color::Black,
                        41 => Color::Red,
                        42 => Color::Green,
                        43 => Color::Yellow,
                        44 => Color::Blue,
                        45 => Color::Magenta,
                        46 => Color::Cyan,
                        47 => Color::White,
                        _ => Color::Default,
                    };
                    self.global_bg = self.current_bg; // Update global background
                }
                90..=97 => {
                    self.current_fg = match param {
                        90 => Color::BrightBlack,
                        91 => Color::BrightRed,
                        92 => Color::BrightGreen,
                        93 => Color::BrightYellow,
                        94 => Color::BrightBlue,
                        95 => Color::BrightMagenta,
                        96 => Color::BrightCyan,
                        97 => Color::BrightWhite,
                        _ => Color::Default,
                    };
                }
                100..=107 => {
                    self.current_bg = match param {
                        100 => Color::BrightBlack,
                        101 => Color::BrightRed,
                        102 => Color::BrightGreen,
                        103 => Color::BrightYellow,
                        104 => Color::BrightBlue,
                        105 => Color::BrightMagenta,
                        106 => Color::BrightCyan,
                        107 => Color::BrightWhite,
                        _ => Color::Default,
                    };
                    self.global_bg = self.current_bg; // Update global background
                }
                38 => {
                    if let Some(color) = Self::parse_ext_color(groups, &mut gi) {
                        self.current_fg = color;
                    }
                }
                48 => {
                    if let Some(color) = Self::parse_ext_color(groups, &mut gi) {
                        self.current_bg = color;
                        self.global_bg = self.current_bg;
                    }
                }
                58 => {
                    // SGR 58: set underline color. We don't render a distinct
                    // underline color yet, but its arguments MUST be consumed so
                    // the legacy `58;2;r;g;b` form doesn't leak r/g/b as SGR codes.
                    let _ = Self::parse_ext_color(groups, &mut gi);
                }
                59 => {
                    // SGR 59: reset underline color to default - no-op.
                }
                _ => {}
            }
            gi += 1;
        }
    }

    /// DECALN (ESC # 8): fill the entire screen with 'E', used for alignment tests.
    fn decaln(&mut self) {
        for row in self.grid.iter_mut() {
            for cell in row.iter_mut() {
                *cell = TerminalCell {
                    character: 'E',
                    foreground: Color::Default,
                    background: Color::Default,
                    flags: StyleFlags::default(),
                };
            }
        }
        self.cursor_row = 0;
        self.cursor_col = 0;
        self.dirty_region.mark_all(self.grid.rows());
        self.mark_rows_dirty(0, self.grid.rows().saturating_sub(1));
    }

    /// RIS (ESC c): reset the terminal to its initial state.
    fn full_reset(&mut self) {
        if self.use_alt_buffer {
            self.reset_mode(1049, true);
        }
        self.current_fg = Color::Default;
        self.current_bg = Color::Default;
        self.global_bg = Color::Default;
        self.current_flags = StyleFlags::default();
        self.g0_charset = Charset::Ascii;
        self.g1_charset = Charset::Ascii;
        self.active_charset = Charset::Ascii;
        self.scroll_region_top = 0;
        self.scroll_region_bottom = self.grid.rows().saturating_sub(1);
        self.tab_stops = Self::default_tab_stops(self.grid.row_len());
        self.saved_cursor = None;
        self.modes = TerminalModes::default();
        self.modes.insert(25); // cursor visible
        self.modes.insert(7); // autowrap on
        self.scroll_offset = 0;
        // xterm RIS also discards saved lines and resets cursor style, dynamic
        // colors, keyboard-protocol state, selection, and any open hyperlink.
        self.scrollback.clear();
        self.cursor_shape = CursorShape::default();
        self.dynamic_fg = None;
        self.dynamic_bg = None;
        self.dynamic_cursor_color = None;
        self.keyboard_enhancement_flags = 0;
        self.keyboard_enhancement_stack.clear();
        self.alt_keyboard_enhancement_flags = 0;
        self.alt_keyboard_enhancement_stack.clear();
        self.selection = None;
        self.current_hyperlink = None;
        self.clear_screen();
    }

    /// DECSTR (CSI ! p): soft terminal reset. Unlike RIS this does NOT clear the
    /// screen or scrollback; it resets modes, margins, SGR, charsets and the
    /// saved cursor to their power-on defaults.
    fn soft_reset(&mut self) {
        self.current_fg = Color::Default;
        self.current_bg = Color::Default;
        self.global_bg = Color::Default;
        self.current_flags = StyleFlags::default();
        self.g0_charset = Charset::Ascii;
        self.g1_charset = Charset::Ascii;
        self.active_charset = Charset::Ascii;
        self.scroll_region_top = 0;
        self.scroll_region_bottom = self.grid.rows().saturating_sub(1);
        self.saved_cursor = None;
        // Reset the modes DECSTR is defined to touch: DECOM (6) off, IRM (4) off,
        // DECTCEM (25) on, DECAWM (7) on. Leave everything else as-is.
        self.modes.remove(&6);
        self.modes.remove(&4);
        self.modes.insert(25);
        self.modes.insert(7);
    }

    fn clear_screen(&mut self) {
        self.clear_screen_no_home();
        self.cursor_row = 0;
        self.cursor_col = 0;
    }

    /// Erase the whole screen WITHOUT moving the cursor (ED / CSI 2 J).
    fn clear_screen_no_home(&mut self) {
        let bg_color = self.current_bg;
        for row in self.grid.iter_mut() {
            for cell in row.iter_mut() {
                *cell = TerminalCell {
                    character: ' ',
                    foreground: Color::Default,
                    background: bg_color,
                    flags: StyleFlags::default(),
                };
            }
        }
        // Mark all rows as dirty
        self.dirty_region.mark_all(self.grid.rows());
        self.mark_rows_dirty(0, self.grid.rows().saturating_sub(1));
    }

    fn set_mode(&mut self, mode: u16, private: bool) {
        if !private {
            // ANSI Set Mode (CSI Pn h). The only one we implement is IRM (4).
            // Everything else (GATM 1, ERM 6, VEM 7, LNM 20, …) is ignored so
            // it can't collide with the identically-numbered DEC private modes.
            if mode == 4 {
                self.modes.insert(4);
            }
            return;
        }
        match mode {
            4 => {
                // DECSCLM (smooth scroll) — accepted and ignored. Must NOT fall
                // through to the IRM bit that ANSI mode 4 uses.
            }
            25 => {
                // Show cursor (mode 25)
                self.modes.insert(25);
            }
            1004 => {
                // Focus event reporting
                self.modes.insert(1004);
            }
            2004 => {
                // Bracketed paste mode
                self.modes.insert(2004);
            }
            1000..=1003 => {
                // Mouse reporting modes
                self.modes.insert(mode);
            }
            1006 => {
                // SGR mouse reporting format
                self.modes.insert(mode);
            }
            1048 => {
                // Save cursor (DECSC equivalent), no buffer switch.
                self.save_cursor();
                self.modes.insert(1048);
            }
            47 | 1047 | 1049 => {
                // Alternate screen buffer (47/1047 = swap only, 1049 also saves
                // the main-screen cursor). We treat all three as a buffer swap.
                if !self.use_alt_buffer {
                    // Save main buffer state (cursor position)
                    self.saved_cursor_row = self.cursor_row;
                    self.saved_cursor_col = self.cursor_col;

                    // Reset scroll offset so we don't show scrollback in alt buffer
                    self.scroll_offset = 0;

                    // Switch to alternate buffer
                    std::mem::swap(&mut self.grid, &mut self.alt_grid);
                    self.alt_cursor_row = self.cursor_row;
                    self.alt_cursor_col = self.cursor_col;
                    std::mem::swap(
                        &mut self.keyboard_enhancement_flags,
                        &mut self.alt_keyboard_enhancement_flags,
                    );
                    std::mem::swap(
                        &mut self.keyboard_enhancement_stack,
                        &mut self.alt_keyboard_enhancement_stack,
                    );
                    self.use_alt_buffer = true;

                    // Selection anchors are absolute (scrollback+grid) row indices
                    // tied to the buffer that was visible. After a buffer swap they
                    // would highlight unrelated lines, so drop the selection.
                    self.selection = None;
                    // DECSTBM is a per-buffer attribute; reset to full-screen so a
                    // partial scroll region from the main buffer doesn't leak in.
                    self.scroll_region_top = 0;
                    self.scroll_region_bottom = self.grid.rows().saturating_sub(1);

                    // Clear alt buffer and move cursor to home
                    self.clear_screen();
                    self.modes.insert(mode);
                }
            }
            2026 => {
                // Synchronized output: suppress rendering until cleared
                self.modes.insert(2026);
                self.sync_output_active = true;
                self.sync_output_start = Some(std::time::Instant::now());
            }
            7 => {
                // Autowrap mode
                self.modes.insert(7);
            }
            _ => {
                // Unknown mode, just store it
                self.modes.insert(mode);
            }
        }
    }

    fn reset_mode(&mut self, mode: u16, private: bool) {
        if !private {
            // ANSI Reset Mode (CSI Pn l). Only IRM (4) is implemented.
            if mode == 4 {
                self.modes.remove(&4);
            }
            return;
        }
        match mode {
            4 => {
                // DECSCLM reset — ignored (see set_mode).
            }
            25 => {
                // Hide cursor
                self.modes.remove(&25);
            }
            1004 => {
                // Disable focus event reporting
                self.modes.remove(&1004);
            }
            2004 => {
                // Disable bracketed paste mode
                self.modes.remove(&2004);
            }
            1000..=1003 => {
                // Disable mouse reporting
                self.modes.remove(&mode);
            }
            1006 => {
                // Disable SGR mouse reporting format
                self.modes.remove(&mode);
            }
            1048 => {
                // Restore cursor (DECRC equivalent), no buffer switch.
                self.restore_cursor();
                self.modes.remove(&1048);
            }
            47 | 1047 | 1049 => {
                // Restore main screen buffer
                if self.use_alt_buffer {
                    // Save alt buffer state (cursor position)
                    self.alt_cursor_row = self.cursor_row;
                    self.alt_cursor_col = self.cursor_col;

                    // Switch back to main buffer
                    std::mem::swap(&mut self.grid, &mut self.alt_grid);
                    self.cursor_row = self.saved_cursor_row;
                    self.cursor_col = self.saved_cursor_col;
                    std::mem::swap(
                        &mut self.keyboard_enhancement_flags,
                        &mut self.alt_keyboard_enhancement_flags,
                    );
                    std::mem::swap(
                        &mut self.keyboard_enhancement_stack,
                        &mut self.alt_keyboard_enhancement_stack,
                    );
                    self.use_alt_buffer = false;
                    self.modes.remove(&mode);

                    // See the matching set_mode arm: clear selection because its
                    // anchors point into the alt buffer, and reset DECSTBM so the
                    // alt buffer's scroll region doesn't carry into the main one.
                    self.selection = None;
                    self.scroll_region_top = 0;
                    self.scroll_region_bottom = self.grid.rows().saturating_sub(1);

                    // Reset SGR attributes to prevent alternate screen colors from bleeding through
                    self.current_fg = Color::Default;
                    self.current_bg = Color::Default;
                    self.global_bg = Color::Default;
                    self.current_flags = StyleFlags::default();

                    // Mark all rows dirty after grid swap to force full re-render
                    // Increment by rows+1 to trigger grid_version_jumped in ui.rs
                    self.grid_version += self.grid.rows() as u64 + 1;
                    for row_ver in &mut self.row_versions {
                        *row_ver = self.grid_version;
                    }
                    self.dirty_region.mark_all(self.grid.rows());
                }
            }
            2026 => {
                // End synchronized output: force full render
                self.modes.remove(&2026);
                self.sync_output_active = false;
                self.sync_output_start = None;
                self.dirty_region.mark_all(self.grid.rows());
                self.mark_rows_dirty(0, self.grid.rows().saturating_sub(1));
            }
            7 => {
                // Disable autowrap
                self.modes.remove(&7);
            }
            _ => {
                // Unknown mode, just remove it
                self.modes.remove(&mode);
            }
        }
    }

    pub fn max_scrollback(&self) -> usize {
        self.max_scrollback
    }

    /// Number of lines currently retained in scrollback (above the live grid).
    /// This is the maximum value `scroll_offset` may take.
    pub fn scrollback_len(&self) -> usize {
        self.scrollback.len()
    }

    /// Set the absolute scrollback offset (0 = live view at bottom), clamped.
    /// No-op while the alternate screen buffer is active.
    pub fn set_scroll_offset(&mut self, offset: usize) {
        if self.use_alt_buffer {
            return;
        }
        self.scroll_offset = offset.min(self.scrollback.len());
    }

    pub fn set_max_scrollback(&mut self, max_scrollback: usize) {
        self.max_scrollback = max_scrollback.max(1);

        while self.scrollback.len() > self.max_scrollback {
            self.scrollback.pop_front();
        }

        self.scroll_offset = self.scroll_offset.min(self.scrollback.len());
    }

    pub fn is_cursor_visible(&self) -> bool {
        // Cursor is visible when mode 25 is SET (via \x1b[?25h)
        // Hidden when mode 25 is RESET (via \x1b[?25l)
        // While viewing scrollback we intentionally hide the live cursor,
        // because the viewport no longer tracks the active prompt line.
        self.modes.contains(&25) && self.scroll_offset == 0
    }

    pub fn scroll_to_bottom(&mut self) {
        self.scroll_offset = 0;
    }

    pub fn get_mouse_report(&self, button: u8, col: usize, row: usize) -> Option<String> {
        // Check if any mouse reporting mode is enabled
        if !self.modes.contains(&1000) && !self.modes.contains(&1002) && !self.modes.contains(&1003)
        {
            return None;
        }

        // SGR format (mode 1006) is preferred: CSI < button ; col ; row M/m
        // Standard format (mode 1000/1002): CSI M button col row (3 bytes)

        if self.modes.contains(&1006) {
            // SGR format: CSI < button ; x ; y M (button press) or m (button release)
            // For now, we'll generate press events (M) - release tracking would need more state
            // SGR encodes coordinates as decimal integers, so the 223/255 cap that
            // applies to the legacy X10 byte form is not needed here.
            let x = col as u32 + 1;
            let y = row as u32 + 1;
            Some(format!("\x1b[<{};{};{}M", button, x, y))
        } else {
            // Standard xterm format: CSI M button col row (raw bytes)
            // Col and row are offset by 32 (space character)
            let button_byte = 32 + button;
            // Clamp the usize coordinate BEFORE the u8 cast: casting first would
            // wrap columns/rows > 255 (the grid can be up to 1024 wide).
            let col_byte = 32 + (col.saturating_add(1).min(223) as u8);
            let row_byte = 32 + (row.saturating_add(1).min(223) as u8);
            Some(format!(
                "\x1b[M{}{}{}",
                button_byte as char, col_byte as char, row_byte as char
            ))
        }
    }

    pub fn get_mouse_release_report(&self, button: u8, col: usize, row: usize) -> Option<String> {
        if !self.modes.contains(&1000) && !self.modes.contains(&1002) && !self.modes.contains(&1003) {
            return None;
        }

        if self.modes.contains(&1006) {
            // SGR format: lowercase 'm' for release
            let x = col as u32 + 1;
            let y = row as u32 + 1;
            Some(format!("\x1b[<{};{};{}m", button, x, y))
        } else {
            // Standard xterm: release is button 3
            let button_byte = 32 + 3u8;
            let col_byte = 32 + (col.saturating_add(1).min(223) as u8);
            let row_byte = 32 + (row.saturating_add(1).min(223) as u8);
            Some(format!(
                "\x1b[M{}{}{}",
                button_byte as char, col_byte as char, row_byte as char
            ))
        }
    }

    pub fn is_mouse_enabled(&self) -> bool {
        self.modes.contains(&1000) || self.modes.contains(&1002) || self.modes.contains(&1003)
    }

    /// True when the app requested button-drag (1002) or any-motion (1003) reporting.
    pub fn is_mouse_motion_enabled(&self) -> bool {
        self.modes.contains(&1002) || self.modes.contains(&1003)
    }

    pub fn is_alt_buffer_active(&self) -> bool {
        self.use_alt_buffer
    }

    pub fn is_bracketed_paste_enabled(&self) -> bool {
        self.modes.contains(&2004)
    }

    pub fn is_application_cursor_keys(&self) -> bool {
        self.modes.contains(&1)
    }

    pub fn is_paste_events_enabled(&self) -> bool {
        self.modes.contains(&5522)
    }

    pub fn keyboard_enhancement_flags(&self) -> u16 {
        self.keyboard_enhancement_flags
    }

    pub fn xterm_modify_other_keys(&self) -> u16 {
        self.xterm_modify_other_keys
    }

    pub fn xterm_format_other_keys(&self) -> u16 {
        self.xterm_format_other_keys
    }

    pub fn is_report_all_keys_enabled(&self) -> bool {
        self.modes.contains(&2031) || (self.keyboard_enhancement_flags & 0b1000) != 0
    }

    pub fn build_paste_event(&mut self, mime_types: &[String]) -> Vec<u8> {
        let password = uuid::Uuid::new_v4().to_string();
        self.pending_paste_password = Some(password.clone());
        let encoded_password =
            base64::engine::general_purpose::STANDARD.encode(password.as_bytes());
        let mut output = Vec::new();

        output.extend_from_slice(b"\x1b]5522;type=read:status=OK:password=");
        output.extend_from_slice(encoded_password.as_bytes());
        output.extend_from_slice(Self::osc_terminator());

        for mime_type in mime_types {
            let encoded_mime =
                base64::engine::general_purpose::STANDARD.encode(mime_type.as_bytes());
            output.extend_from_slice(b"\x1b]5522;type=read:status=DATA:mime=");
            output.extend_from_slice(encoded_mime.as_bytes());
            output.extend_from_slice(Self::osc_terminator());
        }

        output.extend_from_slice(b"\x1b]5522;type=read:status=DONE\x1b\\");
        output
    }

    pub fn take_clipboard_read_requests(&mut self) -> Vec<ClipboardReadRequest> {
        std::mem::take(&mut self.pending_clipboard_requests)
    }

    fn scroll_down(&mut self) {
        if self.grid.rows() > 0 {
            let bg_color = self.current_bg;
            let blank_cell = TerminalCell {
                character: ' ',
                foreground: Color::Default,
                background: bg_color,
                flags: StyleFlags::default(),
            };

            // Compress first row directly from the grid slice before shifting,
            // avoiding a per-line Vec allocation from get_row.
            if !self.use_alt_buffer {
                let line = ScrollbackLine::compress(&self.grid[0], self.grid.row_wrapped[0]);
                self.grid.shift_rows_up();
                self.grid.fill_last_row(blank_cell);
                self.push_scrollback_compressed(line);
            } else {
                self.grid.shift_rows_up();
                self.grid.fill_last_row(blank_cell);
            }

            self.dirty_region.mark_all(self.grid.rows());
            let version = self.grid_version;
            for v in self.row_versions.iter_mut() {
                *v = version;
            }
        }
    }

    pub fn get_visible_cells(&mut self) -> std::sync::Arc<Vec<Vec<TerminalCell>>> {
        if let Some((cached_version, cached_offset, ref cells)) = self.visible_cells_cache {
            if cached_version == self.grid_version && cached_offset == self.scroll_offset {
                return std::sync::Arc::clone(cells);
            }
        }

        // Cache miss - rebuild
        let rows = self.grid.rows();
        let cols = if rows > 0 { self.grid.row_len() } else { 80 };

        // Try to recycle the previous allocation. The renderer drops its returned
        // Arc each frame, so by the next miss we are usually the sole owner and can
        // refill the existing nested Vecs in place instead of reallocating per row.
        let prev = self.visible_cells_cache.take();
        let prev_version = prev.as_ref().map(|(v, _, _)| *v);
        let prev_offset = prev.as_ref().map(|(_, o, _)| *o);
        let mut recycled = prev.map(|(_, _, a)| a);

        if self.scroll_offset == 0 {
            // Fast path: copy current grid, reusing inner Vec capacity when possible.
            if let Some(buf) = recycled.as_mut().and_then(std::sync::Arc::get_mut) {
                // Incremental path: if the recycled buffer already holds a same-sized
                // snapshot taken at scroll_offset==0, only re-copy rows whose
                // row_versions changed since that snapshot. Untouched rows already
                // hold valid data, turning an O(rows*cols) copy into O(dirty cells).
                let can_incremental = prev_offset == Some(0)
                    && buf.len() == rows
                    && buf.iter().all(|r| r.len() == cols);
                if can_incremental {
                    let base = prev_version.unwrap_or(0);
                    for (r, (dst, chunk)) in buf.iter_mut().zip(self.grid.iter()).enumerate() {
                        if self.row_versions[r] > base {
                            dst.clear();
                            dst.extend_from_slice(chunk);
                        }
                    }
                } else {
                    buf.resize_with(rows, Vec::new);
                    for (dst, chunk) in buf.iter_mut().zip(self.grid.iter()) {
                        dst.clear();
                        dst.extend_from_slice(chunk);
                    }
                }
                let arc = recycled.unwrap();
                self.visible_cells_cache =
                    Some((self.grid_version, self.scroll_offset, std::sync::Arc::clone(&arc)));
                return arc;
            }
        }

        let cells = if self.scroll_offset == 0 {
            // Fast path (shared allocation): fresh copy of current grid.
            self.grid.to_vec()
        } else {
            // Slow path: reflow scrollback
            let blank_cell = self.create_blank_cell();

            let mut start_idx = self.scrollback.len().saturating_sub(self.scroll_offset + rows);
            while start_idx > 0 && self.scrollback[start_idx - 1].is_wrapped {
                start_idx -= 1;
            }
            let end_idx = self.scrollback.len();
            let to_reflow: Vec<ScrollbackLine> = self.scrollback
                .iter()
                .skip(start_idx)
                .take(end_idx - start_idx)
                .cloned()
                .collect();

            let reflowed = Self::reflow_lines(&to_reflow, cols, &blank_cell);
            let skip = reflowed.len().saturating_sub(self.scroll_offset + rows);
            let visible_start = skip + (reflowed.len() - skip).saturating_sub(self.scroll_offset);
            let mut result: Vec<Vec<TerminalCell>> = reflowed[visible_start..].iter().map(|l| l.decompress()).collect();

            if result.len() > rows {
                result.truncate(rows);
            }

            for row in self.grid.iter() {
                if result.len() < rows {
                    result.push(self.normalize_line_width(row.to_vec(), cols));
                } else {
                    break;
                }
            }

            while result.len() < rows {
                result.push(self.blank_line(cols));
            }

            result
        };

        // Reuse the recycled Arc's outer allocation if we still solely own it.
        let arc = match recycled.as_mut().and_then(std::sync::Arc::get_mut) {
            Some(buf) => {
                *buf = cells;
                recycled.unwrap()
            }
            None => std::sync::Arc::new(cells),
        };
        self.visible_cells_cache = Some((self.grid_version, self.scroll_offset, std::sync::Arc::clone(&arc)));
        arc
    }

    pub fn get_cursor_pos(&self) -> (usize, usize) {
        (self.cursor_row, self.cursor_col)
    }

    /// 获取当前可见行的wrapped状态，用于跨行链接检测
    pub fn get_visible_row_wrapped(&self) -> Vec<bool> {
        let rows = self.grid.rows();

        if self.scroll_offset == 0 {
            // Fast path: just get current grid wrapped flags
            self.grid.row_wrapped.clone()
        } else {
            // Slow path: need to reconstruct from scrollback
            // For simplicity, when scrolling we disable wrapped link detection
            // by returning all false (can be improved later with full reflow)
            vec![false; rows]
        }
    }

    pub fn get_output(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.output_buffer)
    }

    #[inline]
    fn viewport_row_to_absolute(&self, viewport_row: usize) -> usize {
        self.scrollback.len().saturating_sub(self.scroll_offset) + viewport_row
    }

    #[allow(dead_code)]
    pub fn select_text(&mut self, anchor: (usize, usize), active: (usize, usize)) {
        self.selection = Some(Selection { anchor, active, mode: SelectionMode::Normal });
    }

    /// Start a new selection at a viewport-relative position.
    /// Converts to absolute buffer coordinates internally.
    pub fn start_selection(&mut self, viewport_pos: (usize, usize)) {
        self.start_selection_with_mode(viewport_pos, SelectionMode::Normal);
    }

    pub fn start_block_selection(&mut self, viewport_pos: (usize, usize)) {
        self.start_selection_with_mode(viewport_pos, SelectionMode::Block);
    }

    fn start_selection_with_mode(&mut self, viewport_pos: (usize, usize), mode: SelectionMode) {
        let abs = (
            self.viewport_row_to_absolute(viewport_pos.0),
            viewport_pos.1,
        );
        self.selection = Some(Selection {
            anchor: abs,
            active: abs,
            mode,
        });
    }

    /// Update the active end of the current selection with a viewport-relative position.
    pub fn update_selection(&mut self, viewport_pos: (usize, usize)) {
        let abs_row = self.viewport_row_to_absolute(viewport_pos.0);
        if let Some(ref mut sel) = self.selection {
            sel.active = (abs_row, viewport_pos.1);
        }
    }

    /// Select the word at the given (row, col) position in the visible grid.
    /// Word boundaries are determined by character class: alphanumeric/underscore,
    /// whitespace, or punctuation/symbols.
    pub fn select_word_at(&mut self, row: usize, col: usize) {
        let visible = self.get_visible_cells();
        if row >= visible.len() {
            return;
        }
        let line = &visible[row];
        let cols = line.len();
        if col >= cols {
            return;
        }

        // Skip wide_continuation to find the real character
        let mut start_col = col;
        if line[start_col].flags.wide_continuation() && start_col > 0 {
            start_col -= 1;
        }

        if let Some((left, right)) = Self::select_extended_token_span(line, start_col) {
            let abs_row = self.viewport_row_to_absolute(row);
            self.selection = Some(Selection {
                anchor: (abs_row, left),
                active: (abs_row, right),
                mode: SelectionMode::Normal,
            });
            return;
        }

        let ch = line[start_col].character;
        let class = char_class(ch);

        // Expand left
        let mut left = start_col;
        while left > 0 {
            let prev = left - 1;
            let c = line[prev].character;
            if line[prev].flags.wide_continuation() {
                left = prev;
                continue;
            }
            if char_class(c) != class {
                break;
            }
            left = prev;
        }

        // Expand right
        let mut right = start_col;
        loop {
            let next = if line[right].flags.wide() {
                right + 2
            } else {
                right + 1
            };
            if next >= cols {
                break;
            }
            if line[next].flags.wide_continuation() {
                // shouldn't happen after a non-wide char, but skip
                if next + 1 < cols {
                    if char_class(line[next + 1].character) != class {
                        break;
                    }
                    right = next + 1;
                    continue;
                }
                break;
            }
            if char_class(line[next].character) != class {
                break;
            }
            right = next;
        }
        // If the selected end is a wide char, include its continuation cell
        if line[right].flags.wide() && right + 1 < cols {
            right += 1;
        }

        let abs_row = self.viewport_row_to_absolute(row);
        self.selection = Some(Selection {
            anchor: (abs_row, left),
            active: (abs_row, right),
            mode: SelectionMode::Normal,
        });
    }

    fn select_extended_token_span(
        line: &[TerminalCell],
        start_col: usize,
    ) -> Option<(usize, usize)> {
        let cols = line.len();
        if start_col >= cols {
            return None;
        }

        let start_char = line[start_col].character;
        if !is_extended_token_char(start_char) {
            return None;
        }

        let mut left = start_col;
        while left > 0 {
            let prev = left - 1;
            if line[prev].flags.wide_continuation() {
                left = prev;
                continue;
            }
            if !is_extended_token_char(line[prev].character) {
                break;
            }
            left = prev;
        }

        let mut right = start_col;
        loop {
            let next = if line[right].flags.wide() {
                right + 2
            } else {
                right + 1
            };
            if next >= cols {
                break;
            }
            if line[next].flags.wide_continuation() {
                if next + 1 < cols && is_extended_token_char(line[next + 1].character) {
                    right = next + 1;
                    continue;
                }
                break;
            }
            if !is_extended_token_char(line[next].character) {
                break;
            }
            right = next;
        }

        while left < start_col && is_token_prefix_wrapper(line[left].character) {
            left += 1;
        }

        while right > start_col && is_token_suffix_wrapper(line[right].character) {
            right -= if line[right].flags.wide_continuation() && right > 0 {
                2
            } else {
                1
            };
        }

        if left > right || start_col < left || start_col > right {
            return None;
        }

        let mut has_alnum = false;
        let mut has_separator = false;
        for cell in &line[left..=right] {
            if cell.flags.wide_continuation() {
                continue;
            }
            let ch = cell.character;
            has_alnum |= ch.is_alphanumeric();
            has_separator |= is_extended_token_separator(ch);
        }

        if !has_alnum || !has_separator {
            return None;
        }

        if line[right].flags.wide() && right + 1 < cols {
            right += 1;
        }

        Some((left, right))
    }

    pub fn copy_selection(&self) -> Option<String> {
        self.selection.map(|sel| {
            let (start, end) = if sel.anchor <= sel.active {
                (sel.anchor, sel.active)
            } else {
                (sel.active, sel.anchor)
            };
            let mut result = String::new();
            let scrollback_len = self.scrollback.len();
            let grid_rows = self.grid.rows();
            let cols = self.grid.row_len();
            let total_rows = scrollback_len + grid_rows;

            let block = matches!(sel.mode, SelectionMode::Block);
            for abs_row in start.0..=end.0.min(total_rows.saturating_sub(1)) {
                let (start_col, end_col) = if block {
                    // Rectangular: same column span on every row.
                    let lo = sel.anchor.1.min(sel.active.1);
                    let hi = sel.anchor.1.max(sel.active.1);
                    (lo, hi.min(cols.saturating_sub(1)))
                } else {
                    let s = if abs_row == start.0 { start.1 } else { 0 };
                    let e = if abs_row == end.0 {
                        end.1.min(cols.saturating_sub(1))
                    } else {
                        cols.saturating_sub(1)
                    };
                    (s, e)
                };

                if abs_row < scrollback_len {
                    // Read from scrollback
                    let line = self.scrollback[abs_row].decompress();
                    for col in start_col..=end_col.min(line.len().saturating_sub(1)) {
                        if !line[col].flags.wide_continuation() {
                            result.push(line[col].character);
                        }
                    }
                } else {
                    // Read from current grid
                    let grid_row = abs_row - scrollback_len;
                    if grid_row < grid_rows {
                        for col in start_col..=end_col {
                            let cell = self.grid.get(grid_row, col);
                            if !cell.flags.wide_continuation() {
                                result.push(cell.character);
                            }
                        }
                    }
                }

                if abs_row < end.0 {
                    result.push('\n');
                }
            }

            result
        })
    }

    pub fn scroll(&mut self, lines: isize) {
        // Don't scroll scrollback when in alternate screen buffer (less, vim, git log, etc.)
        if self.use_alt_buffer {
            return;
        }

        if lines > 0 {
            // Scroll up (show earlier lines)
            self.scroll_offset = self.scroll_offset.saturating_add(lines as usize);
        } else {
            // Scroll down (show later lines)
            self.scroll_offset = self.scroll_offset.saturating_sub((-lines) as usize);
        }

        // Clamp scroll_offset to valid range
        let max_scroll = self.scrollback.len();
        self.scroll_offset = self.scroll_offset.min(max_scroll);

        // When scrolling to bottom (offset 0), reset to live view
        if self.scroll_offset == 0 {
            self.scroll_offset = 0;
        }
    }

    fn strip_trailing_blanks(cells: &[TerminalCell]) -> &[TerminalCell] {
        let mut end = cells.len();
        while end > 0 && cells[end - 1].character == ' ' && cells[end - 1].background == Color::Default && !cells[end - 1].flags.wide() && !cells[end - 1].flags.wide_continuation() {
            end -= 1;
        }
        &cells[..end]
    }

    fn reflow_lines(lines: &[ScrollbackLine], new_cols: usize, blank_cell: &TerminalCell) -> Vec<ScrollbackLine> {
        let mut result = Vec::new();
        let len = lines.len();
        let mut i = 0;

        while i < len {
            let mut logical_line: Vec<TerminalCell> = Vec::new();
            let decompressed = lines[i].decompress();
            logical_line.extend_from_slice(Self::strip_trailing_blanks(&decompressed));
            while i < len && lines[i].is_wrapped {
                i += 1;
                if i < len {
                    let dc = lines[i].decompress();
                    logical_line.extend_from_slice(Self::strip_trailing_blanks(&dc));
                }
            }
            i += 1;

            if logical_line.is_empty() {
                result.push(ScrollbackLine::compress(&vec![blank_cell.clone(); new_cols], false));
                continue;
            }

            let chunks: Vec<&[TerminalCell]> = logical_line.chunks(new_cols).collect();
            let num_chunks = chunks.len();
            for (ci, chunk) in chunks.into_iter().enumerate() {
                if chunk.len() == new_cols {
                    result.push(ScrollbackLine::compress(chunk, ci + 1 < num_chunks));
                } else {
                    let mut cells = chunk.to_vec();
                    cells.resize(new_cols, blank_cell.clone());
                    result.push(ScrollbackLine::compress(&cells, ci + 1 < num_chunks));
                }
            }
        }

        result
    }

    pub fn on_resize(&mut self, cols: usize, rows: usize) {
        if cols == 0 || rows == 0 {
            return;
        }

        let (cols, rows) = clamp_terminal_dimensions(cols, rows);

        let old_rows = self.grid.rows();
        let had_full_screen_region = old_rows == 0
            || (self.scroll_region_top == 0 && self.scroll_region_bottom + 1 >= old_rows);

        let blank_cell = self.create_blank_cell();

        // When the row count shrinks on the primary screen, mirror a real
        // terminal: push the oldest on-screen lines into scrollback and shift the
        // rest up, rather than letting TerminalGrid::resize silently truncate the
        // BOTTOM rows (where the prompt/cursor usually live). The cursor is kept
        // on-screen. (Column reflow on width change is not done here.)
        if !self.use_alt_buffer && old_rows > rows {
            let to_remove = old_rows - rows;
            // Take as many rows off the top as possible without scrolling the
            // cursor above row 0; any remainder is truncated from the bottom.
            let top_remove = to_remove.min(self.cursor_row);
            if top_remove > 0 {
                let cols_now = self.grid.row_len();
                for r in 0..top_remove {
                    let line =
                        ScrollbackLine::compress(&self.grid[r], self.grid.row_wrapped[r]);
                    self.push_scrollback_compressed(line);
                }
                let src_start = top_remove * cols_now;
                let total = old_rows * cols_now;
                self.grid.cells.copy_within(src_start..total, 0);
                self.grid.row_wrapped.copy_within(top_remove..old_rows, 0);
                self.cursor_row -= top_remove;
                self.saved_cursor_row = self.saved_cursor_row.saturating_sub(top_remove);
            }
        }

        self.grid.resize(rows, cols, blank_cell.clone());
        self.alt_grid.resize(rows, cols, blank_cell.clone());

        // CRITICAL: Sync row_versions size with grid size to prevent dirty mark loss
        // When grid grows, we need to extend row_versions; when it shrinks, truncate it
        if rows != self.row_versions.len() {
            self.row_versions.resize(rows, self.grid_version);
        }

        // Keep the tab-stop table sized to the new column count, defaulting any
        // newly added columns to the standard every-8 stops.
        if cols != self.tab_stops.len() {
            let old_len = self.tab_stops.len();
            self.tab_stops.resize(cols, false);
            for c in old_len..cols {
                self.tab_stops[c] = c % 8 == 0 && c != 0;
            }
        }

        self.scroll_offset = 0;
        self.cursor_row = self.cursor_row.min(rows.saturating_sub(1));
        self.cursor_col = self.cursor_col.min(cols.saturating_sub(1));
        self.saved_cursor_row = self.saved_cursor_row.min(rows.saturating_sub(1));
        self.saved_cursor_col = self.saved_cursor_col.min(cols.saturating_sub(1));
        self.alt_cursor_row = self.alt_cursor_row.min(rows.saturating_sub(1));
        self.alt_cursor_col = self.alt_cursor_col.min(cols.saturating_sub(1));
        if had_full_screen_region {
            self.scroll_region_top = 0;
            self.scroll_region_bottom = rows.saturating_sub(1);
        } else {
            self.scroll_region_top = self.scroll_region_top.min(rows.saturating_sub(1));
            self.scroll_region_bottom = self.scroll_region_bottom.min(rows.saturating_sub(1));

            if self.scroll_region_top > self.scroll_region_bottom {
                self.scroll_region_top = 0;
                self.scroll_region_bottom = rows.saturating_sub(1);
            }
        }
    }

    pub fn get_dimensions(&self) -> (usize, usize) {
        if self.grid.is_empty() {
            (0, 0)
        } else {
            (self.grid.row_len(), self.grid.rows())
        }
    }

    #[inline]
    pub fn row_selection_cols(&self, viewport_row: usize) -> Option<(usize, usize)> {
        let sel = self.selection?;
        let abs_row = self.viewport_row_to_absolute(viewport_row);
        let (start, end) = if sel.anchor <= sel.active {
            (sel.anchor, sel.active)
        } else {
            (sel.active, sel.anchor)
        };

        if abs_row < start.0 || abs_row > end.0 {
            return None;
        }

        match sel.mode {
            SelectionMode::Block => {
                let col_min = sel.anchor.1.min(sel.active.1);
                let col_max = sel.anchor.1.max(sel.active.1);
                Some((col_min, col_max))
            }
            SelectionMode::Normal => {
                let col_start = if abs_row == start.0 { start.1 } else { 0 };
                let col_end = if abs_row == end.0 { end.1 } else { usize::MAX };
                Some((col_start, col_end))
            }
        }
    }

    // IME support methods
    pub fn set_preedit(&mut self, text: String, selection: Option<std::ops::Range<usize>>) {
        self.preedit_text = text;
        self.preedit_selection = selection;
    }

    pub fn clear_preedit(&mut self) {
        self.preedit_text.clear();
        self.preedit_selection = None;
    }
}

#[cfg(test)]
mod tests {
    use super::{ClipboardReadKind, Color, TerminalState};

    #[test]
    fn resize_preserves_full_screen_scroll_region() {
        let mut terminal = TerminalState::new(4, 3);

        terminal.on_resize(4, 6);

        assert_eq!(terminal.scroll_region_top, 0);
        assert_eq!(terminal.scroll_region_bottom, 5);
    }

    #[test]
    fn linefeed_at_bottom_pushes_to_scrollback_for_full_screen_region() {
        let mut terminal = TerminalState::new(4, 2);
        terminal.grid[0][0].character = 'A';
        terminal.grid[1][0].character = 'B';
        terminal.cursor_row = 1;
        terminal.cursor_col = 0;

        terminal.process_input(b"\n");

        assert_eq!(terminal.scrollback.len(), 1);
        assert_eq!(terminal.scrollback[0].decompress()[0].character, 'A');
        assert_eq!(terminal.grid[0][0].character, 'B');
        assert_eq!(terminal.grid[1][0].character, ' ');
    }

    #[test]
    fn visible_cells_keep_rectangular_shape_after_resize_with_scrollback() {
        let mut terminal = TerminalState::new(4, 2);
        terminal.grid.get_mut(0, 0).character = 'A';
        terminal.grid.get_mut(1, 0).character = 'B';
        terminal.cursor_row = 1;

        terminal.process_input(b"\n");
        terminal.on_resize(5, 2);
        terminal.scroll(1);

        let visible = terminal.get_visible_cells();

        assert_eq!(visible.len(), 2);
        assert!(visible.iter().all(|row| row.len() == 5));
        assert_eq!(visible[0][0].character, 'A');
        assert_eq!(visible[0][4].character, ' ');
    }

    #[test]
    fn cursor_is_hidden_while_viewing_scrollback() {
        let mut terminal = TerminalState::new(4, 2);
        terminal.grid.get_mut(0, 0).character = 'A';
        terminal.grid.get_mut(1, 0).character = 'B';
        terminal.cursor_row = 1;

        terminal.process_input(b"\n");

        assert!(terminal.is_cursor_visible());

        terminal.scroll(1);

        assert!(!terminal.is_cursor_visible());
    }

    #[test]
    fn scroll_to_bottom_restores_live_cursor_visibility() {
        let mut terminal = TerminalState::new(4, 2);
        terminal.grid.get_mut(0, 0).character = 'A';
        terminal.grid.get_mut(1, 0).character = 'B';
        terminal.cursor_row = 1;

        terminal.process_input(b"\n");
        terminal.scroll(1);
        terminal.scroll_to_bottom();

        assert_eq!(terminal.scroll_offset, 0);
        assert!(terminal.is_cursor_visible());
    }

    #[test]
    fn sgr_39_and_49_restore_default_colors() {
        let mut terminal = TerminalState::new(8, 2);

        terminal.process_input(b"\x1b[36;44mA\x1b[39;49mB");

        let first = &terminal.grid[0][0];
        let second = &terminal.grid[0][1];

        assert_eq!(first.foreground, Color::Cyan);
        assert_eq!(first.background, Color::Blue);
        assert_eq!(second.foreground, Color::Default);
        assert_eq!(second.background, Color::Default);
    }

    #[test]
    fn cleared_cells_keep_active_background() {
        let mut terminal = TerminalState::new(8, 2);

        terminal.process_input(b"\x1b[44mAB\x1b[1;1H\x1b[K");

        assert_eq!(terminal.grid[0][0].background, Color::Blue);
        assert_eq!(terminal.grid[0][1].background, Color::Blue);
    }

    #[test]
    fn empty_sgr_sequence_resets_attributes() {
        let mut terminal = TerminalState::new(8, 2);

        terminal.process_input(b"\x1b[7;36;44mA\x1b[mB");

        let first = &terminal.grid[0][0];
        let second = &terminal.grid[0][1];

        assert!(first.flags.inverse());
        assert_eq!(first.foreground, Color::Cyan);
        assert_eq!(first.background, Color::Blue);

        assert!(!second.flags.inverse());
        assert_eq!(second.foreground, Color::Default);
        assert_eq!(second.background, Color::Default);
    }

    #[test]
    fn split_truecolor_sequence_does_not_leak_text() {
        let mut terminal = TerminalState::new(32, 2);

        terminal.process_input(b"\x1b[38");
        terminal.process_input(b";2;81;175;239msrc");

        assert_eq!(terminal.grid[0][0].character, 's');
        assert_eq!(terminal.grid[0][1].character, 'r');
        assert_eq!(terminal.grid[0][2].character, 'c');
        assert_eq!(terminal.grid[0][0].foreground, Color::Rgb(81, 175, 239));
    }

    #[test]
    fn trailing_escape_is_buffered_until_next_chunk() {
        let mut terminal = TerminalState::new(8, 2);

        terminal.process_input(b"\x1b");
        terminal.process_input(b"[31mX");

        assert_eq!(terminal.grid[0][0].character, 'X');
        assert_eq!(terminal.grid[0][0].foreground, Color::Red);
    }

    #[test]
    fn dec_special_graphics_charset_maps_line_drawing() {
        let mut terminal = TerminalState::new(8, 2);

        terminal.process_input(b"\x1b(0qx\x0fA");

        assert_eq!(terminal.grid[0][0].character, '─');
        assert_eq!(terminal.grid[0][1].character, '│');
        assert_eq!(terminal.grid[0][2].character, 'A');
    }

    #[test]
    fn decscusr_with_intermediate_space_does_not_leak_text() {
        let mut terminal = TerminalState::new(8, 2);

        terminal.process_input(b"\x1b[0 qX");

        assert_eq!(terminal.grid[0][0].character, 'X');
    }

    #[test]
    fn private_csi_u_sequence_does_not_restore_cursor_or_leak() {
        let mut terminal = TerminalState::new(8, 2);

        terminal.process_input(b"AB");
        terminal.process_input(b"\x1b[?4uC");

        assert_eq!(terminal.grid[0][0].character, 'A');
        assert_eq!(terminal.grid[0][1].character, 'B');
        assert_eq!(terminal.grid[0][2].character, 'C');
    }

    #[test]
    fn csi_with_gt_prefix_is_consumed_without_printing_parameters() {
        let mut terminal = TerminalState::new(8, 2);

        terminal.process_input(b"\x1b[>4;1mZ");

        assert_eq!(terminal.grid[0][0].character, 'Z');
        assert_eq!(terminal.grid[0][1].character, ' ');
    }

    #[test]
    fn dcs_sequence_is_consumed_without_leaking_text() {
        let mut terminal = TerminalState::new(8, 2);

        terminal.process_input(b"\x1bP$q q\x1b\\X");

        assert_eq!(terminal.grid[0][0].character, 'X');
        assert_eq!(terminal.grid[0][1].character, ' ');
    }

    #[test]
    fn primary_and_secondary_device_attributes_are_reported() {
        let mut terminal = TerminalState::new(8, 2);

        terminal.process_input(b"\x1b[c\x1b[>c");

        assert_eq!(
            String::from_utf8(terminal.get_output()).unwrap(),
            "\x1b[?65;1;9c\x1b[>1;7802;0c"
        );
    }

    #[test]
    fn xtversion_query_is_reported() {
        let mut terminal = TerminalState::new(8, 2);

        terminal.process_input(b"\x1b[>0q");

        assert_eq!(
            String::from_utf8(terminal.get_output()).unwrap(),
            "\x1bP>|VTE(7802)\x1b\\"
        );
    }

    #[test]
    fn double_click_selects_full_url() {
        let mut terminal = TerminalState::new(64, 2);

        terminal.process_input(b"see https://example.com/path?a=1&b=2 now");
        terminal.select_word_at(0, 12);

        assert_eq!(
            terminal.copy_selection().as_deref(),
            Some("https://example.com/path?a=1&b=2")
        );
    }

    #[test]
    fn double_click_selects_file_path_with_line_number() {
        let mut terminal = TerminalState::new(64, 2);

        terminal.process_input(b"open src/main.rs:1480 please");
        terminal.select_word_at(0, 8);

        assert_eq!(
            terminal.copy_selection().as_deref(),
            Some("src/main.rs:1480")
        );
    }

    #[test]
    fn double_click_excludes_wrapping_punctuation() {
        let mut terminal = TerminalState::new(64, 2);

        terminal.process_input(b"(https://example.com/path), next");
        terminal.select_word_at(0, 10);

        assert_eq!(
            terminal.copy_selection().as_deref(),
            Some("https://example.com/path")
        );
    }

    #[test]
    fn bracketed_paste_mode_is_tracked() {
        let mut terminal = TerminalState::new(8, 2);

        terminal.process_input(b"\x1b[?2004h");
        assert!(terminal.is_bracketed_paste_enabled());

        terminal.process_input(b"\x1b[?2004l");
        assert!(!terminal.is_bracketed_paste_enabled());
    }

    #[test]
    fn kitty_keyboard_flags_can_be_set_queried_and_popped() {
        let mut terminal = TerminalState::new(8, 2);

        terminal.process_input(b"\x1b[=1u");
        assert_eq!(terminal.keyboard_enhancement_flags(), 1);

        terminal.process_input(b"\x1b[?u");
        assert_eq!(
            String::from_utf8(terminal.get_output()).unwrap(),
            "\x1b[?1u"
        );

        terminal.process_input(b"\x1b[>5u");
        assert_eq!(terminal.keyboard_enhancement_flags(), 5);

        terminal.process_input(b"\x1b[<u");
        assert_eq!(terminal.keyboard_enhancement_flags(), 1);
    }

    #[test]
    fn xtmodkeys_and_xtfmtkeys_state_is_tracked() {
        let mut terminal = TerminalState::new(8, 2);

        terminal.process_input(b"\x1b[>4;2m\x1b[>4;1f");

        assert_eq!(terminal.xterm_modify_other_keys(), 2);
        assert_eq!(terminal.xterm_format_other_keys(), 1);
    }

    #[test]
    fn vte_report_all_keys_mode_is_tracked() {
        let mut terminal = TerminalState::new(8, 2);

        terminal.process_input(b"\x1b[?2031h");
        assert!(terminal.is_report_all_keys_enabled());

        terminal.process_input(b"\x1b[?2031l");
        assert!(!terminal.is_report_all_keys_enabled());
    }

    #[test]
    fn osc_5522_read_request_is_queued() {
        let mut terminal = TerminalState::new(8, 2);

        terminal.process_input(b"\x1b]5522;type=read;Lg==\x1b\\");

        let requests = terminal.take_clipboard_read_requests();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].kind, ClipboardReadKind::MimeList);
    }

    #[test]
    fn decrqm_reports_5522_support() {
        let mut terminal = TerminalState::new(8, 2);

        terminal.process_input(b"\x1b[?5522$p");

        assert_eq!(
            String::from_utf8(terminal.get_output()).unwrap(),
            "\x1b[?5522;2$y"
        );
    }

    #[test]
    fn scrollback_viewport_pinned_when_new_output_arrives() {
        // Reading history while output streams in should not slide the viewport
        // toward the bottom: scroll_offset must compensate when push_scrollback
        // grows the deque.
        let mut terminal = TerminalState::new(2, 2);
        // Push three lines into scrollback.
        terminal.grid.get_mut(0, 0).character = 'A';
        terminal.grid.get_mut(1, 0).character = 'B';
        terminal.cursor_row = 1;
        terminal.process_input(b"\n");
        terminal.grid.get_mut(1, 0).character = 'C';
        terminal.process_input(b"\n");
        terminal.grid.get_mut(1, 0).character = 'D';
        terminal.process_input(b"\n");

        // Scroll up to view 'A','B'.
        terminal.set_scroll_offset(3);
        let before = terminal.get_visible_cells();
        let top_before = before[0][0].character;
        assert_eq!(top_before, 'A');

        // New line arrives — viewport must still show the same top row.
        terminal.grid.get_mut(1, 0).character = 'E';
        terminal.process_input(b"\n");
        let after = terminal.get_visible_cells();
        assert_eq!(after[0][0].character, 'A');
    }

    #[test]
    fn alt_buffer_switch_clears_selection_and_scroll_region() {
        let mut terminal = TerminalState::new(4, 4);
        // Set a partial scroll region and a selection on the main buffer.
        terminal.process_input(b"\x1b[2;3r");
        assert_eq!(terminal.scroll_region_top, 1);
        assert_eq!(terminal.scroll_region_bottom, 2);
        terminal.start_selection((0, 0));
        terminal.update_selection((1, 1));
        assert!(terminal.selection.is_some());

        // Enter alt buffer.
        terminal.process_input(b"\x1b[?1049h");

        assert!(terminal.is_alt_buffer_active());
        assert!(terminal.selection.is_none(), "selection must clear on alt switch");
        assert_eq!(terminal.scroll_region_top, 0);
        assert_eq!(terminal.scroll_region_bottom, 3);

        // Restore some region & selection in alt buffer, then leave.
        terminal.process_input(b"\x1b[1;2r");
        terminal.start_selection((0, 0));
        terminal.update_selection((1, 1));
        terminal.process_input(b"\x1b[?1049l");

        assert!(!terminal.is_alt_buffer_active());
        assert!(terminal.selection.is_none(), "selection must clear on alt restore");
        assert_eq!(terminal.scroll_region_top, 0);
        assert_eq!(terminal.scroll_region_bottom, 3);
    }

    #[test]
    fn sgr_mouse_report_is_not_capped_at_255() {
        let mut terminal = TerminalState::new(400, 50);
        // Enable mouse tracking + SGR encoding.
        terminal.process_input(b"\x1b[?1000h\x1b[?1006h");

        let report = terminal.get_mouse_report(0, 299, 10).unwrap();
        // 1-indexed: column 300, row 11. Pre-fix this would have been 256.
        assert_eq!(report, "\x1b[<0;300;11M");

        let release = terminal.get_mouse_release_report(0, 299, 10).unwrap();
        assert_eq!(release, "\x1b[<0;300;11m");
    }
}
