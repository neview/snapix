//! 标注编辑器窗口与图像分发。
//!
//! 从 `capture` 拆出的独立模块：窗口 / 图像 / 隐私检测各管一摊，避免 capture.rs 继续膨胀

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use image::RgbaImage;
use tauri::{AppHandle, Emitter, Manager, WebviewUrl, WebviewWindowBuilder};

use crate::capture::{
    crop_frame, hide_overlay, on_main_thread, save_output, write_clipboard, ImagePayload, SelRect,
};
use crate::lock;
use crate::util::blocking;

// ===================== 编辑器 =====================

#[derive(Default)]
pub struct EditorState {
    pub current: Mutex<Option<(u64, RgbaImage)>>,
    seq: AtomicU64,
}

/// 打开标注编辑器并装载图像。
///
/// 编辑器窗口是**按需创建**的（Konva 那套 WebView 常驻要几十 MB）：第一次标注时
/// 才在主线程建出来并保持隐藏，等前端把图画好再 `editor_ready` 显示，避免白窗闪一下。
pub fn open_editor_image(app: &AppHandle, img: RgbaImage) -> Result<(), String> {
    let (w, h) = (img.width(), img.height());
    let state = app.try_state::<EditorState>().ok_or("编辑器状态缺失")?;
    let id = state.seq.fetch_add(1, Ordering::SeqCst) + 1;
    *lock(&state.current) = Some((id, img));

    let payload = ImagePayload {
        image_url: crate::shot_url(&format!("editor/{id}")),
        width: w,
        height: h,
        scale: 1.0,
        display_width: None,
        display_height: None,
        from_line: false,
    };

    // 创建窗口必须回主线程（本函数常在阻塞线程池 / 截图会话线程里被调用）
    let (editor, created) = match app.get_webview_window("editor") {
        Some(win) => (win, false),
        None => {
            let win = on_main_thread(app, ensure_editor_window)?;
            (win, true)
        }
    };

    let _ = editor.emit_to("editor", "editor:open", payload);
    // 已加载过的窗口直接显示；新建的等前端画完再显示（editor_ready）
    if !created {
        reveal_editor(&editor);
    } else {
        let app2 = app.clone();
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(2500));
            // 前端异常没回调时的兜底：窗口不能永远藏着，否则用户以为「标注坏了」
            if let Some(win) = app2.get_webview_window("editor") {
                if !win.is_visible().unwrap_or(false) {
                    reveal_editor(&win);
                }
            }
        });
    }
    Ok(())
}

fn reveal_editor(win: &tauri::WebviewWindow) {
    let _ = win.show();
    let _ = win.unminimize();
    let _ = win.center();
    let _ = win.set_focus();
}

/// 主线程创建编辑器窗口（隐藏）。已存在则直接返回。
fn ensure_editor_window(app: &AppHandle) -> Result<tauri::WebviewWindow, String> {
    if let Some(win) = app.get_webview_window("editor") {
        return Ok(win);
    }
    WebviewWindowBuilder::new(app, "editor", WebviewUrl::App("editor.html".into()))
        .additional_browser_args(crate::BROWSER_ARGS)
        .title("Snapix · 标注")
        .inner_size(1120.0, 760.0)
        .min_inner_size(720.0, 520.0)
        .decorations(false)
        .transparent(true)
        .shadow(true)
        .center()
        .visible(false)
        .build()
        .map_err(|e| format!("创建标注窗口失败：{e}"))
}

/// 前端把待标注图挂上并完成首帧后回调：此时才显示窗口。
#[tauri::command]
pub fn editor_ready(app: AppHandle) {
    if let Some(win) = app.get_webview_window("editor") {
        reveal_editor(&win);
    }
}

/// 编辑器挂载时主动拉取待标注图（窗口刚创建时事件可能早于前端监听）。
#[tauri::command]
pub fn editor_pending(app: AppHandle) -> Option<ImagePayload> {
    let state = app.try_state::<EditorState>()?;
    let guard = lock(&state.current);
    let (id, img) = guard.as_ref()?;
    Some(ImagePayload {
        image_url: crate::shot_url(&format!("editor/{id}")),
        width: img.width(),
        height: img.height(),
        scale: 1.0,
        display_width: None,
        display_height: None,
        from_line: false,
    })
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
pub(crate) fn decode_data_url(data_url: &str) -> Result<RgbaImage, String> {
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
        // 窗口只是隐藏复用，前端还攥着整张图的 Konva 画布（4K 约 33MB，加滤镜缓存更多）。
        // 先通知前端卸掉画布再收窗，避免标注过一次之后内存永久高企。
        let _ = editor.emit_to("editor", "editor:close", ());
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
