//! 滚动长截图：程序化滚动 + 帧间特征匹配拼接。
//!
//! 流程：选区确定 → 隐藏遮罩 → 创建描边窗/控制条窗 → 后台线程循环
//! 「注入滚轮 → 等待渲染 → 抓取区域 → 与上一帧匹配位移 → 追加新内容」，
//! 触底（连续多帧无位移）/ 匹配失败 / 达到上限 / 用户点完成 时装配长图进编辑器。
//!
//! 纵向与横向共用同一套算法：横向时把每帧转置后再拼接，装配完再转置回来
//! （见 `transpose`），因此位移搜索、表头表尾剔除、触底判定都只有一份实现。
//!
//! 拼接算法：每行取 48 个采样点的灰度签名；
//! 首对帧检测「固定表头/表尾」（粘性导航栏等，逐行相同则排除），
//! 之后在内容带上搜索使 SAD 最小的垂直位移 dy，追加帧尾 dy 行新内容。

use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};

use image::RgbaImage;
use serde::Serialize;
use tauri::{
    AppHandle, Emitter, Manager, PhysicalPosition, PhysicalSize, WebviewUrl, WebviewWindowBuilder,
};
use xcap::Monitor;

use crate::capture::{self, SelRect};

const CTL_LABEL: &str = "scroll-ctl";
const BORDER_LABEL: &str = "scroll-border";

const SIG_COLS: usize = 48;
/// 行签名判定「相同」的阈值（平均每采样点灰度差）
const ROW_SAME: u32 = 2 * SIG_COLS as u32;
/// 位移匹配可接受的平均差
const MATCH_THRESH: u64 = 8;
const MIN_DY: u32 = 4;
const MAX_TOTAL_H: u32 = 24_000;
/// 总像素上限。只按行数封顶在宽选区下会失控：3840 宽 × 24000 行 = 9200 万像素，
/// 追加缓冲 368MB，装配时 `assemble` 再整份复制一次，峰值近 700MB。
/// 按像素封顶后，宽屏选区会更早收敛（3840 宽约 1.6 万行），窄选区仍可用满行数上限。
const MAX_TOTAL_PX: u64 = 64_000_000;
const MAX_FRAMES: u32 = 150;

const SIG_RUN: u8 = 0;
const SIG_FINISH: u8 = 1;
const SIG_CANCEL: u8 = 2;

#[derive(Default)]
pub struct ScrollState {
    pub active: AtomicBool,
    pub signal: AtomicU8,
}

pub fn is_active(app: &AppHandle) -> bool {
    app.try_state::<ScrollState>()
        .map(|s| s.active.load(Ordering::SeqCst))
        .unwrap_or(false)
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct Progress {
    height_px: u32,
    frames: u32,
    horizontal: bool,
}

// ===================== 行签名与匹配 =====================

/// 滚动方向。纵向直接按行处理；横向把帧转置后同样按行处理。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Axis {
    Vertical,
    Horizontal,
}

/// 转置一张图（`w×h` → `h×w`）。分块拷贝，避免逐像素跨行跳着写缓存。
fn transpose(img: &RgbaImage) -> RgbaImage {
    let (w, h) = (img.width() as usize, img.height() as usize);
    let src = img.as_raw();
    let mut buf = vec![0u8; src.len()];
    const BLOCK: usize = 16;
    for by in (0..h).step_by(BLOCK) {
        for bx in (0..w).step_by(BLOCK) {
            let y1 = (by + BLOCK).min(h);
            let x1 = (bx + BLOCK).min(w);
            for y in by..y1 {
                for x in bx..x1 {
                    let s = (y * w + x) * 4;
                    let d = (x * h + y) * 4;
                    buf[d..d + 4].copy_from_slice(&src[s..s + 4]);
                }
            }
        }
    }
    RgbaImage::from_raw(h as u32, w as u32, buf).unwrap_or_else(|| img.clone())
}

type RowSig = [u8; SIG_COLS];

