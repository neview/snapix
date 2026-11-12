//! 贴图置顶：窗口创建、图像装载、旋转翻转、穿透与关闭。
//!
//! 从 `capture` 拆出的独立模块：贴图有独立的状态机与窗口生命周期

use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Mutex;

use image::RgbaImage;
use serde::Serialize;
use tauri::{
    AppHandle, Emitter, Manager, PhysicalPosition, PhysicalSize, WebviewUrl, WebviewWindowBuilder,
};

use xcap::Monitor;

use crate::capture::{
    crop_with_origin, frame_geometry, hide_overlay, on_main_thread, save_output, write_clipboard,
    ImagePayload,
};
use crate::editor::{decode_data_url, open_editor_image};
use crate::lock;
use crate::settings;
use crate::util::blocking;

// ===================== 贴图置顶 =====================

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PinPresentation {
    Automatic,
    Deferred { decoded: bool },
}

impl PinPresentation {
    /// true 仅表示普通贴图应该自动显示；准备贴图只改变解码状态。
    fn decoded(&mut self) -> bool {
        match self {
            Self::Automatic => true,
            Self::Deferred { decoded } => {
                *decoded = true;
                false
            }
        }
    }

    fn can_reveal(self) -> bool {
        matches!(self, Self::Automatic | Self::Deferred { decoded: true })
    }
}

/// 隐藏建窗与显示始终共用这一份物理客户区尺寸。
struct PendingPin {
    pos: Option<(i32, i32)>,
    size: (u32, u32),
    presentation: PinPresentation,
    hide_overlay: bool,
    on_ready: Option<std::sync::mpsc::Sender<()>>,
}

#[derive(Default)]
pub struct PinState {
    pub images: Mutex<HashMap<String, (RgbaImage, f32)>>,
    pending: Mutex<HashMap<String, PendingPin>>,
    display_sizes: Mutex<HashMap<String, (u32, u32)>>,
    standby: Mutex<Option<String>>,
    /// 从晾衣绳取下的贴图：label → 原条目沿绳位置（`None` = 原本走自动排布）。
    /// 存在即表示这张贴图可以「挂回」绳子。
    pub line_returns: Mutex<HashMap<String, Option<f64>>>,
    pub counter: AtomicU32,
}

fn pin_display_size(original: (u32, u32), explicit: Option<(u32, u32)>) -> (u32, u32) {
    let (w, h) = explicit.unwrap_or(original);
    (w.max(1), h.max(1))
}

pub(crate) fn reserve_pin_label(app: &AppHandle) -> String {
    let n = app
        .state::<PinState>()
        .counter
        .fetch_add(1, Ordering::SeqCst)
        + 1;
    format!("pin-{n}")
}

pub(crate) fn reserve_deferred_pin_label(app: &AppHandle) -> String {
    let standby = lock(&app.state::<PinState>().standby).take();
    standby
        .filter(|label| app.get_webview_window(label).is_some())
        .unwrap_or_else(|| reserve_pin_label(app))
}

pub(crate) fn prewarm_pin(app: &AppHandle) {
    let app = app.clone();
    std::thread::spawn(move || {
        // 交接结束后再补充空窗，不占用正在飞行的主线程。
        std::thread::sleep(std::time::Duration::from_millis(250));
        let handle = app.clone();
        let _ = app.run_on_main_thread(move || {
            if !settings::current(&handle).line_enabled {
                return;
            }
            let state = handle.state::<PinState>();
            if lock(&state.standby).is_some() {
                return;
            }
            let label = reserve_pin_label(&handle);
            if build_pin_window(&handle, &label, (120, 80), 1.0, true).is_ok() {
                *lock(&state.standby) = Some(label);
            }
        });
    });
}

fn build_pin_window(
    app: &AppHandle,
    label: &str,
    size: (u32, u32),
    scale: f32,
    standby: bool,
) -> Result<tauri::WebviewWindow, String> {
    let url = if standby {
        "pin.html?preload=1"
    } else {
        "pin.html"
    };
    let win = WebviewWindowBuilder::new(app, label, WebviewUrl::App(url.into()))
        .additional_browser_args(crate::BROWSER_ARGS)
        .visible(false)
        .focused(false)
        .decorations(false)
        .transparent(true)
        .always_on_top(true)
        .skip_taskbar(true)
        .resizable(false)
        .shadow(true)
        .inner_size(size.0 as f64 / scale as f64, size.1 as f64 / scale as f64)
        .build()
        .map_err(|e| e.to_string())?;
    crate::winfx::disable_transitions(&win);
    Ok(win)
}

