//! 截图内核：冻屏抓取、选区裁剪、复制/保存/贴图/编辑器分发。
//!
//! 数据流（v0.3）：整屏帧不再走 base64——Rust 持有 RgbaImage，
//! 前端通过 `shot://frame/{id}` 拉取零压缩 BMP（毫秒级编码）；
//! 前端图像 onload 后调 `overlay_ready` 才显示遮罩窗，杜绝旧帧闪烁。
//!
//! 线程模型：同步 `#[tauri::command]` 运行在主 UI 线程，因此所有可能耗时的命令
//! （文件对话框、剪贴板大图编码、PNG/base64 解码、OCR、二维码）都写成
//! `async fn` 并通过 `util::blocking` 下放到阻塞线程池；托盘 / 全局快捷键
//! 回调复用同名 `*_impl` 同步函数。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::Mutex;

use image::RgbaImage;
use serde::Serialize;
use tauri::{
    AppHandle, Emitter, Manager, PhysicalPosition, PhysicalSize, WebviewUrl, WebviewWindowBuilder,
};
use xcap::Monitor;

use crate::lock;
use crate::settings;
use crate::snap::{self, SnapWindow};
use crate::util::blocking;

const OVERLAY_PREFIX: &str = "overlay";

/// 主窗口收进托盘后等待合成器刷新的时间：`hide()` 只是提交请求，DWM 要到下一帧
/// 才真正把窗口从屏幕上拿掉，立刻抓屏仍会把主窗口拍进冻屏帧。
const HIDE_SETTLE_MS: u64 = 150;

/// 主窗口在前台时收进托盘，避免它出现在截图里。
/// 返回是否需要等待合成器刷新（窗口原本可见且未最小化）。
fn hide_main_for_capture(app: &AppHandle) -> bool {
    let Some(win) = app.get_webview_window("main") else {
        return false;
    };
    if !win.is_visible().unwrap_or(false) {
        return false;
    }
    let on_screen = !win.is_minimized().unwrap_or(false);
    win.hide().is_ok() && on_screen
}

/// 一屏冻屏抓取：整屏像素 + 显示器物理原点 + 缩放 + 帧号。
pub struct Frame {
    pub id: u64,
    pub image: RgbaImage,
    pub origin: (i32, i32),
    pub scale: f32,
}

/// 上一次选区（用于「重复上次区域」）。
#[derive(Clone)]
pub struct LastRegion {
    pub monitor_point: (i32, i32),
    pub rect: SelRect,
}