fn signatures(img: &RgbaImage) -> Vec<RowSig> {
    let (w, h) = (img.width() as usize, img.height() as usize);
    let raw = img.as_raw();
    let mut out = Vec::with_capacity(h);
    for y in 0..h {
        let row = &raw[y * w * 4..(y + 1) * w * 4];
        let mut sig = [0u8; SIG_COLS];
        for (c, s) in sig.iter_mut().enumerate() {
            let x = if SIG_COLS > 1 {
                c * (w - 1) / (SIG_COLS - 1)
            } else {
                0
            };
            let p = &row[x * 4..x * 4 + 3];
            *s = ((p[0] as u16 + 2 * p[1] as u16 + p[2] as u16) >> 2) as u8;
        }
        out.push(sig);
    }
    out
}

fn row_sad(a: &RowSig, b: &RowSig) -> u32 {
    a.iter()
        .zip(b)
        .map(|(x, y)| (*x as i32 - *y as i32).unsigned_abs())
        .sum()
}

enum Push {
    /// 匹配到向下位移 dy 行（dy 值供测试断言）
    Moved(#[cfg_attr(not(test), allow(dead_code))] u32),
    Still,
    Fail,
}

struct Stitcher {
    /// 拼接方向；横向时内部一切都是转置后的坐标系
    axis: Axis,
    w: u32,
    h: u32,
    /// 固定表头 / 表尾行数（首对滚动帧上确定一次）
    hdr: u32,
    ftr: u32,
    fixed_decided: bool,
    base: RgbaImage,
    last: RgbaImage,
    prev_sig: Vec<RowSig>,
    /// 追加的新内容行（RGBA 原始字节）
    extra: Vec<u8>,
}

impl Stitcher {
    fn new(first: RgbaImage, axis: Axis) -> Self {
        let first = if axis == Axis::Horizontal {
            transpose(&first)
        } else {
            first
        };
        let sig = signatures(&first);
        Self {
            axis,
            w: first.width(),
            h: first.height(),
            hdr: 0,
            ftr: 0,
            fixed_decided: false,
            base: first.clone(),
            last: first,
            prev_sig: sig,
            extra: Vec::new(),
        }
    }

    fn extra_rows(&self) -> u32 {
        (self.extra.len() / (self.w as usize * 4)) as u32
    }

    fn total_height(&self) -> u32 {
        self.h + self.extra_rows()
    }

    /// 已达到行数 / 像素任一上限，应结束会话。
    fn reached_limit(&self) -> bool {
        let total = self.total_height();
        total >= MAX_TOTAL_H || total as u64 * self.w as u64 >= MAX_TOTAL_PX
    }

