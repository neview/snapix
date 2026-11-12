//! 智能窗口吸附：按 z 序（顶层在前）枚举可见窗口的「可视边界」。
//! 使用 DWMWA_EXTENDED_FRAME_BOUNDS 获取物理像素级的真实可见矩形
//! （GetWindowRect 会包含不可见的阴影边框，导致吸附框偏大）。

use serde::Serialize;

/// 一个可吸附窗口（虚拟桌面物理像素坐标）。
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SnapWindow {
    pub title: String,
    pub x: i32,
    pub y: i32,
    pub w: u32,
    pub h: u32,
}

#[cfg(windows)]
pub fn list_windows() -> Vec<SnapWindow> {
    use windows::core::BOOL;
    use windows::Win32::Foundation::{HWND, LPARAM, RECT};
    use windows::Win32::Graphics::Dwm::{
        DwmGetWindowAttribute, DWMWA_CLOAKED, DWMWA_EXTENDED_FRAME_BOUNDS,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        EnumWindows, GetWindowLongW, GetWindowTextW, IsIconic, IsWindowVisible, GWL_EXSTYLE,
        WS_EX_TOOLWINDOW,
    };

    unsafe extern "system" fn enum_cb(hwnd: HWND, lparam: LPARAM) -> BOOL {
        let out = unsafe { &mut *(lparam.0 as *mut Vec<SnapWindow>) };

        unsafe {
            if !IsWindowVisible(hwnd).as_bool() || IsIconic(hwnd).as_bool() {
                return BOOL(1);
            }

            // 工具窗（浮动面板 / 贴图窗等）不作为吸附目标
            let ex = GetWindowLongW(hwnd, GWL_EXSTYLE) as u32;
            if ex & WS_EX_TOOLWINDOW.0 != 0 {
                return BOOL(1);
            }

            // UWP 挂起等「隐身」窗口
            let mut cloaked: u32 = 0;
            let _ = DwmGetWindowAttribute(
                hwnd,
                DWMWA_CLOAKED,
                &mut cloaked as *mut _ as *mut _,
                std::mem::size_of::<u32>() as u32,
            );
            if cloaked != 0 {
                return BOOL(1);
            }

            // 可视边界（物理像素）
            let mut rect = RECT::default();
            if DwmGetWindowAttribute(
                hwnd,
                DWMWA_EXTENDED_FRAME_BOUNDS,
                &mut rect as *mut _ as *mut _,
                std::mem::size_of::<RECT>() as u32,
            )
            .is_err()
            {
                return BOOL(1);
            }
            let w = rect.right - rect.left;
            let h = rect.bottom - rect.top;
            if w < 8 || h < 8 {
                return BOOL(1);
            }

            let mut buf = [0u16; 256];
            let len = GetWindowTextW(hwnd, &mut buf) as usize;
            let title = String::from_utf16_lossy(&buf[..len]);

            out.push(SnapWindow {
                title,
                x: rect.left,
                y: rect.top,
                w: w as u32,
                h: h as u32,
            });
        }
        BOOL(1)
    }

    let mut windows: Vec<SnapWindow> = Vec::new();
    unsafe {
        // EnumWindows 按 z 序自顶向下回调
        let _ = EnumWindows(Some(enum_cb), LPARAM(&mut windows as *mut _ as isize));
    }
    windows
}

#[cfg(not(windows))]
pub fn list_windows() -> Vec<SnapWindow> {
    Vec::new()
}

/// 前台（活动）窗口的可视边界（虚拟桌面物理像素）。
#[cfg(windows)]
pub fn foreground_window() -> Option<SnapWindow> {
    use windows::Win32::Foundation::RECT;
    use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_EXTENDED_FRAME_BOUNDS};
    use windows::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowTextW, IsIconic};

    unsafe {
        let hwnd = GetForegroundWindow();
        if hwnd.is_invalid() || IsIconic(hwnd).as_bool() {
            return None;
        }
        let mut rect = RECT::default();
        DwmGetWindowAttribute(
            hwnd,
            DWMWA_EXTENDED_FRAME_BOUNDS,
            &mut rect as *mut _ as *mut _,
            std::mem::size_of::<RECT>() as u32,
        )
        .ok()?;
        let w = rect.right - rect.left;
        let h = rect.bottom - rect.top;
        if w < 8 || h < 8 {
            return None;
        }
        let mut buf = [0u16; 256];
        let len = GetWindowTextW(hwnd, &mut buf) as usize;
        Some(SnapWindow {
            title: String::from_utf16_lossy(&buf[..len]),
            x: rect.left,
            y: rect.top,
            w: w as u32,
            h: h as u32,
        })
    }
}

#[cfg(not(windows))]
pub fn foreground_window() -> Option<SnapWindow> {
    None
}