/// 多显示器：每屏一个遮罩窗（overlay-0 / overlay-1 …），各持一帧。
#[derive(Default)]
pub struct CaptureState {
    pub frames: Mutex<HashMap<String, Frame>>,
    /// 各遮罩窗的待取载荷（窗口挂载晚于 emit 时由前端主动拉取）
    pub pending: Mutex<HashMap<String, OverlayPayload>>,
    pub last: Mutex<Option<LastRegion>>,
    frame_seq: AtomicU64,
    /// 抓屏准备中门闩：防止连按快捷键时并发抓屏
    preparing: AtomicBool,
}

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SelRect {
    pub x: i32,
    pub y: i32,
    pub w: u32,
    pub h: u32,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OverlayPayload {
    /// 目标遮罩窗标签（前端校验，防事件串扰到别的屏）
    label: String,
    frame_id: u64,
    frame_url: String,
    width: u32,
    height: u32,
    scale: f32,
    /// 可吸附窗口（显示器相对物理坐标，z 序自顶向下）
    windows: Vec<SnapWindow>,
    /// 预选区域（重复上次截图时携带）
    preselect: Option<SelRect>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImagePayload {
    pub image_url: String,
    pub width: u32,
    pub height: u32,
    pub scale: f32,
    /// 可选的初始客户区物理尺寸；scale 仍表示屏幕 DPI，不是图片 zoom。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_width: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_height: Option<u32>,
    /// 这张贴图是从晾衣绳取下来的：前端据此把「双击」从关闭改成挂回绳子。
    #[serde(default)]
    pub from_line: bool,
}

/// 挂到晾衣绳后的落点（遮罩窗相对 CSS 像素），供遮罩播放「飞过去」的过渡。
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LineLanding {
    pub id: String,
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

/// 帧几何：显示器原点、缩放、帧尺寸（物理像素）。
pub type FrameGeometry = ((i32, i32), f32, (u32, u32));

/// 列出全部遮罩窗口。
fn overlay_windows(app: &AppHandle) -> Vec<(String, tauri::WebviewWindow)> {
    app.webview_windows()
        .into_iter()
        .filter(|(label, _)| label.starts_with(OVERLAY_PREFIX))
        .collect()
}

fn overlay_visible(app: &AppHandle) -> bool {
    overlay_windows(app)
        .iter()
        .any(|(_, w)| w.is_visible().unwrap_or(false))
}

/// 确保每个显示器都有一个（隐藏的）遮罩窗，多余的关闭。
/// 启动时预创建，捕获时校正（应对显示器增减）。
pub fn ensure_overlay_windows(app: &AppHandle, count: usize) {
    for i in 0..count {
        let label = format!("{OVERLAY_PREFIX}-{i}");
        if app.get_webview_window(&label).is_none() {
            let built =
                WebviewWindowBuilder::new(app, &label, WebviewUrl::App("overlay.html".into()))
                    .additional_browser_args(crate::BROWSER_ARGS)
                    .visible(false)
                    .transparent(true)
                    .decorations(false)
                    .always_on_top(true)
                    .skip_taskbar(true)
                    .resizable(false)
                    .shadow(false)
                    .focused(false)
                    .build();
            // 全屏遮罩显示 / 隐藏必须是硬切：DWM 默认的缩放淡出动画会让整屏画面「缩一下」
            match built {
                Ok(win) => crate::winfx::disable_transitions(&win),
                Err(e) => eprintln!("[snapix] 遮罩窗 {label} 创建失败: {e}"),
            }
        }
    }
    for (label, win) in overlay_windows(app) {
        let idx: Option<usize> = label
            .strip_prefix(OVERLAY_PREFIX)
            .and_then(|s| s.strip_prefix('-'))
            .and_then(|s| s.parse().ok());
        if idx.map(|i| i >= count).unwrap_or(true) {
            let _ = win.close();
        }
    }
}

/// 抓取所有屏 → 每屏存帧 → 通知各遮罩窗拉帧（前端 ready 后逐窗显示）。
pub fn show_overlay(app: &AppHandle) {
    show_overlay_impl(app, None, None);
}

/// 重复上次区域：预选选区送到上次所在显示器的遮罩窗。
pub fn repeat_last_capture(app: &AppHandle) {
    let last = app
        .try_state::<CaptureState>()
        .and_then(|s| lock(&s.last).clone());
    match last {
        Some(l) => show_overlay_impl(app, Some(l.monitor_point), Some(l.rect)),
        None => show_overlay_impl(app, None, None),
    }
}

/// 一屏抓屏结果 + 待主线程执行的窗口布置任务。
struct OverlayJob {
    label: String,
    x: i32,
    y: i32,
    w: u32,
    h: u32,
    payload: OverlayPayload,
}

fn show_overlay_impl(app: &AppHandle, at: Option<(i32, i32)>, preselect: Option<SelRect>) {
    // 门闩：遮罩已开 / 滚动中 / 上次仍在准备 → 忽略（去抖，防连按重复抓屏叠加卡顿）
    let Some(state) = app.try_state::<CaptureState>() else {
        return;
    };
    if overlay_visible(app) || crate::scroll::is_active(app) || crate::gifrec::is_active(app) {
        return;
    }
    if state.preparing.swap(true, Ordering::SeqCst) {
        return;
    }

    // 主窗口在前台（例如从主窗口点「立即截图」）时先收进托盘，否则会被拍进冻屏帧
    let wait_for_hide = hide_main_for_capture(app);

    // 抓屏（每屏几十 ms）+ 窗口枚举移到后台线程：show_overlay 常在全局快捷键 / 托盘
    // 回调（主 UI 线程）中触发，此处同步抓屏会冻结界面（即用户反馈的「经常无响应」）。
    // 窗口几何设置与事件下发再 marshal 回主线程执行，否则遮罩不显示。
    let app = app.clone();
    std::thread::spawn(move || {
        // guard 覆盖所有提前 return；正常路径移入主线程闭包，emit 后释放
        let guard = PreparingGuard(app.clone());

        if wait_for_hide {
            std::thread::sleep(std::time::Duration::from_millis(HIDE_SETTLE_MS));
        }

        let monitors = match Monitor::all() {
            Ok(m) if !m.is_empty() => m,
            Ok(_) => {
                eprintln!("[snapix] 抓屏中止：未枚举到显示器");
                return;
            }
            Err(e) => {
                eprintln!("[snapix] 抓屏中止：枚举显示器失败: {e}");
                return;
            }
        };
        let all_windows = snap::list_windows();
        let Some(state) = app.try_state::<CaptureState>() else {
            return;
        };

        let mut jobs: Vec<OverlayJob> = Vec::new();
        for (i, monitor) in monitors.iter().enumerate() {
            let label = format!("{OVERLAY_PREFIX}-{i}");
            let (Ok(mx), Ok(my), Ok(mw), Ok(mh), Ok(scale)) = (
                monitor.x(),
                monitor.y(),
                monitor.width(),
                monitor.height(),
                monitor.scale_factor(),
            ) else {
                eprintln!("[snapix] 显示器 {i} 几何读取失败，跳过");
                continue;
            };
            let image = match monitor.capture_image() {
                Ok(img) => img,
                Err(e) => {
                    eprintln!("[snapix] 显示器 {i} 抓屏失败，跳过: {e}");
                    continue;
                }
            };
            let (width, height) = (image.width(), image.height());

            // 与本屏有交集的吸附窗口 → 显示器相对坐标
            let windows: Vec<SnapWindow> = all_windows
                .iter()
                .filter_map(|w| {
                    let rx = w.x - mx;
                    let ry = w.y - my;
                    if rx + (w.w as i32) <= 0
                        || ry + (w.h as i32) <= 0
                        || rx >= mw as i32
                        || ry >= mh as i32
                    {
                        return None;
                    }
                    Some(SnapWindow {
                        title: w.title.clone(),
                        x: rx,
                        y: ry,
                        w: w.w,
                        h: w.h,
                    })
                })
                .collect();

            let frame_id = state.frame_seq.fetch_add(1, Ordering::SeqCst) + 1;
            lock(&state.frames).insert(
                label.clone(),
                Frame {
                    id: frame_id,
                    image,
                    origin: (mx, my),
                    scale,
                },
            );

            let payload = OverlayPayload {
                label: label.clone(),
                frame_id,
                frame_url: crate::shot_url(&format!("frame/{frame_id}")),
                width,
                height,
                scale,
                windows,
                // 预选选区只送到上次截图所在的显示器
                preselect: match at {
                    Some(p) if p == (mx, my) => preselect,
                    _ => None,
                },
            };
            lock(&state.pending).insert(label.clone(), payload.clone());

            jobs.push(OverlayJob {
                label,
                x: mx,
                y: my,
                w: mw,
                h: mh,
                payload,
            });
        }

        let count = monitors.len();
        let app_cb = app.clone();
        // 窗口创建 + 几何 + emit 全部回主线程执行（若 marshal 失败，guard 随闭包 drop 解锁）
        let _ = app.run_on_main_thread(move || {
            let _guard = guard;
            ensure_overlay_windows(&app_cb, count);
            for job in jobs {
                if let Some(win) = app_cb.get_webview_window(&job.label) {
                    // 先移动到目标屏（进入其 DPI 上下文）再设置物理尺寸
                    let _ = win.set_position(PhysicalPosition::new(job.x, job.y));
                    let _ = win.set_size(PhysicalSize::new(job.w, job.h));
                    let _ = win.emit_to(&job.label, "overlay:open", job.payload);
                }
            }
        });
    });
}

/// 解除 preparing 门闩（覆盖后台线程提前 return 与主线程 emit 完成两条路径）。
struct PreparingGuard(AppHandle);
impl Drop for PreparingGuard {
    fn drop(&mut self) {
        if let Some(state) = self.0.try_state::<CaptureState>() {
            state.preparing.store(false, Ordering::SeqCst);
        }
    }
}

/// 遮罩窗挂载时主动拉取待处理载荷（应对 emit 早于监听器注册的竞态）。
#[tauri::command]
pub fn overlay_fetch(window: tauri::WebviewWindow, app: AppHandle) -> Option<OverlayPayload> {
    let state = app.try_state::<CaptureState>()?;
    let pending = lock(&state.pending);
    pending.get(window.label()).cloned()
}

/// 某遮罩窗开始交互（拖选/吸附点选）：通知其他屏的遮罩清空选区。
#[tauri::command]
pub fn overlay_claim(window: tauri::WebviewWindow, app: AppHandle) {
    for (label, win) in overlay_windows(&app) {
        if label != window.label() {
            let _ = win.emit_to(&label, "overlay:standby", ());
        }
    }
}

/// 前端冻屏图渲染完毕，安全显示对应遮罩窗（帧号必须仍是当前帧）。
/// 显示后再次断言窗口几何：跨 DPI 移动时隐藏态的 set_size 可能被系统重算。
/// `async`：在 WebView2 回调内同步 show 自己所在的窗口有互锁风险（见 lib.rs 说明）。
#[tauri::command]
pub async fn overlay_ready(window: tauri::WebviewWindow, app: AppHandle, frame_id: u64) {
    let Some(state) = app.try_state::<CaptureState>() else {
        return;
    };
    let label = window.label().to_string();
    let geometry = {
        let frames = lock(&state.frames);
        match frames.get(&label) {
            Some(f) if f.id == frame_id => Some((f.origin, (f.image.width(), f.image.height()))),
            _ => None,
        }
    };
    let Some(((mx, my), (mw, mh))) = geometry else {
        return;
    };

    let _ = window.show();
    let _ = window.set_position(PhysicalPosition::new(mx, my));
    let _ = window.set_size(PhysicalSize::new(mw, mh));
    let _ = window.set_always_on_top(true);

    // 光标所在屏获得键盘焦点（其余屏靠鼠标移入时自取焦点）
    let on_cursor_monitor = app
        .cursor_position()
        .map(|c| {
            let (cx, cy) = (c.x as i32, c.y as i32);
            cx >= mx && cx < mx + mw as i32 && cy >= my && cy < my + mh as i32
        })
        .unwrap_or(false);
    if on_cursor_monitor {
        let _ = window.set_focus();
    }
}

/// 延时截图。倒计时一开始就收起主窗口，留出时间给用户布置画面。
pub fn capture_delayed_impl(app: AppHandle, secs: u64) {
    hide_main_for_capture(&app);
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_secs(secs));
        show_overlay(&app);
    });
}

