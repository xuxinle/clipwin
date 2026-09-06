// Prevents additional console window on Windows in release, DO NOT REMOVE!!
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    // PerMonitorV2 DPI 感知：所有 Win32 坐标（光标/窗口矩形/虚拟屏）使用真实物理像素，
    // 避免 DPI 虚拟化导致的位置记忆/光标定位错乱（多显示器混合缩放尤其必须）
    unsafe {
        use windows::Win32::UI::HiDpi::{SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2};
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
    clipwin_lib::run()
}
