//! 热键唤出与回贴模块（M1）
//! - Ctrl+Alt+Q 全局热键唤出主窗口（光标处定位）
//! - 回贴：记录唤出前的前台窗口 → 写剪贴板 → SetForegroundWindow 恢复 → SendInput Ctrl+V
//! - 失焦自动隐藏

use tauri::AppHandle;
use tauri::Manager;
use windows::Win32::Foundation::{HANDLE, POINT};
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, GetClipboardSequenceNumber, OpenClipboard, SetClipboardData,
};
use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};
use windows::Win32::System::Ole::CF_UNICODETEXT;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP, VK_CONTROL, VK_V,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetCursorPos, GetForegroundWindow, GetSystemMetrics, SetForegroundWindow, SetWindowPos,
    HWND_TOPMOST, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SM_CXSCREEN, SM_CYSCREEN,
};

use crate::clipboard_watcher::IGNORE_SEQ;
use std::sync::atomic::Ordering;
use std::sync::Mutex;

/// 唤出前的目标窗口（回贴目标）。None = 唤出时无前台窗口
pub static LAST_FG: Mutex<Option<isize>> = Mutex::new(None);

/// 当前窗口（本进程主窗口）是否仍是前台：拖动标题栏时 WebView2 子窗口
/// 焦点切换会发 Focused(false)，但主窗口仍在最前——不算真正失焦，不应隐藏
pub fn is_own_hwnd_foreground(window: &tauri::Window<tauri::Wry>) -> bool {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::WindowsAndMessaging::{GetAncestor, GetWindowThreadProcessId, GA_ROOT};
    unsafe {
        let fg = GetForegroundWindow();
        if fg.0 as usize == 0 {
            return false;
        }
        // 情形 1：前台窗口的顶层祖先就是本窗口（拖动标题栏/webview 子窗口持焦点时成立）
        if let Ok(hwnd) = window.hwnd() {
            let root = GetAncestor(HWND(hwnd.0), GA_ROOT);
            if root == fg {
                return true;
            }
        }
        // 情形 2：前台窗口属于本进程（其他自有窗口）
        let mut fg_pid = 0u32;
        GetWindowThreadProcessId(fg, Some(&mut fg_pid));
        fg_pid == std::process::id()
    }
}

/// 隐藏主窗口：统一入口——保存窗口位置到 settings（下次唤出恢复）
pub fn hide_main(app: &AppHandle) {
    if let Some(win) = app.get_webview_window("main") {
        save_win_rect(app, &win);
        let _ = win.hide();
    }
}

fn save_win_rect(app: &AppHandle, win: &tauri::WebviewWindow) {
    // 统一 Win32 物理像素（tauri API 在 DPI 换算上与 Win32 不一致，混用会错位）
    if let Ok(h61) = win.hwnd() {
        unsafe {
            use windows::Win32::Foundation::RECT;
            use windows::Win32::UI::WindowsAndMessaging::GetWindowRect;
            let mut r = RECT::default();
            if GetWindowRect(windows::Win32::Foundation::HWND(h61.0), &mut r).is_ok() {
                let rect = format!("{},{},{},{}", r.left, r.top, r.right - r.left, r.bottom - r.top);
                if let Some(state) = app.try_state::<crate::commands::AppState>() {
                    if let Ok(s) = state.store.lock() {
                        s.set_setting("win_rect", &rect);
                    }
                }
            }
        }
    }
}

/// 读取并校验上次窗口位置（不应用）；显示器拔掉/越界返回 None
fn validated_win_rect(app: &AppHandle) -> Option<(i32, i32, u32, u32)> {
    let state = app.try_state::<crate::commands::AppState>()?;
    let s = state.store.lock().ok()?;
    let rect = s.get_setting("win_rect")?;
    let mut it = rect.split(',');
    let x: i32 = it.next()?.parse().ok()?;
    let y: i32 = it.next()?.parse().ok()?;
    let w: u32 = it.next()?.parse().ok()?;
    let h: u32 = it.next()?.parse().ok()?;
    unsafe {
        use windows::Win32::UI::WindowsAndMessaging::{GetSystemMetrics, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN};
        let vx = GetSystemMetrics(SM_XVIRTUALSCREEN);
        let vy = GetSystemMetrics(SM_YVIRTUALSCREEN);
        let vw = GetSystemMetrics(SM_CXVIRTUALSCREEN);
        let vh = GetSystemMetrics(SM_CYVIRTUALSCREEN);
        if vw == 0 || vh == 0 { return None; }
        if x + 60 > vx + vw || y + 60 > vy + vh || x + (w as i32) - 60 < vx || y + (h as i32) - 60 < vy {
            return None;
        }
    }
    Some((x, y, w, h))
}

