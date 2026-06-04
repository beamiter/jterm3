use iced::Color;
use serde::{Deserialize, Serialize};
use std::path::Path;

/// 终端颜色配置
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TerminalColors {
    pub foreground: [u8; 3],
    pub background: [u8; 3],
    pub cursor: [u8; 3],
    pub selection: [u8; 4],         // RGBA
    pub ansi_colors: [[u8; 3]; 16], // 标准 16 色
}

/// UI 组件颜色
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct UIColors {
    pub window_bg: [u8; 3],
    pub panel_bg: [u8; 3],
    pub border: [u8; 3],
    pub text: [u8; 3],
    pub text_disabled: [u8; 3],
}

/// 滚动条颜色
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ScrollbarColors {
    pub track_normal: [u8; 4],
    pub track_hover: [u8; 4],
    pub track_drag: [u8; 4],
    pub thumb_normal: [u8; 4],
    pub thumb_hover: [u8; 4],
    pub thumb_drag: [u8; 4],
}

/// 标签栏颜色
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TabbarColors {
    pub bg: [u8; 3],
    pub border: [u8; 3],
    pub inactive_text: [u8; 3],
    pub active_text: [u8; 3],
    pub active_border: [u8; 3],
    pub close_btn_bg: [u8; 3],
    pub close_btn_hover: [u8; 3],
}

/// 搜索框颜色
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SearchColors {
    pub bg: [u8; 3],
    pub border: [u8; 3],
    pub text: [u8; 3],
}

/// 命令调板颜色
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CommandPaletteColors {
    pub session_color: [u8; 3],
    pub edit_color: [u8; 3],
    pub search_color: [u8; 3],
    pub terminal_color: [u8; 3],
    pub window_color: [u8; 3],
}

/// 完整的主题定义
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Theme {
    pub name: String,
    pub terminal: TerminalColors,
    pub ui: UIColors,
    pub scrollbar: ScrollbarColors,
    pub tabbar: TabbarColors,
    pub search: SearchColors,
    pub palette: CommandPaletteColors,
}

impl Theme {
    /// 获取内置的深色主题
    pub fn builtin_dark() -> Self {
        Theme {
            name: "dark".to_string(),
            terminal: TerminalColors {
                foreground: [220, 220, 220],
                background: [29, 29, 29],
                cursor: [200, 200, 200],
                selection: [200, 200, 200, 100],
                ansi_colors: [
                    [0, 0, 0],       // Black
                    [205, 49, 49],   // Red
                    [13, 177, 47],   // Green
                    [229, 178, 34],  // Yellow
                    [36, 114, 200],  // Blue
                    [188, 63, 60],   // Magenta
                    [17, 168, 205],  // Cyan
                    [230, 230, 230], // White
                    [80, 80, 80],    // Bright Black
                    [241, 76, 76],   // Bright Red
                    [35, 209, 139],  // Bright Green
                    [245, 245, 67],  // Bright Yellow
                    [59, 142, 234],  // Bright Blue
                    [214, 112, 214], // Bright Magenta
                    [41, 184, 219],  // Bright Cyan
                    [255, 255, 255], // Bright White
                ],
            },
            ui: UIColors {
                window_bg: [29, 29, 29],
                panel_bg: [29, 29, 29],
                border: [80, 80, 80],
                text: [220, 220, 220],
                text_disabled: [120, 120, 120],
            },
            scrollbar: ScrollbarColors {
                track_normal: [72, 72, 80, 42],
                track_hover: [84, 84, 92, 64],
                track_drag: [92, 92, 100, 88],
                thumb_normal: [146, 146, 156, 118],
                thumb_hover: [166, 166, 176, 156],
                thumb_drag: [188, 188, 196, 188],
            },
            tabbar: TabbarColors {
                bg: [40, 40, 40],
                border: [120, 120, 130],
                inactive_text: [180, 180, 190],
                active_text: [220, 220, 220],
                active_border: [100, 200, 255],
                close_btn_bg: [100, 50, 50],
                close_btn_hover: [255, 150, 150],
            },
            search: SearchColors {
                bg: [40, 40, 40],
                border: [100, 100, 100],
                text: [220, 220, 220],
            },
            palette: CommandPaletteColors {
                session_color: [100, 150, 255],
                edit_color: [100, 200, 100],
                search_color: [255, 200, 100],
                terminal_color: [150, 150, 255],
                window_color: [200, 100, 200],
            },
        }
    }

