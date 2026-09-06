//! Tauri IPC 命令层（M0 PoC）

use std::sync::Mutex;

use tauri::{AppHandle, Emitter, Manager, State};
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData,
};
use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};
use windows::Win32::System::Ole::CF_UNICODETEXT;

use crate::clipboard_watcher::{self, ClipEvent, IGNORE_SEQ};
use crate::store::{self, ClipRow, Store, TodoRow};

pub struct AppState {
    pub store: Mutex<Store>,
}

/// 查询列表（query 为空 = 最新列表）
#[tauri::command]
pub fn search(state: State<AppState>, query: String, limit: Option<i64>) -> Result<Vec<ClipRow>, String> {
    state
        .store
        .lock()
        .map_err(|e| e.to_string())?
        .search(&query, limit.unwrap_or(500))
        .map_err(|e| e.to_string())
}

/// 取单条
#[tauri::command]
pub fn get_clip(state: State<AppState>, id: i64) -> Result<Option<ClipRow>, String> {
    state.store.lock().map_err(|e| e.to_string())?.get(id).map_err(|e| e.to_string())
}

/// 生成一条真实剪贴板写入（自写自听路径验证）
#[tauri::command]
pub fn write_clipboard(_app: AppHandle, _state: State<AppState>, text: String) -> Result<(), String> {
    unsafe {
        if OpenClipboard(None).is_err() {
            return Err("OpenClipboard failed".into());
        }
        let _ = EmptyClipboard();
        let bytes: Vec<u16> = text.encode_utf16().chain(std::iter::once(0u16)).collect();
        let byte_len = bytes.len() * 2;
        let hg = GlobalAlloc(GMEM_MOVEABLE, byte_len).map_err(|e| e.to_string())?;
        let ptr = GlobalLock(hg);
        if ptr.is_null() {
            let _ = CloseClipboard();
            return Err("GlobalLock failed".into());
        }
        std::ptr::copy_nonoverlapping(bytes.as_ptr() as *const u8, ptr as *mut u8, byte_len);
        let _ = GlobalUnlock(hg);
        // SetClipboardData 成功后所有权转移，此处不需要 GlobalFree
        if SetClipboardData(CF_UNICODETEXT.0 as u32, Some(windows::Win32::Foundation::HANDLE(hg.0))).is_err() {
            let _ = CloseClipboard();
            return Err("SetClipboardData failed".into());
        }
        let _ = CloseClipboard();
        // 关键：写入完成后读新序列号存入 IGNORE_SEQ；
        // 监听线程收到 WM_CLIPBOARDUPDATE 时序列号已更新，两者相等则跳过
        let seq = windows::Win32::System::DataExchange::GetClipboardSequenceNumber();
        IGNORE_SEQ.store(seq, std::sync::atomic::Ordering::SeqCst);
    }
    Ok(())
}

/// 数据库统计
#[tauri::command]
pub fn stats(state: State<AppState>) -> Result<serde_json::Value, String> {
    let store = state.store.lock().map_err(|e| e.to_string())?;
    let count = store.count().map_err(|e| e.to_string())?;
    Ok(serde_json::json!({ "count": count }))
}

/// 回贴：选中项写剪贴板并粘贴到唤出前窗口
#[tauri::command]
pub fn paste_back(app: AppHandle, state: State<AppState>, id: i64) -> Result<(), String> {
    let row = state
        .store
        .lock()
        .map_err(|e| e.to_string())?
        .get(id)
        .map_err(|e| e.to_string())?;
    let row = row.ok_or("记录不存在")?;
    // 回贴即置顶：更新 ts，下次唤出该条在最上面（CopyQ/Ditto 行为）
    state.store.lock().map_err(|e| e.to_string())?.touch(id).map_err(|e| e.to_string())?;
    match row.kind.as_str() {
        "text" => {
            let text = row.text.clone().ok_or("文本条目无内容")?;
            // 富文本条目：双格式写入（CF_UNICODETEXT + HTML Format），目标应用自选
            if let Some(hp) = &row.html_path {
                let html = std::fs::read_to_string(hp).ok();
                crate::hotkey::paste_rich_to_last_fg(&app, &text, html.as_deref())
            } else {
                crate::hotkey::paste_to_last_fg(&app, &text)
            }
        }
        "image" => crate::hotkey::paste_image_to_last_fg(&app, &row.image_path.unwrap_or_default()),
        "files" => {
            let files: Vec<String> = serde_json::from_str(&row.files_json.clone().unwrap_or_default()).unwrap_or_default();
            crate::hotkey::paste_to_last_fg(&app, &files.join("\r\n"))
        }
        _ => Err("未知条目类型".into()),
    }
}

