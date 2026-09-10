//! 热键唤出与回贴模块（M1）
//! - Ctrl+Alt+Q 全局热键唤出主窗口（光标处定位）
//! - 回贴：记录唤出前的前台窗口 → 写剪贴板 → SetForegroundWindow 恢复 → 发送粘贴键（终端类目标自适应 Shift+Insert，其余 Ctrl+V）
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
    SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP, VK_CONTROL, VK_INSERT,
    VK_SHIFT, VK_V,
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

/// 终端/IDE 进程名单：这些目标一律用 Shift+Insert 粘贴。
/// 终端拦截或不透传合成的 Ctrl+V（VS Code 集成终端、conhost 等）；
/// IDE 编辑器本身也默认支持 Shift+Insert（IntelliJ/VS Code keymap 均内置），统一安全
const TERMINAL_EXES: &[&str] = &[
    // 原生终端
    "windowsterminal.exe", "openconsole.exe", "conhost.exe", "cmd.exe",
    "powershell.exe", "pwsh.exe", "mintty.exe", "alacritty.exe", "wezterm-gui.exe",
    "tabby.exe", "kitty.exe", "hyper.exe", "warp.exe",
    // SSH / 远程终端
    "putty.exe", "termius.exe", "xshell.exe", "mobaxterm.exe", "securecrt.exe", "electerm.exe",
    // VS Code 家族（集成终端拦截 Ctrl+V，编辑器支持 Shift+Insert）
    "code.exe", "code - insiders.exe", "vscodium.exe", "codium.exe", "cursor.exe", "windsurf.exe", "trae.exe",
    // JetBrains 家族（终端同理，编辑器 Shift+Insert 内置绑定 Paste）
    "idea64.exe", "pycharm64.exe", "webstorm64.exe", "goland64.exe", "clion64.exe",
    "rider64.exe", "datagrip64.exe", "rubymine64.exe", "phpstorm64.exe", "studio64.exe",
];

/// 浏览器进程名单：Web 终端（code-server / ttyd / WebSSH 等）跑在浏览器里，进程名区分不了，看标题
const BROWSER_EXES: &[&str] = &[
    "chrome.exe", "msedge.exe", "firefox.exe", "brave.exe", "opera.exe", "vivaldi.exe",
];

/// 浏览器标题里的终端关键词（小写匹配）：Web 终端标签页标题通常含这些
const BROWSER_TITLE_KEYS: &[&str] = &[
    "terminal", "终端", "命令提示符", "命令行", "powershell", "pwsh", "bash",
    "ssh", "shell", "ttyd", "webssh", "xterm", "code-server", "visual studio code",
    "vscode", "控制台",
];

/// 回贴目标是否终端类：是则发 Shift+Insert（终端世界通用粘贴键），否则 Ctrl+V。
/// 启发式 = 终端/IDE 进程名单 + 浏览器标题关键词（Web 终端场景）；误判代价低——
/// 浏览器/编辑器里 Shift+Insert 同样触发粘贴，且剪贴板内容已写入，手动 Ctrl+V 也能补救
fn target_wants_shift_insert(hwnd: windows::Win32::Foundation::HWND) -> bool {
    use windows::core::PWSTR;
    use windows::Win32::System::Threading::{
        OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
    };
    unsafe {
        // 1. HWND → PID → 进程名（小写）
        let mut pid = 0u32;
        windows::Win32::UI::WindowsAndMessaging::GetWindowThreadProcessId(hwnd, Some(&mut pid));
        if pid == 0 {
            return false;
        }
        let exe = (|| -> Option<String> {
            use windows::Win32::Foundation::CloseHandle;
            let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
            let mut buf = [0u16; 512];
            let mut len = buf.len() as u32;
            let r = QueryFullProcessImageNameW(h, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut len).is_ok();
            let _ = CloseHandle(h);
            r.then(|| {
                let full = String::from_utf16_lossy(&buf[..len as usize]).to_lowercase();
                full.rsplit('\\').next().unwrap_or("").to_string()
            })
        })();
        let Some(exe) = exe else { return false };

        // 2. 名单判定
        if TERMINAL_EXES.contains(&exe.as_str()) {
            return true;
        }
        if BROWSER_EXES.contains(&exe.as_str()) {
            let mut buf = [0u16; 512];
            let n = windows::Win32::UI::WindowsAndMessaging::GetWindowTextW(hwnd, &mut buf);
            let title = String::from_utf16_lossy(&buf[..n.max(0) as usize]).to_lowercase();
            return BROWSER_TITLE_KEYS.iter().any(|k| title.contains(k));
        }
        false
    }
}

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