    /// 获取内置的浅色主题
    pub fn builtin_light() -> Self {
        Theme {
            name: "light".to_string(),
            terminal: TerminalColors {
                foreground: [20, 20, 20],
                background: [250, 250, 250],
                cursor: [50, 50, 50],
                selection: [200, 220, 255, 150],
                ansi_colors: [
                    [0, 0, 0],
                    [200, 0, 0],
                    [0, 180, 0],
                    [180, 140, 0],
                    [0, 80, 200],
                    [200, 0, 200],
                    [0, 170, 170],
                    [200, 200, 200],
                    [100, 100, 100],
                    [255, 0, 0],
                    [0, 255, 0],
                    [255, 200, 0],
                    [0, 100, 255],
                    [255, 0, 255],
                    [0, 200, 200],
                    [255, 255, 255],
                ],
            },
            ui: UIColors {
                window_bg: [250, 250, 250],
                panel_bg: [240, 240, 240],
                border: [150, 150, 150],
                text: [20, 20, 20],
                text_disabled: [120, 120, 120],
            },
            scrollbar: ScrollbarColors {
                track_normal: [220, 220, 220, 60],
                track_hover: [200, 200, 200, 90],
                track_drag: [180, 180, 180, 120],
                thumb_normal: [150, 150, 150, 150],
                thumb_hover: [120, 120, 120, 180],
                thumb_drag: [100, 100, 100, 200],
            },
            tabbar: TabbarColors {
                bg: [240, 240, 240],
                border: [150, 150, 150],
                inactive_text: [80, 80, 80],
                active_text: [0, 0, 0],
                active_border: [100, 150, 255],
                close_btn_bg: [200, 150, 150],
                close_btn_hover: [255, 100, 100],
            },
            search: SearchColors {
                bg: [240, 240, 240],
                border: [150, 150, 150],
                text: [20, 20, 20],
            },
            palette: CommandPaletteColors {
                session_color: [50, 100, 200],
                edit_color: [50, 150, 50],
                search_color: [200, 150, 0],
                terminal_color: [100, 100, 180],
                window_color: [150, 50, 150],
            },
        }
    }

    /// 获取内置的 Solarized Dark 主题
    pub fn builtin_solarized_dark() -> Self {
        Theme {
            name: "solarized-dark".to_string(),
            terminal: TerminalColors {
                foreground: [131, 148, 150],
                background: [0, 43, 54],
                cursor: [133, 153, 0],
                selection: [7, 54, 66, 100],
                ansi_colors: [
                    [7, 54, 66],     // Black
                    [220, 50, 47],   // Red
                    [133, 153, 0],   // Green
                    [181, 137, 0],   // Yellow
                    [38, 139, 210],  // Blue
                    [108, 113, 196], // Magenta
                    [42, 161, 152],  // Cyan
                    [131, 148, 150], // White
                    [101, 123, 131], // Bright Black
                    [203, 75, 75],   // Bright Red
                    [88, 110, 117],  // Bright Green
                    [101, 123, 131], // Bright Yellow
                    [131, 148, 150], // Bright Blue
                    [108, 113, 196], // Bright Magenta
                    [147, 161, 161], // Bright Cyan
                    [253, 246, 227], // Bright White
                ],
            },
            ui: UIColors {
                window_bg: [0, 43, 54],
                panel_bg: [7, 54, 66],
                border: [101, 123, 131],
                text: [131, 148, 150],
                text_disabled: [88, 110, 117],
            },
            scrollbar: ScrollbarColors {
                track_normal: [7, 54, 66, 100],
                track_hover: [7, 54, 66, 150],
                track_drag: [101, 123, 131, 180],
                thumb_normal: [42, 161, 152, 150],
                thumb_hover: [42, 161, 152, 180],
                thumb_drag: [38, 139, 210, 200],
            },
            tabbar: TabbarColors {
                bg: [7, 54, 66],
                border: [101, 123, 131],
                inactive_text: [88, 110, 117],
                active_text: [131, 148, 150],
                active_border: [38, 139, 210],
                close_btn_bg: [220, 50, 47],
                close_btn_hover: [220, 50, 47],
            },
            search: SearchColors {
                bg: [7, 54, 66],
                border: [101, 123, 131],
                text: [131, 148, 150],
            },
            palette: CommandPaletteColors {
                session_color: [38, 139, 210],
                edit_color: [133, 153, 0],
                search_color: [181, 137, 0],
                terminal_color: [42, 161, 152],
                window_color: [108, 113, 196],
            },
        }
    }

