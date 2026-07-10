/// 命令面板模块：可模糊搜索的动作列表（Ctrl+Shift+P 打开）。
use fuzzy_matcher::skim::SkimMatcherV2;
use fuzzy_matcher::FuzzyMatcher;

/// 面板可分发的动作，每一项都 1:1 对应一个已有的 jterm3 操作。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PaletteAction {
    NewTab,
    CloseTab,
    NextTab,
    PrevTab,
    Copy,
    Paste,
    OpenSearch,
    SplitVertical,
    SplitHorizontal,
    FocusNextPane,
    ClosePane,
    ToggleSidebar,
    OpenSettings,
    QuickTabSwitch,
    OpenHelp,
    ZoomIn,
    ZoomOut,
    ZoomReset,
    ScrollToTop,
    ScrollToBottom,
    ClearScreen,
}

/// 面板中的一条命令项（展示信息 + 关联动作）。
#[derive(Clone, Copy, Debug)]
pub struct PaletteItem {
    pub name: &'static str,
    pub description: &'static str,
    /// 静态快捷键提示（jterm3 键位硬编码于 handle_tab_shortcut，无需注册表）。
    pub shortcut: &'static str,
    pub action: PaletteAction,
}

/// 命令面板状态。
pub struct PaletteState {
    pub is_open: bool,
    pub query: String,
    /// 当前过滤结果中的高亮位置。
    pub selected: usize,
    all: Vec<PaletteItem>,
    matcher: SkimMatcherV2,
    /// Most-recent-first list of actions the user has executed. Drives the
    /// empty-query order so frequent commands surface first. Capped to 16.
    mru: Vec<PaletteAction>,
}

impl Default for PaletteState {
    fn default() -> Self {
        Self::new()
    }
}

impl PaletteState {
    pub fn new() -> Self {
        let all = vec![
            PaletteItem {
                name: "New Tab",
                description: "Open a new terminal tab",
                shortcut: "Ctrl+Shift+T",
                action: PaletteAction::NewTab,
            },
            PaletteItem {
                name: "Close Tab",
                description: "Close the current tab",
                shortcut: "Ctrl+Shift+W",
                action: PaletteAction::CloseTab,
            },
            PaletteItem {
                name: "Next Tab",
                description: "Switch to the next tab",
                shortcut: "",
                action: PaletteAction::NextTab,
            },
            PaletteItem {
                name: "Previous Tab",
                description: "Switch to the previous tab",
                shortcut: "",
                action: PaletteAction::PrevTab,
            },
            PaletteItem {
                name: "Copy",
                description: "Copy selected text to the clipboard",
                shortcut: "Ctrl+Shift+C",
                action: PaletteAction::Copy,
            },
            PaletteItem {
                name: "Paste",
                description: "Paste from the clipboard",
                shortcut: "Ctrl+Shift+V",
                action: PaletteAction::Paste,
            },
            PaletteItem {
                name: "Find",
                description: "Open the search overlay",
                shortcut: "Ctrl+Shift+F",
                action: PaletteAction::OpenSearch,
            },
            PaletteItem {
                name: "Split Right",
                description: "Split the terminal into left and right panes",
                shortcut: "Ctrl+Shift+D",
                action: PaletteAction::SplitVertical,
            },
            PaletteItem {
                name: "Split Down",
                description: "Split the terminal into top and bottom panes",
                shortcut: "Ctrl+Shift+E",
                action: PaletteAction::SplitHorizontal,
            },
            PaletteItem {
                name: "Focus Next Pane",
                description: "Move keyboard focus to the other pane",
                shortcut: "Ctrl+Shift+J",
                action: PaletteAction::FocusNextPane,
            },
            PaletteItem {
                name: "Close Focused Pane",
                description: "Close the current pane, or its tab when unsplit",
                shortcut: "Ctrl+Shift+W",
                action: PaletteAction::ClosePane,
            },
            PaletteItem {
                name: "Toggle Sidebar",
                description: "Show or hide the tabs and files sidebar",
                shortcut: "Ctrl+Shift+B",
                action: PaletteAction::ToggleSidebar,
            },
            PaletteItem {
                name: "Settings",
                description: "Open terminal appearance and behavior settings",
                shortcut: "Ctrl+Shift+O",
                action: PaletteAction::OpenSettings,
            },
            PaletteItem {
                name: "Switch Tab",
                description: "Fuzzy-find and switch to an open tab",
                shortcut: "Ctrl+Shift+K",
                action: PaletteAction::QuickTabSwitch,
            },
            PaletteItem {
                name: "Keyboard Shortcuts",
                description: "Show the built-in shortcut reference",
                shortcut: "Ctrl+Shift+/",
                action: PaletteAction::OpenHelp,
            },
            PaletteItem {
                name: "Zoom In",
                description: "Increase terminal font size",
                shortcut: "Ctrl++",
                action: PaletteAction::ZoomIn,
            },
            PaletteItem {
                name: "Zoom Out",
                description: "Decrease terminal font size",
                shortcut: "Ctrl+-",
                action: PaletteAction::ZoomOut,
            },
            PaletteItem {
                name: "Reset Zoom",
                description: "Restore the configured terminal font size",
                shortcut: "Ctrl+0",
                action: PaletteAction::ZoomReset,
            },
            PaletteItem {
                name: "Scroll to Top",
                description: "Jump to the top of the scrollback",
                shortcut: "Shift+Home",
                action: PaletteAction::ScrollToTop,
            },
            PaletteItem {
                name: "Scroll to Bottom",
                description: "Jump to the live view",
                shortcut: "Shift+End",
                action: PaletteAction::ScrollToBottom,
            },
            PaletteItem {
                name: "Clear Screen",
                description: "Clear the terminal screen",
                shortcut: "",
                action: PaletteAction::ClearScreen,
            },
        ];
        Self {
            is_open: false,
            query: String::new(),
            selected: 0,
            all,
            matcher: SkimMatcherV2::default(),
            mru: Vec::new(),
        }
    }