/// 应用窗口矩形（show 之后调用；隐藏窗口上 set_size/set_position 会静默失败）
fn apply_win_rect(win: &tauri::WebviewWindow, x: i32, y: i32, w: u32, h: u32) {
    // tauri set_position/set_size 的 DPI 换算在不同版本行为不一，直接走 Win32 物理像素最稳
    if let Ok(h61) = win.hwnd() {
        unsafe {
            let hwnd = windows::Win32::Foundation::HWND(h61.0);
            let _ = windows::Win32::UI::WindowsAndMessaging::SetWindowPos(
                hwnd, None, x, y, w as i32, h as i32,
                windows::Win32::UI::WindowsAndMessaging::SWP_NOZORDER | windows::Win32::UI::WindowsAndMessaging::SWP_NOACTIVATE,
            );
        }
    }
}

/// Ctrl+Alt+Q / 托盘触发：唤出主窗口（剪贴板 Tab）
pub fn popup_at_cursor(app: &AppHandle) {
    popup_at_cursor_tab(app, "clips");
}

/// Ctrl+Alt+W 触发：唤出主窗口（待办 Tab）
pub fn popup_at_cursor_todos(app: &AppHandle) {
    popup_at_cursor_tab(app, "todos");
}

/// 唤出主窗口（有记忆位置则原位恢复，否则光标处定位）；tab = "clips" | "todos"
pub fn popup_at_cursor_tab(app: &AppHandle, tab: &str) {
    crate::POPUP_TS.store(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0),
        Ordering::Relaxed,
    );
    // 1. 记录当前前台窗口（回贴目标）
    unsafe {
        let fg = GetForegroundWindow();
        let fg_pid = fg.0 as isize;
        let mut last = LAST_FG.lock().unwrap();
        *last = if fg_pid != 0 { Some(fg_pid) } else { None };
    }

    // 2. 窗口定位：先 show（隐藏窗口上 set_size/set_position 会失败），再恢复记忆位置；
    //    无记忆/出屏则回退光标定位（多显示器：限制在虚拟屏幕内，避免半出屏）
    if let Some(win) = app.get_webview_window("main") {
        let _ = win.show();
        let remembered = validated_win_rect(app);
        match remembered {
            Some((x, y, w, h)) => apply_win_rect(&win, x, y, w, h),
            None => unsafe {
                let mut pt = POINT::default();
                let _ = GetCursorPos(&mut pt);
                let vw = GetSystemMetrics(SM_CXSCREEN);
                let vh = GetSystemMetrics(SM_CYSCREEN);
                let (w, h) = (460.0, 720.0);
                let scale = win.scale_factor().unwrap_or(1.5);
                let (w, h) = ((w * scale) as i32, (h * scale) as i32);
                let x = (pt.x + 12).min(vw - w).max(0);
                let y = (pt.y + 18).min(vh - h).max(0);
                apply_win_rect(&win, x, y, w as u32, h as u32);
            },
        }
    }
    if let Some(win) = app.get_webview_window("main") {
        unsafe {
            if let Ok(hwnd61) = win.hwnd() {
                // tauri 内部是 windows 0.61 的 HWND，重包为本 crate 的 0.62 HWND
                let hwnd = windows::Win32::Foundation::HWND(hwnd61.0);
                force_foreground(hwnd);
                let _ = SetWindowPos(hwnd, Some(HWND_TOPMOST), 0, 0, 0, 0, SWP_NOSIZE | SWP_NOMOVE | SWP_NOACTIVATE);
            }
        }        let _ = win.set_focus();
        // 通知前端聚焦（payload 指定唤出后落在哪个 Tab）
        use tauri::Emitter;
        let _ = app.emit("clipwin:popup", serde_json::json!({ "tab": tab }));
    }
}