/// 延时截图（命令入口）。
#[tauri::command]
pub async fn capture_delayed(app: AppHandle, secs: u64) {
    capture_delayed_impl(app, secs);
}

/// 发送系统通知（静默失败，不阻塞主链路）。
pub fn notify(app: &AppHandle, body: &str) {
    use tauri_plugin_notification::NotificationExt;
    let _ = app
        .notification()
        .builder()
        .title("Snapix")
        .body(body)
        .show();
}

/// 依设置把图静默存进默认目录（「直接保存」模式），返回文件名。
fn silent_save(app: &AppHandle, img: &RgbaImage) -> Option<String> {
    let s = settings::current(app);
    if s.save_ask {
        return None;
    }
    let dir = settings::resolve_save_dir(app, &s);
    let stem = settings::render_name(&s.file_template, img.width(), img.height());
    let ext = if s.image_format == "jpg" {
        "jpg"
    } else {
        "png"
    };
    let path = settings::unique_path(&dir, &stem, ext);
    save_image_to(img, &path).ok()?;
    path.file_name().map(|n| n.to_string_lossy().to_string())
}

/// 全屏即拍：截取光标所在屏 → 剪贴板（+ 依设置直存目录），零交互。
pub fn capture_full_impl(app: &AppHandle) {
    // 遮罩打开 / 滚动或录制进行中时忽略（避免把辅助 UI 拍进去）
    if overlay_visible(app) || crate::scroll::is_active(app) || crate::gifrec::is_active(app) {
        return;
    }
    let wait_for_hide = hide_main_for_capture(app);
    let app = app.clone();
    std::thread::spawn(move || {
        if wait_for_hide {
            std::thread::sleep(std::time::Duration::from_millis(HIDE_SETTLE_MS));
        }
        let cursor = app.cursor_position().ok();
        let monitor = cursor
            .and_then(|p| Monitor::from_point(p.x as i32, p.y as i32).ok())
            .or_else(|| Monitor::all().ok().and_then(|m| m.into_iter().next()));
        let Some(monitor) = monitor else { return };
        let Ok(img) = monitor.capture_image() else {
            return;
        };

        let _ = write_clipboard(&app, &img);
        let saved_name = silent_save(&app, &img);
        crate::history::record(&app, img, "capture");
        match saved_name {
            Some(name) => notify(&app, &format!("整屏截图已复制，并保存为 {name}")),
            None => notify(&app, "整屏截图已复制到剪贴板"),
        }
    });
}

