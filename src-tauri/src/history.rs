//! 历史记录：截图/标注产出自动落盘到 appdata/history，
//! index.json 维护元数据，容量超限自动裁剪；另存一张缩略图供面板展示。

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use image::RgbaImage;
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager};

use crate::lock;
use crate::settings;
use crate::util::{blocking, write_atomic};

/// 缩略图最长边（面板卡片高 128 CSS px，2x DPI 也够用）
const THUMB_MAX: u32 = 360;

/// 历史目录总大小上限（字节）。张数上限挡不住体积：4K PNG 单张 5~15MB，
/// 200 张轻松吃到 1~2GB。超出后按时间从旧到新淘汰。
const MAX_TOTAL_BYTES: u64 = 1_500 * 1024 * 1024;

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryItem {
    pub id: String,
    /// Unix 毫秒
    pub ts: i64,
    pub width: u32,
    pub height: u32,
    /// capture | editor
    pub source: String,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryEntryOut {
    #[serde(flatten)]
    pub item: HistoryItem,
    pub url: String,
}

#[derive(Default)]
pub struct HistoryState {
    pub items: Mutex<Vec<HistoryItem>>,
    seq: AtomicU64,
}

fn history_dir(app: &AppHandle) -> Option<PathBuf> {
    let dir = app.path().app_data_dir().ok()?.join("history");
    fs::create_dir_all(&dir).ok()?;
    Some(dir)
}

fn index_path(app: &AppHandle) -> Option<PathBuf> {
    history_dir(app).map(|d| d.join("index.json"))
}

/// 历史 id 形如 `{ts}-{seq}`，只允许数字与连字符；所有按 id 定位文件的入口都经此校验，
/// 防止前端传入 `..\` 之类的路径穿越出历史目录。
fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 40 && id.chars().all(|c| c.is_ascii_digit() || c == '-')
}

fn file_path(app: &AppHandle, id: &str) -> Option<PathBuf> {
    if !valid_id(id) {
        return None;
    }
    history_dir(app).map(|d| d.join(format!("{id}.png")))
}

fn thumb_path(app: &AppHandle, id: &str) -> Option<PathBuf> {
    if !valid_id(id) {
        return None;
    }
    history_dir(app).map(|d| d.join(format!("{id}.thumb.png")))
}

fn remove_files(app: &AppHandle, id: &str) {
    for p in [file_path(app, id), thumb_path(app, id)]
        .into_iter()
        .flatten()
    {
        let _ = fs::remove_file(p);
    }
}

/// 单条历史占用的磁盘字节（原图 + 缩略图）。
fn item_bytes(app: &AppHandle, id: &str) -> u64 {
    [file_path(app, id), thumb_path(app, id)]
        .into_iter()
        .flatten()
        .filter_map(|p| fs::metadata(p).ok())
        .map(|m| m.len())
        .sum()
}

/// 启动时加载索引。
///
/// 顺带核对每条的图片文件是否还在：用户可能手动清理过目录，或上次写盘中途退出，
/// 留着会变成面板里的破图卡片（缩略图与原图都读不出来）。失效条目直接摘掉并回写。
pub fn load(app: &AppHandle) {
    let Some(path) = index_path(app) else { return };
    let items: Vec<HistoryItem> = fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();

    let total = items.len();
    let kept: Vec<HistoryItem> = items
        .into_iter()
        .filter(|it| file_path(app, &it.id).is_some_and(|p| p.exists()))
        .collect();

    if let Some(st) = app.try_state::<HistoryState>() {
        let mut cur = lock(&st.items);
        // 只在真的摘掉条目时才回写，避免每次启动都整份重写索引
        let dirty = kept.len() != total;
        *cur = kept;
        if dirty {
            persist(app, &cur);
        }
    }
}

/// 写索引。调用方需持有 `items` 锁，保证磁盘顺序与内存一致（并发入库不丢条目）。
fn persist(app: &AppHandle, items: &[HistoryItem]) {
    if let Some(path) = index_path(app) {
        if let Ok(json) = serde_json::to_string(items) {
            let _ = write_atomic(&path, json.as_bytes());
        }
    }
}