    /// Monokai theme
    pub fn builtin_monokai() -> Self {
        Theme {
            name: "monokai".to_string(),
            terminal: TerminalColors {
                foreground: [248, 248, 242],
                background: [39, 40, 34],
                cursor: [248, 248, 240],
                selection: [73, 72, 62, 120],
                ansi_colors: [
                    [39, 40, 34],    // Black
                    [249, 38, 114],  // Red
                    [166, 226, 46],  // Green
                    [244, 191, 117], // Yellow
                    [102, 217, 239], // Blue
                    [174, 129, 255], // Magenta
                    [161, 239, 228], // Cyan
                    [248, 248, 242], // White
                    [117, 113, 94],  // Bright Black
                    [249, 38, 114],  // Bright Red
                    [166, 226, 46],  // Bright Green
                    [253, 151, 31],  // Bright Yellow
                    [102, 217, 239], // Bright Blue
                    [174, 129, 255], // Bright Magenta
                    [161, 239, 228], // Bright Cyan
                    [249, 248, 245], // Bright White
                ],
            },
            ui: UIColors {
                window_bg: [39, 40, 34],
                panel_bg: [49, 50, 44],
                border: [117, 113, 94],
                text: [248, 248, 242],
                text_disabled: [117, 113, 94],
            },
            scrollbar: ScrollbarColors {
                track_normal: [73, 72, 62, 40],
                track_hover: [73, 72, 62, 80],
                track_drag: [73, 72, 62, 120],
                thumb_normal: [117, 113, 94, 120],
                thumb_hover: [117, 113, 94, 160],
                thumb_drag: [117, 113, 94, 200],
            },
            tabbar: TabbarColors {
                bg: [34, 35, 30],
                border: [117, 113, 94],
                inactive_text: [117, 113, 94],
                active_text: [248, 248, 242],
                active_border: [166, 226, 46],
                close_btn_bg: [100, 50, 50],
                close_btn_hover: [249, 38, 114],
            },
            search: SearchColors {
                bg: [49, 50, 44],
                border: [117, 113, 94],
                text: [248, 248, 242],
            },
            palette: CommandPaletteColors {
                session_color: [102, 217, 239],
                edit_color: [166, 226, 46],
                search_color: [253, 151, 31],
                terminal_color: [174, 129, 255],
                window_color: [249, 38, 114],
            },
        }
    }

    /// Dracula theme
    pub fn builtin_dracula() -> Self {
        Theme {
            name: "dracula".to_string(),
            terminal: TerminalColors {
                foreground: [248, 248, 242],
                background: [40, 42, 54],
                cursor: [248, 248, 242],
                selection: [68, 71, 90, 140],
                ansi_colors: [
                    [33, 34, 44],    // Black
                    [255, 85, 85],   // Red
                    [80, 250, 123],  // Green
                    [241, 250, 140], // Yellow
                    [189, 147, 249], // Blue (purple accent)
                    [255, 121, 198], // Magenta (pink)
                    [139, 233, 253], // Cyan
                    [248, 248, 242], // White
                    [98, 114, 164],  // Bright Black (comment)
                    [255, 110, 110], // Bright Red
                    [105, 255, 148], // Bright Green
                    [255, 255, 165], // Bright Yellow
                    [210, 172, 255], // Bright Blue
                    [255, 146, 213], // Bright Magenta
                    [164, 255, 255], // Bright Cyan
                    [255, 255, 255], // Bright White
                ],
            },
            ui: UIColors {
                window_bg: [40, 42, 54],
                panel_bg: [50, 52, 64],
                border: [98, 114, 164],
                text: [248, 248, 242],
                text_disabled: [98, 114, 164],
            },
            scrollbar: ScrollbarColors {
                track_normal: [68, 71, 90, 40],
                track_hover: [68, 71, 90, 80],
                track_drag: [68, 71, 90, 120],
                thumb_normal: [189, 147, 249, 100],
                thumb_hover: [189, 147, 249, 150],
                thumb_drag: [189, 147, 249, 200],
            },
            tabbar: TabbarColors {
                bg: [33, 34, 44],
                border: [98, 114, 164],
                inactive_text: [98, 114, 164],
                active_text: [248, 248, 242],
                active_border: [189, 147, 249],
                close_btn_bg: [100, 50, 60],
                close_btn_hover: [255, 85, 85],
            },
            search: SearchColors {
                bg: [50, 52, 64],
                border: [98, 114, 164],
                text: [248, 248, 242],
            },
            palette: CommandPaletteColors {
                session_color: [189, 147, 249],
                edit_color: [80, 250, 123],
                search_color: [241, 250, 140],
                terminal_color: [139, 233, 253],
                window_color: [255, 121, 198],
            },
        }
    }