/// 强制把窗口提到前台：AttachThreadInput 绕过 Windows 前台锁定
unsafe fn force_foreground(hwnd: windows::Win32::Foundation::HWND) {
    use windows::Win32::System::Threading::AttachThreadInput;
    use windows::Win32::UI::WindowsAndMessaging::{
        BringWindowToTop, GetForegroundWindow, GetWindowThreadProcessId, SetForegroundWindow,
        ShowWindow, SW_SHOW,
    };
    let fg = GetForegroundWindow();
    let mut fg_tid = 0u32;
    if fg.0 as usize != 0 {
        fg_tid = GetWindowThreadProcessId(fg, None);
    }
    let cur_tid = windows::Win32::System::Threading::GetCurrentThreadId();
    let attached = fg_tid != 0 && AttachThreadInput(cur_tid, fg_tid, true).as_bool();
    let _ = SetForegroundWindow(hwnd);
    let _ = BringWindowToTop(hwnd);
    let _ = ShowWindow(hwnd, SW_SHOW);
    if attached {
        let _ = AttachThreadInput(cur_tid, fg_tid, false);
    }
}

/// 回贴：写剪贴板 → 恢复目标窗口前台 → SendInput Ctrl+V
pub fn paste_to_last_fg(app: &AppHandle, text: &str) -> Result<(), String> {
    // 1. 写剪贴板（自写自听跳过）
    write_text(text)?;

    // 2. 隐藏自己 + 恢复目标窗口前台
    if let Some(win) = app.get_webview_window("main") {
        let _ = win.hide();
    }
    let target = {
        let last = LAST_FG.lock().unwrap();
        *last
    };
    if let Some(hwnd_val) = target {
        unsafe {
            let hwnd = windows::Win32::Foundation::HWND(hwnd_val as *mut _);
            let _ = SetForegroundWindow(hwnd);
            // 等目标窗口就绪
            std::thread::sleep(std::time::Duration::from_millis(30));
            // 3. SendInput Ctrl+V
            send_ctrl_v();
        }
        Ok(())
    } else {
        Err("无回贴目标窗口".into())
    }
}

/// 图片回贴：读 .dib 文件 → 写 CF_DIBV5 → 恢复前台 → Ctrl+V（待办图片条目用）
pub fn paste_image_to_last_fg(app: &AppHandle, dib_path: &str) -> Result<(), String> {
    let dib = std::fs::read(dib_path).map_err(|e| format!("读图片失败: {e}"))?;
    unsafe {
        if OpenClipboard(None).is_err() {
            return Err("OpenClipboard failed".into());
        }
        let _ = EmptyClipboard();
        let hg = GlobalAlloc(GMEM_MOVEABLE, dib.len())
            .map_err(|e| e.to_string())?;
        let ptr = GlobalLock(hg);
        if ptr.is_null() {
            let _ = CloseClipboard();
            return Err("GlobalLock failed".into());
        }
        std::ptr::copy_nonoverlapping(dib.as_ptr(), ptr as *mut u8, dib.len());
        let _ = GlobalUnlock(hg);
        use windows::Win32::System::Ole::CF_DIBV5;
        if SetClipboardData(CF_DIBV5.0 as u32, Some(HANDLE(hg.0))).is_err() {
            let _ = CloseClipboard();
            return Err("SetClipboardData(DIBV5) failed".into());
        }
        let _ = CloseClipboard();
        IGNORE_SEQ.store(GetClipboardSequenceNumber(), Ordering::SeqCst);
    }
    if let Some(win) = app.get_webview_window("main") {
        let _ = win.hide();
    }
    let target = { *LAST_FG.lock().unwrap() };
    if let Some(hwnd_val) = target {
        unsafe {
            let hwnd = windows::Win32::Foundation::HWND(hwnd_val as *mut _);
            let _ = SetForegroundWindow(hwnd);
            std::thread::sleep(std::time::Duration::from_millis(30));
            send_ctrl_v();
        }
        Ok(())
    } else {
        Err("无回贴目标窗口".into())
    }
}

/// 纯文本写入剪贴板（与 commands::write_clipboard 相同逻辑，含自听跳过）
pub fn write_text(text: &str) -> Result<(), String> {
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
        if SetClipboardData(CF_UNICODETEXT.0 as u32, Some(HANDLE(hg.0))).is_err() {
            let _ = CloseClipboard();
            return Err("SetClipboardData failed".into());
        }
        let _ = CloseClipboard();
        let seq = GetClipboardSequenceNumber();
        IGNORE_SEQ.store(seq, Ordering::SeqCst);
    }
    Ok(())
}

