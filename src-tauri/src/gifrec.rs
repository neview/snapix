//! GIF 录屏：对遮罩选区按 10fps 连续抓帧，增量编码为 GIF。
//!
//! 架构复用滚动截图：穿透描边窗标记录制区 + 控制条窗（停止/取消/时长），
//! 后台捕获线程按节拍抓帧，编码在独立线程增量写盘（NeuQuant 量化较重，
//! 队列积压时丢帧保节奏，不涨内存）。停止后成品直存保存目录并在资源管理器中定位。

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use image::RgbaImage;
use serde::Serialize;
use tauri::{
    AppHandle, Emitter, Manager, PhysicalPosition, PhysicalSize, WebviewUrl, WebviewWindowBuilder,
};
use xcap::Monitor;

use crate::capture::{self, SelRect};
use crate::settings;

const CTL_LABEL: &str = "gif-ctl";
const BORDER_LABEL: &str = "gif-border";

/// 帧间隔（100ms = 10fps；GIF delay 单位为 10ms）
const FRAME_MS: u64 = 100;
const GIF_DELAY: u16 = 10;
/// 单帧 delay 下限：多数播放器把 <2 当作默认 10，避免被拉快
const MIN_DELAY: u16 = 2;
/// 录制时长上限
const MAX_SECS: u64 = 60;
/// 输出最长边上限（超出等比缩小：控制文件体积与量化耗时）
const MAX_DIM: u32 = 1280;

const SIG_RUN: u8 = 0;
const SIG_FINISH: u8 = 1;
const SIG_CANCEL: u8 = 2;

#[derive(Default)]
pub struct GifState {
    pub active: AtomicBool,
    pub signal: AtomicU8,
}