    /// Nord theme
    pub fn builtin_nord() -> Self {
        Theme {
            name: "nord".to_string(),
            terminal: TerminalColors {
                foreground: [216, 222, 233],
                background: [46, 52, 64],
                cursor: [216, 222, 233],
                selection: [67, 76, 94, 140],
                ansi_colors: [
                    [59, 66, 82],    // Black (nord1)
                    [191, 97, 106],  // Red (nord11)
                    [163, 190, 140], // Green (nord14)
                    [235, 203, 139], // Yellow (nord13)
                    [129, 161, 193], // Blue (nord9)
                    [180, 142, 173], // Magenta (nord15)
                    [136, 192, 208], // Cyan (nord8)
                    [229, 233, 240], // White (nord5)
                    [76, 86, 106],   // Bright Black (nord3)
                    [191, 97, 106],  // Bright Red
                    [163, 190, 140], // Bright Green
                    [235, 203, 139], // Bright Yellow
                    [129, 161, 193], // Bright Blue
                    [180, 142, 173], // Bright Magenta
                    [143, 188, 187], // Bright Cyan (nord7)
                    [236, 239, 244], // Bright White (nord6)
                ],
            },
            ui: UIColors {
                window_bg: [46, 52, 64],
                panel_bg: [59, 66, 82],
                border: [76, 86, 106],
                text: [216, 222, 233],
                text_disabled: [76, 86, 106],
            },
            scrollbar: ScrollbarColors {
                track_normal: [67, 76, 94, 40],
                track_hover: [67, 76, 94, 80],
                track_drag: [67, 76, 94, 120],
                thumb_normal: [136, 192, 208, 100],
                thumb_hover: [136, 192, 208, 150],
                thumb_drag: [136, 192, 208, 200],
            },
            tabbar: TabbarColors {
                bg: [46, 52, 64],
                border: [76, 86, 106],
                inactive_text: [76, 86, 106],
                active_text: [216, 222, 233],
                active_border: [136, 192, 208],
                close_btn_bg: [100, 60, 60],
                close_btn_hover: [191, 97, 106],
            },
            search: SearchColors {
                bg: [59, 66, 82],
                border: [76, 86, 106],
                text: [216, 222, 233],
            },
            palette: CommandPaletteColors {
                session_color: [129, 161, 193],
                edit_color: [163, 190, 140],
                search_color: [235, 203, 139],
                terminal_color: [136, 192, 208],
                window_color: [180, 142, 173],
            },
        }
    }

