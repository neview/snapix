//! 晾衣绳：截图产出自动挂到屏幕顶端一根弯曲的绳子上。
//!
//! 一个透明的置顶条窗（`line`）铺在屏幕顶部，前端画绳子与夹子、把每张图挂在
//! 绳子上；图片可沿绳拖动改位置。条目落盘到 appdata/line（原图 + 缩略图），
//! 只保留最近 `MAX_ITEMS` 张。
//!
//! 关键交互：条窗虽然只在屏幕顶部占一条，但它横跨整屏且置顶，直接吃掉鼠标
//! 会挡住用户点浏览器标签栏。因此由后台线程按「光标是否落在某张图上」轮询
//! 开关 `ignore_cursor_events`——空白处点击穿透到下面的窗口，只有图能点。

use std::collections::HashMap;
use std::fs;
use std::io::BufWriter;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Mutex};
use std::time::Duration;

use image::{
    codecs::png::{CompressionType, FilterType, PngEncoder},
    ImageEncoder, RgbaImage,
};
use serde::{Deserialize, Serialize};
use tauri::{
    AppHandle, Emitter, Manager, PhysicalPosition, PhysicalSize, WebviewUrl, WebviewWindow,
    WebviewWindowBuilder,
};

use crate::lock;
use crate::util::{blocking, write_atomic};

const LINE_LABEL: &str = "line";

/// 一根绳子上最多挂几张（再多屏幕就挤了）
pub const MAX_ITEMS: usize = 12;

/// 条窗高度（逻辑像素）：内容带只有这么高，往下的部分是给拖动/滑落留的画布。
/// 窗口本身铺满工作区，但输入只在这条带内可能被接管，其余一律穿透。
const HIT_BAND: f64 = 520.0;

/// 缩略图最长边（展示尺寸约 150–220 逻辑像素，2x 屏也够）
const THUMB_MAX: u32 = 460;

/// 飞行动画落地兜底超时（毫秒）：超过它还没收到 `line_landed` 就强制收遮罩
const LAND_TIMEOUT_MS: u64 = 3000;

/// 落地必须由前端确认解码 + 两帧绘制，超时安全收遮罩并报错。
const ITEM_READY_TIMEOUT: Duration = Duration::from_millis(2000);
const SAVE_WAIT_TIMEOUT: Duration = Duration::from_secs(10);
const PIN_READY_TIMEOUT: Duration = Duration::from_secs(5);
/// 未完成交接（包括前端放弃）的事务都有限存活；失败不撤掉绳上原图。
const DETACH_TIMEOUT: Duration = Duration::from_secs(15);

/// 光标判定容差（物理像素）：让「差一点点」也能抓住图，手感不必像素级
const HIT_PAD: f64 = 10.0;

// ===== 排版常量 =====
// 与 `src/windows/Line.tsx` 中的 ROPE_Y / HANG / sagOf / fit 逐项对齐：
// 飞行落点必须和绳上真实排版重合，差 1px 落地时就会「跳」一下。
// 改那边记得同步改这里（`slot_rect_matches_frontend` 测试会兜住一部分回归）。

/// 绳子两端距窗口顶部
const ROPE_Y: f64 = 40.0;
/// 吊线长度：从绳子垂到图片上沿
const HANG: f64 = 14.0;
/// 图片展示高度与宽度上下限
const PHOTO_H: f64 = 146.0;
const PHOTO_MAX_W: f64 = 226.0;
const PHOTO_MIN_W: f64 = 64.0;
const PHOTO_MAX_H: f64 = 176.0;

/// 绳子中段下垂量
fn sag_of(css_w: f64) -> f64 {
    (css_w * 0.028).clamp(22.0, 84.0)
}

/// 绳子在归一化位置 t 处的高度（抛物线，与前端 SVG 的二次贝塞尔一致）
fn rope_y(t: f64, sag: f64) -> f64 {
    ROPE_Y + 4.0 * sag * t * (1.0 - t)
}

/// 图片展示尺寸（保持宽高比）
fn fit_css(iw: u32, ih: u32) -> (f64, f64) {
    let ratio = iw as f64 / ih.max(1) as f64;
    let mut h = PHOTO_H;
    let mut w = h * ratio;
    if w > PHOTO_MAX_W {
        w = PHOTO_MAX_W;
        h = w / ratio;
    }
    if w < PHOTO_MIN_W {
        w = PHOTO_MIN_W;
        h = w / ratio;
    }
    if h > PHOTO_MAX_H {
        h = PHOTO_MAX_H;
        w = h * ratio;
    }
    (w, h)
}

/// 沿绳位置 `t` 处图片框（绳窗相对 CSS 像素）：横向以 t 为中心，纵向贴着绳子的抛物线
fn rect_at(t: f64, css_w: f64, iw: u32, ih: u32) -> (f64, f64, f64, f64) {
    let t = t.clamp(0.0, 1.0);
    let (w, h) = fit_css(iw, ih);
    (t * css_w - w / 2.0, rope_y(t, sag_of(css_w)) + HANG, w, h)
}