    fn push(&mut self, img: &RgbaImage) -> Push {
        // 横向滚动：转置到纵向坐标系，后面全按行处理
        let turned;
        let img = if self.axis == Axis::Horizontal {
            turned = transpose(img);
            &turned
        } else {
            img
        };
        if img.width() != self.w || img.height() != self.h {
            return Push::Fail;
        }
        let new_sig = signatures(img);
        let h = self.h;

        if !self.fixed_decided {
            let mut hdr = 0u32;
            while hdr < h * 2 / 5
                && row_sad(&self.prev_sig[hdr as usize], &new_sig[hdr as usize]) < ROW_SAME
            {
                hdr += 1;
            }
            let mut ftr = 0u32;
            while ftr < h / 4
                && row_sad(
                    &self.prev_sig[(h - 1 - ftr) as usize],
                    &new_sig[(h - 1 - ftr) as usize],
                ) < ROW_SAME
            {
                ftr += 1;
            }
            if h - hdr.min(h * 2 / 5) - ftr.min(h / 4) < 80 {
                // 几乎整帧相同：尚未发生滚动
                return if self.band_still(&new_sig, 0, 0) {
                    Push::Still
                } else {
                    Push::Fail
                };
            }
            self.hdr = hdr;
            self.ftr = ftr;
            self.fixed_decided = true;
        }

        let (hdr, ftr) = (self.hdr, self.ftr);
        let band_h = h - hdr - ftr;

        if self.band_still(&new_sig, hdr, ftr) {
            return Push::Still;
        }

        let min_overlap = (band_h / 5).max(40).min(band_h.saturating_sub(MIN_DY));
        if band_h <= min_overlap + MIN_DY {
            return Push::Fail;
        }

        // 搜索位移：prev 带内容上移 dy 后与 new 对齐
        let mut best_dy = 0u32;
        let mut best_score = u64::MAX;
        for dy in MIN_DY..=(band_h - min_overlap) {
            let overlap = band_h - dy;
            let step = (overlap / 48).max(1) as usize;
            let mut acc = 0u64;
            let mut n = 0u64;
            let mut k = 0u32;
            while k < overlap {
                acc += row_sad(
                    &self.prev_sig[(hdr + dy + k) as usize],
                    &new_sig[(hdr + k) as usize],
                ) as u64;
                n += 1;
                k += step as u32;
            }
            let score = acc / n / SIG_COLS as u64;
            if score < best_score {
                best_score = score;
                best_dy = dy;
                if score == 0 {
                    break;
                }
            }
        }

        if best_score > MATCH_THRESH || best_dy == 0 {
            return Push::Fail;
        }

        // 追加新内容：帧尾（去表尾）的 dy 行
        let w4 = self.w as usize * 4;
        let start = (h - ftr - best_dy) as usize * w4;
        let end = (h - ftr) as usize * w4;
        self.extra.extend_from_slice(&img.as_raw()[start..end]);

        self.prev_sig = new_sig;
        self.last = img.clone();
        Push::Moved(best_dy)
    }

    /// 内容带是否与上一帧几乎完全相同（触底判定）。
    fn band_still(&self, new_sig: &[RowSig], hdr: u32, ftr: u32) -> bool {
        let h = self.h;
        let band_h = h - hdr - ftr;
        let step = (band_h / 64).max(1) as usize;
        let mut acc = 0u64;
        let mut n = 0u64;
        let mut k = 0u32;
        while k < band_h {
            acc += row_sad(
                &self.prev_sig[(hdr + k) as usize],
                &new_sig[(hdr + k) as usize],
            ) as u64;
            n += 1;
            k += step as u32;
        }
        acc / n < ROW_SAME as u64
    }