pub fn is_active(app: &AppHandle) -> bool {
    app.try_state::<GifState>()
        .map(|s| s.active.load(Ordering::SeqCst))
        .unwrap_or(false)
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct Progress {
    secs: f32,
    frames: u32,
}

/// 输出尺寸：最长边夹紧到 MAX_DIM，等比缩小。
fn output_dims(w: u32, h: u32) -> (u32, u32) {
    let long = w.max(h);
    if long <= MAX_DIM {
        return (w, h);
    }
    let k = MAX_DIM as f64 / long as f64;
    (
        ((w as f64 * k).round() as u32).max(1),
        ((h as f64 * k).round() as u32).max(1),
    )
}

fn capture_region(origin: (i32, i32), rect: SelRect) -> Option<RgbaImage> {
    let monitor = Monitor::from_point(origin.0 + 2, origin.1 + 2).ok()?;
    monitor
        .capture_region(rect.x as u32, rect.y as u32, rect.w, rect.h)
        .ok()
}

// ===================== 辅助窗口（描边 + 控制条） =====================

fn create_windows(app: &AppHandle, abs: (i32, i32, u32, u32), scale: f64) {
    let (ax, ay, aw, ah) = abs;

    // 描边窗：环在选区外 3px，穿透点击，纯视觉（红色调以示「录制中」）
    if let Ok(win) =
        WebviewWindowBuilder::new(app, BORDER_LABEL, WebviewUrl::App("gif.html".into()))
            .additional_browser_args(crate::BROWSER_ARGS)
            .decorations(false)
            .transparent(true)
            .always_on_top(true)
            .skip_taskbar(true)
            .resizable(false)
            .shadow(false)
            .focused(false)
            .inner_size((aw as f64 + 6.0) / scale, (ah as f64 + 6.0) / scale)
            .build()
    {
        let _ = win.set_ignore_cursor_events(true);
        let _ = win.set_position(PhysicalPosition::new(ax - 3, ay - 3));
        let _ = win.set_size(PhysicalSize::new(aw + 6, ah + 6));
    }

    // 控制条：选区下方 → 上方 → 选区内底部
    let cw = (300.0 * scale) as u32;
    let ch = (56.0 * scale) as u32;
    let monitor_bottom = Monitor::from_point(ax + 2, ay + 2)
        .ok()
        .and_then(|m| Some(m.y().ok()? + m.height().ok()? as i32))
        .unwrap_or(ay + ah as i32);

    let cx = ax + (aw as i32 - cw as i32) / 2;
    let below = ay + ah as i32 + 12;
    let above = ay - ch as i32 - 12;
    let cy = if below + (ch as i32) <= monitor_bottom {
        below
    } else if above >= 0 {
        above
    } else {
        ay + ah as i32 - ch as i32 - 12
    };

    if let Ok(win) = WebviewWindowBuilder::new(app, CTL_LABEL, WebviewUrl::App("gif.html".into()))
        .additional_browser_args(crate::BROWSER_ARGS)
        .decorations(false)
        .transparent(true)
        .always_on_top(true)
        .skip_taskbar(true)
        .resizable(false)
        .shadow(false)
        .inner_size(cw as f64 / scale, ch as f64 / scale)
        .build()
    {
        let _ = win.set_size(PhysicalSize::new(cw, ch));
        let _ = win.set_position(PhysicalPosition::new(cx.max(0), cy.max(0)));
        let _ = win.set_focus();
    }
}

fn close_windows(app: &AppHandle) {
    for label in [CTL_LABEL, BORDER_LABEL] {
        if let Some(w) = app.get_webview_window(label) {
            let _ = w.close();
        }
    }
}

/// 会话守卫：无论正常结束还是录制线程 panic，都关闭辅助窗并复位 `active` 门闩。
struct SessionGuard(AppHandle);

impl Drop for SessionGuard {
    fn drop(&mut self) {
        close_windows(&self.0);
        if let Some(s) = self.0.try_state::<GifState>() {
            s.active.store(false, Ordering::SeqCst);
        }
    }
}

/// 两帧捕获时刻之差 → GIF delay（厘秒）。丢帧时前一帧自然显示更久，成品时长与实际一致。
fn delay_between(prev: Instant, next: Instant) -> u16 {
    let cs = (next.duration_since(prev).as_millis() / 10) as u64;
    cs.clamp(MIN_DELAY as u64, u16::MAX as u64) as u16
}

/// 分片睡眠，期间响应停止信号；返回当前信号。
fn sleep_signal(state: &GifState, total_ms: u64) -> u8 {
    let mut waited = 0;
    while waited < total_ms {
        let s = state.signal.load(Ordering::SeqCst);
        if s != SIG_RUN {
            return s;
        }
        std::thread::sleep(Duration::from_millis(20));
        waited += 20;
    }
    state.signal.load(Ordering::SeqCst)
}

// ===================== 会话 =====================

fn run_session(app: AppHandle, origin: (i32, i32), rect: SelRect, guard: SessionGuard) {
    let state = app.state::<GifState>();
    let (ow, oh) = output_dims(rect.w, rect.h);

    // 临时文件：编码线程增量写入，完成后移入保存目录
    let tmp: PathBuf = std::env::temp_dir().join(format!(
        "snapix-rec-{}.gif",
        chrono::Local::now().format("%Y%m%d%H%M%S")
    ));

    // 编码线程：有界队列，积压时捕获侧丢帧。帧带捕获时刻，编码侧滞后一帧写入，
    // 用相邻两帧的真实间隔作为前一帧的 delay（丢帧不会让成品变快）。
    let (tx, rx) = mpsc::sync_channel::<(RgbaImage, Instant)>(2);
    let tmp2 = tmp.clone();
    let encoder_thread = std::thread::spawn(move || -> bool {
        let Ok(file) = fs::File::create(&tmp2) else {
            return false;
        };
        let mut w = std::io::BufWriter::new(file);
        let Ok(mut enc) = gif::Encoder::new(&mut w, ow as u16, oh as u16, &[]) else {
            return false;
        };
        let _ = enc.set_repeat(gif::Repeat::Infinite);
        let mut wrote = false;
        let mut write = |img: RgbaImage, delay: u16| -> bool {
            let mut raw = img.into_raw();
            // speed 12：NeuQuant 采样加速（1..30，越大越快、色彩略降）
            let mut frame = gif::Frame::from_rgba_speed(ow as u16, oh as u16, &mut raw, 12);
            frame.delay = delay;
            enc.write_frame(&frame).is_ok()
        };
        let mut pending: Option<(RgbaImage, Instant)> = None;
        for (img, at) in rx {
            if let Some((prev, prev_at)) = pending.take() {
                if !write(prev, delay_between(prev_at, at)) {
                    return wrote;
                }
                wrote = true;
            }
            pending = Some((img, at));
        }
        if let Some((last, _)) = pending {
            if write(last, GIF_DELAY) {
                wrote = true;
            }
        }
        wrote
    });

    let started = Instant::now();
    let mut frames_sent = 0u32;
    let mut cancelled = false;

    loop {
        let sig = state.signal.load(Ordering::SeqCst);
        if sig == SIG_CANCEL {
            cancelled = true;
            break;
        }
        if sig == SIG_FINISH || started.elapsed() >= Duration::from_secs(MAX_SECS) {
            break;
        }

        let t0 = Instant::now();
        if let Some(img) = capture_region(origin, rect) {
            let out = if (img.width(), img.height()) == (ow, oh) {
                img
            } else {
                image::imageops::resize(&img, ow, oh, image::imageops::FilterType::Triangle)
            };
            // 队列满（编码跟不上）则丢帧，保住实时节奏
            if tx.try_send((out, t0)).is_ok() {
                frames_sent += 1;
                let _ = app.emit_to(
                    CTL_LABEL,
                    "gif:progress",
                    Progress {
                        secs: started.elapsed().as_secs_f32(),
                        frames: frames_sent,
                    },
                );
            }
        }

        let spent = t0.elapsed().as_millis() as u64;
        if spent < FRAME_MS && sleep_signal(&state, FRAME_MS - spent) == SIG_CANCEL {
            cancelled = true;
            break;
        }
    }

    drop(tx);
    let wrote = encoder_thread.join().unwrap_or(false);

    // 关辅助窗、放开门闩；之后只剩文件搬运与通知
    drop(guard);

    if cancelled {
        let _ = fs::remove_file(&tmp);
        return;
    }
    if !wrote {
        let _ = fs::remove_file(&tmp);
        capture::notify(&app, "GIF 保存失败：未捕获到有效帧");
        return;
    }

    // 成品移入保存目录（模板命名 + 去重），并在资源管理器中定位
    let s = settings::current(&app);
    let dir = settings::resolve_save_dir(&app, &s);
    let stem = settings::render_name(&s.file_template, rect.w, rect.h);
    let dest = settings::unique_path(&dir, &stem, "gif");
    let moved = fs::rename(&tmp, &dest).or_else(|_| {
        // 跨盘符 rename 会失败，回退为复制
        fs::copy(&tmp, &dest).map(|_| ()).inspect(|_| {
            let _ = fs::remove_file(&tmp);
        })
    });
    if moved.is_ok() {
        use tauri_plugin_opener::OpenerExt;
        let name = dest
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        capture::notify(&app, &format!("GIF 已保存：{name}"));
        let _ = app
            .opener()
            .reveal_item_in_dir(dest.to_string_lossy().to_string());
    } else {
        let _ = fs::remove_file(&tmp);
        capture::notify(&app, "GIF 保存失败：无法写入保存目录");
    }
}

// ===================== 命令 =====================

/// 开始 GIF 录制（坐标为显示器相对物理像素，来自遮罩选区）。
/// `async`：会创建控制条 / 边框窗并收起遮罩；同步命令在 WebView2 回调线程内做这些
/// 窗口操作会互锁（见 lib.rs 说明）。
#[tauri::command]
pub async fn gif_record_start(
    window: tauri::WebviewWindow,
    app: AppHandle,
    x: i32,
    y: i32,
    width: u32,
    height: u32,
) -> Result<(), String> {
    let (origin, scale, dims) =
        capture::frame_geometry(&app, window.label()).ok_or("截图会话已失效")?;

    let cx = x.max(0) as u32;
    let cy = y.max(0) as u32;
    if cx >= dims.0 || cy >= dims.1 {
        return Err("无效的选区".into());
    }
    let cw = width.min(dims.0 - cx);
    let ch = height.min(dims.1 - cy);
    if cw < 32 || ch < 32 {
        return Err("选区太小，无法录制".into());
    }
    let rect = SelRect {
        x: cx as i32,
        y: cy as i32,
        w: cw,
        h: ch,
    };

    if crate::scroll::is_active(&app) {
        return Err("滚动截图进行中".into());
    }
    let state = app.state::<GifState>();
    if state.active.swap(true, Ordering::SeqCst) {
        return Err("已有录制进行中".into());
    }
    // 门闩已占，从这里开始由守卫负责复位
    let guard = SessionGuard(app.clone());
    state.signal.store(SIG_RUN, Ordering::SeqCst);

    capture::hide_overlay(&app);
    create_windows(
        &app,
        (origin.0 + rect.x, origin.1 + rect.y, rect.w, rect.h),
        scale.max(0.1) as f64,
    );

    let app2 = app.clone();
    std::thread::spawn(move || run_session(app2, origin, rect, guard));
    Ok(())
}

/// 停止并保存 GIF。
#[tauri::command]
pub fn gif_record_stop(app: AppHandle) {
    if let Some(s) = app.try_state::<GifState>() {
        s.signal.store(SIG_FINISH, Ordering::SeqCst);
    }
}

/// 取消录制并丢弃。
#[tauri::command]
pub fn gif_record_cancel(app: AppHandle) {
    if let Some(s) = app.try_state::<GifState>() {
        s.signal.store(SIG_CANCEL, Ordering::SeqCst);
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    #[test]
    fn delay_tracks_real_interval() {
        let t0 = Instant::now();
        // 正常 10fps
        assert_eq!(
            super::delay_between(t0, t0 + Duration::from_millis(100)),
            10
        );
        // 丢了两帧：前一帧要显示 300ms
        assert_eq!(
            super::delay_between(t0, t0 + Duration::from_millis(300)),
            30
        );
        // 过短间隔夹紧到下限
        assert_eq!(
            super::delay_between(t0, t0 + Duration::from_millis(5)),
            super::MIN_DELAY
        );
    }

    #[test]
    fn output_dims_clamps_long_side() {
        assert_eq!(super::output_dims(800, 600), (800, 600));
        let (w, h) = super::output_dims(2560, 1440);
        assert_eq!(w, 1280);
        assert_eq!(h, 720);
        let (w, h) = super::output_dims(1440, 2560);
        assert_eq!(w, 720);
        assert_eq!(h, 1280);
    }
}