/// 第 `n` 张（0 基）挂上去后的图片框（绳窗相对 CSS 像素）
fn slot_rect(n: usize, css_w: f64, iw: u32, ih: u32) -> (f64, f64, f64, f64) {
    let n = n.min(MAX_ITEMS - 1);
    let t = (n as f64 + 0.5) / (n as f64 + 1.0);
    rect_at(t, css_w, iw, ih)
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LineItem {
    pub id: String,
    /// Unix 毫秒
    pub ts: i64,
    pub width: u32,
    pub height: u32,
    /// 沿绳子方向的归一化位置（0..1）；`None` = 参与自动均分排布
    #[serde(default)]
    pub x: Option<f64>,
    /// 正在飞向绳子的途中：位置已确定但先不显示，等落地动画播完再摘掉这个标记。
    /// 不入库（`load` 时统一清掉），免得动画途中退出后这张图永远藏起来。
    #[serde(default)]
    pub pending: bool,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LineEntry {
    #[serde(flatten)]
    pub item: LineItem,
    pub url: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DetachPhase {
    Preparing,
    Ready,
    Revealed,
    Cancelled,
}

struct DetachPreparation {
    label: String,
    rect: (i32, i32, u32, u32),
    phase: DetachPhase,
    waiters: Vec<mpsc::Sender<Result<String, String>>>,
}

impl DetachPreparation {
    fn owns(&self, label: &str, phase: DetachPhase) -> bool {
        self.label == label && self.phase == phase
    }
}

struct Flight {
    frame: (String, u64),
    saved: bool,
    save_waiters: Vec<mpsc::Sender<Result<(), String>>>,
    landing_started: bool,
    ready: Option<mpsc::Sender<bool>>,
}

/// 贴图「挂回绳子」的交接事务：条目已经建好并处于 pending，等绳窗把飞行演完。
struct ReturnJob {
    label: String,
}

/// 挂回的飞行动作：起点是贴图窗的内容矩形（屏幕物理像素），终点是绳窗内的落点（CSS 像素）。
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReturnFlight {
    pub id: String,
    pub label: String,
    pub url: String,
    pub from: [f64; 4],
    pub to: [f64; 4],
}

#[derive(Default)]
pub struct LineState {
    pub items: Mutex<Vec<LineItem>>,
    seq: AtomicU64,
    /// 前端上报的可点区域（窗口相对物理像素：x, y, w, h）
    hits: Mutex<Vec<[f64; 4]>>,
    /// 当前是否已设为鼠标穿透（避免每帧重复调系统接口）
    passthrough: AtomicBool,
    /// 拖动中：此时绝不能切回穿透，否则拖到一半就丢鼠标了
    dragging: AtomicBool,
    /// 截图时把绳子举出来当「投放提示」：此时钉死穿透，绝不能抢遮罩的鼠标
    hint: AtomicBool,
    /// 所有准备事务变更与窗口操作在主线程串行；读图 / 等就绪在后台。
    detaches: Mutex<HashMap<String, DetachPreparation>>,
    flights: Mutex<HashMap<String, Flight>>,
    /// id → 正在从贴图飞回绳子的条目
    returns: Mutex<HashMap<String, ReturnJob>>,
}

// ===================== 存储 =====================

fn line_dir(app: &AppHandle) -> Option<PathBuf> {
    let dir = app.path().app_data_dir().ok()?.join("line");
    fs::create_dir_all(&dir).ok()?;
    Some(dir)
}

fn index_path(app: &AppHandle) -> Option<PathBuf> {
    line_dir(app).map(|d| d.join("index.json"))
}

/// id 形如 `{ts}-{seq}`，仅数字与连字符（防路径穿越）
fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 40 && id.chars().all(|c| c.is_ascii_digit() || c == '-')
}

fn file_path(app: &AppHandle, id: &str) -> Option<PathBuf> {
    if !valid_id(id) {
        return None;
    }
    line_dir(app).map(|d| d.join(format!("{id}.png")))
}

fn thumb_path(app: &AppHandle, id: &str) -> Option<PathBuf> {
    if !valid_id(id) {
        return None;
    }
    line_dir(app).map(|d| d.join(format!("{id}.thumb.png")))
}

fn remove_files(app: &AppHandle, id: &str) {
    for p in [file_path(app, id), thumb_path(app, id)]
        .into_iter()
        .flatten()
    {
        let _ = fs::remove_file(p);
    }
}

fn persist(app: &AppHandle, items: &[LineItem]) {
    if let Some(path) = index_path(app) {
        if let Ok(json) = serde_json::to_string(items) {
            let _ = write_atomic(&path, json.as_bytes());
        }
    }
}

/// 启动时加载；有存货就把绳子亮出来。
pub fn load(app: &AppHandle) {
    let items: Vec<LineItem> = index_path(app)
        .and_then(|p| fs::read_to_string(p).ok())
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();
    // 清理已找不到文件的记录
    let alive: Vec<LineItem> = items
        .into_iter()
        .filter(|it| file_path(app, &it.id).map(|p| p.exists()).unwrap_or(false))
        // 上次在飞行动画途中退出会留下 pending，这里统一清掉，别让图永远藏着
        .map(|mut it| {
            it.pending = false;
            it
        })
        .collect();
    let has = !alive.is_empty();
    if let Some(st) = app.try_state::<LineState>() {
        *lock(&st.items) = alive;
    }
    if crate::settings::current(app).line_enabled {
        crate::pin::prewarm_pin(app);
        let handle = app.clone();
        let _ = app.run_on_main_thread(move || {
            // 即使空绳也预热隐藏 WebView，让首次飞行不再承担冷启动。
            let _ = window(&handle);
            if has {
                show_now(&handle);
            }
        });
    }
}

/// 通知绳子前端刷新。`landed` 带上「刚刚落地的那张」，前端据此跳过入场动画
/// （这张是飞过来的，再播一次「从上方落下」会撞在一起）。
fn notify(app: &AppHandle, landed: Option<&str>) {
    let _ = app.emit_to(LINE_LABEL, "line:changed", landed.map(|id| id.to_string()));
}

// ===================== 窗口 =====================

fn build_window(app: &AppHandle) -> Option<WebviewWindow> {
    let monitor = app.primary_monitor().ok().flatten().or_else(|| {
        app.available_monitors()
            .ok()
            .and_then(|m| m.into_iter().next())
    })?;
    let area = monitor.work_area();

    let win = WebviewWindowBuilder::new(app, LINE_LABEL, WebviewUrl::App("line.html".into()))
        .additional_browser_args(crate::BROWSER_ARGS)
        .title("晾衣绳")
        .decorations(false)
        .transparent(true)
        .always_on_top(true)
        .skip_taskbar(true)
        .resizable(false)
        .shadow(false)
        .focused(false)
        .visible(false)
        .build()
        .ok()?;

    // 物理像素直接定位：跨 DPI 下逻辑尺寸的换算容易被系统重算。
    // 高度铺满整块工作区（不是只留一条）：往下拖图、松手滑落都要在窗口里画，
    // 窗口一旦需要中途 resize，那一两帧的卡顿就藏不住——干脆一开始就够高。
    // 输入穿透由 watcher 按「顶部带宽」收窄，行为和只铺一条时完全一致。
    let _ = win.set_position(PhysicalPosition::new(area.position.x, area.position.y));
    let _ = win.set_size(PhysicalSize::new(area.size.width, area.size.height));
    // 不进 Alt+Tab、不被窗口吸附当成目标
    crate::winfx::mark_tool_window(&win);
    // 别把自己拍进截图里。开发时可用 SNAPIX_LINE_CAPTURABLE=1 关掉这个行为，
    // 方便截图审阅绳子的实际观感（release 构建下始终排除）。
    let capturable = cfg!(debug_assertions)
        && std::env::var_os("SNAPIX_LINE_CAPTURABLE").is_some_and(|v| v != "0");
    if !capturable {
        crate::winfx::exclude_from_capture(&win);
    }
    Some(win)
}

fn window(app: &AppHandle) -> Option<WebviewWindow> {
    app.get_webview_window(LINE_LABEL)
        .or_else(|| build_window(app))
}

/// 显示绳子（无图则什么也不做）。
/// 投递到主线程：窗口创建 / 显示要在事件循环线程上做（挂图来自后台编码线程）。
pub fn show(app: &AppHandle) {
    let handle = app.clone();
    let _ = app.run_on_main_thread(move || show_now(&handle));
}

fn show_now(app: &AppHandle) {
    let has = app
        .try_state::<LineState>()
        .map(|st| !lock(&st.items).is_empty())
        .unwrap_or(false);
    if !has {
        return;
    }
    if let Some(win) = window(app) {
        let _ = win.show();
        notify(app, None);
    }
}

pub fn hide(app: &AppHandle) {
    if let Some(win) = app.get_webview_window(LINE_LABEL) {
        let _ = win.hide();
    }
}

pub fn is_visible(app: &AppHandle) -> bool {
    app.get_webview_window(LINE_LABEL)
        .and_then(|w| w.is_visible().ok())
        .unwrap_or(false)
}

/// 托盘 / 设置里的开关。
pub fn toggle(app: &AppHandle) {
    if is_visible(app) {
        hide(app);
    } else {
        show(app);
    }
}

// ===================== 挂图 =====================

fn write_png_fast(img: &RgbaImage, path: &std::path::Path) -> Result<(), String> {
    use std::io::Write;
    let file = fs::File::create(path).map_err(|e| e.to_string())?;
    let mut out = BufWriter::new(file);
    PngEncoder::new_with_quality(&mut out, CompressionType::Fast, FilterType::Adaptive)
        .write_image(
            img.as_raw(),
            img.width(),
            img.height(),
            image::ExtendedColorType::Rgba8,
        )
        .map_err(|e| e.to_string())?;
    out.flush().map_err(|e| e.to_string())
}

fn write_thumb(img: &RgbaImage, path: &std::path::Path) -> Result<(), String> {
    let (w, h) = img.dimensions();
    if w.max(h) <= THUMB_MAX {
        return write_png_fast(img, path);
    }
    let k = THUMB_MAX as f32 / w.max(h) as f32;
    let tw = ((w as f32 * k).round() as u32).max(1);
    let th = ((h as f32 * k).round() as u32).max(1);
    write_png_fast(&image::imageops::thumbnail(img, tw, th), path)
}

/// 把一张产出图挂到绳子上（编解码较重，调用方应已处于后台线程）。
pub fn hang(app: &AppHandle, img: &RgbaImage) {
    let _ = write_item(app, img, false);
}

/// 写盘 + 入库，返回条目 id。pending 条目也通知预加载，但图片等落地才可见。
fn write_item(app: &AppHandle, img: &RgbaImage, pending: bool) -> Option<String> {
    let (id, ts) = allocate_item(app)?;
    save_item(app, img, &id, ts, pending).ok()?;
    Some(id)
}

fn allocate_item(app: &AppHandle) -> Option<(String, i64)> {
    let st = app.try_state::<LineState>()?;
    let ts = chrono::Local::now().timestamp_millis();
    let seq = st.seq.fetch_add(1, Ordering::SeqCst);
    Some((format!("{ts}-{seq}"), ts))
}

/// id 在 emit 之前分配，编码 / 原图 / 缩略图失败时都不发布 pending 条目。
fn save_item(
    app: &AppHandle,
    img: &RgbaImage,
    id: &str,
    ts: i64,
    pending: bool,
) -> Result<(), String> {
    save_item_at(app, img, id, ts, pending, None)
}

/// 同上，但可以带一个明确的沿绳位置（挂回时沿用它原来的 `x`）。
fn save_item_at(
    app: &AppHandle,
    img: &RgbaImage,
    id: &str,
    ts: i64,
    pending: bool,
    x: Option<f64>,
) -> Result<(), String> {
    let st = app.try_state::<LineState>().ok_or("晾衣绳状态缺失")?;
    let path = file_path(app, id).ok_or("无法创建晾衣绳目录")?;
    let thumb = thumb_path(app, id).ok_or("无法创建缩略图目录")?;
    if let Err(e) = write_png_fast(img, &path).and_then(|_| write_thumb(img, &thumb)) {
        remove_files(app, id);
        return Err(e);
    }
    let item = LineItem {
        id: id.to_string(),
        ts,
        width: img.width(),
        height: img.height(),
        x,
        pending,
    };
    let removed: Vec<LineItem> = {
        let mut items = lock(&st.items);
        items.push(item);
        let overflow = items.len().saturating_sub(MAX_ITEMS);
        let removed: Vec<LineItem> = if overflow > 0 {
            items.drain(..overflow).collect()
        } else {
            Vec::new()
        };
        persist(app, &items);
        removed
    };
    for r in &removed {
        cancel_detach_later(app, &r.id, None, "条目已从绳上移除");
        remove_files(app, &r.id);
    }
    // pending 也通知，让绳窗在飞行途中预加载图片，但不提前绘制它。
    show(app);
    notify(app, None);
    Ok(())
}

/// 挂图 + 算出「它会落在哪」：返回 (条目 id, 遮罩窗相对 CSS 像素的矩形)。
///
/// 给「对勾 → 飞向晾衣绳」用。坐标要从绳窗（主屏工作区顶部）折算到**发起截图的
/// 那块屏的遮罩窗**里——两者常常不是同一个窗口，更不能指望前端去猜 DPI。
/// 返回 `None` 表示这张图没法飞（跨屏，遮罩画不到绳子上），调用方退化成直接挂上。
pub fn hang_for_flight(
    app: &AppHandle,
    img: &RgbaImage,
    overlay_label: &str,
) -> Result<Option<crate::capture::LineLanding>, String> {
    let Some(st) = app.try_state::<LineState>() else {
        return Ok(None);
    };
    let geometry = (|| {
        let n = lock(&st.items).len().min(MAX_ITEMS - 1);
        let monitor = app.primary_monitor().ok().flatten().or_else(|| {
            app.available_monitors()
                .ok()
                .and_then(|m| m.into_iter().next())
        })?;
        let area = monitor.work_area();
        let line_scale = monitor.scale_factor();
        let css_w = area.size.width as f64 / line_scale;
        let (sx, sy, sw, sh) = slot_rect(n, css_w, img.width(), img.height());
        let ((ox, oy), o_scale, (fw, fh)) = crate::capture::frame_geometry(app, overlay_label)?;
        let o_scale = o_scale as f64;
        let lx = (area.position.x as f64 + sx * line_scale - ox as f64) / o_scale;
        let ly = (area.position.y as f64 + sy * line_scale - oy as f64) / o_scale;
        let lw = sw * line_scale / o_scale;
        let lh = sh * line_scale / o_scale;
        if lx < -1.0
            || ly < -1.0
            || lx + lw > fw as f64 / o_scale + 1.0
            || ly + lh > fh as f64 / o_scale + 1.0
        {
            return None;
        }
        Some((
            [lx, ly, lw, lh],
            crate::capture::frame_token(app, overlay_label)?,
        ))
    })();
    let Some(([x, y, w, h], frame)) = geometry else {
        return Ok(None);
    };
    let (id, ts) = allocate_item(app).ok_or("无法分配晾衣绳条目")?;
    let landing = crate::capture::LineLanding {
        id: id.clone(),
        x,
        y,
        w,
        h,
    };
    // 动画可能早于写盘结束；先登记飞行，line_landed 可以安全地等待保存完成。
    lock(&st.flights).insert(
        id.clone(),
        Flight {
            frame,
            saved: false,
            save_waiters: Vec::new(),
            landing_started: false,
            ready: None,
        },
    );
    let _ = app.emit_to(overlay_label, "line:flight", landing.clone());
    // 此时前端已有 frame 动画，无需等剪贴板和 PNG 编码。
    let result =
        crate::capture::write_clipboard(app, img).and_then(|_| save_item(app, img, &id, ts, true));
    let waiters = {
        let mut flights = lock(&st.flights);
        if result.is_ok() {
            if let Some(flight) = flights.get_mut(&id) {
                flight.saved = true;
                std::mem::take(&mut flight.save_waiters)
            } else {
                Vec::new()
            }
        } else {
            flights
                .remove(&id)
                .map(|f| f.save_waiters)
                .unwrap_or_default()
        }
    };
    for tx in waiters {
        let _ = tx.send(result.clone());
    }
    // 保存失败不伪装成跨屏 fallback，也不清除原截图帧。
    result?;
    if !lock(&st.flights).contains_key(&id) {
        // 保存比落地等待上限还慢：桌面已安全释放，迟到的图片仍需正常显示。
        clear_pending(app, &id);
        show(app);
        notify(app, Some(&id));
    }
    Ok(Some(landing))
}

// ===================== 落地 =====================

/// 摘掉 pending 标记（绳上那张从「藏着」变成「可见」）。
fn clear_pending(app: &AppHandle, id: &str) {
    if let Some(st) = app.try_state::<LineState>() {
        // 注意作用域：持锁期间绝不能 show / notify —— `show_now` 自己也要拿这把锁，
        // 主线程上重入同一把 std Mutex 就是永久自锁。
        let mut items = lock(&st.items);
        if let Some(it) = items.iter_mut().find(|it| it.id == id) {
            if it.pending {
                it.pending = false;
                persist(app, &items);
            }
        }
    }
}

/// 后台等待真实的图片绘制确认；等待者必须在 pending 改变之前安装。
pub fn land(app: &AppHandle, id: &str) -> Result<(), String> {
    let st = app.try_state::<LineState>().ok_or("晾衣绳状态缺失")?;
    let (ready_tx, ready_rx) = mpsc::channel();
    let (save_tx, save_rx) = mpsc::channel();
    let (frame, saved) = {
        let mut flights = lock(&st.flights);
        let f = flights.get_mut(id).ok_or("飞行已结束或不存在")?;
        if f.landing_started {
            return Err("正在等待该条目落地".into());
        }
        f.landing_started = true;
        f.ready = Some(ready_tx);
        if !f.saved {
            f.save_waiters.push(save_tx);
        }
        (f.frame.clone(), f.saved)
    };
    if !saved {
        match save_rx.recv_timeout(SAVE_WAIT_TIMEOUT) {
            Ok(Ok(())) => {}
            Ok(Err(e)) => return Err(e), // 写盘失败：copy_to_line 同样报错，保留原截图
            Err(_) => {
                let _ = finish_flight(app, id, frame);
                return Err("等待晾衣绳保存超时".into());
            }
        }
    }
    clear_pending(app, id);
    let item_id = id.to_string();
    let shown = crate::capture::on_main_thread(app, move |app| {
        show_now(app);
        notify(app, Some(&item_id));
        Ok(())
    });
    let result = shown.and_then(|_| match ready_rx.recv_timeout(ITEM_READY_TIMEOUT) {
        Ok(true) => Ok(()),
        Ok(false) => Err("晾衣绳图片加载失败".into()),
        Err(_) => Err("等待晾衣绳图片绘制超时".into()),
    });
    // 成功才正常揭幕；失败 / 超时同样有限等待后释放桌面，不能永久挡住用户。
    let hidden = finish_flight(app, id, frame);
    result.and(hidden)
}

fn finish_flight(app: &AppHandle, id: &str, frame: (String, u64)) -> Result<(), String> {
    if let Some(st) = app.try_state::<LineState>() {
        lock(&st.flights).remove(id);
    }
    let (tx, rx) = mpsc::channel();
    let handle = app.clone();
    // 安全释放任务不可因排队超时被丢弃；即使主线程稍后才恢复，也必须执行帧号校验后的隐藏。
    app.run_on_main_thread(move || {
        crate::capture::hide_overlay_for_frame_now(&handle, &frame);
        let _ = tx.send(());
    })
    .map_err(|e| e.to_string())?;
    rx.recv_timeout(Duration::from_secs(5))
        .map_err(|_| "收起截图遮罩超时".to_string())
}

/// 飞行结束命令完全丢失时的最后兜底。帧号检查使旧任务不可能收掉新截图。
pub fn spawn_land_timeout(app: &AppHandle, id: &str) {
    let frame = app
        .try_state::<LineState>()
        .and_then(|st| lock(&st.flights).get(id).map(|f| f.frame.clone()));
    let Some(frame) = frame else { return };
    let app = app.clone();
    let id = id.to_string();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(LAND_TIMEOUT_MS));
        let unfinished = app.try_state::<LineState>().is_some_and(|st| {
            st.flights
                .try_lock()
                .map(|f| f.contains_key(&id))
                .unwrap_or(true)
        });
        if !unfinished {
            return;
        }
        // 先投递安全释放遮罩；持状态锁或磁盘出问题也不能长期挡桌面。
        let handle = app.clone();
        let _ = app.run_on_main_thread(move || {
            crate::capture::hide_overlay_for_frame_now(&handle, &frame)
        });
        if let Some(st) = app.try_state::<LineState>() {
            if let Ok(mut flights) = st.flights.try_lock() {
                flights.remove(&id); // 唤醒正在等待 ready 的 land
            }
            if let Ok(mut items) = st.items.try_lock() {
                if let Some(it) = items.iter_mut().find(|it| it.id == id) {
                    it.pending = false;
                }
                persist(&app, &items);
            }
        }
        show(&app);
        notify(&app, Some(&id));
    });
}

