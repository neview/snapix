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
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::Instant;

use image::RgbaImage;
use serde::Serialize;
use tauri::{
    AppHandle, Emitter, Manager, PhysicalPosition, PhysicalSize, WebviewUrl, WebviewWindowBuilder,
};
use xcap::Monitor;

use crate::cursor;
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
    /// 本次唤起的起点：用于记录「快捷键 → 遮罩首帧」耗时
    pub capture_started: Mutex<Option<Instant>>,
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
                Err(e) => log::warn!("遮罩窗 {label} 创建失败: {e}"),
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
    *lock(&state.capture_started) = Some(Instant::now());

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
                log::warn!("抓屏中止：未枚举到显示器");
                return;
            }
            Err(e) => {
                log::warn!("抓屏中止：枚举显示器失败: {e}");
                return;
            }
        };
        let all_windows = snap::list_windows();
        let want_cursor = settings::current(&app).capture_cursor;
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
                log::warn!("显示器 {i} 几何读取失败，跳过");
                continue;
            };
            let image = match monitor.capture_image() {
                Ok(img) => img,
                Err(e) => {
                    log::warn!("显示器 {i} 抓屏失败，跳过: {e}");
                    continue;
                }
            };
            let (width, height) = (image.width(), image.height());
            // 带光标：合成到冻屏帧里，后续裁剪 / 复制 / 保存都自带光标
            let image = if want_cursor {
                let mut img = image;
                cursor::apply(&mut img, (mx, my));
                img
            } else {
                image
            };

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
    if on_cursor_monitor {
        if let Some(start) = lock(&state.capture_started).take() {
            log::info!("遮罩唤起 {} ms", start.elapsed().as_millis());
        }
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
pub(crate) fn silent_save(app: &AppHandle, img: &RgbaImage) -> Option<String> {
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
        let (Ok(mx), Ok(my)) = (monitor.x(), monitor.y()) else {
            return;
        };
        let img = if settings::current(&app).capture_cursor {
            let mut img = img;
            cursor::apply(&mut img, (mx, my));
            img
        } else {
            img
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
        let img = if settings::current(&app).capture_cursor {
            let mut img = img;
            cursor::apply(&mut img, (mx + x0 as i32, my + y0 as i32));
            img
        } else {
            img
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
pub(crate) fn crop_frame(
    app: &AppHandle,
    label: &str,
    x: i32,
    y: i32,
    w: u32,
    h: u32,
) -> Option<RgbaImage> {
    crop_with_origin(app, label, x, y, w, h).map(|(img, _, _)| img)
}

/// 裁剪选区，并返回选区在虚拟桌面上的物理左上角（用于贴图原位显示）。
pub(crate) fn crop_with_origin(
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