unsafe fn send_ctrl_v() {
    let mk = |vk: windows::Win32::UI::Input::KeyboardAndMouse::VIRTUAL_KEY, up: bool| INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: vk,
                wScan: 0,
                dwFlags: if up { KEYEVENTF_KEYUP } else { Default::default() },
                time: 0,
                dwExtraInfo: 0,
            },
        },
    };
    let seq = [
        mk(VK_CONTROL, false),
        mk(VK_V, false),
        mk(VK_V, true),
        mk(VK_CONTROL, true),
    ];
    let sent = SendInput(&seq, std::mem::size_of::<INPUT>() as i32);
    if sent != 4 {
        eprintln!("[clipwin] SendInput 不完整: {sent}/4");
    }
}

/// 富文本双格式回贴：同时写 CF_UNICODETEXT 与 "HTML Format"，Word/邮件收富文本、记事本收纯文本
pub fn paste_rich_to_last_fg(app: &AppHandle, text: &str, html: Option<&str>) -> Result<(), String> {
    unsafe {
        if OpenClipboard(None).is_err() {
            return Err("OpenClipboard failed".into());
        }
        let _ = EmptyClipboard();
        // 1) 纯文本
        let bytes: Vec<u16> = text.encode_utf16().chain(std::iter::once(0u16)).collect();
        let byte_len = bytes.len() * 2;
        let hg = GlobalAlloc(GMEM_MOVEABLE, byte_len).map_err(|e| e.to_string())?;
        let ptr = GlobalLock(hg);
        if !ptr.is_null() {
            std::ptr::copy_nonoverlapping(bytes.as_ptr() as *const u8, ptr as *mut u8, byte_len);
            let _ = GlobalUnlock(hg);
            if SetClipboardData(CF_UNICODETEXT.0 as u32, Some(HANDLE(hg.0))).is_err() {
                let _ = CloseClipboard();
                return Err("SetClipboardData(text) failed".into());
            }
        }
        // 2) HTML Format：CF_HTML 需带偏移头（标准头 105 字节起，Chrome/Word/Outlook 通用写法）
        if let Some(h) = html {
            let body = h.trim();
            // 完整文档直接用；片段则包 StartFragment 注释壳
            let (frag, frag_off) = if body.to_lowercase().starts_with("<html") {
                (body.to_string(), 0usize)
            } else {
                let pre = "<html><body>\r\n<!--StartFragment-->";
                (format!("{}{}<!--EndFragment-->\r\n</body></html>", pre, body), pre.len())
            };
            let header = "Version:0.9\r\nStartHtml:0000000105\r\nEndHtml:0000000000\r\nStartFragment:0000000000\r\nEndFragment:0000000000\r\n";
            let hlen = header.len();
            let start_frag = hlen + frag_off;
            let end_frag = hlen + frag_off + body.len();
            let end_html = hlen + frag.len();
            let header = format!(
                "Version:0.9\r\nStartHtml:{:010}\r\nEndHtml:{:010}\r\nStartFragment:{:010}\r\nEndFragment:{:010}\r\n",
                hlen, end_html, start_frag, end_frag
            );
            // 重算：头长度固定 133（format 生成 10 位数字宽度恒定）
            let cf_html = format!("{}{}", header, frag);
            let hb = cf_html.into_bytes();
            let hg2 = GlobalAlloc(GMEM_MOVEABLE, hb.len()).map_err(|e| e.to_string())?;
            let p2 = GlobalLock(hg2);
            if !p2.is_null() {
                std::ptr::copy_nonoverlapping(hb.as_ptr(), p2 as *mut u8, hb.len());
                let _ = GlobalUnlock(hg2);
                use windows::Win32::System::DataExchange::RegisterClipboardFormatW;
                use windows::core::w;
                let fmt = RegisterClipboardFormatW(w!("HTML Format"));
                if fmt != 0 {
                    let _ = SetClipboardData(fmt, Some(HANDLE(hg2.0)));
                }
            }
        }
        let _ = CloseClipboard();
        IGNORE_SEQ.store(GetClipboardSequenceNumber(), Ordering::SeqCst);
    }
    if let Some(win) = app.get_webview_window("main") {
        let _ = win.hide();
    }
    let target = { *LAST_FG.lock().unwrap() };
    if let Some(hwnd_val) = target {
        unsafe {
            let hwnd = windows::Win32::Foundation::HWND(hwnd_val as *mut _);
            let _ = SetForegroundWindow(hwnd);
            std::thread::sleep(std::time::Duration::from_millis(30));
            send_ctrl_v();
        }
        Ok(())
    } else {
        Err("无回贴目标窗口".into())
    }
}