fn notify_changed(app: &AppHandle) {
    let _ = app.emit_to("main", "history:changed", ());
}

/// 生成缩略图（小图直接跳过，面板回退到原图）。
fn write_thumb(img: &RgbaImage, path: &Path) {
    let (w, h) = img.dimensions();
    if w.max(h) <= THUMB_MAX {
        return;
    }
    let k = THUMB_MAX as f32 / w.max(h) as f32;
    let tw = ((w as f32 * k).round() as u32).max(1);
    let th = ((h as f32 * k).round() as u32).max(1);
    let _ = image::imageops::thumbnail(img, tw, th).save(path);
}

/// 记录一张产出图（后台线程编码写盘，不阻塞主链路）。
///
/// 顺带把图挂到「晾衣绳」：两者共用同一次编码线程，避免为同一张图再复制一份
/// 4K 位图（约 33MB）。历史记录关掉时晾衣绳仍可独立工作。
pub fn record(app: &AppHandle, img: RgbaImage, source: &str) {
    record_with(app, img, source, true);
}

/// 同上，但**不挂绳**。给「对勾 → 飞向晾衣绳」用：那条路径已经先把图挂上
/// （带 pending 标记，等飞行动画落地），若再让 `record` 挂一次就会多出一张。
pub fn record_no_line(app: &AppHandle, img: RgbaImage, source: &str) {
    record_with(app, img, source, false);
}

fn record_with(app: &AppHandle, img: RgbaImage, source: &str, hang_line: bool) {
    let s = settings::current(app);
    let want_line = hang_line && s.line_enabled;
    if !s.history_enabled && !want_line {
        return;
    }
    let Some(st) = app.try_state::<HistoryState>() else {
        return;
    };

    let ts = chrono::Local::now().timestamp_millis();
    let seq = st.seq.fetch_add(1, Ordering::SeqCst);
    let id = format!("{ts}-{seq}");
    let (width, height) = (img.width(), img.height());
    let paths = if s.history_enabled {
        match (file_path(app, &id), thumb_path(app, &id)) {
            (Some(p), Some(t)) => Some((p, t)),
            _ => None,
        }
    } else {
        None
    };
    let index = s.history_enabled.then_some(id.clone());

    let app = app.clone();
    let source = source.to_string();
    std::thread::spawn(move || {
        if want_line {
            crate::line::hang(&app, &img);
        }
        let (Some((path, thumb)), Some(id)) = (paths, index) else {
            return;
        };
        if img.save(&path).is_err() {
            return;
        }
        write_thumb(&img, &thumb);

        let item = HistoryItem {
            id,
            ts,
            width,
            height,
            source,
        };

        let limit = settings::current(&app).history_limit.max(10) as usize;
        let removed: Vec<HistoryItem> = {
            let st = app.state::<HistoryState>();
            let mut items = lock(&st.items);
            items.insert(0, item);
            let mut removed = if items.len() > limit {
                items.split_off(limit)
            } else {
                Vec::new()
            };
            // 张数达标后再按体积淘汰最旧的（items 首项最新，从尾部弹出）
            let mut total: u64 = items.iter().map(|it| item_bytes(&app, &it.id)).sum();
            while total > MAX_TOTAL_BYTES && items.len() > 1 {
                let last = items.pop().expect("len > 1");
                total = total.saturating_sub(item_bytes(&app, &last.id));
                removed.push(last);
            }
            persist(&app, &items);
            removed
        };
        for r in &removed {
            remove_files(&app, &r.id);
        }
        notify_changed(&app);
    });
}

fn load_image(app: &AppHandle, id: &str) -> Result<RgbaImage, String> {
    let path = file_path(app, id).ok_or("无效的历史记录")?;
    let img = image::open(path).map_err(|e| e.to_string())?;
    Ok(img.to_rgba8())
}

