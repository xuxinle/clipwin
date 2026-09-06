//! 剪贴板监听模块（M0 PoC）：AddClipboardFormatListener + 隐藏消息窗口
//! 独立监听线程跑消息循环；收到 WM_CLIPBOARDUPDATE 后快速拷贝快照，
//! 通过 mpsc 交给存储线程（含 SQLite 写入）解析入库。
//! PoC 范围：文本 / 图片(DIBV5 原样落盘) / 文件列表三种格式。

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc::Sender;
use std::sync::mpsc::Receiver;
use std::thread;
use std::time::Duration;

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HGLOBAL, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::DataExchange::{
    AddClipboardFormatListener, CloseClipboard, GetClipboardData, GetClipboardSequenceNumber,
    IsClipboardFormatAvailable, OpenClipboard, RegisterClipboardFormatW,
    RemoveClipboardFormatListener,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Memory::{GlobalLock, GlobalSize, GlobalUnlock};
use windows::Win32::System::Ole::CF_UNICODETEXT;
use windows::Win32::UI::Shell::{DragQueryFileW, HDROP};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, PeekMessageW,
    RegisterClassW, TranslateMessage, HWND_MESSAGE, WINDOW_EX_STYLE, WINDOW_STYLE,
    GWLP_USERDATA, MSG, PM_REMOVE, WM_CLIPBOARDUPDATE, WM_NCCREATE, WM_DESTROY, WNDCLASSW,
    CREATESTRUCTW, GetWindowLongPtrW, SetWindowLongPtrW,
};

/// CF_DIBV5 = 17（windows-rs 的 Ole 模块未直接导出此常量，本地定义）
const CF_DIBV5: u32 = 17;
/// CF_HDROP = 15
const CF_HDROP: u32 = 15;

/// 一条剪贴板事件快照（监听线程已拷贝完原始数据，未解析）
#[derive(Debug)]
pub struct ClipEvent {
    #[allow(dead_code)] // seq 用于去重调试，保留在事件上
    pub seq: u32,
    pub ts: i64,
    pub text: Option<String>,
    /// CF_HTML 原始字节（"HTML Format" 注册格式，含偏移头）
    pub html: Option<Vec<u8>>,
    /// CF_DIBV5 原始字节（BITMAPV5HEADER + 像素）
    pub dib: Option<Vec<u8>>,
    pub files: Option<Vec<String>>,
}

/// 自写自听跳过：粘贴回填前记录当前序列号，相同则跳过
pub static IGNORE_SEQ: AtomicU32 = AtomicU32::new(0);

/// 暂停监听标志（true = 收到更新直接丢弃，不读不存）
pub static PAUSED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// 监听线程主循环
pub fn run(tx: Sender<ClipEvent>, stop_rx: Receiver<()>) {
    unsafe {
        if let Err(e) = run_unsafe(tx, stop_rx) {
            eprintln!("[clipwin] watcher error: {e}");
        }
    }
}

unsafe fn run_unsafe(tx: Sender<ClipEvent>, stop_rx: Receiver<()>) -> windows::core::Result<()> {
    let hinstance = GetModuleHandleW(None)?;
    let wc = WNDCLASSW {
        lpfnWndProc: Some(wnd_proc),
        hInstance: hinstance.into(),
        lpszClassName: WATCHER_CLASS_NAME,
        ..Default::default()
    };
    let atom = RegisterClassW(&wc);
    if atom == 0 {
        return Err(windows::core::Error::from_hresult(windows::core::HRESULT(-1)));
    }

    // HWND_MESSAGE：纯消息窗口（不可见、不接收用户输入）
    let ctx = Box::new(ThreadCtx { tx });
    let box_ptr = Box::into_raw(ctx);
    let hwnd: HWND = CreateWindowExW(
        WINDOW_EX_STYLE(0),
        WATCHER_CLASS_NAME,
        w!("clipwin-watcher"),
        WINDOW_STYLE(0),
        0, 0, 0, 0,
        Some(HWND_MESSAGE),
        None,
        Some(hinstance.into()),
        Some(box_ptr as _),
    )?;

    // 注册监听（失败重试，系统冷启动场景）
    let mut ok = false;
    for attempt in 0..5 {
        if AddClipboardFormatListener(hwnd).is_ok() {
            ok = true;
            break;
        }
        thread::sleep(Duration::from_millis(20 * (attempt as u64 + 1)));
    }
    if !ok {
        let _ = DestroyWindow(hwnd);
        return Err(windows::core::Error::from_hresult(windows::core::HRESULT(-2)));
    }

    // 消息循环：PeekMessage 轮询 + stop_rx 心跳（50ms，空闲 CPU 占用极低）
    // 说明：WM_CLIPBOARDUPDATE 属发送型消息，会在 PeekMessageW 调用期间直接派发到 wnd_proc
    let mut msg = MSG::default();
    loop {
        if stop_rx.try_recv().is_ok() {
            break;
        }
        let r = PeekMessageW(&mut msg, Some(hwnd), 0, 0, PM_REMOVE);
        if r.as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
            if msg.message == WM_DESTROY { return Ok(()); }
        } else {
            thread::sleep(Duration::from_millis(50));
        }
    }

    let _ = RemoveClipboardFormatListener(hwnd);
    let _ = DestroyWindow(hwnd);
    Ok(())
}

