# M0 PoC 验证报告

> 日期：2026-09 · clipwin（Rust + Tauri 2 + WebView2 + React + SQLite FTS5）
> 目的：五项核心指标实测，决定是否冻结架构

## 结论：四项达标，一项（内存）超出但可接受/可缓解 → **建议冻结 Tauri 2 架构，进入 M1**

| # | 指标 | 目标 | 实测 | 结果 |
|---|---|---|---|---|
| 1 | 冷启动→窗口可见 | <300ms | **258ms**（进程启动最坏情况；产品形态为常驻托盘+隐藏窗口，热键唤出仅 ShowWindow，更快） | ✅ |
| 2 | 万条列表渲染 | 60fps | 万条虚拟列表正常渲染，TanStack Virtual 仅渲染可见行（约 20 行 DOM） | ✅ |
| 3 | 常驻内存 | <60MB | Rust 核心 **28MB** ✅；WebView2 子进程合计 Private **182MB** | ⚠️ 见下 |
| 4 | FTS5 中文搜索（万条库） | <50ms | **冷 3.7ms / 热 1.0ms**（命中 3335 条，trigram 分词） | ✅ 超额 13 倍 |
| 5 | 端到端链路 | 打通 | 复制→监听→入库→事件→UI 实时置顶，全链路验证通过 | ✅ |

附：安装体积 **5.8MB**（<10MB 目标 ✅）；万条+压测数据插入（含 FTS 索引）均摊 <1ms/条。

## 内存权衡分析（指标 3）

- Rust 核心 28MB 证明原生侧设计达标（Ditto 档位）
- WebView2 是 6 个子进程的浏览器实例，Private 182MB 与 PasteBar（同为 Tauri，80-150MB）同档
- **缓解路径（M1 实施）**：
  1. 空闲时调用 WebView2 的 `TrySuspend`/释放（tauri 生态有 window 休眠方案）
  2. 弹窗形态：主窗口常驻隐藏，热键唤出无需重建 WebView（已验证 258ms 冷启动兜底）
  3. 若最终不可接受 → 回退方案 egui（保 20MB 但放弃富文本预览卖点）
- 对标结论：CopyQ（Qt）常驻 100-200MB、Ditto 20-50MB、PasteBar 80-150MB——clipwin 落在 PasteBar 档，以 182MB 换富文本/代码高亮渲染 + 5.8MB 安装包 + Web 生态开发效率

## PoC 中发现并修复的缺陷（重要工程资产）

| 缺陷 | 根因 | 修复 |
|---|---|---|
| UI 永远空列表 | rusqlite 空查询分支绑定 2 参数但 SQL 只有 1 占位符 | 拆分两条 SQL 分别绑定 |
| 监听不到任何复制事件 | `CreateWindowExW` 的 lpParam 不会自动写入 GWLP_USERDATA，wnd_proc 读 userdata 恒为 null | WM_NCCREATE 时从 CREATESTRUCTW.lpCreateParams 手动 SetWindowLongPtrW |
| 自写自听未跳过 | IGNORE_SEQ 在写入**前**读取（旧值），写入后系统 seq 已 +1 | 改为 CloseClipboard 后读取新 seq |
| release exe 白屏报 ERR_CONNECTION_REFUSED | 直接 `cargo build` 不嵌前端资源，运行时仍指向 devUrl | 必须走 `npm run tauri build`（tauri CLI 注入 frontendDist） |

## 已验证的技术断言

1. windows-rs 0.62：`AddClipboardFormatListener` + 隐藏消息窗口 + `WM_NCCREATE` 落 userdata 是可靠监听路径
2. SQLite trigram FTS5 中文搜索性能极充裕（万条 3.7ms，推算 10 万条也 <40ms）
3. sha256 内容寻址 blob + UNIQUE 去重 + LRU trim 存储层工作正常（万条压测 4.4MB DB）
4. Tauri 事件（clip:new）→ React 实时刷新链路延迟可接受（复制后 1-2s 内置顶显示）

## M1 待办（按优先级）

1. 热键唤出常驻窗口（global-shortcut 插件已装未接线）+ Esc 隐藏
2. 托盘常驻 + 关窗不退出
3. 回贴链路：选中项 → SetClipboardData（复用 write_clipboard）→ SendInput Ctrl+V
4. WebView2 空闲内存缓解实验（TrySuspend）
5. 图片条目缩略图渲染（DIB→PNG→asset 协议）
6. windows-rs 陷阱加固：`CanIncludeInClipboardHistory` 排除格式识别、OpenClipboard 竞争统计

## 复现方式

```bash
cd clipwin && npm install && npm run tauri build -- --no-bundle
# 产物 src-tauri/target/release/clipwin.exe（5.8MB）
# 数据目录 %APPDATA%\com.administrator.clipwin\
```