/// 全屏即拍（命令入口）。
#[tauri::command]
pub async fn capture_full(app: AppHandle) {
    capture_full_impl(&app);
}

/// 窗口即拍：截取前台（活动）窗口 → 剪贴板（+ 依设置直存），零交互。
pub fn capture_window_impl(app: &AppHandle) {
    if overlay_visible(app) || crate::scroll::is_active(app) || crate::gifrec::is_active(app) {
        return;
    }
    let app = app.clone();
    std::thread::spawn(move || {
        let Some(win) = snap::foreground_window() else {
            notify(&app, "未找到可截取的前台窗口");
            return;
        };
        // 前台窗口就是自家晾衣绳（用户刚点过上面的图）：不做无意义的条状截图
        if win.title == "晾衣绳" {
            return;
        }
        // 取窗口中心所在显示器，裁剪窗口与该屏的交集
        let cx = win.x + win.w as i32 / 2;
        let cy = win.y + win.h as i32 / 2;
        let Ok(monitor) = Monitor::from_point(cx, cy) else {
            notify(&app, "无法定位窗口所在显示器");
            return;
        };
        let (Ok(mx), Ok(my), Ok(mw), Ok(mh)) =
            (monitor.x(), monitor.y(), monitor.width(), monitor.height())
        else {
            return;
        };
        let x0 = (win.x - mx).max(0) as u32;
        let y0 = (win.y - my).max(0) as u32;
        let x1 = ((win.x + win.w as i32 - mx).min(mw as i32)).max(0) as u32;
        let y1 = ((win.y + win.h as i32 - my).min(mh as i32)).max(0) as u32;
        if x1 <= x0 + 4 || y1 <= y0 + 4 {
            notify(&app, "窗口不在可截取范围内");
            return;
        }
        let Ok(img) = monitor.capture_region(x0, y0, x1 - x0, y1 - y0) else {
            notify(&app, "窗口截取失败");
            return;
        };

        let _ = write_clipboard(&app, &img);
        let saved_name = silent_save(&app, &img);
        crate::history::record(&app, img, "capture");
        let title = if win.title.trim().is_empty() {
            "窗口".to_string()
        } else {
            win.title.clone()
        };
        match saved_name {
            Some(name) => notify(&app, &format!("「{title}」已复制，并保存为 {name}")),
            None => notify(&app, &format!("「{title}」截图已复制到剪贴板")),
        }
    });
}