/// 前端只在 pending=false 的图片 decode + 两帧之后回调；旧 / 重复确认是空操作。
#[tauri::command]
pub fn line_item_ready(app: AppHandle, id: String, ready: bool) {
    let Some(st) = app.try_state::<LineState>() else {
        return;
    };
    if !lock(&st.items).iter().any(|it| it.id == id && !it.pending) {
        return;
    }
    let tx = lock(&st.flights).get_mut(&id).and_then(|f| f.ready.take());
    if let Some(tx) = tx {
        let _ = tx.send(ready);
    }
}

/// 载入原图（复制 / 贴图 / 另存用）。
fn load_image(app: &AppHandle, id: &str) -> Result<RgbaImage, String> {
    let path = file_path(app, id).ok_or("无效的晾衣绳图片")?;
    Ok(image::open(path).map_err(|e| e.to_string())?.to_rgba8())
}

/// 预览字节（供 shot://line/{id}）：优先缩略图。
pub fn preview_bytes(app: &AppHandle, id: &str) -> Option<Vec<u8>> {
    if !valid_id(id) {
        return None;
    }
    if let Some(bytes) = thumb_path(app, id).and_then(|p| fs::read(p).ok()) {
        return Some(bytes);
    }
    fs::read(file_path(app, id)?).ok()
}

