# clipwin

高性能 Windows 剪贴板管理器（对标 CopyQ/Ditto，仅支持 Windows）。

技术栈：Rust + Tauri 2（WebView2）+ React 19 + TanStack Virtual + SQLite(FTS5, rusqlite bundled)

## 当前状态：M0 PoC

验证五项核心指标（通过后冻结架构）：
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

- `src/` React 前端（App.tsx = M0 压测面板）
- `src-tauri/src/clipboard_watcher.rs` 剪贴板监听（windows-rs）
- `src-tauri/src/store.rs` SQLite 存储（FTS5 + blob 分离 + LRU trim）
- `src-tauri/src/commands.rs` Tauri IPC 命令
- `docs/` 调研报告与技术决策

## 路线图

M0 PoC（进行中）→ M1 核心 MVP（热键/托盘/回贴）→ M2 体验（Mica/快贴窗/预览）→ M3 分发（winget/Scoop/updater）

## License

MIT