// ===================== 命令 =====================

#[tauri::command]
pub fn history_list(app: AppHandle) -> Vec<HistoryEntryOut> {
    let Some(st) = app.try_state::<HistoryState>() else {
        return Vec::new();
    };
    let items = lock(&st.items);
    items
        .iter()
        .map(|it| HistoryEntryOut {
            item: it.clone(),
            url: crate::shot_url(&format!("history/{}", it.id)),
        })
        .collect()
}

#[tauri::command]
pub async fn history_delete(app: AppHandle, id: String) -> Result<(), String> {
    blocking(move || {
        if !valid_id(&id) {
            return Err("无效的历史记录".to_string());
        }
        let st = app.try_state::<HistoryState>().ok_or("历史状态缺失")?;
        {
            let mut items = lock(&st.items);
            items.retain(|it| it.id != id);
            persist(&app, &items);
        }
        remove_files(&app, &id);
        notify_changed(&app);
        Ok(())
    })
    .await
}

#[tauri::command]
pub async fn history_clear(app: AppHandle) -> Result<(), String> {
    blocking(move || {
        let st = app.try_state::<HistoryState>().ok_or("历史状态缺失")?;
        let old: Vec<HistoryItem> = {
            let mut items = lock(&st.items);
            let old = std::mem::take(&mut *items);
            persist(&app, &items);
            old
        };
        for it in &old {
            remove_files(&app, &it.id);
        }
        notify_changed(&app);
        Ok(())
    })
    .await
}

#[tauri::command]
pub fn history_open_dir(app: AppHandle) -> Result<(), String> {
    use tauri_plugin_opener::OpenerExt;
    let dir = history_dir(&app).ok_or("无法定位历史目录")?;
    app.opener()
        .open_path(dir.to_string_lossy(), None::<&str>)
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn history_copy(app: AppHandle, id: String) -> Result<(), String> {
    blocking(move || {
        use tauri_plugin_clipboard_manager::ClipboardExt;
        let img = load_image(&app, &id)?;
        let image = tauri::image::Image::new(img.as_raw(), img.width(), img.height());
        app.clipboard()
            .write_image(&image)
            .map_err(|e| e.to_string())
    })
    .await
}

#[tauri::command]
pub async fn history_save_as(
    window: tauri::WebviewWindow,
    app: AppHandle,
    id: String,
) -> Result<Option<String>, String> {
    blocking(move || {
        let img = load_image(&app, &id)?;
        let saved = crate::capture::save_with_dialog(&app, &img, Some(&window))?;
        Ok(saved.map(|p| p.to_string_lossy().to_string()))
    })
    .await
}

/// 面板预览字节（供 shot://history/{id}）：优先缩略图，没有则回退原图。
pub fn preview_bytes(app: &AppHandle, id: &str) -> Option<Vec<u8>> {
    if let Some(bytes) = thumb_path(app, id).and_then(|p| fs::read(p).ok()) {
        return Some(bytes);
    }
    fs::read(file_path(app, id)?).ok()
}

#[tauri::command]
pub async fn history_pin(app: AppHandle, id: String) -> Result<(), String> {
    blocking(move || {
        let img = load_image(&app, &id)?;
        crate::capture::spawn_pin(&app, img, None, 1.0)
    })
    .await
}

#[tauri::command]
pub async fn history_edit(app: AppHandle, id: String) -> Result<(), String> {
    blocking(move || {
        let img = load_image(&app, &id)?;
        crate::capture::open_editor_image(&app, img)
    })
    .await
}

#[cfg(test)]
mod tests {
    #[test]
    fn id_validation_blocks_traversal() {
        assert!(super::valid_id("1725270000000-3"));
        assert!(!super::valid_id(""));
        assert!(!super::valid_id("../../secret"));
        assert!(!super::valid_id("..\\x"));
        assert!(!super::valid_id("1725270000000-3.png"));
        assert!(!super::valid_id(&"9".repeat(41)));
    }
}