// ===================== 点击穿透 =====================

/// 上报可点区域（窗口相对物理像素）。前端在布局变化后调用。
#[tauri::command]
pub fn line_hit_rects(app: AppHandle, rects: Vec<[f64; 4]>) {
    if let Some(st) = app.try_state::<LineState>() {
        *lock(&st.hits) = rects;
    }
}

/// 拖动开始 / 结束：拖动期间钉住「可点」状态。
#[tauri::command]
pub fn line_dragging(app: AppHandle, dragging: bool) {
    if let Some(st) = app.try_state::<LineState>() {
        st.dragging.store(dragging, Ordering::SeqCst);
    }
}

fn set_passthrough(st: &LineState, win: &WebviewWindow, on: bool) -> bool {
    if st.passthrough.swap(on, Ordering::SeqCst) != on {
        let _ = win.set_ignore_cursor_events(on);
    }
    on
}

/// 左键当前是否按着（拖动兜底用）。
#[cfg(windows)]
fn left_button_down() -> bool {
    use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_LBUTTON};
    unsafe { (GetAsyncKeyState(VK_LBUTTON.0 as i32) as u16 & 0x8000) != 0 }
}

#[cfg(not(windows))]
fn left_button_down() -> bool {
    true
}

/// 后台轮询：光标落在图上 → 让窗口可点；否则整条窗穿透到下层。
pub fn spawn_watcher(app: AppHandle) {
    std::thread::spawn(move || {
        let mut budget = 0u32;
        // 拖动标志的兜底：pointerup 有可能丢（窗口被别的窗口抢走鼠标、组件被卸载等），
        // 一旦它永远为 true，整块全屏条窗就会一直吃鼠标，桌面所有点击都失效。
        let mut stale_drag = 0u32;
        let mut was_visible = false;
        loop {
            std::thread::sleep(Duration::from_millis(60));
            let Some(win) = app.get_webview_window(LINE_LABEL) else {
                continue;
            };
            let Some(st) = app.try_state::<LineState>() else {
                return;
            };
            if !win.is_visible().unwrap_or(false) {
                was_visible = false;
                continue;
            }
            if !was_visible {
                // 窗口刚被重新显示：系统可能把「忽略鼠标」的状态重置掉，
                // 而 set_passthrough 只在值变化时才下发，这里强制按当前状态重下一次。
                was_visible = true;
                let _ = win.set_ignore_cursor_events(st.passthrough.load(Ordering::SeqCst));
            }
            if st.dragging.load(Ordering::SeqCst) {
                if left_button_down() {
                    stale_drag = 0;
                } else {
                    // 左键早就抬起、前端却没报结束 → 连续 3 次（约 180ms）后强制解除
                    stale_drag += 1;
                    if stale_drag >= 3 {
                        stale_drag = 0;
                        st.dragging.store(false, Ordering::SeqCst);
                        set_passthrough(&st, &win, true);
                        budget = 0;
                        continue;
                    }
                }
                set_passthrough(&st, &win, false);
                budget = 0;
                continue;
            }
            stale_drag = 0;
            // 遮罩正在拖选区、绳子只是被举出来当投放目标：全程穿透，
            // 否则光标划过缩略图时窗口会把鼠标抢走，拖选当场断掉
            if st.hint.load(Ordering::SeqCst) {
                set_passthrough(&st, &win, true);
                budget = 0;
                continue;
            }

            let (Ok(pos), Ok(size), Ok(origin)) = (
                app.cursor_position(),
                win.outer_size(),
                win.outer_position(),
            ) else {
                continue;
            };
            // 光标在「内容带」里吗（左上原点，物理像素）。窗口铺满整块工作区，
            // 但只有顶部这条带可能接管输入——带外永远穿透，跟只铺一条时一模一样。
            let lx = pos.x - origin.x as f64;
            let ly = pos.y - origin.y as f64;
            let scale = win.scale_factor().unwrap_or(1.0);
            let band = HIT_BAND * scale;
            let inside = lx >= 0.0
                && ly >= 0.0
                && lx < size.width as f64
                && ly < band.min(size.height as f64);
            if !inside {
                // 明显离开了内容带（比如往屏幕中间去）→ 立刻切穿透，别让下一次点击落空；
                // 只在贴着带边缘时留两次轮询的余量，免得边界抖动反复调系统接口。
                if ly > band + 140.0 * scale {
                    set_passthrough(&st, &win, true);
                    budget = 0;
                    continue;
                }
                budget = budget.saturating_add(1);
                if budget >= 2 {
                    set_passthrough(&st, &win, true);
                }
                continue;
            }
            budget = 0;
            let hit = {
                let rects = lock(&st.hits);
                rects.iter().any(|[x, y, w, h]| {
                    lx >= x - HIT_PAD
                        && lx <= x + w + HIT_PAD
                        && ly >= y - HIT_PAD
                        && ly <= y + h + HIT_PAD
                })
            };
            set_passthrough(&st, &win, !hit);
        }
    });
}