    /// Gruvbox Dark theme
    pub fn builtin_gruvbox_dark() -> Self {
        Theme {
            name: "gruvbox-dark".to_string(),
            terminal: TerminalColors {
                foreground: [235, 219, 178],
                background: [40, 40, 40],
                cursor: [235, 219, 178],
                selection: [80, 73, 69, 140],
                ansi_colors: [
                    [40, 40, 40],    // Black
                    [204, 36, 29],   // Red
                    [152, 151, 26],  // Green
                    [215, 153, 33],  // Yellow
                    [69, 133, 136],  // Blue
                    [177, 98, 134],  // Magenta
                    [104, 157, 106], // Cyan (aqua)
                    [168, 153, 132], // White (fg4)
                    [146, 131, 116], // Bright Black (gray)
                    [251, 73, 52],   // Bright Red
                    [184, 187, 38],  // Bright Green
                    [250, 189, 47],  // Bright Yellow
                    [131, 165, 152], // Bright Blue
                    [211, 134, 155], // Bright Magenta
                    [142, 192, 124], // Bright Cyan
                    [235, 219, 178], // Bright White (fg)
                ],
            },
            ui: UIColors {
                window_bg: [40, 40, 40],
                panel_bg: [50, 48, 47],
                border: [102, 92, 84],
                text: [235, 219, 178],
                text_disabled: [146, 131, 116],
            },
            scrollbar: ScrollbarColors {
                track_normal: [80, 73, 69, 40],
                track_hover: [80, 73, 69, 80],
                track_drag: [80, 73, 69, 120],
                thumb_normal: [215, 153, 33, 100],
                thumb_hover: [215, 153, 33, 150],
                thumb_drag: [215, 153, 33, 200],
            },
            tabbar: TabbarColors {
                bg: [29, 32, 33],
                border: [102, 92, 84],
                inactive_text: [146, 131, 116],
                active_text: [235, 219, 178],
                active_border: [250, 189, 47],
                close_btn_bg: [100, 50, 50],
                close_btn_hover: [251, 73, 52],
            },
            search: SearchColors {
                bg: [50, 48, 47],
                border: [102, 92, 84],
                text: [235, 219, 178],
            },
            palette: CommandPaletteColors {
                session_color: [131, 165, 152],
                edit_color: [184, 187, 38],
                search_color: [250, 189, 47],
                terminal_color: [142, 192, 124],
                window_color: [211, 134, 155],
            },
        }
    }

    /// Tokyo Night theme
    pub fn builtin_tokyo_night() -> Self {
        Theme {
            name: "tokyo-night".to_string(),
            terminal: TerminalColors {
                foreground: [192, 202, 245],
                background: [26, 27, 38],
                cursor: [192, 202, 245],
                selection: [43, 48, 82, 140],
                ansi_colors: [
                    [21, 22, 30],    // Black
                    [247, 118, 142], // Red
                    [158, 206, 106], // Green
                    [224, 175, 104], // Yellow
                    [122, 162, 247], // Blue
                    [187, 154, 247], // Magenta
                    [125, 207, 255], // Cyan
                    [192, 202, 245], // White
                    [65, 72, 104],   // Bright Black (comment)
                    [247, 118, 142], // Bright Red
                    [158, 206, 106], // Bright Green
                    [224, 175, 104], // Bright Yellow
                    [122, 162, 247], // Bright Blue
                    [187, 154, 247], // Bright Magenta
                    [125, 207, 255], // Bright Cyan
                    [200, 211, 245], // Bright White
                ],
            },
            ui: UIColors {
                window_bg: [26, 27, 38],
                panel_bg: [36, 40, 59],
                border: [65, 72, 104],
                text: [192, 202, 245],
                text_disabled: [65, 72, 104],
            },
            scrollbar: ScrollbarColors {
                track_normal: [43, 48, 82, 40],
                track_hover: [43, 48, 82, 80],
                track_drag: [43, 48, 82, 120],
                thumb_normal: [122, 162, 247, 100],
                thumb_hover: [122, 162, 247, 150],
                thumb_drag: [122, 162, 247, 200],
            },
            tabbar: TabbarColors {
                bg: [21, 22, 30],
                border: [65, 72, 104],
                inactive_text: [65, 72, 104],
                active_text: [192, 202, 245],
                active_border: [122, 162, 247],
                close_btn_bg: [100, 50, 60],
                close_btn_hover: [247, 118, 142],
            },
            search: SearchColors {
                bg: [36, 40, 59],
                border: [65, 72, 104],
                text: [192, 202, 245],
            },
            palette: CommandPaletteColors {
                session_color: [122, 162, 247],
                edit_color: [158, 206, 106],
                search_color: [224, 175, 104],
                terminal_color: [125, 207, 255],
                window_color: [187, 154, 247],
            },
        }
    }