/// 窗口类名
pub const WATCHER_CLASS_NAME: PCWSTR = w!("ClipwinWatcherWnd");

struct ThreadCtx {
    tx: Sender<ClipEvent>,
}

unsafe extern "system" fn wnd_proc(
    hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_NCCREATE => {
            // lpParam 经 CREATESTRUCT 传递，需在此时落到 GWLP_USERDATA
            let cs = &*(lparam.0 as *const CREATESTRUCTW);
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, cs.lpCreateParams as isize);
            DefWindowProcW(hwnd, msg, wparam, lparam)
        }
        WM_CLIPBOARDUPDATE => {
            let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut ThreadCtx;
            if !ptr.is_null() {
                handle_update(&*ptr);
            }
            LRESULT(0)
        }
        WM_DESTROY => LRESULT(0),
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}


unsafe fn handle_update(ctx: &ThreadCtx) {
    if PAUSED.load(std::sync::atomic::Ordering::Relaxed) { return; }
    let seq = GetClipboardSequenceNumber();
    if IGNORE_SEQ.load(Ordering::SeqCst) == seq {
        return;
    }
    let ev = capture_locked(seq);
    if let Some(ev) = ev {
        let _ = ctx.tx.send(ev);
    }
}

/// 立即抓取当前剪贴板（命令层调用：待办 Ctrl+V）。seq 传当前序列号。
pub fn capture_now() -> Option<ClipEvent> {
    unsafe {
        let seq = GetClipboardSequenceNumber();
        capture_locked(seq)
    }
}

/// OpenClipboard 指数退避重试；锁内只拷贝，锁外解析
unsafe fn capture_locked(seq: u32) -> Option<ClipEvent> {
    let mut attempt = 0u32;
    loop {
        if OpenClipboard(None).is_ok() {
            break;
        }
        attempt += 1;
        if attempt >= 8 {
            return None;
        }
        thread::sleep(Duration::from_millis(10 * (1 << attempt.min(3))));
    }
    let ev = read_formats_locked(seq);
    while CloseClipboard().is_err() {
        thread::sleep(Duration::from_millis(1));
    }
    ev
}