// ===================== 命令 =====================

#[tauri::command]
pub fn line_list(app: AppHandle) -> Vec<LineEntry> {
    let Some(st) = app.try_state::<LineState>() else {
        return Vec::new();
    };
    let items = lock(&st.items);
    items
        .iter()
        .map(|it| LineEntry {
            item: it.clone(),
            url: crate::shot_url(&format!("line/{}", it.id)),
        })
        .collect()
}

/// 拖动后回写位置（`None` = 回到自动排布）。
#[tauri::command]
pub fn line_move(app: AppHandle, id: String, x: Option<f64>) {
    let Some(st) = app.try_state::<LineState>() else {
        return;
    };
    let mut items = lock(&st.items);
    if let Some(it) = items.iter_mut().find(|it| it.id == id) {
        it.x = x.map(|v| v.clamp(0.0, 1.0));
        persist(&app, &items);
    }
}

/// 截图时把晾衣架「举到遮罩之上」/ 收回去。
///
/// 用户在遮罩里把选区拖到屏幕顶端时，需要看见晾衣架才知道能挂；松手离开顶部就收回。
/// 只做 z 序与显隐，不动几何——所以不会让正在进行的拖选掉帧。
#[tauri::command]
pub async fn line_drop_hint(app: AppHandle, on: bool) {
    let handle = app.clone();
    if let Some(st) = app.try_state::<LineState>() {
        st.hint.store(on, Ordering::SeqCst);
    }
    if !on {
        // 只解除「钉死穿透」，绳子本身继续留着——它本来就在桌面上
        return;
    }
    let _ = app.run_on_main_thread(move || {
        show_now(&handle); // 有条目才显示
        if let Some(win) = handle.get_webview_window(LINE_LABEL) {
            // show() 不保证抬升 z 序：贴图窗用的是同一招（先取消置顶再置顶）
            let _ = win.set_always_on_top(false);
            let _ = win.set_always_on_top(true);
        }
    });
}

/// 飞行动画播完：绳上条目正式出现，然后才收起遮罩（见 `land`）。
///
/// **必须是 `async`**：`land` 会 show 绳窗、hide 遮罩窗，而同步命令运行在
/// WebView2 的 IPC 回调内部，此时动窗口会与之互锁（见 lib.rs 顶部说明）。这条
/// 命令恰好是从遮罩自己的回调里调进来的，一旦同步执行，遮罩再也收不回去，
/// 全屏置顶窗会挡住整个桌面、连托盘都点不到。
#[tauri::command]
pub async fn line_landed(app: AppHandle, id: String) -> Result<(), String> {
    blocking(move || land(&app, &id)).await
}

#[tauri::command]
pub async fn line_remove(app: AppHandle, id: String) {
    let _ = blocking(move || {
        remove_entry(&app, &id);
        Ok::<(), String>(())
    })
    .await;
}

/// 从绳上摘掉一条（取下 / 清空共用）。
fn remove_entry(app: &AppHandle, id: &str) {
    cancel_detach_later(app, id, None, "绳上条目已移除");
    if let Some(st) = app.try_state::<LineState>() {
        let mut items = lock(&st.items);
        items.retain(|it| it.id != id);
        persist(app, &items);
        let empty = items.is_empty();
        drop(items);
        remove_files(app, id);
        if empty {
            hide(app);
        }
        notify(app, None);
    }
}

/// 拖下来松手：把这张图变成贴图窗，并从松手位置动画过渡到屏幕中间偏下。
///
/// 取下后的落点（虚拟桌面物理矩形），前端拿它来演「滑到中间偏下」的动画。
#[tauri::command]
pub fn line_detach_target(app: AppHandle, x: i32, y: i32, width: u32, height: u32) -> [f64; 4] {
    detach_target(&app, x, y, width, height)
}

/// 与前端动画并行准备。返回时图片已解码，但目标物理矩形上的 pin 仍隐藏。
#[tauri::command]
pub async fn line_prepare_detach(
    app: AppHandle,
    id: String,
    x: i32,
    y: i32,
    width: u32,
    height: u32,
) -> Result<String, String> {
    if !valid_id(&id) || width == 0 || height == 0 {
        return Err("无效的晾衣绳图片或目标尺寸".into());
    }
    let (tx, rx) = mpsc::channel();
    let handle = app.clone();
    let deadline = std::time::Instant::now() + DETACH_TIMEOUT;
    // 在第一次 await / 阻塞池调度之前排入主线程，后来的 cancel 不会抢先于登记。
    app.run_on_main_thread(move || {
        let result = if std::time::Instant::now() >= deadline {
            Err("准备贴图超时".to_string())
        } else {
            prepare_detach_now(&handle, id, (x, y, width, height), deadline, tx.clone())
        };
        if let Err(e) = result {
            let _ = tx.send(Err(e));
        }
    })
    .map_err(|e| e.to_string())?;
    blocking(move || {
        rx.recv_timeout(DETACH_TIMEOUT + Duration::from_secs(1))
            .map_err(|_| "准备贴图超时".to_string())?
    })
    .await
}