    /// One Dark theme (Atom)
    pub fn builtin_one_dark() -> Self {
        Theme {
            name: "one-dark".to_string(),
            terminal: TerminalColors {
                foreground: [171, 178, 191],
                background: [40, 44, 52],
                cursor: [171, 178, 191],
                selection: [62, 68, 81, 140],
                ansi_colors: [
                    [40, 44, 52],    // Black
                    [224, 108, 117], // Red
                    [152, 195, 121], // Green
                    [229, 192, 123], // Yellow
                    [97, 175, 239],  // Blue
                    [198, 120, 221], // Magenta
                    [86, 182, 194],  // Cyan
                    [171, 178, 191], // White
                    [92, 99, 112],   // Bright Black (comment)
                    [224, 108, 117], // Bright Red
                    [152, 195, 121], // Bright Green
                    [229, 192, 123], // Bright Yellow
                    [97, 175, 239],  // Bright Blue
                    [198, 120, 221], // Bright Magenta
                    [86, 182, 194],  // Bright Cyan
                    [200, 204, 212], // Bright White
                ],
            },
            ui: UIColors {
                window_bg: [40, 44, 52],
                panel_bg: [50, 54, 62],
                border: [92, 99, 112],
                text: [171, 178, 191],
                text_disabled: [92, 99, 112],
            },
            scrollbar: ScrollbarColors {
                track_normal: [62, 68, 81, 40],
                track_hover: [62, 68, 81, 80],
                track_drag: [62, 68, 81, 120],
                thumb_normal: [97, 175, 239, 100],
                thumb_hover: [97, 175, 239, 150],
                thumb_drag: [97, 175, 239, 200],
            },
            tabbar: TabbarColors {
                bg: [33, 37, 43],
                border: [92, 99, 112],
                inactive_text: [92, 99, 112],
                active_text: [171, 178, 191],
                active_border: [97, 175, 239],
                close_btn_bg: [100, 50, 50],
                close_btn_hover: [224, 108, 117],
            },
            search: SearchColors {
                bg: [50, 54, 62],
                border: [92, 99, 112],
                text: [171, 178, 191],
            },
            palette: CommandPaletteColors {
                session_color: [97, 175, 239],
                edit_color: [152, 195, 121],
                search_color: [229, 192, 123],
                terminal_color: [86, 182, 194],
                window_color: [198, 120, 221],
            },
        }
    }

    /// Catppuccin Mocha theme
    pub fn builtin_catppuccin_mocha() -> Self {
        Theme {
            name: "catppuccin-mocha".to_string(),
            terminal: TerminalColors {
                foreground: [205, 214, 244],
                background: [30, 30, 46],
                cursor: [245, 224, 220],
                selection: [88, 91, 112, 140],
                ansi_colors: [
                    [69, 71, 90],    // Black (surface1)
                    [243, 139, 168], // Red
                    [166, 227, 161], // Green
                    [249, 226, 175], // Yellow
                    [137, 180, 250], // Blue
                    [203, 166, 247], // Magenta (mauve)
                    [148, 226, 213], // Cyan (teal)
                    [186, 194, 222], // White (subtext1)
                    [88, 91, 112],   // Bright Black (surface2)
                    [243, 139, 168], // Bright Red
                    [166, 227, 161], // Bright Green
                    [249, 226, 175], // Bright Yellow
                    [137, 180, 250], // Bright Blue
                    [203, 166, 247], // Bright Magenta
                    [148, 226, 213], // Bright Cyan
                    [205, 214, 244], // Bright White (text)
                ],
            },
            ui: UIColors {
                window_bg: [30, 30, 46],
                panel_bg: [36, 39, 58],
                border: [88, 91, 112],
                text: [205, 214, 244],
                text_disabled: [108, 112, 134],
            },
            scrollbar: ScrollbarColors {
                track_normal: [49, 50, 68, 40],
                track_hover: [49, 50, 68, 80],
                track_drag: [49, 50, 68, 120],
                thumb_normal: [203, 166, 247, 100],
                thumb_hover: [203, 166, 247, 150],
                thumb_drag: [203, 166, 247, 200],
            },
            tabbar: TabbarColors {
                bg: [24, 24, 37],
                border: [88, 91, 112],
                inactive_text: [108, 112, 134],
                active_text: [205, 214, 244],
                active_border: [203, 166, 247],
                close_btn_bg: [100, 50, 60],
                close_btn_hover: [243, 139, 168],
            },
            search: SearchColors {
                bg: [36, 39, 58],
                border: [88, 91, 112],
                text: [205, 214, 244],
            },
            palette: CommandPaletteColors {
                session_color: [137, 180, 250],
                edit_color: [166, 227, 161],
                search_color: [249, 226, 175],
                terminal_color: [148, 226, 213],
                window_color: [203, 166, 247],
            },
        }
    }

