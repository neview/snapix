//! 控件级吸附：UI Automation 检测光标下最深层元素的可视边界。
//!
//! 难点：遮罩窗全屏置顶，`ElementFromPoint` 命中的永远是我们自己。
//! 解法：查询瞬间给遮罩窗加 `WS_EX_TRANSPARENT`（对命中测试透明），
//! 查询完立刻还原——间隙仅几毫秒，交互无感。
//!
//! 线程：前端每 ~90ms 轮询一次，命令为 `async`，实际查询在阻塞线程池上以 MTA
//! 公寓执行（UIA 客户端推荐模式），`IUIAutomation` 实例按线程缓存，不再每次 CoCreate。

use serde::Serialize;
use tauri::AppHandle;

use crate::util::blocking;

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ElementHit {
    pub x: i32,
    pub y: i32,
    pub w: u32,
    pub h: u32,
    pub name: String,
}

#[cfg(windows)]
fn element_at_impl(hwnd_raw: isize, geometry: crate::capture::FrameGeometry) -> Option<ElementHit> {
    use std::cell::RefCell;
    use windows::Win32::Foundation::{HWND, POINT};
    use windows::Win32::System::Com::{
        CoCreateInstance, CoInitializeEx, CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED,
    };
    use windows::Win32::UI::Accessibility::{CUIAutomation, IUIAutomation};
    use windows::Win32::UI::WindowsAndMessaging::{
        GetCursorPos, GetWindowLongW, SetWindowLongW, GWL_EXSTYLE, WS_EX_TRANSPARENT,
    };

    thread_local! {
        static AUTOMATION: RefCell<Option<IUIAutomation>> = const { RefCell::new(None) };
    }

    let (origin, _scale, dims) = geometry;
    let hwnd = HWND(hwnd_raw as *mut core::ffi::c_void);

    let mut pt = POINT::default();
    unsafe { GetCursorPos(&mut pt).ok()? };

    let (rect, name) = unsafe {
        // 阻塞线程池线程首次进入时初始化为 MTA；已初始化则返回 S_FALSE，忽略即可
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        let automation = AUTOMATION.with(|cell| -> Option<IUIAutomation> {
            let mut slot = cell.borrow_mut();
            if slot.is_none() {
                *slot = CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER).ok();
            }
            slot.clone()
        })?;

        let ex = GetWindowLongW(hwnd, GWL_EXSTYLE);
        let _ = SetWindowLongW(hwnd, GWL_EXSTYLE, ex | WS_EX_TRANSPARENT.0 as i32);
        let el = automation.ElementFromPoint(pt);
        let _ = SetWindowLongW(hwnd, GWL_EXSTYLE, ex);

        let el = el.ok()?;
        // 命中自家窗口（还原竞态）时放弃本次结果
        if el.CurrentProcessId().ok()? == std::process::id() as i32 {
            return None;
        }
        let rect = el.CurrentBoundingRectangle().ok()?;
        let name = el.CurrentName().map(|b| b.to_string()).unwrap_or_default();
        (rect, name)
    };

    // 屏幕绝对物理坐标 → 显示器相对，夹紧到冻屏帧内
    let (fw, fh) = (dims.0 as i32, dims.1 as i32);
    let x0 = (rect.left - origin.0).clamp(0, fw);
    let y0 = (rect.top - origin.1).clamp(0, fh);
    let x1 = (rect.right - origin.0).clamp(0, fw);
    let y1 = (rect.bottom - origin.1).clamp(0, fh);
    if x1 - x0 < 4 || y1 - y0 < 4 {
        return None;
    }

    Some(ElementHit {
        x: x0,
        y: y0,
        w: (x1 - x0) as u32,
        h: (y1 - y0) as u32,
        name: name.chars().take(40).collect(),
    })
}

#[cfg(not(windows))]
fn element_at_impl(
    _hwnd_raw: isize,
    _geometry: crate::capture::FrameGeometry,
) -> Option<ElementHit> {
    None
}

/// 光标下 UI 元素的可视边界（显示器相对物理像素）。
#[tauri::command]
pub async fn element_at(window: tauri::WebviewWindow, app: AppHandle) -> Option<ElementHit> {
    let geometry = crate::capture::frame_geometry(&app, window.label())?;
    #[cfg(windows)]
    let hwnd_raw = window.hwnd().ok()?.0 as isize;
    #[cfg(not(windows))]
    let hwnd_raw = 0isize;

    blocking(move || Ok::<_, String>(element_at_impl(hwnd_raw, geometry)))
        .await
        .ok()
        .flatten()
}