fn prepare_detach_now(
    app: &AppHandle,
    id: String,
    rect: (i32, i32, u32, u32),
    deadline: std::time::Instant,
    tx: mpsc::Sender<Result<String, String>>,
) -> Result<(), String> {
    let st = app.try_state::<LineState>().ok_or("晾衣绳状态缺失")?;
    log::info!("detach 准备 id={id}");
    // 记下它原本挂在绳上的位置：双击挂回时飞回原处（原来是自动排布的就回末尾）。
    let return_slot = {
        let items = lock(&st.items);
        let Some(item) = items.iter().find(|it| it.id == id && !it.pending) else {
            return Err("绳上图片不存在或尚未落地".into());
        };
        item.x
    };
    let label = {
        let mut detaches = lock(&st.detaches);
        if let Some(p) = detaches.get_mut(&id) {
            if p.rect != rect {
                return Err("该图片已有不同的准备目标，请先取消".into());
            }
            match p.phase {
                DetachPhase::Preparing => p.waiters.push(tx),
                DetachPhase::Ready | DetachPhase::Revealed => {
                    let _ = tx.send(Ok(p.label.clone()));
                }
                DetachPhase::Cancelled => return Err("贴图正在取消".into()),
            }
            return Ok(());
        }
        let label = crate::pin::reserve_deferred_pin_label(app);
        detaches.insert(
            id.clone(),
            DetachPreparation {
                label: label.clone(),
                rect,
                phase: DetachPhase::Preparing,
                waiters: vec![tx],
            },
        );
        label
    };
    if let Some(pin_state) = app.try_state::<crate::pin::PinState>() {
        lock(&pin_state.line_returns).insert(label.clone(), return_slot);
    }
    let timeout_app = app.clone();
    let timeout_id = id.clone();
    let timeout_label = label.clone();
    std::thread::spawn(move || {
        std::thread::sleep(deadline.saturating_duration_since(std::time::Instant::now()));
        cancel_detach_later(
            &timeout_app,
            &timeout_id,
            Some(&timeout_label),
            "贴图交接超时",
        );
    });
    let app = app.clone();
    std::thread::spawn(move || {
        let result = (|| {
            let img = load_image(&app, &id)?;
            let (ready_tx, ready_rx) = mpsc::channel();
            let create_id = id.clone();
            let create_label = label.clone();
            crate::capture::on_main_thread(&app, move |app| {
                let st = app.state::<LineState>();
                let alive = lock(&st.detaches)
                    .get(&create_id)
                    .is_some_and(|p| p.owns(&create_label, DetachPhase::Preparing));
                if !alive {
                    return Err("贴图准备已取消".into());
                }
                // 取消、建窗和显示全部在主线程排队，检查与建窗之间不会插入取消。
                crate::pin::create_deferred_pin_now(app, create_label, img, rect, ready_tx)?;
                Ok(())
            })?;
            ready_rx
                .recv_timeout(PIN_READY_TIMEOUT)
                .map_err(|_| "等待贴图解码超时或窗口已关闭".to_string())?;
            let ready_id = id.clone();
            let ready_label = label.clone();
            crate::capture::on_main_thread(&app, move |app| {
                let st = app.state::<LineState>();
                let waiters = {
                    let mut detaches = lock(&st.detaches);
                    let p = detaches.get_mut(&ready_id).ok_or("贴图准备已取消")?;
                    if !p.owns(&ready_label, DetachPhase::Preparing) {
                        return Err("贴图准备已取消".into());
                    }
                    p.phase = DetachPhase::Ready;
                    std::mem::take(&mut p.waiters)
                };
                for tx in waiters {
                    let _ = tx.send(Ok(ready_label.clone()));
                }
                Ok(())
            })
        })();
        if let Err(e) = result {
            cancel_detach_later(&app, &id, Some(&label), &e);
        }
    });
    Ok(())
}

/// 几何重断言 + show + 置顶全部成功才返回；此时绝不删除源条目。
#[tauri::command]
pub async fn line_reveal_detach(app: AppHandle, id: String, label: String) -> Result<(), String> {
    blocking(move || {
        crate::capture::on_main_thread(&app, move |app| {
            let st = app.state::<LineState>();
            let phase = {
                let detaches = lock(&st.detaches);
                let p = detaches.get(&id).ok_or("贴图准备不存在或已超时")?;
                if p.label != label {
                    return Err("贴图不属于此条目".into());
                }
                p.phase
            };
            if !lock(&st.items).iter().any(|it| it.id == id) {
                return Err("绳上原图已移除".into());
            }
            if phase == DetachPhase::Revealed {
                return app
                    .get_webview_window(&label)
                    .ok_or_else(|| "贴图窗口已关闭".to_string())
                    .map(|_| ());
            }
            if phase != DetachPhase::Ready {
                return Err("贴图尚未准备好".into());
            }
            if let Err(e) = crate::pin::reveal_pin_now(app, &label) {
                let _ = cancel_detach_now(app, &id, Some(&label), &e);
                return Err(e);
            }
            if let Some(p) = lock(&st.detaches).get_mut(&id) {
                p.phase = DetachPhase::Revealed;
            }
            log::info!("detach 显示 id={id}");
            Ok(())
        })
    })
    .await
}

/// 前端交接淡出后提交：只移除绳上源与事务，保留已经显示的 pin。
#[tauri::command]
pub async fn line_finish_detach(app: AppHandle, id: String, label: String) -> Result<(), String> {
    blocking(move || {
        crate::capture::on_main_thread(&app, move |app| {
            let st = app.state::<LineState>();
            let valid = lock(&st.detaches)
                .get(&id)
                .is_some_and(|p| p.owns(&label, DetachPhase::Revealed));
            if !valid {
                return Err("贴图尚未显示或不属于此条目".into());
            }
            let win = app.get_webview_window(&label).ok_or("贴图窗口已关闭")?;
            if !win.is_visible().map_err(|e| e.to_string())? {
                return Err("贴图窗口不可见".into());
            }
            lock(&st.detaches).remove(&id);
            remove_entry(app, &id);
            crate::pin::prewarm_pin(app);
            log::info!("detach 完成 id={id}");
            Ok(())
        })
    })
    .await
}

#[tauri::command]
pub async fn line_cancel_detach(app: AppHandle, id: String) -> Result<(), String> {
    let (tx, rx) = mpsc::channel();
    let handle = app.clone();
    // 与 prepare 同样在首次 await 前排队，避免阻塞池调度颠倒取消 / 重试的顺序。
    app.run_on_main_thread(move || {
        let _ = tx.send(cancel_detach_now(&handle, &id, None, "贴图准备已取消"));
    })
    .map_err(|e| e.to_string())?;
    blocking(move || {
        rx.recv_timeout(Duration::from_secs(5))
            .map_err(|_| "取消贴图超时".to_string())?
    })
    .await
}

fn cancel_detach_later(app: &AppHandle, id: &str, label: Option<&str>, reason: &str) {
    let handle = app.clone();
    let id = id.to_string();
    let label = label.map(str::to_string);
    let reason = reason.to_string();
    let _ = app.run_on_main_thread(move || {
        let _ = cancel_detach_now(&handle, &id, label.as_deref(), &reason);
    });
}

fn cancel_detach_now(
    app: &AppHandle,
    id: &str,
    expected_label: Option<&str>,
    reason: &str,
) -> Result<(), String> {
    let st = app.try_state::<LineState>().ok_or("晾衣绳状态缺失")?;
    let (label, waiters) = {
        let mut detaches = lock(&st.detaches);
        let Some(p) = detaches.get_mut(id) else {
            return Ok(());
        };
        if expected_label.is_some_and(|label| p.label != label) {
            return Ok(());
        }
        p.phase = DetachPhase::Cancelled;
        (p.label.clone(), std::mem::take(&mut p.waiters))
    };
    for tx in waiters {
        let _ = tx.send(Err(reason.to_string()));
    }
    // 若还在读图，后来的创建会看到事务失效；若已建窗，则此处立即关掉。
    crate::pin::close_pin_now(app, &label)?;
    lock(&st.detaches).remove(id);
    crate::pin::prewarm_pin(app);
    Ok(())
}

/// 取下后的落点：屏幕中间偏下，宽度取工作区 42%（上限 900 逻辑像素），保持原图比例。
fn detach_target(app: &AppHandle, px: i32, py: i32, w: u32, h: u32) -> [f64; 4] {
    let monitor = app
        .monitor_from_point(px as f64, py as f64)
        .ok()
        .flatten()
        .or_else(|| app.primary_monitor().ok().flatten());
    let Some(m) = monitor else {
        return [px as f64, py as f64, w.max(1) as f64, h.max(1) as f64];
    };
    let area = m.work_area();
    detach_rect(
        (
            area.position.x,
            area.position.y,
            area.size.width,
            area.size.height,
        ),
        m.scale_factor(),
        w,
        h,
    )
}