/// 窗口即拍（命令入口）。
#[tauri::command]
pub fn capture_window(app: AppHandle) {
    capture_window_impl(&app);
}

/// 重复上次区域（命令入口）。
#[tauri::command]
pub async fn repeat_capture(app: AppHandle) {
    repeat_last_capture(&app);
}

/// 隐藏全部遮罩窗并清空冻屏缓存。
pub fn hide_overlay(app: &AppHandle) {
    for (_, win) in overlay_windows(app) {
        let _ = win.hide();
    }
    if let Some(state) = app.try_state::<CaptureState>() {
        lock(&state.frames).clear();
        lock(&state.pending).clear();
    }
}

/// 飞行只拥有发起时的这一帧；旧落地 / 超时不能收掉下一次截图。
pub(crate) fn frame_token(app: &AppHandle, label: &str) -> Option<(String, u64)> {
    let state = app.try_state::<CaptureState>()?;
    let frames = lock(&state.frames);
    Some((label.to_string(), frames.get(label)?.id))
}

/// 必须在主线程调用，与窗口交接串行；持帧锁期间不做窗口操作。
pub(crate) fn hide_overlay_for_frame_now(app: &AppHandle, token: &(String, u64)) {
    let matches = app.try_state::<CaptureState>().is_some_and(|state| {
        !state.preparing.load(Ordering::SeqCst)
            && lock(&state.frames)
                .get(&token.0)
                .is_some_and(|f| f.id == token.1)
    });
    if matches {
        hide_overlay(app);
    }
}

/// 后台任务需要窗口操作的真实结果时使用。主线程本身调用时 Tauri 会立即执行闭包。
pub(crate) fn on_main_thread<T: Send + 'static>(
    app: &AppHandle,
    f: impl FnOnce(&AppHandle) -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    let (tx, rx) = std::sync::mpsc::channel();
    let handle = app.clone();
    let timeout = std::time::Duration::from_secs(5);
    let deadline = std::time::Instant::now() + timeout;
    app.run_on_main_thread(move || {
        // 已经超时的排队任务不能迟到后再创建 / 显示 / 删除源图片。
        let result = if std::time::Instant::now() >= deadline {
            Err("主线程窗口操作超时".to_string())
        } else {
            f(&handle)
        };
        let _ = tx.send(result);
    })
    .map_err(|e| e.to_string())?;
    rx.recv_timeout(timeout)
        .map_err(|_| "主线程窗口操作超时".to_string())?
}

/// 某遮罩窗当前帧的几何信息。
pub fn frame_geometry(app: &AppHandle, label: &str) -> Option<FrameGeometry> {
    let state = app.try_state::<CaptureState>()?;
    let frames = lock(&state.frames);
    let f = frames.get(label)?;
    Some((f.origin, f.scale, (f.image.width(), f.image.height())))
}

/// 帧号对应的 BMP（供 shot://frame/{id}）。
pub fn frame_bmp(app: &AppHandle, id: u64) -> Option<Vec<u8>> {
    let state = app.try_state::<CaptureState>()?;
    let frames = lock(&state.frames);
    let frame = frames.values().find(|f| f.id == id)?;
    Some(crate::fastimg::rgba_to_bmp(&frame.image))
}

/// 从指定遮罩窗的冻屏帧中裁出选区（坐标为显示器相对物理像素，自动夹紧），
/// 并记录「上次区域」。
fn crop_frame(app: &AppHandle, label: &str, x: i32, y: i32, w: u32, h: u32) -> Option<RgbaImage> {
    crop_with_origin(app, label, x, y, w, h).map(|(img, _, _)| img)
}