/// 贴图窗口从建窗到前端报告就绪的最长等待；超时则强制显示，避免遮罩一直挂着。
const PIN_REVEAL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);

/// 创建无边框置顶贴图窗（pos 为虚拟桌面物理坐标；None = 由系统放置）。
pub fn spawn_pin(
    app: &AppHandle,
    img: RgbaImage,
    pos: Option<(i32, i32)>,
    scale: f32,
) -> Result<(), String> {
    create_pin(app, img, pos, scale, false, None).map(|_| ())
}

/// 创建贴图窗并返回其标签。
///
/// 窗口先隐藏创建并摆到目标位置，等前端图片渲染完调 `pin_ready` 再显示：
/// 若按默认可见建窗，窗口会先在系统默认位置以创建尺寸闪现一下、再跳到选区位置，
/// 最后图片才加载出来——即用户看到的「先出现在别处再挪回去」。
pub(crate) fn create_pin(
    app: &AppHandle,
    img: RgbaImage,
    pos: Option<(i32, i32)>,
    scale: f32,
    hide_overlay: bool,
    on_ready: Option<std::sync::mpsc::Sender<()>>,
) -> Result<String, String> {
    let label = reserve_pin_label(app);
    on_main_thread(app, move |app| {
        create_pin_now(app, label, img, pos, scale, None, hide_overlay, on_ready)
    })
}

/// 仅在主线程使用：调用方先验证准备事务仍存活，再创建其预留 label。
pub(crate) fn create_deferred_pin_now(
    app: &AppHandle,
    label: String,
    img: RgbaImage,
    rect: (i32, i32, u32, u32),
    on_ready: std::sync::mpsc::Sender<()>,
) -> Result<String, String> {
    let (x, y, w, h) = rect;
    let scale = app
        .monitor_from_point(x as f64, y as f64)
        .ok()
        .flatten()
        .map(|m| m.scale_factor() as f32)
        .unwrap_or(1.0);
    create_pin_now(
        app,
        label,
        img,
        Some((x, y)),
        scale,
        Some((w, h)),
        false,
        Some(on_ready),
    )
}

#[allow(clippy::too_many_arguments)]
fn create_pin_now(
    app: &AppHandle,
    label: String,
    img: RgbaImage,
    pos: Option<(i32, i32)>,
    scale: f32,
    display_size: Option<(u32, u32)>,
    hide_overlay: bool,
    on_ready: Option<std::sync::mpsc::Sender<()>>,
) -> Result<String, String> {
    let (w, h) = pin_display_size(img.dimensions(), display_size);
    let scale = scale.max(0.1);
    let state = app.state::<PinState>();
    lock(&state.images).insert(label.clone(), (img, scale));
    if display_size.is_some() {
        lock(&state.display_sizes).insert(label.clone(), (w, h));
    }
    lock(&state.pending).insert(
        label.clone(),
        PendingPin {
            pos,
            size: (w, h),
            presentation: if display_size.is_some() {
                PinPresentation::Deferred { decoded: false }
            } else {
                PinPresentation::Automatic
            },
            hide_overlay,
            on_ready,
        },
    );
    let result = (|| {
        let existing = app.get_webview_window(&label);
        let reused = existing.is_some();
        let win = match existing {
            Some(win) => win,
            None => build_pin_window(app, &label, (w, h), scale, false)?,
        };
        match pos {
            Some((x, y)) => place_pin_content(&win, x, y, w, h)?,
            None => {
                win.set_size(PhysicalSize::new(w, h))
                    .map_err(|e| e.to_string())?;
                win.center().map_err(|e| e.to_string())?;
            }
        }
        if reused {
            app.emit_to(&label, "pin:load", ())
                .map_err(|e| e.to_string())?;
        }
        Ok(label.clone())
    })();
    if result.is_err() {
        let _ = close_pin_now(app, &label);
    }
    result
}

