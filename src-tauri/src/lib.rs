// clipwin · M1 入口
mod clipboard_watcher;
mod commands;
mod hotkey;
mod store;

use commands::AppState;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use tauri::Manager;
use tauri_plugin_global_shortcut::{Code, Modifiers, ShortcutState};

/// 最近一次弹窗时间（ms，Unix）：失焦隐藏需防抖，避免弹窗瞬间焦点过渡被误杀
pub static POPUP_TS: AtomicU64 = AtomicU64::new(0);

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            None,
        ))
        .plugin(tauri_plugin_global_shortcut::Builder::new().build())
        .on_window_event(|window, event| {
            // 关窗 = 隐藏（常驻）；失焦 = 隐藏（快贴窗交互模式）
            match event {
                tauri::WindowEvent::CloseRequested { api, .. } => {
                    let app2 = window.app_handle().clone();
                    crate::hotkey::hide_main(&app2);
                    api.prevent_close();
                }
                tauri::WindowEvent::Focused(false) => {
                    // 防抖：弹窗后 600ms 内的失焦事件是焦点过渡噪声，不隐藏
                    let elapsed = now_ms().saturating_sub(POPUP_TS.load(Ordering::Relaxed));
                    if elapsed > 600 && !hotkey::is_own_hwnd_foreground(window) {
                        // 仅当真正失去前台（点到了别的应用）才隐藏；
                        // 拖动标题栏时 WebView2 子窗口焦点切换会发 Focused(false)，但主窗口仍是前台，不应隐藏
                        let app2 = window.app_handle().clone();
                        crate::hotkey::hide_main(&app2);
                    }
                }
                _ => {}
            }
        })
        .setup(|app| {
            let handle = app.handle().clone();

            // ---- 数据目录与存储 ----
            let data_dir = handle
                .path()
                .app_data_dir()
                .expect("resolve app data dir");
            std::fs::create_dir_all(&data_dir).ok();
            let db = data_dir.join("clipwin.db");
            let blobs = data_dir.join("blobs");
            let store = store::Store::open(&db, blobs).expect("open store");
            app.manage(AppState { store: Mutex::new(store) });
            commands::spawn_pipeline(handle.clone());

            // 恢复暂停状态设置
            {
                let state = app.state::<AppState>();
                let paused = state.store.lock().unwrap().get_setting("paused").as_deref() == Some("1");
                crate::clipboard_watcher::PAUSED.store(paused, std::sync::atomic::Ordering::Relaxed);
            }

            // Win11 Mica 材质（Win10 优雅降级：DWM 调用失败即保持纯色背景）
            if let Some(win) = handle.get_webview_window("main") {
                if let Ok(h) = win.hwnd() {
                    unsafe {
                        use windows::Win32::Graphics::Dwm::{DwmSetWindowAttribute, DWMWA_SYSTEMBACKDROP_TYPE, DWM_SYSTEMBACKDROP_TYPE, DWMSBT_MAINWINDOW};
                        // DWMSBT_MAINWINDOW = Mica（Win11 22H2+）；旧系统失败静默忽略
                        let backdrop = DWMSBT_MAINWINDOW;
                        let _ = DwmSetWindowAttribute(
                            windows::Win32::Foundation::HWND(h.0),
                            DWMWA_SYSTEMBACKDROP_TYPE,
                            &backdrop as *const _ as *const core::ffi::c_void,
                            std::mem::size_of::<DWM_SYSTEMBACKDROP_TYPE>() as u32,
                        );
                    }
                }
            }

            // ---- 托盘常驻 ----
            use tauri::menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem};
            let show_i = MenuItem::with_id(app, "show", "显示剪贴板 (Ctrl+Alt+Q)", true, None::<&str>)?;
            let todo_i = MenuItem::with_id(app, "todos", "显示待办 (Ctrl+Alt+W)", true, None::<&str>)?;
            let pause_i = CheckMenuItem::with_id(app, "pause", "暂停监听（隐私模式）", true, false, None::<&str>)?;
            let clear_i = MenuItem::with_id(app, "clear", "清空历史（保留收藏）", true, None::<&str>)?;
            let sep1 = PredefinedMenuItem::separator(app)?;
            let about_i = MenuItem::with_id(app, "about", "关于 clipwin", true, None::<&str>)?;
            let quit_i = MenuItem::with_id(app, "quit", "退出 clipwin", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&show_i, &todo_i, &pause_i, &clear_i, &sep1, &about_i, &quit_i])?;
            let tray = tauri::tray::TrayIconBuilder::with_id("main-tray")
                .icon(app.default_window_icon().unwrap().clone())
                .tooltip("clipwin · Ctrl+Alt+Q 唤出")
                .menu(&menu)
                .on_menu_event(|app, event| {
                    match event.id.as_ref() {
                        "show" => {
                            if let Some(win) = app.get_webview_window("main") {
                                if win.is_visible().unwrap_or(false) {
                                    hotkey::hide_main(app);
                                } else {
                                    hotkey::popup_at_cursor(app);
                                }
                            }
                        }
                        "todos" => {
                            commands::toggle_main_todos(app);
                        }
                        "pause" => {
                            let paused = !crate::clipboard_watcher::PAUSED.load(std::sync::atomic::Ordering::Relaxed);
                            crate::clipboard_watcher::PAUSED.store(paused, std::sync::atomic::Ordering::Relaxed);
                            // 同步设置存储 + 托盘勾选态（经 state 拿 CheckMenuItem 不便，直接改 tooltip）
                            if let Some(state) = app.try_state::<AppState>() {
                                if let Ok(s) = state.store.lock() { s.set_setting("paused", if paused { "1" } else { "0" }); }
                            }
                            let _ = app.tray_by_id("main-tray").map(|t| t.set_tooltip(Some(if paused { "clipwin · 已暂停监听" } else { "clipwin · Ctrl+Alt+Q 唤出" })));
                            // 菜单项勾选态

                        }
                        "clear" => {
                            if let Some(state) = app.try_state::<AppState>() {
                                if let Ok(mut s) = state.store.lock() { let _ = s.clips_clear(); }
                            }
                            use tauri::Emitter as _;
                            let _ = app.emit("clip:new", serde_json::json!({"ts": 0}));
                        }
                        "about" => {
                            use tauri_plugin_opener::OpenerExt as _;
                            let _ = app.opener().open_url("https://github.com/clipwin/clipwin", None::<&str>);
                        }
                        "quit" => {
                            app.exit(0);
                        }
                        _ => {}
                    }
                })
                .build(app)?;
            // 左键托盘 = 切换显示
            tray.on_tray_icon_event(|tray, event| {
                if let tauri::tray::TrayIconEvent::Click { button: tauri::tray::MouseButton::Left, button_state: tauri::tray::MouseButtonState::Up, .. } = event {
                    let app = tray.app_handle().clone();
                    if let Some(win) = app.get_webview_window("main") {
                        if win.is_visible().unwrap_or(false) {
                            hotkey::hide_main(&app);
                        } else {
                            hotkey::popup_at_cursor(&app);
                        }
                    }
                }
            });

            // ---- 全局热键：从设置读取（默认 ctrl+alt+q 剪贴板 / ctrl+alt+w 待办），唤出/隐藏切换 ----
            use tauri_plugin_global_shortcut::GlobalShortcutExt;
            use tauri_plugin_global_shortcut::Shortcut;
            {
                let state = app.state::<AppState>();
                let (hk, hk_t) = {
                    let s = state.store.lock().unwrap();
                    // 旧默认迁移：v0.1.1 及之前默认 alt+v/alt+t，随 v0.1.2 改默认——
                    // 仍是旧默认值的（=用户从未自定义）一次性迁移到新默认
                    if s.get_setting("hotkey").as_deref() == Some("alt+v") {
                        s.set_setting("hotkey", "ctrl+alt+q");
                    }
                    if s.get_setting("hotkey_todos").as_deref() == Some("alt+t") {
                        s.set_setting("hotkey_todos", "ctrl+alt+w");
                    }
                    (
                        s.get_setting("hotkey").unwrap_or_else(|| "ctrl+alt+q".into()),
                        s.get_setting("hotkey_todos").unwrap_or_else(|| "ctrl+alt+w".into()),
                    )
                };
                let sc: Shortcut = hk.parse().unwrap_or_else(|_| Shortcut::new(Some(Modifiers::CONTROL | Modifiers::ALT), Code::KeyQ));
                app.global_shortcut().on_shortcut(sc, move |_app, _s, e| {
                    if e.state() == ShortcutState::Pressed { commands::toggle_main(&_app.clone()); }
                })
                .map_err(|e| format!("注册热键失败: {e}"))?;
                let sc_t: Shortcut = hk_t.parse().unwrap_or_else(|_| Shortcut::new(Some(Modifiers::CONTROL | Modifiers::ALT), Code::KeyW));
                app.global_shortcut().on_shortcut(sc_t, move |_app, _s, e| {
                    if e.state() == ShortcutState::Pressed { commands::toggle_main_todos(&_app.clone()); }
                })
                .map_err(|e| format!("注册待办热键失败: {e}"))?;
            }

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::search,
            commands::get_clip,
                        commands::write_clipboard,
            commands::stats,
            commands::paste_back,
            commands::todos_add,
            commands::todos_list,
            commands::todos_toggle,
            commands::todos_update_full,
            commands::todos_delete,
            commands::todos_get,
            commands::clip_to_todo,
            commands::todo_paste_from_clipboard,
                        commands::todos_stats,
            commands::todos_set_priority,
            commands::todos_set_due,
            commands::todos_toggle_pin,
            commands::todos_set_tags,
            commands::hide_window,
            commands::clip_toggle_pin,
            commands::clip_delete,
            commands::clips_clear,
            commands::settings_all,
            commands::set_hotkey,
            commands::set_hotkey_todos,
            commands::set_paused,
            commands::set_history_limit
        ])
        .run(tauri::generate_context!())
        .expect("error while running clipwin");
}