// ---------------- 待办命令（M2）：与剪贴板联动 ----------------

#[tauri::command]
pub fn todos_add(state: State<AppState>, input: store::TodoInput) -> Result<TodoRow, String> {
    let mut s = state.store.lock().map_err(|e| e.to_string())?;
    let id = s.todos_add(&input).map_err(|e| e.to_string())?;
    s.todos_get(id).map_err(|e| e.to_string())?.ok_or_else(|| "插入后读取失败".into())
}

#[tauri::command]
pub fn todos_list(
    state: State<AppState>,
    filter: Option<String>,
    tag: Option<String>,
    include_done: Option<bool>,
) -> Result<Vec<TodoRow>, String> {
    state
        .store
        .lock()
        .map_err(|e| e.to_string())?
        .todos_list(
            filter.as_deref().unwrap_or("open"),
            tag.as_deref().unwrap_or(""),
            include_done.unwrap_or(false),
        )
        .map_err(|e| e.to_string())
}

/// 待办统计（chips 计数）
#[tauri::command]
pub fn todos_stats(state: State<AppState>) -> Result<serde_json::Value, String> {
    state.store.lock().map_err(|e| e.to_string())?.todos_stats().map_err(|e| e.to_string())
}

#[tauri::command]
pub fn todos_toggle(state: State<AppState>, id: i64) -> Result<bool, String> {
    state.store.lock().map_err(|e| e.to_string())?.todos_toggle(id).map_err(|e| e.to_string())
}