    /// Solarized Light theme
    pub fn builtin_solarized_light() -> Self {
        Theme {
            name: "solarized-light".to_string(),
            terminal: TerminalColors {
                foreground: [101, 123, 131],
                background: [253, 246, 227],
                cursor: [101, 123, 131],
                selection: [238, 232, 213, 140],
                ansi_colors: [
                    [7, 54, 66],     // Black (base02)
                    [220, 50, 47],   // Red
                    [133, 153, 0],   // Green
                    [181, 137, 0],   // Yellow
                    [38, 139, 210],  // Blue
                    [108, 113, 196], // Magenta (violet)
                    [42, 161, 152],  // Cyan
                    [238, 232, 213], // White (base2)
                    [0, 43, 54],     // Bright Black (base03)
                    [203, 75, 22],   // Bright Red (orange)
                    [88, 110, 117],  // Bright Green (base01)
                    [101, 123, 131], // Bright Yellow (base00)
                    [131, 148, 150], // Bright Blue (base0)
                    [108, 113, 196], // Bright Magenta
                    [147, 161, 161], // Bright Cyan (base1)
                    [253, 246, 227], // Bright White (base3)
                ],
            },
            ui: UIColors {
                window_bg: [253, 246, 227],
                panel_bg: [238, 232, 213],
                border: [147, 161, 161],
                text: [101, 123, 131],
                text_disabled: [147, 161, 161],
            },
            scrollbar: ScrollbarColors {
                track_normal: [238, 232, 213, 60],
                track_hover: [238, 232, 213, 100],
                track_drag: [238, 232, 213, 140],
                thumb_normal: [147, 161, 161, 120],
                thumb_hover: [147, 161, 161, 160],
                thumb_drag: [147, 161, 161, 200],
            },
            tabbar: TabbarColors {
                bg: [238, 232, 213],
                border: [147, 161, 161],
                inactive_text: [147, 161, 161],
                active_text: [101, 123, 131],
                active_border: [38, 139, 210],
                close_btn_bg: [200, 150, 150],
                close_btn_hover: [220, 50, 47],
            },
            search: SearchColors {
                bg: [238, 232, 213],
                border: [147, 161, 161],
                text: [101, 123, 131],
            },
            palette: CommandPaletteColors {
                session_color: [38, 139, 210],
                edit_color: [133, 153, 0],
                search_color: [181, 137, 0],
                terminal_color: [42, 161, 152],
                window_color: [108, 113, 196],
            },
        }
    }

    /// 从文件加载主题
    pub fn from_file(path: &Path) -> Result<Self, Box<dyn std::error::Error>> {
        let content = std::fs::read_to_string(path)?;
        let theme: Theme = toml::from_str(&content)?;
        Ok(theme)
    }

    /// 保存主题到文件
    pub fn save(&self, path: &Path) -> Result<(), Box<dyn std::error::Error>> {
        let content = toml::to_string_pretty(self)?;
        std::fs::write(path, content)?;
        Ok(())
    }

    /// Custom themes directory
    pub fn custom_themes_dir() -> Option<std::path::PathBuf> {
        dirs::config_dir().map(|d| d.join("jterm3").join("themes"))
    }