    /// 装配最终长图：base 主体（去表尾）+ 追加行 + 最后一帧表尾。
    fn assemble(self) -> RgbaImage {
        let w4 = self.w as usize * 4;
        let body_h = (self.h - self.ftr) as usize;
        let total_h = body_h + self.extra_rows() as usize + self.ftr as usize;

        let mut buf = Vec::with_capacity(total_h * w4);
        buf.extend_from_slice(&self.base.as_raw()[..body_h * w4]);
        buf.extend_from_slice(&self.extra);
        buf.extend_from_slice(&self.last.as_raw()[body_h * w4..]);

        let out = RgbaImage::from_raw(self.w, total_h as u32, buf).unwrap_or(self.base);
        if self.axis == Axis::Horizontal {
            transpose(&out)
        } else {
            out
        }
    }
}

// ===================== 滚轮注入 =====================

#[cfg(windows)]
fn cursor_pos() -> Option<(i32, i32)> {
    use windows::Win32::Foundation::POINT;
    use windows::Win32::UI::WindowsAndMessaging::GetCursorPos;
    let mut p = POINT::default();
    unsafe { GetCursorPos(&mut p).ok()? };
    Some((p.x, p.y))
}

#[cfg(windows)]
fn set_cursor(x: i32, y: i32) {
    use windows::Win32::UI::WindowsAndMessaging::SetCursorPos;
    unsafe {
        let _ = SetCursorPos(x, y);
    }
}

/// 激活点位下的目标窗口并置前台（否则注入的滚轮会被忽略）。
/// 按 z 序枚举，跳过本进程窗口（描边/控制条），命中第一个包含该点的可见顶层窗口。
#[cfg(windows)]
fn activate_window_at(pt: (i32, i32)) {
    use windows::core::BOOL;
    use windows::Win32::Foundation::{HWND, LPARAM, RECT};
    use windows::Win32::UI::WindowsAndMessaging::{
        EnumWindows, GetWindowRect, GetWindowThreadProcessId, IsWindowVisible, SetForegroundWindow,
    };

    struct Ctx {
        pt: (i32, i32),
        self_pid: u32,
        found: Option<HWND>,
    }

    unsafe extern "system" fn cb(hwnd: HWND, lparam: LPARAM) -> BOOL {
        let ctx = unsafe { &mut *(lparam.0 as *mut Ctx) };
        if ctx.found.is_some() {
            return BOOL(0);
        }
        unsafe {
            if !IsWindowVisible(hwnd).as_bool() {
                return BOOL(1);
            }
            let mut pid = 0u32;
            GetWindowThreadProcessId(hwnd, Some(&mut pid));
            if pid == ctx.self_pid {
                return BOOL(1); // 跳过自家窗口（描边 / 控制条）
            }
            let mut r = RECT::default();
            if GetWindowRect(hwnd, &mut r).is_err() {
                return BOOL(1);
            }
            if ctx.pt.0 >= r.left && ctx.pt.0 < r.right && ctx.pt.1 >= r.top && ctx.pt.1 < r.bottom
            {
                ctx.found = Some(hwnd);
                return BOOL(0);
            }
        }
        BOOL(1)
    }

    let mut ctx = Ctx {
        pt,
        self_pid: std::process::id(),
        found: None,
    };
    unsafe {
        let _ = EnumWindows(Some(cb), LPARAM(&mut ctx as *mut _ as isize));
        if let Some(hwnd) = ctx.found {
            let _ = SetForegroundWindow(hwnd);
        }
    }
}

#[cfg(windows)]
fn send_wheel(notches: i32) {
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        SendInput, INPUT, INPUT_0, INPUT_MOUSE, MOUSEEVENTF_WHEEL, MOUSEINPUT,
    };
    let input = INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dx: 0,
                dy: 0,
                mouseData: (-notches * 120) as u32,
                dwFlags: MOUSEEVENTF_WHEEL,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    };
    unsafe {
        SendInput(&[input], std::mem::size_of::<INPUT>() as i32);
    }
}

/// 横向滚轮（MOUSEEVENTF_HWHEEL）：正数向右，与纵向的正负方向相反。
#[cfg(windows)]
fn send_wheel_h(notches: i32) {
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        SendInput, INPUT, INPUT_0, INPUT_MOUSE, MOUSEEVENTF_HWHEEL, MOUSEINPUT,
    };
    let input = INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dx: 0,
                dy: 0,
                mouseData: (notches * 120) as u32,
                dwFlags: MOUSEEVENTF_HWHEEL,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    };
    unsafe {
        SendInput(&[input], std::mem::size_of::<INPUT>() as i32);
    }
}

#[cfg(not(windows))]
fn cursor_pos() -> Option<(i32, i32)> {
    None
}
#[cfg(not(windows))]
fn set_cursor(_x: i32, _y: i32) {}
#[cfg(not(windows))]
fn send_wheel(_notches: i32) {}
#[cfg(not(windows))]
fn send_wheel_h(_notches: i32) {}

// ===================== 辅助窗口 =====================