/// 裁剪选区，并返回选区在虚拟桌面上的物理左上角（用于贴图原位显示）。
fn crop_with_origin(
    app: &AppHandle,
    label: &str,
    x: i32,
    y: i32,
    w: u32,
    h: u32,
) -> Option<(RgbaImage, i32, i32)> {
    let state = app.try_state::<CaptureState>()?;
    let frames = lock(&state.frames);
    let frame = frames.get(label)?;
    let img = &frame.image;

    let (iw, ih) = (img.width(), img.height());
    let cx = x.max(0) as u32;
    let cy = y.max(0) as u32;
    if cx >= iw || cy >= ih {
        return None;
    }
    let cw = w.min(iw - cx);
    let ch = h.min(ih - cy);
    if cw == 0 || ch == 0 {
        return None;
    }

    let cropped = image::imageops::crop_imm(img, cx, cy, cw, ch).to_image();
    let origin = frame.origin;

    *lock(&state.last) = Some(LastRegion {
        monitor_point: origin,
        rect: SelRect {
            x: cx as i32,
            y: cy as i32,
            w: cw,
            h: ch,
        },
    });

    Some((cropped, origin.0 + cx as i32, origin.1 + cy as i32))
}

// ===================== 输出：复制 / 保存 =====================

pub(crate) fn write_clipboard(app: &AppHandle, img: &RgbaImage) -> Result<(), String> {
    use tauri_plugin_clipboard_manager::ClipboardExt;
    let image = tauri::image::Image::new(img.as_raw(), img.width(), img.height());
    app.clipboard()
        .write_image(&image)
        .map_err(|e| e.to_string())
}

/// 按扩展名写盘（jpg 需去 alpha）。
fn save_image_to(img: &RgbaImage, path: &Path) -> Result<(), String> {
    let is_jpg = path
        .extension()
        .map(|e| {
            let e = e.to_string_lossy().to_ascii_lowercase();
            e == "jpg" || e == "jpeg"
        })
        .unwrap_or(false);
    if is_jpg {
        image::DynamicImage::ImageRgba8(img.clone())
            .to_rgb8()
            .save(path)
            .map_err(|e| e.to_string())
    } else {
        img.save(path).map_err(|e| e.to_string())
    }
}

/// 弹对话框另存（默认目录 / 文件名来自设置）。
/// 阻塞等待用户操作——必须在 `util::blocking` 线程池而非主线程调用。
///
/// `parent` 决定对话框的属主窗口。遮罩 / 贴图窗都是 `always_on_top`，只有把对话框
/// 挂成它们的子窗口（Windows 强制「子窗必在属主之上」），原生对话框才不会被置顶
/// 遮罩盖住——否则只能先收起遮罩再弹框，用户就会看到截图界面提前消失。
pub fn save_with_dialog(
    app: &AppHandle,
    img: &RgbaImage,
    parent: Option<&tauri::WebviewWindow>,
) -> Result<Option<PathBuf>, String> {
    use tauri_plugin_dialog::DialogExt;

    let s = settings::current(app);
    let dir = settings::resolve_save_dir(app, &s);
    let stem = settings::render_name(&s.file_template, img.width(), img.height());
    let ext = if s.image_format == "jpg" {
        "jpg"
    } else {
        "png"
    };

    let mut dialog = app
        .dialog()
        .file()
        .add_filter("图片", &["png", "jpg", "jpeg"])
        .set_directory(dir)
        .set_file_name(format!("{stem}.{ext}"));
    if let Some(win) = parent {
        dialog = dialog.set_parent(win);
    }
    let file = dialog.blocking_save_file();

    match file {
        Some(f) => {
            let path = f.into_path().map_err(|e| e.to_string())?;
            save_image_to(img, &path)?;
            Ok(Some(path))
        }
        None => Ok(None),
    }
}

/// 依设置保存（询问 or 直存默认目录），返回实际路径；含「保存后复制」。
pub(crate) fn save_output(
    app: &AppHandle,
    img: &RgbaImage,
    parent: Option<&tauri::WebviewWindow>,
) -> Result<Option<PathBuf>, String> {
    let s = settings::current(app);
    let saved = if s.save_ask {
        save_with_dialog(app, img, parent)?
    } else {
        let dir = settings::resolve_save_dir(app, &s);
        let stem = settings::render_name(&s.file_template, img.width(), img.height());
        let ext = if s.image_format == "jpg" {
            "jpg"
        } else {
            "png"
        };
        let path = settings::unique_path(&dir, &stem, ext);
        save_image_to(img, &path)?;
        Some(path)
    };
    if saved.is_some() && s.copy_after_save {
        let _ = write_clipboard(app, img);
    }
    Ok(saved)
}

/// 裁剪选区并写入系统剪贴板。
#[tauri::command]
pub async fn copy_region(
    window: tauri::WebviewWindow,
    app: AppHandle,
    x: i32,
    y: i32,
    width: u32,
    height: u32,
) -> Result<(), String> {
    let label = window.label().to_string();
    blocking(move || {
        let cropped = crop_frame(&app, &label, x, y, width, height).ok_or("无效的选区")?;
        write_clipboard(&app, &cropped)?;
        hide_overlay(&app);
        crate::history::record(&app, cropped, "capture");
        Ok(())
    })
    .await
}

