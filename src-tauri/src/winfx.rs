//! 窗口视觉过渡的原生控制（Windows）。
//!
//! DWM 会给窗口的显示 / 隐藏套一段约 200ms 的缩放 + 淡出动画。对普通窗口这是好事，
//! 对全屏遮罩和贴图却是灾难：遮罩隐藏时整屏画面会「缩一下」，贴图显示时又「弹一下」，
//! 两个动画叠在一起就是用户感受到的抖动。这里把它们关掉，让切换变成瞬间完成的硬切。

#[cfg(windows)]
pub fn disable_transitions(window: &tauri::WebviewWindow) {
    use windows::core::BOOL;
    use windows::Win32::Foundation::HWND;
    use windows::Win32::Graphics::Dwm::{DwmSetWindowAttribute, DWMWA_TRANSITIONS_FORCEDISABLED};

    let Ok(hwnd) = window.hwnd() else {
        return;
    };
    let disabled = BOOL(1);
    let r = unsafe {
        DwmSetWindowAttribute(
            HWND(hwnd.0),
            DWMWA_TRANSITIONS_FORCEDISABLED,
            &disabled as *const BOOL as *const core::ffi::c_void,
            std::mem::size_of::<BOOL>() as u32,
        )
    };
    if let Err(e) = r {
        log::warn!("关闭窗口 {} 的 DWM 过渡动画失败: {e}", window.label());
    }
}

#[cfg(not(windows))]
pub fn disable_transitions(_window: &tauri::WebviewWindow) {}

/// 让窗口对截屏完全不可见（含本应用自己的截屏）。
///
/// 晾衣绳横在屏幕顶部，如果被拍进冻屏帧，用户每张截图顶上都会挂着几张旧图。
/// `WDA_EXCLUDEFROMCAPTURE` 让 DWM 在抓屏合成时直接跳过这个窗口——比自己
/// 「截前隐藏、截后恢复」可靠得多：没有隐藏/显示的时序竞态，也不会有动画残影。
#[cfg(windows)]
pub fn exclude_from_capture(window: &tauri::WebviewWindow) {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::WindowsAndMessaging::{
        SetWindowDisplayAffinity, WDA_EXCLUDEFROMCAPTURE,
    };

    let Ok(hwnd) = window.hwnd() else {
        return;
    };
    // 系统过旧（Win10 2004 之前）会失败：截图里能看到绳子，仅影响观感
    let _ = unsafe { SetWindowDisplayAffinity(HWND(hwnd.0), WDA_EXCLUDEFROMCAPTURE) };
}

#[cfg(not(windows))]
pub fn exclude_from_capture(_window: &tauri::WebviewWindow) {}

/// 标成工具窗（`WS_EX_TOOLWINDOW`）：不进 Alt+Tab、也不被「窗口吸附」枚举到。
///
/// Tauri 的 `skip_taskbar` 只是把窗口从任务栏移除（ITaskbarList::DeleteTab），
/// 并不会改扩展样式；晾衣绳横在屏幕顶部，若不标成工具窗，遮罩里悬停/点击会把
/// 这条 330px 的条窗当成可吸附窗口。
#[cfg(windows)]
pub fn mark_tool_window(window: &tauri::WebviewWindow) {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::WindowsAndMessaging::{
        GetWindowLongW, SetWindowLongW, GWL_EXSTYLE, WS_EX_TOOLWINDOW,
    };

    let Ok(hwnd) = window.hwnd() else {
        return;
    };
    unsafe {
        let ex = GetWindowLongW(HWND(hwnd.0), GWL_EXSTYLE) as u32;
        SetWindowLongW(HWND(hwnd.0), GWL_EXSTYLE, (ex | WS_EX_TOOLWINDOW.0) as i32);
    }
}

#[cfg(not(windows))]
pub fn mark_tool_window(_window: &tauri::WebviewWindow) {}

/// 无边框 + 阴影窗口的客户区相对窗口矩形的偏移：`(左, 上, 总宽差, 总高差)`，物理像素。
///
/// 阴影是靠保留 `WS_CAPTION` 让 DWM 画的，代价是窗口矩形比客户区多出一圈不可见边框
/// （100% 缩放下左右各 8px、上 1px、下 8px）。`set_position` 设的是窗口矩形，
/// 要让**内容**落在指定位置，必须先量出这圈偏移再反向补偿。
#[cfg(windows)]
pub fn frame_insets(window: &tauri::WebviewWindow) -> Option<(i32, i32, i32, i32)> {
    use windows::Win32::Foundation::{HWND, POINT, RECT};
    use windows::Win32::Graphics::Gdi::ClientToScreen;
    use windows::Win32::UI::WindowsAndMessaging::{GetClientRect, GetWindowRect};

    let hwnd = HWND(window.hwnd().ok()?.0);
    let mut win = RECT::default();
    let mut client = RECT::default();
    let mut origin = POINT::default();
    unsafe {
        GetWindowRect(hwnd, &mut win).ok()?;
        GetClientRect(hwnd, &mut client).ok()?;
        if !ClientToScreen(hwnd, &mut origin).as_bool() {
            return None;
        }
    }
    let dw = (win.right - win.left) - (client.right - client.left);
    let dh = (win.bottom - win.top) - (client.bottom - client.top);
    Some((origin.x - win.left, origin.y - win.top, dw, dh))
}

#[cfg(not(windows))]
pub fn frame_insets(_window: &tauri::WebviewWindow) -> Option<(i32, i32, i32, i32)> {
    None
}