fn detach_rect(area: (i32, i32, u32, u32), scale: f64, w: u32, h: u32) -> [f64; 4] {
    let (ax, ay, aw, ah) = area;
    let (aw, ah) = (aw.max(1) as f64, ah.max(1) as f64);
    let ratio = w.max(1) as f64 / h.max(1) as f64;
    let mut tw = (aw * 0.42).min(900.0 * scale.max(0.1));
    let mut th = tw / ratio;
    if th > ah * 0.82 {
        th = ah * 0.82;
        tw = th * ratio;
    }
    // 先舍入尺寸再 clamp 坐标，竖长图 / 极端比例 / 负原点都不越工作区。
    let tw = tw.round().clamp(1.0, aw);
    let th = th.round().clamp(1.0, ah);
    let x = (ax as f64 + (aw - tw) / 2.0)
        .round()
        .clamp(ax as f64, ax as f64 + aw - tw);
    let y = (ay as f64 + ah * 0.58 - th / 2.0)
        .round()
        .clamp(ay as f64, ay as f64 + ah - th);
    [x, y, tw, th]
}

/// 清空整条绳（会隐藏绳窗，同样不能在 IPC 回调里同步做——见 `line_landed`）。
#[tauri::command]
pub async fn line_clear(app: AppHandle) {
    let _ = blocking(move || {
        clear_all(&app);
        Ok::<(), String>(())
    })
    .await;
}

/// 清空绳子的实际动作（命令与托盘菜单共用）。
/// 托盘菜单在主线程事件循环里调用它是安全的：那里没有 WebView2 的 IPC 回调栈。
pub fn clear_all(app: &AppHandle) {
    if let Some(st) = app.try_state::<LineState>() {
        let old = {
            let mut items = lock(&st.items);
            let old = std::mem::take(&mut *items);
            persist(app, &items);
            old
        };
        for it in &old {
            cancel_detach_later(app, &it.id, None, "晾衣绳已清空");
            remove_files(app, &it.id);
        }
    }
    hide(app);
    // 清空后必须通知前端：否则绳窗里可能还留着上一次飞行动画的替身图层，
    // 下次截图把绳子重新显示出来时，那张旧图会盖在新内容上面。
    notify(app, None);
}

#[tauri::command]
pub async fn line_copy(app: AppHandle, id: String) -> Result<(), String> {
    blocking(move || {
        let img = load_image(&app, &id)?;
        crate::capture::write_clipboard(&app, &img)
    })
    .await
}

#[tauri::command]
pub async fn line_save(
    window: WebviewWindow,
    app: AppHandle,
    id: String,
) -> Result<Option<String>, String> {
    blocking(move || {
        let img = load_image(&app, &id)?;
        let saved = crate::capture::save_output(&app, &img, Some(&window))?;
        Ok(saved.map(|p| p.to_string_lossy().to_string()))
    })
    .await
}

#[tauri::command]
pub async fn line_pin(app: AppHandle, id: String) -> Result<(), String> {
    blocking(move || {
        let img = load_image(&app, &id)?;
        crate::pin::spawn_pin(&app, img, None, 1.0)
    })
    .await
}

// ===================== 从贴图挂回绳子 =====================

/// 绳窗没接住挂回事件（重载 / 崩溃）时的兜底上限
const RETURN_TIMEOUT_MS: u64 = 4000;

struct ReturnPrep {
    id: String,
    ts: i64,
    img: RgbaImage,
    /// 起点：贴图窗内容矩形（屏幕物理像素）
    from: [f64; 4],
    /// 终点：绳窗内落点（CSS 像素）
    to: [f64; 4],
    /// 要沿用的沿绳位置；`None` = 继续走自动排布
    x: Option<f64>,
    /// 起点是否落在绳窗覆盖范围内（被拖到副屏就没法在绳窗里画飞行）
    fly: bool,
}

/// 双击贴图：挂回晾衣绳，并由绳窗播放「从贴图位置飞回绳上」的动画。
///
/// 返回 `true` 表示已交给绳窗演飞行；`false` 表示贴图不在绳窗覆盖范围（拖到了
/// 副屏等），已直接收回绳上、没有飞行。任何一步失败都保持贴图原样。
#[tauri::command]
pub async fn line_return_pin(app: AppHandle, label: String) -> Result<bool, String> {
    blocking(move || {
        // 窗口几何必须在主线程读；PNG 编码反而要留在当前阻塞线程，别卡 UI 线程。
        let prep = crate::capture::on_main_thread(&app, {
            let label = label.clone();
            move |app| prepare_return(app, &label)
        })?;
        let id = prep.id.clone();
        if let Err(e) = save_item_at(&app, &prep.img, &id, prep.ts, true, prep.x) {
            // 还没起飞就失败了：把占位事务清掉，贴图原样保留，可以再试
            if let Some(st) = app.try_state::<LineState>() {
                lock(&st.returns).remove(&id);
            }
            return Err(e);
        }
        let launched = crate::capture::on_main_thread(&app, {
            let label = label.clone();
            move |app| launch_return(app, prep, &label)
        });
        if launched.is_err() {
            // 主线程没接住：别让条目永远藏在 pending 里
            if let Some(st) = app.try_state::<LineState>() {
                lock(&st.returns).remove(&id);
            }
            clear_pending(&app, &id);
            notify(&app, None);
        }
        launched
    })
    .await
}

/// 主线程：校验贴图来源、读窗口几何、算出落点。不做任何写盘 / 建窗之外的慢活。
fn prepare_return(app: &AppHandle, label: &str) -> Result<ReturnPrep, String> {
    let pin_state = app
        .try_state::<crate::pin::PinState>()
        .ok_or("贴图状态缺失")?;
    let slot = *lock(&pin_state.line_returns)
        .get(label)
        .ok_or("该贴图不是从晾衣绳取下的")?;
    let pin = app.get_webview_window(label).ok_or("贴图窗口不存在")?;
    if !pin.is_visible().unwrap_or(false) {
        return Err("贴图不可见".into());
    }
    let img = {
        let images = lock(&pin_state.images);
        images
            .get(label)
            .map(|(i, _)| i.clone())
            .ok_or("贴图内容已释放")?
    };

    let line = window(app).ok_or("无法创建晾衣绳窗口")?;
    let line_scale = line.scale_factor().map_err(|e| e.to_string())?.max(0.1);
    let line_pos = line.outer_position().map_err(|e| e.to_string())?;
    let line_size = line.outer_size().map_err(|e| e.to_string())?;
    let css_w = (line_size.width as f64 / line_scale).max(1.0);
    let (pw, ph) = (img.width(), img.height());

    let n = lock(&app.state::<LineState>().items)
        .len()
        .min(MAX_ITEMS - 1);
    let auto_t = (n as f64 + 0.5) / (n as f64 + 1.0);
    let (photo_w, _) = fit_css(pw, ph);
    let half = photo_w / 2.0 / css_w;
    let t = slot
        .unwrap_or(auto_t)
        .clamp(half.min(0.5), (1.0 - half).max(0.5));
    let (lx, ly, lw, lh) = rect_at(t, css_w, pw, ph);

    // 贴图窗带阴影边框：外层位置补上客户区偏移才是图片真正占的矩形。
    let pin_pos = pin.outer_position().map_err(|e| e.to_string())?;
    let pin_size = pin.inner_size().map_err(|e| e.to_string())?;
    let (ix, iy, _, _) = crate::winfx::frame_insets(&pin).unwrap_or((0, 0, 0, 0));
    let from = [
        (pin_pos.x + ix) as f64,
        (pin_pos.y + iy) as f64,
        pin_size.width as f64,
        pin_size.height as f64,
    ];
    let fly = from[0] >= line_pos.x as f64 - 2.0
        && from[1] >= line_pos.y as f64 - 2.0
        && from[0] + from[2] <= line_pos.x as f64 + line_size.width as f64 + 2.0
        && from[1] + from[3] <= line_pos.y as f64 + line_size.height as f64 + 2.0;

    let (id, ts) = allocate_item(app).ok_or("无法分配晾衣绳条目")?;
    // 主线程上「查重 + 占位」是串行的：连点两次双击只会成功一次
    {
        let st = app.state::<LineState>();
        let mut returns = lock(&st.returns);
        if returns.values().any(|j| j.label == label) {
            return Err("这张贴图正在挂回绳子".into());
        }
        returns.insert(
            id.clone(),
            ReturnJob {
                label: label.to_string(),
            },
        );
    }
    // 原来固定过位置的按原 t 回去；自动排布的继续自动（落在末尾）。
    let x = slot.map(|_| t);
    Ok(ReturnPrep {
        id,
        ts,
        img,
        from,
        to: [lx, ly, lw, lh],
        x,
        fly,
    })
}

