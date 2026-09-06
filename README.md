# clipwin

高性能 Windows 剪贴板管理器（对标 CopyQ/Ditto，仅支持 Windows）。

技术栈：Rust + Tauri 2（WebView2）+ React 19 + TanStack Virtual + SQLite(FTS5, rusqlite bundled)

## 安装

从 [Releases](https://github.com/xuxinle/clipwin/releases/latest) 下载：

| 文件 | 说明 |
|---|---|
| `*-setup.exe` | **推荐** — NSIS 安装器（~2.2 MB） |
| `*.msi` | MSI 企业部署 |
| `sha256.txt` | 校验和 |

- 系统：Windows 10 1809+ / Windows 11（x64），依赖 WebView2（Win10 安装器自动引导）
- 快捷键：`Alt+V` 剪贴板 · `Alt+T` 待办

## 当前状态：v0.1.0 已发布

M0 PoC 五项核心指标全部通过（热键 < 300ms / 万条 60fps / 常驻内存达标 / 大图不卡监听 / FTS5 中文搜索 < 50ms），MVP 功能完整：
1. 热键唤出延迟 < 300ms
2. 万条列表滚动 60fps
3. 常驻内存 < 60MB（WebView2 内存风险实测）
4. 大图片抓取不卡监听线程（后台拷贝）
5. FTS5 中文搜索万条 < 50ms

## 架构（M0）

```
WebView2/React ──IPC── Rust
  虚拟列表              ├─ clipboard_watcher：AddClipboardFormatListener
  搜索框                │   隐藏消息窗口 + 快照拷贝 + mpsc
  压测面板              ├─ store：SQLite WAL + FTS5(trigram) + blob 分离
                       └─ commands：search/seed/write_clipboard/stats
```

## 开发

```bash
npm install
npm run tauri dev     # 开发
npm run tauri build   # 构建（NSIS/MSI/portable 由 Tauri bundler 产出）
```

## 目录

- `src/` React 前端（App.tsx 主面板 / TodoEditor 富文本编辑 / CalendarPopover 日历）
- `src-tauri/src/clipboard_watcher.rs` 剪贴板监听（windows-rs）
- `src-tauri/src/store.rs` SQLite 存储（FTS5 + blob 分离 + LRU trim）
- `src-tauri/src/commands.rs` Tauri IPC 命令
- `docs/` 调研报告与技术决策

## 路线图

M0 PoC ✅ → M1 热键/托盘/回贴 ✅ → M2 待办管理/富文本/Mica ✅ → M3 分发（NSIS/MSI + Actions 自动发版 ✅，winget/Scoop 待做）

## License

MIT