unsafe fn read_formats_locked(seq: u32) -> Option<ClipEvent> {
    // 隐私排除：密码管理器等应用写入的剪贴板会携带这些标记格式，必须跳过不入库
    // "Can Include in Clipboard History"（Win10 1809+）/ "Exclude Clipboard Content from Monitor Processing"
    const EXCLUDE_FORMATS: [&str; 2] = [
        "Can Include in Clipboard History",
        "Exclude Clipboard Content from Monitor Processing",
    ];
    for fmt in EXCLUDE_FORMATS {
        let mut buf: [u16; 64] = [0; 64];
        let name = wide_str(fmt, &mut buf);
        if IsClipboardFormatAvailable(RegisterClipboardFormatW(name)).is_ok() {
            return None; // 明确排除：不入库不推事件
        }
    }
    let mut ev = ClipEvent {
        seq, ts: now_ms(), text: None, html: None, dib: None, files: None,
    };
    // 文本优先（PoC 简化：有文本则不再读图片/文件）
    if IsClipboardFormatAvailable(CF_UNICODETEXT.0 as u32).is_ok() {
        ev.text = read_unicode_text();
        // 富文本双格式：浏览器/Word 复制时同时携带 "HTML Format"，一并读取
        if let Some(fmt) = find_html_format() {
            ev.html = read_raw_format(fmt);
        }
    }
    if ev.text.is_none() && IsClipboardFormatAvailable(CF_DIBV5).is_ok() {
        ev.dib = read_dibv5();
    }
    if ev.text.is_none() && IsClipboardFormatAvailable(CF_HDROP).is_ok() {
        ev.files = read_hdrop();
    }
    Some(ev)
}

/// UTF-8 → 栈上宽字符（免分配，用于格式名注册）
unsafe fn wide_str(s: &str, buf: &mut [u16]) -> PCWSTR {
    let mut n = 0;
    for (i, c) in s.encode_utf16().enumerate() {
        if i >= buf.len() - 1 { break; }
        buf[i] = c;
        n = i + 1;
    }
    buf[n] = 0;
    PCWSTR(buf.as_ptr())
}

unsafe fn read_unicode_text() -> Option<String> {
    let h = GetClipboardData(CF_UNICODETEXT.0 as u32).ok()?;
    let hg = HGLOBAL(h.0);
    let ptr = GlobalLock(hg);
    if ptr.is_null() { return None; }
    let len_u16 = GlobalSize(hg) / 2;
    let slice = std::slice::from_raw_parts(ptr as *const u16, len_u16);
    let end = slice.iter().position(|&c| c == 0).unwrap_or(len_u16);
    let s = String::from_utf16_lossy(&slice[..end]);
    let _ = GlobalUnlock(hg);
    Some(s)
}

unsafe fn read_dibv5() -> Option<Vec<u8>> {
    let h = GetClipboardData(CF_DIBV5).ok()?;
    let hg = HGLOBAL(h.0);
    let ptr = GlobalLock(hg);
    if ptr.is_null() { return None; }
    let len = GlobalSize(hg);
    let bytes = std::slice::from_raw_parts(ptr as *const u8, len).to_vec();
    let _ = GlobalUnlock(hg);
    Some(bytes)
}

unsafe fn read_hdrop() -> Option<Vec<String>> {
    let h = GetClipboardData(CF_HDROP).ok()?;
    let drop = HDROP(h.0);
    let hg = HGLOBAL(h.0);
    let ptr = GlobalLock(hg);
    if ptr.is_null() { return None; }
    let count = DragQueryFileW(drop, 0xFFFFFFFF, None);
    let mut paths = Vec::with_capacity(count as usize);
    for i in 0..count {
        let len = DragQueryFileW(drop, i, None) as usize;
        let mut buf = vec![0u16; len + 1];
        DragQueryFileW(drop, i, Some(&mut buf));
        paths.push(String::from_utf16_lossy(&buf[..len]));
    }
    let _ = GlobalUnlock(hg);
    Some(paths)
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

/// 查找 "HTML Format" 注册格式 id（每次进入剪贴板锁内调用，EnumClipboardFormats 方案的开销替代：直接注册比对）
unsafe fn find_html_format() -> Option<u32> {
    let mut buf: [u16; 32] = [0; 32];
    let name = wide_str("HTML Format", &mut buf);
    let fmt = RegisterClipboardFormatW(name);
    if fmt == 0 { None } else { Some(fmt) }
}

/// 读任意注册格式的原始字节（GlobalLock 拷贝）
unsafe fn read_raw_format(fmt: u32) -> Option<Vec<u8>> {
    let h = GetClipboardData(fmt).ok()?;
    let hg = HGLOBAL(h.0);
    let ptr = GlobalLock(hg);
    if ptr.is_null() { return None; }
    let len = GlobalSize(hg);
    let bytes = std::slice::from_raw_parts(ptr as *const u8, len).to_vec();
    let _ = GlobalUnlock(hg);
    Some(bytes)
}