fn create_windows(app: &AppHandle, abs: (i32, i32, u32, u32), scale: f64) {
    let (ax, ay, aw, ah) = abs;

    // 选区描边（外扩 3px，环在选区外，不污染捕获内容；穿透点击）
    if let Ok(win) =
        WebviewWindowBuilder::new(app, BORDER_LABEL, WebviewUrl::App("scroll.html".into()))
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

    // 控制条：优先放选区下方，其次上方，兜底选区内底部（会被表尾剔除逻辑吸收）
    let cw = (320.0 * scale) as u32;
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

    if let Ok(win) =
        WebviewWindowBuilder::new(app, CTL_LABEL, WebviewUrl::App("scroll.html".into()))
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

/// 会话守卫：无论正常结束还是会话线程 panic，都关闭辅助窗并复位 `active` 门闩。
/// 否则一次崩溃会让所有截图入口（都以 `is_active` 为门闩）永久静默失效。
struct SessionGuard(AppHandle);

impl Drop for SessionGuard {
    fn drop(&mut self) {
        close_windows(&self.0);
        if let Some(s) = self.0.try_state::<ScrollState>() {
            s.active.store(false, Ordering::SeqCst);
        }
    }
}

// ===================== 会话 =====================

fn capture_region(origin: (i32, i32), rect: SelRect) -> Result<RgbaImage, String> {
    let monitor = Monitor::from_point(origin.0 + 2, origin.1 + 2).map_err(|e| e.to_string())?;
    monitor
        .capture_region(rect.x as u32, rect.y as u32, rect.w, rect.h)
        .map_err(|e| e.to_string())
}

/// 分片睡眠，期间响应停止信号。返回收到的信号。
fn sleep_signal(state: &ScrollState, total_ms: u64) -> u8 {
    let mut waited = 0;
    while waited < total_ms {
        let s = state.signal.load(Ordering::SeqCst);
        if s != SIG_RUN {
            return s;
        }
        std::thread::sleep(std::time::Duration::from_millis(40));
        waited += 40;
    }
    state.signal.load(Ordering::SeqCst)
}

fn emit_progress(app: &AppHandle, height_px: u32, frames: u32, axis: Axis) {
    let _ = app.emit_to(
        CTL_LABEL,
        "scroll:progress",
        Progress {
            height_px,
            frames,
            horizontal: axis == Axis::Horizontal,
        },
    );
}

fn run_session(app: AppHandle, origin: (i32, i32), rect: SelRect, guard: SessionGuard, axis: Axis) {
    let state = app.state::<ScrollState>();
    let center = (
        origin.0 + rect.x + rect.w as i32 / 2,
        origin.1 + rect.y + rect.h as i32 / 2,
    );
    let saved_cursor = cursor_pos();

    // 依选区在滚动方向上的长度自适应滚动力度，保证足够重叠
    let span = if axis == Axis::Horizontal {
        rect.w
    } else {
        rect.h
    };
    let notches = if span > 900 {
        3
    } else if span > 450 {
        2
    } else {
        1
    };

    // 遮罩刚隐藏，先把目标窗口带到前台并稳定，滚轮注入才会被接收
    #[cfg(windows)]
    {
        activate_window_at(center);
        std::thread::sleep(std::time::Duration::from_millis(150));
    }

    let result: Option<Stitcher> = (|| {
        let first = capture_region(origin, rect).ok()?;
        let mut stitcher = Stitcher::new(first, axis);
        let mut frames = 1u32;
        let mut still = 0u32;
        let mut fails = 0u32;
        emit_progress(&app, stitcher.total_height(), frames, axis);

        loop {
            if state.signal.load(Ordering::SeqCst) == SIG_CANCEL {
                return None;
            }
            if state.signal.load(Ordering::SeqCst) == SIG_FINISH {
                return Some(stitcher);
            }

            set_cursor(center.0, center.1);
            match axis {
                Axis::Vertical => send_wheel(notches),
                Axis::Horizontal => send_wheel_h(notches),
            }
            if sleep_signal(&state, 340) == SIG_CANCEL {
                return None;
            }

            let Ok(img) = capture_region(origin, rect) else {
                return Some(stitcher);
            };
            frames += 1;

            match stitcher.push(&img) {
                Push::Moved(_) => {
                    still = 0;
                    fails = 0;
                    emit_progress(&app, stitcher.total_height(), frames, axis);
                }
                Push::Still => {
                    still += 1;
                    if still >= 3 {
                        return Some(stitcher); // 触底
                    }
                }
                Push::Fail => {
                    fails += 1;
                    if fails >= 4 {
                        return Some(stitcher);
                    }
                }
            }

            if stitcher.reached_limit() || frames >= MAX_FRAMES {
                return Some(stitcher);
            }
        }
    })();

    if let Some((x, y)) = saved_cursor {
        set_cursor(x, y);
    }
    // 先关辅助窗、放开门闩，再装配长图进编辑器
    drop(guard);

    if let Some(stitcher) = result {
        let img = stitcher.assemble();
        if let Err(e) = crate::editor::open_editor_image(&app, img) {
            capture::notify(&app, &format!("滚动截图完成，但无法打开编辑器：{e}"));
        }
    }
}

// ===================== 命令 =====================

/// 开始滚动截图（坐标为显示器相对物理像素，来自遮罩选区）。
/// `async`：会创建控制条 / 边框窗并收起遮罩；同步命令在 WebView2 回调线程内做这些
/// 窗口操作会互锁（见 lib.rs 说明）。
#[tauri::command]
pub async fn scroll_capture_start(
    window: tauri::WebviewWindow,
    app: AppHandle,
    x: i32,
    y: i32,
    width: u32,
    height: u32,
    horizontal: Option<bool>,
) -> Result<(), String> {
    #[cfg(not(windows))]
    {
        let _ = (window, x, y, width, height, horizontal);
        return Err("滚动截图目前仅支持 Windows".into());
    }

    #[cfg(windows)]
    {
        let (origin, scale, dims) =
            capture::frame_geometry(&app, window.label()).ok_or("截图会话已失效")?;

        // 夹紧到冻屏帧边界
        let cx = x.max(0) as u32;
        let cy = y.max(0) as u32;
        if cx >= dims.0 || cy >= dims.1 {
            return Err("无效的选区".into());
        }
        let cw = width.min(dims.0 - cx);
        let ch = height.min(dims.1 - cy);
        if cw < 120 || ch < 120 {
            return Err("选区太小，无法滚动拼接".into());
        }
        let rect = SelRect {
            x: cx as i32,
            y: cy as i32,
            w: cw,
            h: ch,
        };

        let state = app.state::<ScrollState>();
        if state.active.swap(true, Ordering::SeqCst) {
            return Err("已有滚动截图进行中".into());
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

        let axis = if horizontal.unwrap_or(false) {
            Axis::Horizontal
        } else {
            Axis::Vertical
        };
        let app2 = app.clone();
        std::thread::spawn(move || run_session(app2, origin, rect, guard, axis));
        Ok(())
    }
}

/// 结束并装配长图。
#[tauri::command]
pub fn scroll_capture_stop(app: AppHandle) {
    if let Some(s) = app.try_state::<ScrollState>() {
        s.signal.store(SIG_FINISH, Ordering::SeqCst);
    }
}

/// 取消并丢弃。
#[tauri::command]
pub fn scroll_capture_cancel(app: AppHandle) {
    if let Some(s) = app.try_state::<ScrollState>() {
        s.signal.store(SIG_CANCEL, Ordering::SeqCst);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 造一张竖向渐变纹理图，模拟滚动内容。
    fn make_page(w: u32, h: u32) -> RgbaImage {
        RgbaImage::from_fn(w, h, |x, y| {
            let v = ((y * 7 + (y / 13) * 31) % 256) as u8;
            let u = ((x * 3) % 97) as u8;
            image::Rgba([v ^ u, v.wrapping_add(u), v, 255])
        })
    }

    fn viewport(page: &RgbaImage, top: u32, h: u32) -> RgbaImage {
        image::imageops::crop_imm(page, 0, top, page.width(), h).to_image()
    }

    #[test]
    fn stitch_scrolling_page() {
        let page = make_page(300, 3000);
        let vh = 600;
        let mut st = Stitcher::new(viewport(&page, 0, vh), Axis::Vertical);

        let mut top = 0;
        for _ in 0..10 {
            top += 173; // 模拟一次滚动
            match st.push(&viewport(&page, top, vh)) {
                Push::Moved(dy) => assert_eq!(dy, 173),
                _ => panic!("应识别为滚动"),
            }
        }

        let out = st.assemble();
        assert_eq!(out.width(), 300);
        assert_eq!(out.height(), vh + 173 * 10);
        // 拼接结果应与原始页面逐像素一致
        let expect = viewport(&page, 0, vh + 173 * 10);
        assert_eq!(out.as_raw(), expect.as_raw());
    }

    #[test]
    fn detects_bottom() {
        let page = make_page(300, 1200);
        let vh = 600;
        let mut st = Stitcher::new(viewport(&page, 0, vh), Axis::Vertical);
        assert!(matches!(
            st.push(&viewport(&page, 300, vh)),
            Push::Moved(300)
        ));
        // 到底后画面不再变化
        assert!(matches!(st.push(&viewport(&page, 300, vh)), Push::Still));
    }

    #[test]
    fn excludes_sticky_header() {
        let page = make_page(300, 3000);
        let vh = 600;
        let hdr_h = 60;
        // 视口 = 固定表头 + 滚动内容
        let compose = |top: u32| {
            let mut v = viewport(&page, top, vh);
            let header = viewport(&page, 2500, hdr_h);
            image::imageops::replace(&mut v, &header, 0, 0);
            v
        };
        let mut st = Stitcher::new(compose(0), Axis::Vertical);
        match st.push(&compose(150)) {
            Push::Moved(dy) => assert_eq!(dy, 150),
            _ => panic!("应识别滚动并剔除表头"),
        }
        assert_eq!(st.hdr, hdr_h);
        let out = st.assemble();
        assert_eq!(out.height(), vh + 150);
    }

    #[test]
    fn transpose_is_involutive() {
        let page = make_page(37, 53);
        let back = transpose(&transpose(&page));
        assert_eq!(back.dimensions(), page.dimensions());
        assert_eq!(back.as_raw(), page.as_raw());
    }

    /// 横向页面：每列纹理都不同，模拟宽表格向右滚动。
    fn make_wide(w: u32, h: u32) -> RgbaImage {
        RgbaImage::from_fn(w, h, |x, y| {
            let mut s = x.wrapping_mul(2654435761).rotate_left(7);
            s ^= s >> 11;
            let v = (s & 0xFF) as u8;
            let u = ((y * 5 + (y / 7) * 13) % 251) as u8;
            image::Rgba([v, v.wrapping_add(u), v ^ u, 255])
        })
    }

    fn viewport_x(page: &RgbaImage, left: u32, w: u32, h: u32) -> RgbaImage {
        image::imageops::crop_imm(page, left, 0, w, h).to_image()
    }

    #[test]
    fn stitch_horizontal_page() {
        let page = make_wide(3000, 320);
        let (vw, vh) = (600u32, 320u32);
        let mut st = Stitcher::new(viewport_x(&page, 0, vw, vh), Axis::Horizontal);

        let mut left = 0;
        for _ in 0..10 {
            left += 173;
            match st.push(&viewport_x(&page, left, vw, vh)) {
                Push::Moved(d) => assert_eq!(d, 173),
                _ => panic!("应识别为横向滚动"),
            }
        }

        let out = st.assemble();
        assert_eq!(out.width(), vw + 173 * 10);
        assert_eq!(out.height(), vh);
        let expect = viewport_x(&page, 0, vw + 173 * 10, vh);
        assert_eq!(out.as_raw(), expect.as_raw());
    }

    #[test]
    fn horizontal_detects_right_edge() {
        let page = make_wide(700, 200);
        let (vw, vh) = (400u32, 200u32);
        let mut st = Stitcher::new(viewport_x(&page, 0, vw, vh), Axis::Horizontal);
        // 滚到最右：300..700
        assert!(matches!(
            st.push(&viewport_x(&page, 300, vw, vh)),
            Push::Moved(300)
        ));
        // 到边后画面不再变化
        assert!(matches!(
            st.push(&viewport_x(&page, 300, vw, vh)),
            Push::Still
        ));
    }
}