/// 把贴图窗摆到让**内容**（客户区）左上角正好落在物理坐标 `(x, y)`、尺寸 `w×h` 的位置。
///
/// 带阴影的无边框窗口，窗口矩形比客户区多出一圈不可见边框（左 8 / 上 1 / 右 8 / 下 8 物理
/// 像素，随 DPI 缩放）。直接 `set_position(x, y)` 摆的是窗口矩形，内容会整体偏右下约 8px，
/// 与遮罩上的选区错位——遮罩一收，画面就「跳」了一下。这里先移到目标屏（进入其 DPI 上下文）
/// 让边框尺寸稳定，再量出偏移反向补偿。
fn place_pin_content(
    win: &tauri::WebviewWindow,
    x: i32,
    y: i32,
    w: u32,
    h: u32,
) -> Result<(), String> {
    win.set_position(PhysicalPosition::new(x, y))
        .map_err(|e| e.to_string())?;
    win.set_size(PhysicalSize::new(w, h))
        .map_err(|e| e.to_string())?;
    if let Some((left, top, _, _)) = crate::winfx::frame_insets(win) {
        win.set_position(PhysicalPosition::new(x - left, y - top))
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// 显示一个待显示的贴图窗：再次断言几何（跨 DPI 移动时隐藏态的尺寸可能被系统重算）、
/// 取焦点，然后按需收起遮罩并通知 `pin_region`。只对首次调用生效——
/// 旋转 / 翻转后图片重新加载、或超时兜底与 `pin_ready` 竞争时，后到的一方是空操作。
///
/// 关键：所有窗口操作（show / 定位 / hide_overlay / set_focus）与就绪信号都在一次
/// `run_on_main_thread` 闭包里执行。show/hide 这类操作若从 tokio / 阻塞线程池直接调用，
/// 在 Windows 上会偶发与 WebView2 互锁——此后遮罩不消失、贴图不出现、所有 IPC 静默失效
/// （即用户反馈的「点固定后什么都点不了」）。放到主线程串行执行可根除该竞态。
pub(crate) fn reveal_pin(app: &AppHandle, label: &str) {
    let handle = app.clone();
    let label = label.to_string();
    let _ = app.run_on_main_thread(move || {
        // 普通贴图的超时兜底不能误显示一个 deferred 窗口。
        let automatic = handle.try_state::<PinState>().is_some_and(|state| {
            lock(&state.pending)
                .get(&label)
                .is_some_and(|p| p.presentation == PinPresentation::Automatic)
        });
        if automatic && reveal_pin_now(&handle, &label).is_err() {
            // 普通 pin 的最后超时兜底：即使窗口无法显示，也必须归还桌面。
            let should_hide = handle.try_state::<PinState>().is_some_and(|state| {
                lock(&state.pending)
                    .get(&label)
                    .is_some_and(|p| p.hide_overlay)
            });
            if should_hide {
                hide_overlay(&handle);
            }
        }
    });
}

/// 主线程内返回真实操作结果；失败仍保留 pending，交给取消 / 超时回收。
pub(crate) fn reveal_pin_now(app: &AppHandle, label: &str) -> Result<(), String> {
    let state = app.try_state::<PinState>().ok_or("贴图状态缺失")?;
    let (pos, (w, h), presentation) = {
        let pending = lock(&state.pending);
        let p = pending.get(label).ok_or("贴图没有待显示事务")?;
        if !p.presentation.can_reveal() {
            return Err("贴图尚未解码完成".into());
        }
        (p.pos, p.size, p.presentation)
    };
    let win = app.get_webview_window(label).ok_or("贴图窗口不存在")?;
    match pos {
        Some((x, y)) => place_pin_content(&win, x, y, w, h)?,
        None => {
            win.set_size(PhysicalSize::new(w, h))
                .map_err(|e| e.to_string())?;
            win.center().map_err(|e| e.to_string())?;
        }
    }
    win.show().map_err(|e| e.to_string())?;
    if matches!(presentation, PinPresentation::Deferred { .. }) {
        win.set_always_on_top(false).map_err(|e| e.to_string())?;
        win.set_always_on_top(true).map_err(|e| e.to_string())?;
    }
    // Windows 可以拒绝抢焦点；这不代表已经成功的定位 / show / 置顶失败。
    let _ = win.set_focus();
    let pending = lock(&state.pending).remove(label);
    if let Some(pending) = pending {
        if pending.hide_overlay {
            hide_overlay(app);
        }
        if let Some(tx) = pending.on_ready {
            let _ = tx.send(());
        }
    }
    Ok(())
}

/// deferred 只通知解码完成，绝不自动 show；普通截图贴图仍自动显示。
#[tauri::command]
pub async fn pin_ready(window: tauri::WebviewWindow, app: AppHandle) {
    let label = window.label().to_string();
    let handle = app.clone();
    let _ = app.run_on_main_thread(move || {
        let Some(state) = handle.try_state::<PinState>() else {
            return;
        };
        let (automatic, tx) = {
            let mut pending = lock(&state.pending);
            let Some(p) = pending.get_mut(&label) else {
                return;
            };
            let automatic = p.presentation.decoded();
            (automatic, if automatic { None } else { p.on_ready.take() })
        };
        if let Some(tx) = tx {
            let _ = tx.send(());
        }
        if automatic {
            let _ = reveal_pin_now(&handle, &label);
        }
    });
}

/// 裁剪选区 → 原位生成贴图窗。
///
/// 遮罩不在这里收：先建贴图窗，等它渲染完在选区原位显示后再收遮罩（见 `reveal_pin`），
/// 否则中间几百毫秒会露出真实屏幕、再弹出贴图——即用户看到的「闪一下」。
/// 命令本身等到贴图显示后才返回，前端据此才释放冻屏帧。
#[tauri::command]
pub async fn pin_region(
    window: tauri::WebviewWindow,
    app: AppHandle,
    x: i32,
    y: i32,
    width: u32,
    height: u32,
) -> Result<(), String> {
    let label = window.label().to_string();
    blocking(move || {
        let (cropped, abs_x, abs_y) =
            crop_with_origin(&app, &label, x, y, width, height).ok_or("无效的选区")?;
        let scale = frame_geometry(&app, &label)
            .map(|(_, s, _)| s)
            .unwrap_or(1.0);

        crate::history::record(&app, cropped.clone(), "capture");
        let (tx, rx) = std::sync::mpsc::channel();
        let pin = create_pin(&app, cropped, Some((abs_x, abs_y)), scale, true, Some(tx))
            .inspect_err(|e| {
                log::warn!("贴图窗口创建失败: {e}");
                hide_overlay(&app);
            })?;
        // 贴图页面迟迟不报就绪（加载异常）也要兜底显示并收遮罩，不能让全屏遮罩挂着
        if rx.recv_timeout(PIN_REVEAL_TIMEOUT).is_err() {
            reveal_pin(&app, &pin);
        }
        Ok(())
    })
    .await
}

/// 贴图窗口挂载时获取自己的图（不移除，窗口存续期间可反复取）。
#[tauri::command]
pub fn get_pin_image(app: AppHandle, label: String) -> Option<ImagePayload> {
    let state = app.try_state::<PinState>()?;
    let display = lock(&state.display_sizes).get(&label).copied();
    let from_line = lock(&state.line_returns).contains_key(&label);
    let guard = lock(&state.images);
    let (img, scale) = guard.get(&label)?;
    Some(ImagePayload {
        image_url: crate::shot_url(&format!("pin/{label}")),
        width: img.width(),
        height: img.height(),
        scale: *scale,
        display_width: display.map(|d| d.0),
        display_height: display.map(|d| d.1),
        from_line,
    })
}

/// 贴图图像 BMP（供 shot://pin/{label}）。
pub fn pin_bmp(app: &AppHandle, label: &str) -> Option<Vec<u8>> {
    let state = app.try_state::<PinState>()?;
    let guard = lock(&state.images);
    let (img, _) = guard.get(label)?;
    Some(crate::fastimg::rgba_to_bmp(img))
}

/// 取某贴图位图的副本。
fn pin_image(app: &AppHandle, label: &str) -> Result<RgbaImage, String> {
    let state = app.try_state::<PinState>().ok_or("贴图状态缺失")?;
    let guard = lock(&state.images);
    guard
        .get(label)
        .map(|(i, _)| i.clone())
        .ok_or_else(|| "贴图不存在".to_string())
}

/// 复制贴图内容。
#[tauri::command]
pub async fn pin_copy(app: AppHandle, label: String) -> Result<(), String> {
    blocking(move || {
        let img = pin_image(&app, &label)?;
        write_clipboard(&app, &img)
    })
    .await
}

/// 另存贴图内容。
#[tauri::command]
pub async fn pin_save(
    window: tauri::WebviewWindow,
    app: AppHandle,
    label: String,
) -> Result<Option<String>, String> {
    blocking(move || {
        let img = pin_image(&app, &label)?;
        let saved = save_output(&app, &img, Some(&window))?;
        Ok(saved.map(|p| p.to_string_lossy().to_string()))
    })
    .await
}

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PinDims {
    pub width: u32,
    pub height: u32,
}

/// 就地修改贴图位图，并返回新尺寸供前端刷新缩放基准。
fn pin_mutate(
    app: &AppHandle,
    label: &str,
    f: impl FnOnce(&RgbaImage) -> RgbaImage,
) -> Result<PinDims, String> {
    let state = app.try_state::<PinState>().ok_or("贴图状态缺失")?;
    let mut guard = lock(&state.images);
    let entry = guard.get_mut(label).ok_or("贴图不存在")?;
    let next = f(&entry.0);
    let dims = PinDims {
        width: next.width(),
        height: next.height(),
    };
    entry.0 = next;
    Ok(dims)
}

/// 贴图顺时针旋转 90°。
#[tauri::command]
pub async fn pin_rotate(app: AppHandle, label: String) -> Result<PinDims, String> {
    blocking(move || pin_mutate(&app, &label, image::imageops::rotate90)).await
}

/// 贴图水平镜像翻转。
#[tauri::command]
pub async fn pin_flip(app: AppHandle, label: String) -> Result<PinDims, String> {
    blocking(move || pin_mutate(&app, &label, image::imageops::flip_horizontal)).await
}

/// 贴图开启鼠标穿透（点击落到下层窗口；解除需托盘「解除贴图鼠标穿透」）。
/// `async`：窗口属性操作不能在 WebView2 的 IPC 回调里同步做（见 lib.rs 顶部说明）。
#[tauri::command]
pub async fn pin_passthrough(app: AppHandle, label: String) -> Result<(), String> {
    blocking(move || {
        let win = app.get_webview_window(&label).ok_or("贴图窗口不存在")?;
        win.set_ignore_cursor_events(true)
            .map_err(|e| e.to_string())
    })
    .await
}

/// 解除全部贴图的鼠标穿透。
#[tauri::command]
pub fn pins_passthrough_off(app: AppHandle) {
    for (label, win) in app.webview_windows() {
        if label.starts_with("pin-") {
            let _ = win.set_ignore_cursor_events(false);
        }
    }
}

/// 把贴图内容送进标注编辑器（贴图保留）。
#[tauri::command]
pub async fn pin_annotate(app: AppHandle, label: String) -> Result<(), String> {
    blocking(move || {
        let img = pin_image(&app, &label)?;
        open_editor_image(&app, img)
    })
    .await
}

/// 光标附近的贴图落点与所在屏缩放（贴剪贴板图 / 标注成图时使用）。
fn pin_spot_at_cursor(app: &AppHandle) -> (Option<(i32, i32)>, f32) {
    let cursor = app.cursor_position().ok();
    let pos = cursor.map(|p| (p.x as i32 + 16, p.y as i32 + 16));
    let scale = cursor
        .and_then(|p| Monitor::from_point(p.x as i32, p.y as i32).ok())
        .and_then(|m| m.scale_factor().ok())
        .unwrap_or(1.0);
    (pos, scale)
}

/// 把标注成图（data URL）直接生成贴图（编辑器「贴图」按钮，贴在光标旁）。
#[tauri::command]
pub async fn pin_data(app: AppHandle, data_url: String) -> Result<(), String> {
    blocking(move || {
        let img = decode_data_url(&data_url)?;
        let (pos, scale) = pin_spot_at_cursor(&app);
        spawn_pin(&app, img, pos, scale)
    })
    .await
}

/// 读取剪贴板位图。
fn clipboard_image(app: &AppHandle) -> Result<RgbaImage, String> {
    use tauri_plugin_clipboard_manager::ClipboardExt;
    let clip = app
        .clipboard()
        .read_image()
        .map_err(|_| "剪贴板中没有图片")?;
    let (w, h) = (clip.width(), clip.height());
    RgbaImage::from_raw(w, h, clip.rgba().to_vec()).ok_or_else(|| "剪贴板图像数据异常".into())
}

/// 把剪贴板中的图片送进标注编辑器（托盘 / 快捷键 / 命令共用）。
pub fn edit_clipboard_impl(app: &AppHandle) -> Result<(), String> {
    let img = clipboard_image(app)?;
    open_editor_image(app, img)
}

/// 把剪贴板中的图片送进标注编辑器。
#[tauri::command]
pub async fn edit_clipboard(app: AppHandle) -> Result<(), String> {
    blocking(move || edit_clipboard_impl(&app)).await
}

/// 把剪贴板中的图片贴为置顶贴图（光标附近；托盘 / 快捷键 / 命令共用）。
pub fn pin_clipboard_impl(app: &AppHandle) -> Result<(), String> {
    let img = clipboard_image(app)?;
    let (pos, scale) = pin_spot_at_cursor(app);
    spawn_pin(app, img, pos, scale)
}

/// 把剪贴板中的图片贴为置顶贴图。
#[tauri::command]
pub async fn pin_clipboard(app: AppHandle) -> Result<(), String> {
    blocking(move || pin_clipboard_impl(&app)).await
}

/// 一键隐藏 / 显示全部贴图窗口。
#[tauri::command]
pub fn toggle_pins(app: AppHandle) {
    let pins: Vec<_> = app
        .webview_windows()
        .into_iter()
        .filter(|(label, _)| {
            label.starts_with("pin-")
                && !app.try_state::<PinState>().is_some_and(|state| {
                    lock(&state.pending).contains_key(label)
                        || !lock(&state.images).contains_key(label)
                })
        })
        .collect();
    if pins.is_empty() {
        return;
    }
    let any_visible = pins.iter().any(|(_, w)| w.is_visible().unwrap_or(false));
    for (_, w) in pins {
        let _ = if any_visible { w.hide() } else { w.show() };
    }
}

/// 关闭某个贴图窗口并释放其图像（只允许操作 pin-* 窗口）。
/// `async`：可能连带收起遮罩窗，窗口操作不能在 WebView2 回调内同步执行（见 lib.rs 说明）。
#[tauri::command]
pub async fn close_pin(app: AppHandle, label: String) -> Result<(), String> {
    blocking(move || on_main_thread(&app, move |app| close_pin_now(app, &label))).await
}

pub(crate) fn close_pin_now(app: &AppHandle, label: &str) -> Result<(), String> {
    if !label.starts_with("pin-") {
        return Err("不是贴图窗口".into());
    }
    if let Some(w) = app.get_webview_window(label) {
        w.close().map_err(|e| e.to_string())?;
    }
    if let Some(state) = app.try_state::<PinState>() {
        {
            let mut standby = lock(&state.standby);
            if standby.as_deref() == Some(label) {
                *standby = None;
            }
        }
        lock(&state.images).remove(label);
        lock(&state.display_sizes).remove(label);
        lock(&state.line_returns).remove(label);
        let pending = lock(&state.pending).remove(label);
        // 丢弃 Sender 立即唤醒准备线程；不持锁调用窗口方法。
        if pending.is_some_and(|p| p.hide_overlay) {
            hide_overlay(app);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn automatic_pins_keep_original_physical_size() {
        // DPI/scale 不参与物理尺寸计算，普通截图 / 历史贴图仍是原图大小。
        assert_eq!(pin_display_size((1600, 900), None), (1600, 900));
    }

    #[test]
    fn deferred_pins_keep_explicit_physical_size_instead_of_original() {
        assert_eq!(pin_display_size((4000, 3000), Some((801, 601))), (801, 601));
        assert_eq!(pin_display_size((4000, 3000), Some((0, 0))), (1, 1));
    }

    #[test]
    fn deferred_decode_never_requests_automatic_show() {
        let mut deferred = PinPresentation::Deferred { decoded: false };
        assert!(!deferred.can_reveal());
        assert!(!deferred.decoded());
        assert!(deferred.can_reveal());
        assert!(!deferred.decoded());
        let mut automatic = PinPresentation::Automatic;
        assert!(automatic.can_reveal()); // 普通 pin 的现有超时兜底
        assert!(automatic.decoded());
    }
}