/// 回贴：写剪贴板 → 恢复目标窗口前台 → 发送粘贴键（终端类目标自适应 Shift+Insert）
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
            // 3. 发送粘贴键：终端类目标自适应 Shift+Insert，其余 Ctrl+V
            send_paste_key(target_wants_shift_insert(hwnd));
        }
        Ok(())
    } else {
        Err("无回贴目标窗口".into())
    }
}

/// 图片回贴：读 .dib 文件 → 写 CF_DIBV5 → 恢复前台 → 发送粘贴键（终端类目标自适应 Shift+Insert）
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
            send_paste_key(target_wants_shift_insert(hwnd));
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

/// 发送粘贴键：终端类目标合成 Shift+Insert（终端通用粘贴键），其余 Ctrl+V
unsafe fn send_paste_key(shift_insert: bool) {
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        KEYBD_EVENT_FLAGS, KEYEVENTF_EXTENDEDKEY,
    };
    let (mod_vk, key_vk) = if shift_insert {
        (VK_SHIFT, VK_INSERT)
    } else {
        (VK_CONTROL, VK_V)
    };
    // Insert 属扩展键（扫描码 E0 52）：不带 KEYEVENTF_EXTENDEDKEY 时，接收方会把它
    // 当数字键盘 0（NumPad0）解析，终端收不到 Insert —— 粘贴静默失败（v0.1.4 终端回贴失效的根因）
    let key_ext = shift_insert;
    let mk = |vk: windows::Win32::UI::Input::KeyboardAndMouse::VIRTUAL_KEY, up: bool, ext: bool| INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: vk,
                wScan: 0,
                dwFlags: {
                    let mut f = if up { KEYEVENTF_KEYUP } else { KEYBD_EVENT_FLAGS::default() };
                    if ext {
                        f |= KEYEVENTF_EXTENDEDKEY;
                    }
                    f
                },
                time: 0,
                dwExtraInfo: 0,
            },
        },
    };
    let seq = [
        mk(mod_vk, false, false),
        mk(key_vk, false, key_ext),
        mk(key_vk, true, key_ext),
        mk(mod_vk, true, false),
    ];
    let sent = SendInput(&seq, std::mem::size_of::<INPUT>() as i32);
    if sent != 4 {
        eprintln!("[clipwin] SendInput 不完整: {sent}/4");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 真实窗口枚举验证启发式：打印每个可见顶层窗口的 进程名/标题/判定结果
    #[test]
    fn dump_detection_matrix() {
        use windows::core::BOOL;
        use windows::Win32::Foundation::{HWND, LPARAM};
        use windows::Win32::UI::WindowsAndMessaging::{
            EnumWindows, GetWindowTextW, IsWindowVisible,
        };
        // 枚举回调只能用 extern "system" fn，用 thread_local 收集
        thread_local! {
            static REPORT: std::cell::RefCell<Vec<(bool, String, String)>> =
                std::cell::RefCell::new(Vec::new());
        }
        unsafe extern "system" fn cb(hwnd: HWND, _: LPARAM) -> BOOL {
            if IsWindowVisible(hwnd).as_bool() {
                let mut buf = [0u16; 256];
                let n = GetWindowTextW(hwnd, &mut buf);
                let title = String::from_utf16_lossy(&buf[..n as usize]);
                if !title.trim().is_empty() {
                    let mut pid = 0u32;
                    windows::Win32::UI::WindowsAndMessaging::GetWindowThreadProcessId(hwnd, Some(&mut pid));
                    let exe = (|| -> Option<String> {
                        use windows::core::PWSTR;
                        use windows::Win32::System::Threading::{
                            OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32,
                            PROCESS_QUERY_LIMITED_INFORMATION,
                        };
                        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
                        let mut b = [0u16; 512];
                        let mut l = b.len() as u32;
                        QueryFullProcessImageNameW(h, PROCESS_NAME_WIN32, PWSTR(b.as_mut_ptr()), &mut l)
                            .ok()?;
                        let full = String::from_utf16_lossy(&b[..l as usize]);
                        Some(full.rsplit('\\').next().unwrap_or("").to_lowercase())
                    })()
                    .unwrap_or_default();
                    REPORT.with(|r| r.borrow_mut().push((target_wants_shift_insert(hwnd), exe, title)));
                }
            }
            true.into()
        }
        unsafe {
            let _ = EnumWindows(Some(cb), LPARAM(0));
            REPORT.with(|r| {
                println!("判定矩阵（终端类→Shift+Insert / 其余→Ctrl+V）：");
                for (hit, exe, title) in r.borrow().iter() {
                    println!("  [{}] {:<24} | {}", if *hit { "X" } else { " " }, exe, title);
                }
            });
        }
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
            send_paste_key(target_wants_shift_insert(hwnd));
        }
        Ok(())
    } else {
        Err("无回贴目标窗口".into())
    }
}
