# jterm3

jterm3 是一个面向 Linux 的现代终端模拟器，使用 Rust、iced 和 wgpu 构建。它把多标签、分屏、完整回滚搜索、会话恢复和 GPU 渲染放进一个轻量桌面应用，同时默认收紧远程终端可触达的宿主能力。

## 主要能力

- 多标签、拖动排序、快速标签切换，以及 tmux 风格的树状分屏（任意 pane 可再沿任一方向嵌套拆分）
- 搜索当前屏幕与全部 scrollback，支持大小写匹配、正则和自动滚动定位
- UTF-8、中文宽字符、True Color、256 色、鼠标报告、括号粘贴和扩展键盘协议
- Kitty 图像直接传输（PNG、RGB、RGBA），带传输、像素、解压内存和放置数量上限
- 文件侧栏、路径插入、链接识别、命令面板、主题编辑和实时设置
- 自动保存标签工作目录并恢复会话；多实例之间不会互相覆盖恢复数据
- OSC 10/11/12 动态颜色、OSC 52/5522 剪贴板和桌面通知
- 有界 PTY 输入/输出队列、稳定会话身份校验和繁忙进程关闭保护

## 构建与运行

目前支持 Linux。需要稳定版 Rust、Fontconfig，以及 Wayland/X11 和 OpenGL/EGL 的开发库。Ubuntu/Debian 可安装：

```bash
sudo apt-get install pkg-config libfontconfig1-dev libwayland-dev \
  libx11-dev libx11-xcb-dev libxcb1-dev libxcb-render0-dev \
  libxcb-shape0-dev libxcb-xfixes0-dev libxcursor-dev libxi-dev \
  libxrandr-dev libxkbcommon-dev libegl1-mesa-dev libgl1-mesa-dev
```

然后构建：

```bash
cargo build --release --locked
./target/release/jterm3
```

如需安装到当前用户：

```bash
install -Dm755 target/release/jterm3 "$HOME/.local/bin/jterm3"
```

默认字体会优先使用 SauceCodePro Nerd Font；未安装时 iced/Fontconfig 会回退到系统字体。可以在设置面板中选择任意已安装的等宽字体。

## 常用快捷键

| 操作 | 快捷键 |
| --- | --- |
| 新建标签 | `Ctrl+Shift+T` |
| 复制 / 粘贴 | `Ctrl+Shift+C` / `Ctrl+Shift+V` |
| 搜索全部回滚 | `Ctrl+Shift+F` |
| 命令面板 | `Ctrl+Shift+P` |
| 快速切换标签 | `Ctrl+Shift+L` |
| 标签 1–8 / 最后一个 | `Ctrl+1`…`Ctrl+8` / `Ctrl+9` |
| 左右 / 上下分屏 | `Ctrl+Shift+E` / `Ctrl+Shift+D`（拆分聚焦 pane；同向并入同级，异向嵌套子分屏，最多 12 个 pane） |
| 方向聚焦 Pane | `Ctrl+Alt+方向键`（按几何位置跨嵌套跳转，边缘不回绕） |
| 调整 Pane 大小 | `Ctrl+Alt+Shift+方向键`（双击分割线均分该节点） |
| Pane 缩放（临时全屏） | `Ctrl+Shift+Z` |
| 交换相邻 Pane | `Ctrl+Shift+X` |
| 关闭聚焦 Pane | `Ctrl+Shift+W`（其余 pane 保持分屏） |
| 关闭当前标签或 pane | `Ctrl+Shift+W` |
| 文件/标签侧栏 | `Ctrl+\` |
| 设置 | `Ctrl+Shift+O` |
| 放大 / 缩小 / 重置字体 | `Ctrl+=` / `Ctrl+-` / `Ctrl+0` |

快捷键从 `$XDG_CONFIG_HOME/jterm3/keybindings.toml`（通常是 `~/.config/jterm3/keybindings.toml`）加载，并与默认绑定合并。

## 配置

主配置位于 `$XDG_CONFIG_HOME/jterm3/config.toml`。设置面板中的修改会自动保存，外部编辑也会热重载。示例：

```toml
font_family = "JetBrains Mono Nerd Font"
font_size = 14.0
line_spacing = 1.0
padding = 2.0
scrollback_lines = 20000
scroll_speed = 3
theme = "tokyo-night"
tab_position = "top"
restore_session = true
disable_alt_screen = false

# 可选：明确指定 shell
shell = "/bin/bash"

# 安全默认值。开启后，SSH 中的程序也能读取宿主剪贴板。
allow_clipboard_read = false
```

内置主题包括 Dark、Light、Monokai、Dracula、Nord、Gruvbox Dark、Tokyo Night、One Dark、Catppuccin Mocha 和 Solarized Light。自定义主题保存在 `~/.config/jterm3/themes/`。

## 安全说明

终端控制序列来自本地或远程程序，不能天然视为可信输入。jterm3 默认拒绝 OSC 52/5522 读取宿主剪贴板；如果显式开启 `allow_clipboard_read`，通过 SSH 运行的程序也可能获得剪贴板内容。剪贴板写入仍按主流终端兼容行为允许。Kitty 图像和通知均有资源或频率限制。

## 开发验证

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --all-features --locked -- -D warnings
cargo test --all-targets --all-features --locked
cargo build --release --all-features --locked
```

CI 对格式、零警告 Clippy、全量测试和 release 构建分别设有独立质量门槛。