/// 裁剪选区 → 复制 + 挂到晾衣绳，**先不收起遮罩**，把绳上的落点交给前端播过渡动画。
///
/// 与 `copy_region` 的区别只在收尾时序：这里要等前端把「从选区飞到绳上」演完、
/// 回调 `line_landed` 才收遮罩（`land` 里做）。返回 `None` 表示这次不飞——
/// 晾衣绳关闭、或者截图在副屏而绳子在主屏（遮罩跨不出自己的屏），此时按老路径
/// 直接挂上并立刻收遮罩，前端不需要做任何动画。
#[tauri::command]
pub async fn copy_to_line(
    window: tauri::WebviewWindow,
    app: AppHandle,
    x: i32,
    y: i32,
    width: u32,
    height: u32,
) -> Result<Option<LineLanding>, String> {
    let label = window.label().to_string();
    blocking(move || {
        let (cropped, _, _) =
            crop_with_origin(&app, &label, x, y, width, height).ok_or("无效的选区")?;
        if !settings::current(&app).line_enabled {
            write_clipboard(&app, &cropped)?;
            hide_overlay(&app);
            crate::history::record(&app, cropped, "capture");
            return Ok(None);
        }

        // 飞行事件先于剪贴板 / PNG 工作；promise 仍在所有工作成功后返回同一落点。
        match crate::line::hang_for_flight(&app, &cropped, &label)? {
            Some(landing) => {
                crate::line::spawn_land_timeout(&app, &landing.id);
                crate::history::record_no_line(&app, cropped, "capture");
                Ok(Some(landing))
            }
            None => {
                // 关闭 / 跨屏路径仍先复制，复制失败不得收起原截图。
                write_clipboard(&app, &cropped)?;
                hide_overlay(&app);
                crate::line::hang(&app, &cropped);
                crate::history::record_no_line(&app, cropped, "capture");
                Ok(None)
            }
        }
    })
    .await
}

/// 裁剪选区并保存（依设置：询问 / 直存），返回保存路径。
///
/// 遮罩只在**真正存盘成功后**才收起：对话框期间冻屏画面 + 选区框线保持原样，
/// 用户取消或保存失败时回到选区继续操作（既能重试保存，也能改选别的区域）。
#[tauri::command]
pub async fn save_region(
    window: tauri::WebviewWindow,
    app: AppHandle,
    x: i32,
    y: i32,
    width: u32,
    height: u32,
) -> Result<Option<String>, String> {
    let label = window.label().to_string();
    blocking(move || {
        let cropped = crop_frame(&app, &label, x, y, width, height).ok_or("无效的选区")?;
        match save_output(&app, &cropped, Some(&window)) {
            Ok(Some(path)) => {
                hide_overlay(&app);
                crate::history::record(&app, cropped, "capture");
                Ok(Some(path.to_string_lossy().to_string()))
            }
            Ok(None) => Ok(None),
            Err(e) => Err(e),
        }
    })
    .await
}

// ===================== 编辑器 =====================

#[derive(Default)]
pub struct EditorState {
    pub current: Mutex<Option<(u64, RgbaImage)>>,
    seq: AtomicU64,
}

/// 打开标注编辑器并装载图像。
pub fn open_editor_image(app: &AppHandle, img: RgbaImage) -> Result<(), String> {
    let (w, h) = (img.width(), img.height());
    let state = app.try_state::<EditorState>().ok_or("编辑器状态缺失")?;
    let id = state.seq.fetch_add(1, Ordering::SeqCst) + 1;
    *lock(&state.current) = Some((id, img));

    let editor = app
        .get_webview_window("editor")
        .ok_or("编辑器窗口不存在，请重启应用")?;
    let _ = editor.show();
    let _ = editor.unminimize();
    let _ = editor.center();
    let _ = editor.set_focus();
    editor
        .emit_to(
            "editor",
            "editor:open",
            ImagePayload {
                image_url: crate::shot_url(&format!("editor/{id}")),
                width: w,
                height: h,
                scale: 1.0,
                display_width: None,
                display_height: None,
                from_line: false,
            },
        )
        .map_err(|e| e.to_string())
}

/// 编辑器图像 BMP（供 shot://editor/{id}）。
pub fn editor_bmp(app: &AppHandle, id: u64) -> Option<Vec<u8>> {
    let state = app.try_state::<EditorState>()?;
    let guard = lock(&state.current);
    let (cur, img) = guard.as_ref()?;
    if *cur != id {
        return None;
    }
    Some(crate::fastimg::rgba_to_bmp(img))
}

/// 裁剪选区 → 打开标注编辑器。
#[tauri::command]
pub async fn open_editor(
    window: tauri::WebviewWindow,
    app: AppHandle,
    x: i32,
    y: i32,
    width: u32,
    height: u32,
) -> Result<(), String> {
    let label = window.label().to_string();
    blocking(move || {
        let cropped = crop_frame(&app, &label, x, y, width, height).ok_or("无效的选区")?;
        hide_overlay(&app);
        open_editor_image(&app, cropped)
    })
    .await
}