    /// Load all custom themes from the themes directory
    pub fn load_custom_themes() -> Vec<Self> {
        let Some(dir) = Self::custom_themes_dir() else {
            return Vec::new();
        };
        let Ok(entries) = std::fs::read_dir(&dir) else {
            return Vec::new();
        };
        let mut themes = Vec::new();
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_some_and(|e| e == "toml") {
                if let Ok(theme) = Self::from_file(&path) {
                    themes.push(theme);
                }
            }
        }
        themes.sort_by(|a, b| a.name.cmp(&b.name));
        themes
    }

    /// Save this theme as a custom theme file
    pub fn save_custom_theme(&self) -> Result<(), Box<dyn std::error::Error>> {
        let dir = Self::custom_themes_dir().ok_or("Cannot determine config directory")?;
        std::fs::create_dir_all(&dir)?;
        let filename = format!("{}.toml", self.name);
        let path = dir.join(filename);
        self.save(&path)
    }

    /// Delete a custom theme file
    pub fn delete_custom_theme(name: &str) -> Result<(), Box<dyn std::error::Error>> {
        let dir = Self::custom_themes_dir().ok_or("Cannot determine config directory")?;
        let filename = format!("{}.toml", name);
        let path = dir.join(filename);
        if path.exists() {
            std::fs::remove_file(&path)?;
        }
        Ok(())
    }

    /// Get a theme by name — checks builtins first, then custom themes
    pub fn get_theme(name: &str) -> Option<Self> {
        if let Some(t) = Self::get_builtin(name) {
            return Some(t);
        }
        Self::load_custom_themes()
            .into_iter()
            .find(|t| t.name == name)
    }

    /// Check if a theme name is a builtin
    pub fn is_builtin(name: &str) -> bool {
        Self::get_builtin(name).is_some()
    }

    /// 将 RGB 数组转换为 iced::Color
    pub fn rgb_to_color32(rgb: [u8; 3]) -> Color {
        Color::from_rgb8(rgb[0], rgb[1], rgb[2])
    }

    /// 将 RGBA 数组转换为 iced::Color
    #[allow(dead_code)]
    pub fn rgba_to_color32(rgba: [u8; 4]) -> Color {
        Color::from_rgba8(rgba[0], rgba[1], rgba[2], rgba[3] as f32 / 255.0)
    }

    /// 获取终端前景色
    pub fn terminal_foreground(&self) -> Color {
        Self::rgb_to_color32(self.terminal.foreground)
    }

    /// 获取终端背景色
    pub fn terminal_background(&self) -> Color {
        Self::rgb_to_color32(self.terminal.background)
    }

    /// 获取光标颜色
    pub fn cursor_color(&self) -> Color {
        Self::rgb_to_color32(self.terminal.cursor)
    }

    /// 获取选择背景色 - 基于前景色计算，确保与任意主题的高对比度
    pub fn selection_color(&self) -> Color {
        let fg = self.terminal.foreground;
        Color::from_rgba8(fg[0], fg[1], fg[2], 90.0 / 255.0)
    }

    /// 获取选中文本的前景色 - 使用背景色确保与选择背景的对比度
    pub fn selection_fg_color(&self) -> Color {
        Self::rgb_to_color32(self.terminal.background)
    }

    /// 搜索匹配项高亮色（半透明黄色叠加）
    pub fn search_match_color(&self) -> Color {
        Color::from_rgba(0.85, 0.75, 0.20, 0.40)
    }

    /// 当前搜索匹配项高亮色（更强的橙色叠加）
    pub fn search_current_color(&self) -> Color {
        Color::from_rgba(1.0, 0.55, 0.10, 0.70)
    }

    /// 获取 ANSI 颜色
    pub fn ansi_color(&self, index: usize) -> Color {
        if index < 16 {
            Self::rgb_to_color32(self.terminal.ansi_colors[index])
        } else {
            Self::rgb_to_color32(self.terminal.foreground)
        }
    }

    /// 获取可用的主题列表
    pub fn available_themes() -> Vec<&'static str> {
        vec![
            "dark",
            "light",
            "solarized-dark",
            "solarized-light",
            "monokai",
            "dracula",
            "nord",
            "gruvbox-dark",
            "tokyo-night",
            "one-dark",
            "catppuccin-mocha",
        ]
    }

    /// 根据名称获取内置主题
    pub fn get_builtin(name: &str) -> Option<Self> {
        match name {
            "dark" => Some(Self::builtin_dark()),
            "light" => Some(Self::builtin_light()),
            "solarized-dark" => Some(Self::builtin_solarized_dark()),
            "solarized-light" => Some(Self::builtin_solarized_light()),
            "monokai" => Some(Self::builtin_monokai()),
            "dracula" => Some(Self::builtin_dracula()),
            "nord" => Some(Self::builtin_nord()),
            "gruvbox-dark" => Some(Self::builtin_gruvbox_dark()),
            "tokyo-night" => Some(Self::builtin_tokyo_night()),
            "one-dark" => Some(Self::builtin_one_dark()),
            "catppuccin-mocha" => Some(Self::builtin_catppuccin_mocha()),
            _ => None,
        }
    }
}

impl Default for Theme {
    fn default() -> Self {
        Self::builtin_dark()
    }
}