/// 编辑弹窗保存：标题 + 富文本内容（HTML 片段）+ 纯文本（可选）
#[tauri::command]
pub fn todos_update_full(
    state: State<AppState>, id: i64, title: String, content_html: Option<String>, text: Option<String>,
) -> Result<(), String> {
    let title = if title.trim().is_empty() { "待办".into() } else { title.trim().to_string() };
    state.store.lock().map_err(|e| e.to_string())?
        .todos_update_full(id, &title, content_html.as_deref(), text.as_deref())
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn todos_set_priority(state: State<AppState>, id: i64, priority: i64) -> Result<(), String> {
    state.store.lock().map_err(|e| e.to_string())?.todos_set_priority(id, priority).map_err(|e| e.to_string())
}

/// due_ms: None = 清除截止日
#[tauri::command]
#[allow(non_snake_case)]
pub fn todos_set_due(state: State<AppState>, id: i64, due_ms: Option<i64>) -> Result<(), String> {
    state.store.lock().map_err(|e| e.to_string())?.todos_set_due(id, due_ms).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn todos_toggle_pin(state: State<AppState>, id: i64) -> Result<bool, String> {
    state.store.lock().map_err(|e| e.to_string())?.todos_toggle_pin(id).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn todos_set_tags(state: State<AppState>, id: i64, tags: String) -> Result<(), String> {
    // 逗号分隔 → JSON 数组文本（去 # 前缀、去空白）
    let arr: Vec<String> = tags
        .split(',')
        .map(|s| s.trim().trim_start_matches('#').to_string())
        .filter(|s| !s.is_empty())
        .collect();
    state
        .store
        .lock()
        .map_err(|e| e.to_string())?
        .todos_set_tags(id, &serde_json::to_string(&arr).unwrap())
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn todos_delete(state: State<AppState>, id: i64) -> Result<(), String> {
    state.store.lock().map_err(|e| e.to_string())?.todos_delete(id).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn todos_get(state: State<AppState>, id: i64) -> Result<Option<TodoRow>, String> {
    state.store.lock().map_err(|e| e.to_string())?.todos_get(id).map_err(|e| e.to_string())
}

/// 剪贴板条目 → 待办（右键菜单）：直接引用 blob 文件（不拷贝，剪贴板 LRU 清理时引用计数保护）
#[tauri::command]
pub fn clip_to_todo(state: State<AppState>, id: i64) -> Result<TodoRow, String> {
    let mut s = state.store.lock().map_err(|e| e.to_string())?;
    let row = s.get(id).map_err(|e| e.to_string())?.ok_or("记录不存在")?;
    let input = store::TodoInput {
        kind: row.kind.clone(),
        title: None, // 默认标题：todos_add 内取 text 前 50 字
        content: row.html_path.as_ref().and_then(|p| std::fs::read_to_string(p).ok()),
        text: row.text.clone(),
        image_path: row.image_path.clone(),
        files_json: row.files_json.clone(),
        size: row.size,
        priority: None,
        due_at: None,
        tags: None,
    };
    let tid = s.todos_add(&input).map_err(|e| e.to_string())?;
    s.todos_get(tid).map_err(|e| e.to_string())?.ok_or_else(|| "插入后读取失败".into())
}

/// 待办视图 Ctrl+V：读当前系统剪贴板（文本/图片/文件任一格式）存为待办
/// 复用监听模块的锁内拷贝逻辑；图片直接存 DIB blob（与剪贴板条目同规格）
#[tauri::command]
pub fn todo_paste_from_clipboard(state: State<AppState>) -> Result<TodoRow, String> {
    let ev = clipboard_watcher::capture_now().ok_or("剪贴板为空或不可读")?;
    let input = match (&ev.text, &ev.dib, &ev.files) {
        (Some(t), _, _) if !t.trim().is_empty() => store::TodoInput {
            kind: "text".into(), title: None, content: None, text: Some(t.clone()), image_path: None, files_json: None,
            size: t.len() as i64, priority: None, due_at: None, tags: None,
        },
        (_, Some(dib), _) => {
            let hash = {
                use sha2::Digest;
                let mut h = sha2::Sha256::new();
                sha2::Digest::update(&mut h, dib);
                format!("{:x}", h.finalize())
            };            let dir = state_blob_dir(&state)?;
            let path = dir.join(format!("{hash}.dib"));
            if !path.exists() {
                std::fs::write(&path, dib).map_err(|e| e.to_string())?;
            }
            // PNG 渲染副本（前端 <img> 用）
            let png = dir.join(format!("{hash}.png"));
            if !png.exists() {
                if let Some(img) = crate::store::dib_to_png(dib) {
                    std::fs::write(&png, img).ok();
                }
            }
            store::TodoInput { kind: "image".into(), title: None, content: None, text: None, image_path: Some(path.to_string_lossy().into_owned()), files_json: None, size: dib.len() as i64, priority: None, due_at: None, tags: None }
        }
        (_, _, Some(files)) => store::TodoInput {
            kind: "files".into(), title: None, content: None, text: None, image_path: None,
            files_json: Some(serde_json::to_string(files).unwrap_or_default()),
            size: files.len() as i64, priority: None, due_at: None, tags: None,
        },
        _ => return Err("剪贴板无可保存内容".into()),
    };
    let mut s = state.store.lock().map_err(|e| e.to_string())?;
    let id = s.todos_add(&input).map_err(|e| e.to_string())?;
    s.todos_get(id).map_err(|e| e.to_string())?.ok_or_else(|| "插入后读取失败".into())
}

/// AppState 里拿 blob 目录：经 Store 公开 getter
fn state_blob_dir(state: &State<AppState>) -> Result<std::path::PathBuf, String> {
    Ok(state
        .store
        .lock()
        .map_err(|e| e.to_string())?
        .blob_dir()
        .to_path_buf())
}

/// 前端隐藏窗口统一入口（Esc 等）：保存位置到 settings 再隐藏（位置记忆）
#[tauri::command]
pub fn hide_window(app: AppHandle) {
    crate::hotkey::hide_main(&app);
}

/// 监听启动入口（lib.rs 的 setup 中调用）
pub fn spawn_pipeline(app: AppHandle) {
    std::thread::Builder::new()
        .name("clip-store-pipeline".into())
        .spawn(move || {
            let (tx, rx) = std::sync::mpsc::channel::<ClipEvent>();
            let (stop_tx, stop_rx) = std::sync::mpsc::channel::<()>();
            let watcher = std::thread::Builder::new()
                .name("clip-watcher".into())
                .spawn(move || clipboard_watcher::run(tx, stop_rx))
                .expect("spawn watcher");
            for ev in rx {
                let state: State<AppState> = app.state();
                let mut store = state.store.lock().unwrap();
                let id = store.insert(&ev);
                let ts = ev.ts;
                // 按设置的上限自动清理（0=无限制）
                let limit: i64 = store.get_setting("history_limit").and_then(|v| v.parse().ok()).unwrap_or(10000);
                if limit > 0 { let _ = store.trim(limit); }
                drop(store);
                if id.is_ok() {
                    // 前端事件：通知列表刷新
                    let _ = app.emit("clip:new", serde_json::json!({ "ts": ts }));
                }
            }
            let _ = stop_tx.send(());
            let _ = watcher.join();
        })
        .expect("spawn pipeline");
}

/// 收藏/取消收藏剪贴板条目
#[tauri::command]
pub fn clip_toggle_pin(state: State<AppState>, id: i64) -> Result<bool, String> {
    state.store.lock().map_err(|e| e.to_string())?.clip_toggle_pin(id).map_err(|e| e.to_string())
}

/// 删除单条剪贴板条目（含 blob 清理）
#[tauri::command]
pub fn clip_delete(state: State<AppState>, id: i64) -> Result<(), String> {
    state.store.lock().map_err(|e| e.to_string())?.clip_delete(id).map_err(|e| e.to_string())
}

/// 清空历史（保护收藏条目）
#[tauri::command]
pub fn clips_clear(state: State<AppState>) -> Result<usize, String> {
    state.store.lock().map_err(|e| e.to_string())?.clips_clear().map_err(|e| e.to_string())
}

// ---------------- 设置中心（M4） ----------------

/// 读全部设置（前端设置页回显）
#[tauri::command]
pub fn settings_all(state: State<AppState>) -> Result<serde_json::Value, String> {
    let s = state.store.lock().map_err(|e| e.to_string())?;
    let get = |k: &str, d: &str| s.get_setting(k).unwrap_or_else(|| d.to_string());
    Ok(serde_json::json!({
        "hotkey": get("hotkey", "alt+v"),
        "hotkey_todos": get("hotkey_todos", "alt+t"),
        "history_limit": get("history_limit", "10000").parse::<i64>().unwrap_or(10000),
        "paused": crate::clipboard_watcher::PAUSED.load(std::sync::atomic::Ordering::Relaxed),
        "autostart": autostart_enabled(),
    }))
}

/// 查询开机自启（注册表 HKCU Run / CurrentVersion）
fn autostart_enabled() -> bool {
    false // 前端经 autostart 插件 API 管理，settings 页直接调插件
}

/// 重注册全部热键（改键/回滚共用）：clips + todos 两个，从 settings 读
fn re_register_hotkeys(app: &AppHandle) -> Result<(), String> {
    use tauri_plugin_global_shortcut::GlobalShortcutExt as _;
    use tauri_plugin_global_shortcut::Shortcut;
    let state = app.state::<AppState>();
    let (hk, hk_t) = {
        let s = state.store.lock().map_err(|e| e.to_string())?;
        (
            s.get_setting("hotkey").unwrap_or_else(|| "alt+v".into()),
            s.get_setting("hotkey_todos").unwrap_or_else(|| "alt+t".into()),
        )
    };
    let mgr = app.global_shortcut();
    let _ = mgr.unregister_all();
    let sc: Shortcut = hk.parse().map_err(|e| format!("热键解析失败: {e}"))?;
    let app2 = app.clone();
    mgr.on_shortcut(sc, move |_a, _s, e| {
        if e.state() == tauri_plugin_global_shortcut::ShortcutState::Pressed { toggle_main(&app2); }
    }).map_err(|e| format!("注册失败（可能与其他软件冲突）: {e}"))?;
    let sc_t: Shortcut = hk_t.parse().map_err(|e| format!("待办热键解析失败: {e}"))?;
    let app3 = app.clone();
    mgr.on_shortcut(sc_t, move |_a, _s, e| {
        if e.state() == tauri_plugin_global_shortcut::ShortcutState::Pressed { toggle_main_todos(&app3); }
    }).map_err(|e| format!("注册失败（可能与其他软件冲突）: {e}"))?;
    Ok(())
}

/// 改剪贴板热键：失败回滚旧设置重注册
#[tauri::command]
pub fn set_hotkey(app: AppHandle, state: State<AppState>, accelerator: String) -> Result<(), String> {
    let old = { state.store.lock().map_err(|e| e.to_string())?.get_setting("hotkey").unwrap_or_else(|| "alt+v".into()) };
    { let s = state.store.lock().map_err(|e| e.to_string())?; s.set_setting("hotkey", &accelerator); }
    if let Err(e) = re_register_hotkeys(&app) {
        { let s = state.store.lock().map_err(|e| e.to_string())?; s.set_setting("hotkey", &old); }
        let _ = re_register_hotkeys(&app);
        return Err(e);
    }
    Ok(())
}

/// 改待办热键（设置中心第二行）
#[tauri::command]
pub fn set_hotkey_todos(app: AppHandle, state: State<AppState>, accelerator: String) -> Result<(), String> {
    let old = { state.store.lock().map_err(|e| e.to_string())?.get_setting("hotkey_todos").unwrap_or_else(|| "alt+t".into()) };
    { let s = state.store.lock().map_err(|e| e.to_string())?; s.set_setting("hotkey_todos", &accelerator); }
    if let Err(e) = re_register_hotkeys(&app) {
        { let s = state.store.lock().map_err(|e| e.to_string())?; s.set_setting("hotkey_todos", &old); }
        let _ = re_register_hotkeys(&app);
        return Err(e);
    }
    Ok(())
}

/// 暂停/恢复监听
#[tauri::command]
pub fn set_paused(paused: bool) {
    crate::clipboard_watcher::PAUSED.store(paused, std::sync::atomic::Ordering::Relaxed);
}

/// 设置历史上限并立即执行清理
#[tauri::command]
pub fn set_history_limit(state: State<AppState>, limit: i64) -> Result<usize, String> {
    let mut s = state.store.lock().map_err(|e| e.to_string())?;
    s.set_setting("history_limit", &limit.to_string());
    if limit > 0 {
        s.trim(limit).map_err(|e| e.to_string())
    } else {
        Ok(0)
    }
}

/// 唤出/隐藏主窗口（热键/托盘共用）
pub fn toggle_main(app: &AppHandle) {
    if let Some(win) = app.get_webview_window("main") {
        if win.is_visible().unwrap_or(false) {
            let _ = win.hide();
        } else {
            crate::hotkey::popup_at_cursor(app);
        }
    }
}

/// 唤出/隐藏主窗口（待办 Tab；Alt+T）
pub fn toggle_main_todos(app: &AppHandle) {
    if let Some(win) = app.get_webview_window("main") {
        if win.is_visible().unwrap_or(false) {
            let _ = win.hide();
        } else {
            crate::hotkey::popup_at_cursor_todos(app);
        }
    }
}