/// 解析 data URL 为图像（编辑器导出成图仍走 data URL——由前端 canvas 合成）。
fn decode_data_url(data_url: &str) -> Result<RgbaImage, String> {
    use base64::{engine::general_purpose::STANDARD, Engine as _};
    let b64 = data_url.split(',').nth(1).ok_or("无效的图片数据")?;
    let bytes = STANDARD.decode(b64).map_err(|e| e.to_string())?;
    Ok(image::load_from_memory(&bytes)
        .map_err(|e| e.to_string())?
        .to_rgba8())
}

/// 把标注成图写入剪贴板。
#[tauri::command]
pub async fn copy_image(app: AppHandle, data_url: String) -> Result<(), String> {
    blocking(move || {
        let img = decode_data_url(&data_url)?;
        write_clipboard(&app, &img)?;
        crate::history::record(&app, img, "editor");
        Ok(())
    })
    .await
}

/// 把标注成图保存（依设置），返回路径。
#[tauri::command]
pub async fn save_image(
    window: tauri::WebviewWindow,
    app: AppHandle,
    data_url: String,
) -> Result<Option<String>, String> {
    blocking(move || {
        let img = decode_data_url(&data_url)?;
        let saved = save_output(&app, &img, Some(&window))?;
        if saved.is_some() {
            crate::history::record(&app, img, "editor");
        }
        Ok(saved.map(|p| p.to_string_lossy().to_string()))
    })
    .await
}

/// 对编辑器当前图像做隐私检测（OCR 词框 + 正则），返回需打码区域（图像坐标）。
#[tauri::command]
pub async fn editor_detect_privacy(app: AppHandle) -> Result<Vec<SelRect>, String> {
    blocking(move || {
        let img = {
            let state = app.try_state::<EditorState>().ok_or("编辑器状态缺失")?;
            let guard = lock(&state.current);
            guard.as_ref().map(|(_, i)| i.clone())
        }
        .ok_or("编辑器中没有图像")?;

        let lines = crate::ocr::recognize_lines(&img)?;
        Ok(crate::ocr::privacy_rects(&lines))
    })
    .await
}

/// 隐藏编辑器窗口并释放当前图像（命令与窗口关闭拦截共用）。
pub fn close_editor_impl(app: &AppHandle) {
    if let Some(editor) = app.get_webview_window("editor") {
        let _ = editor.hide();
    }
    if let Some(state) = app.try_state::<EditorState>() {
        *lock(&state.current) = None;
    }
}

/// 关闭编辑器窗口（`async`：编辑器 WebView 回调内同步隐藏自身窗口有互锁风险）。
#[tauri::command]
pub async fn close_editor(app: AppHandle) {
    close_editor_impl(&app);
}

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
                eprintln!("[snapix] 贴图窗口创建失败: {e}");
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

// ===================== 文本 / OCR =====================

/// 把文本写入剪贴板。
#[tauri::command]
pub fn copy_text(app: AppHandle, text: String) -> Result<(), String> {
    use tauri_plugin_clipboard_manager::ClipboardExt;
    app.clipboard().write_text(text).map_err(|e| e.to_string())
}

/// 对选区做离线 OCR（Windows 内置识别引擎），返回识别文本。
#[tauri::command]
pub async fn ocr_region(
    window: tauri::WebviewWindow,
    app: AppHandle,
    x: i32,
    y: i32,
    width: u32,
    height: u32,
) -> Result<String, String> {
    let label = window.label().to_string();
    blocking(move || {
        let cropped = crop_frame(&app, &label, x, y, width, height).ok_or("无效的选区")?;
        crate::ocr::recognize(&cropped)
    })
    .await
}

/// 识别选区内的二维码（rqrr 纯离线解码），返回全部解码文本。
#[tauri::command]
pub async fn qr_region(
    window: tauri::WebviewWindow,
    app: AppHandle,
    x: i32,
    y: i32,
    width: u32,
    height: u32,
) -> Result<Vec<String>, String> {
    let label = window.label().to_string();
    blocking(move || {
        let cropped = crop_frame(&app, &label, x, y, width, height).ok_or("无效的选区")?;
        let gray = image::imageops::grayscale(&cropped);
        let (w, h) = (gray.width() as usize, gray.height() as usize);
        let mut prepared = rqrr::PreparedImage::prepare_from_greyscale(w, h, |x, y| {
            gray.get_pixel(x as u32, y as u32).0[0]
        });
        let mut out = Vec::new();
        for grid in prepared.detect_grids() {
            if let Ok((_meta, content)) = grid.decode() {
                if !content.is_empty() {
                    out.push(content);
                }
            }
        }
        Ok(out)
    })
    .await
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