/// 主线程：把绳子抬到贴图之上、交出飞行起点（事务已在 `prepare_return` 占位）。
fn launch_return(app: &AppHandle, prep: ReturnPrep, label: &str) -> Result<bool, String> {
    if let Some(line) = app.get_webview_window(LINE_LABEL) {
        let _ = line.show();
        // 贴图也是置顶窗且创建更晚：取消置顶再置顶，把绳子抬到它上面，飞行层才可见。
        let _ = line.set_always_on_top(false);
        let _ = line.set_always_on_top(true);
    }
    let fly = prep.fly && app.get_webview_window(LINE_LABEL).is_some();
    if !fly {
        finish_return(app, &prep.id, label);
        return Ok(false);
    }
    let flight = ReturnFlight {
        id: prep.id.clone(),
        label: label.to_string(),
        url: crate::shot_url(&format!("line/{}", prep.id)),
        from: prep.from,
        to: prep.to,
    };
    if app.emit_to(LINE_LABEL, "line:return", flight).is_err() {
        finish_return(app, &prep.id, label);
        return Ok(false);
    }
    spawn_return_timeout(app, &prep.id);
    Ok(true)
}

/// 绳窗已经画好飞行首帧：贴图窗可以关了，此后屏幕上的那一份由飞行层接管。
#[tauri::command]
pub async fn line_return_takeoff(app: AppHandle, id: String, label: String) -> Result<(), String> {
    blocking(move || {
        crate::capture::on_main_thread(&app, move |app| {
            let st = app.state::<LineState>();
            let known = lock(&st.returns).get(&id).is_some_and(|j| j.label == label);
            if !known {
                return Err("挂回事务已结束".into());
            }
            let _ = crate::pin::close_pin_now(app, &label);
            Ok(())
        })
    })
    .await
}

/// 飞行落地：摘掉 pending 让真图出现，并释放贴图窗。
#[tauri::command]
pub async fn line_return_landed(app: AppHandle, id: String, label: String) -> Result<(), String> {
    blocking(move || {
        crate::capture::on_main_thread(&app, move |app| {
            let st = app.state::<LineState>();
            let known = lock(&st.returns).get(&id).is_some_and(|j| j.label == label);
            if !known {
                return Ok(());
            }
            finish_return(app, &id, &label);
            Ok(())
        })
    })
    .await
}

/// 收尾（主线程）：条目转为可见、贴图窗关掉。重复调用是空操作。
fn finish_return(app: &AppHandle, id: &str, label: &str) {
    let Some(st) = app.try_state::<LineState>() else {
        return;
    };
    if lock(&st.returns).remove(id).is_none() {
        return;
    }
    clear_pending(app, id);
    let _ = crate::pin::close_pin_now(app, label);
    crate::pin::prewarm_pin(app);
    notify(app, None);
}

fn spawn_return_timeout(app: &AppHandle, id: &str) {
    let app = app.clone();
    let id = id.to_string();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(RETURN_TIMEOUT_MS));
        let label = app
            .try_state::<LineState>()
            .and_then(|st| lock(&st.returns).get(&id).map(|j| j.label.clone()));
        let Some(label) = label else { return };
        let handle = app.clone();
        let _ = app.run_on_main_thread(move || finish_return(&handle, &id, &label));
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detach_rect_stays_inside_work_area_and_uses_integer_pixels() {
        for area in [
            (0, 0, 1920, 1040),
            (-2560, -300, 2560, 1400),
            (33, 21, 1, 1),
        ] {
            for (w, h) in [(100, 9000), (9000, 1), (0, 0), (1234, 777)] {
                for scale in [1.0, 1.25, 2.0] {
                    let [x, y, tw, th] = detach_rect(area, scale, w, h);
                    assert!([x, y, tw, th].iter().all(|v| v.fract() == 0.0));
                    assert!(tw >= 1.0 && th >= 1.0);
                    assert!(x >= area.0 as f64 && y >= area.1 as f64);
                    assert!(x + tw <= area.0 as f64 + area.2 as f64);
                    assert!(y + th <= area.1 as f64 + area.3 as f64);
                }
            }
        }
    }

    #[test]
    fn detach_target_respects_dpi_cap_without_changing_aspect_ratio() {
        let [_, _, w, h] = detach_rect((0, 0, 7680, 4320), 1.5, 1600, 900);
        assert_eq!(w, 1350.0);
        assert!((w / h - 1600.0 / 900.0).abs() < 0.003);
    }

    #[test]
    fn full_rope_uses_last_visible_slot() {
        let expected = slot_rect(MAX_ITEMS - 1, 1920.0, 1600, 900);
        assert_eq!(slot_rect(MAX_ITEMS, 1920.0, 1600, 900), expected);
        assert_eq!(slot_rect(usize::MAX, 1920.0, 1600, 900), expected);
    }

    #[test]
    fn detach_label_and_phase_are_both_required_for_handoff() {
        let mut p = DetachPreparation {
            label: "pin-1".into(),
            rect: (10, 20, 300, 200),
            phase: DetachPhase::Preparing,
            waiters: Vec::new(),
        };
        assert!(!p.owns("pin-1", DetachPhase::Ready));
        p.phase = DetachPhase::Ready;
        assert!(p.owns("pin-1", DetachPhase::Ready));
        assert!(!p.owns("pin-2", DetachPhase::Ready));
        assert!(!p.owns("pin-1", DetachPhase::Revealed));
        p.phase = DetachPhase::Revealed;
        assert!(p.owns("pin-1", DetachPhase::Revealed));
        p.phase = DetachPhase::Cancelled;
        assert!(!p.owns("pin-1", DetachPhase::Preparing));
        assert!(!p.owns("pin-1", DetachPhase::Ready));
        assert!(!p.owns("pin-1", DetachPhase::Revealed));
    }

    #[test]
    fn id_validation() {
        assert!(super::valid_id("1725270000000-1"));
        assert!(!super::valid_id(""));
        assert!(!super::valid_id("../x"));
        assert!(!super::valid_id("1-2.png"));
    }

    /// 落点公式必须与 `src/windows/Line.tsx` 的排版一致，否则飞行落地会跳一下
    #[test]
    fn slot_rect_matches_frontend() {
        // 1920 CSS 宽、正方形：t = 0.5 → sag = 53.76 → ropeY = 93.76
        let (x, y, w, h) = super::slot_rect(0, 1920.0, 1000, 1000);
        assert!((w - 146.0).abs() < 0.01, "w={w}");
        assert!((h - 146.0).abs() < 0.01, "h={h}");
        assert!((x - (960.0 - 73.0)).abs() < 0.01, "x={x}");
        assert!((y - (40.0 + 53.76 + 14.0)).abs() < 0.01, "y={y}");

        // 超宽图先卡 226 宽；第二张（n=1）落在 3/4 处
        let (x2, _, w2, h2) = super::slot_rect(1, 1920.0, 4000, 1000);
        assert!((w2 - 226.0).abs() < 0.01, "w2={w2}");
        assert!((h2 - 56.5).abs() < 0.01, "h2={h2}");
        assert!((x2 - (1440.0 - 113.0)).abs() < 0.01, "x2={x2}");
    }
}