    /// Record an action as just-used so it sorts to the top of the empty-query
    /// list next time the palette is opened. Caps at 16 entries; duplicate
    /// inserts are deduplicated to the front.
    pub fn record_use(&mut self, action: PaletteAction) {
        self.mru.retain(|a| *a != action);
        self.mru.insert(0, action);
        const MRU_CAP: usize = 16;
        if self.mru.len() > MRU_CAP {
            self.mru.truncate(MRU_CAP);
        }
    }

    pub fn open(&mut self) {
        self.is_open = true;
        self.query.clear();
        self.selected = 0;
    }

    pub fn close(&mut self) {
        self.is_open = false;
    }

    pub fn toggle(&mut self) {
        if self.is_open {
            self.close();
        } else {
            self.open();
        }
    }

    /// 当前过滤结果，元素为 `(all 中的索引, 命令项)`。空查询时按 MRU 排序(最近使用
    /// 优先,其余按声明顺序);否则按模糊匹配分数降序排列,丢弃不匹配项。
    pub fn filtered(&self) -> Vec<(usize, &PaletteItem)> {
        if self.query.is_empty() {
            // MRU first (preserving recency order), then everything else in
            // declaration order so the list is stable and complete.
            let mut out: Vec<(usize, &PaletteItem)> = Vec::with_capacity(self.all.len());
            let mut seen = vec![false; self.all.len()];
            for a in &self.mru {
                if let Some((i, item)) = self.all.iter().enumerate().find(|(_, it)| it.action == *a)
                {
                    if !seen[i] {
                        seen[i] = true;
                        out.push((i, item));
                    }
                }
            }
            for (i, item) in self.all.iter().enumerate() {
                if !seen[i] {
                    out.push((i, item));
                }
            }
            return out;
        }
        let mut scored: Vec<(i64, usize, &PaletteItem)> = self
            .all
            .iter()
            .enumerate()
            .filter_map(|(i, item)| {
                let haystack = format!("{} {}", item.name, item.description);
                self.matcher
                    .fuzzy_match(&haystack, &self.query)
                    .map(|score| (score, i, item))
            })
            .collect();
        scored.sort_by(|a, b| b.0.cmp(&a.0));
        scored.into_iter().map(|(_, i, item)| (i, item)).collect()
    }

    /// 高亮项下移（在过滤结果中循环）。
    pub fn select_next(&mut self) {
        let len = self.filtered().len();
        if len == 0 {
            self.selected = 0;
        } else {
            self.selected = (self.selected + 1) % len;
        }
    }

    /// 高亮项上移（在过滤结果中循环）。
    pub fn select_prev(&mut self) {
        let len = self.filtered().len();
        if len == 0 {
            self.selected = 0;
        } else {
            self.selected = if self.selected == 0 {
                len - 1
            } else {
                self.selected - 1
            };
        }
    }

    /// 当前高亮项的动作（按过滤结果中的位置）。
    pub fn selected_action(&self) -> Option<PaletteAction> {
        self.filtered()
            .get(self.selected)
            .map(|(_, item)| item.action)
    }

    /// 按 `all` 中的索引取动作（用于鼠标点击分发）。
    pub fn action_at(&self, index: usize) -> Option<PaletteAction> {
        self.all.get(index).map(|item| item.action)
    }
}
