//! 设置中心：Rust 侧单一数据源（appconfig/settings.json）。
//! 前端通过 get_settings / update_settings 读写；
//! 快捷键与开机自启在 update 时即时生效。

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager};

use crate::lock;
use crate::util::{blocking, write_atomic};

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Settings {
    /// 截图快捷键，如 "Ctrl+Shift+A"
    pub hotkey_capture: String,
    /// 重复上次区域快捷键
    pub hotkey_repeat: String,
    /// 贴图剪贴板图片快捷键
    pub hotkey_pin_clip: String,
    /// 全屏即拍快捷键（截取光标所在屏 → 剪贴板）
    pub hotkey_full: String,
    /// 窗口即拍快捷键（截取前台窗口 → 剪贴板）
    pub hotkey_window: String,
    /// 隐藏 / 显示全部贴图快捷键
    pub hotkey_toggle_pins: String,
    /// 默认保存目录（空 = 图片库/Snapix）
    pub save_dir: String,
    /// 文件命名模板：{date} {time} {w} {h}
    pub file_template: String,
    /// png | jpg
    pub image_format: String,
    /// true = 保存时弹出对话框；false = 直接存到默认目录
    pub save_ask: bool,
    /// 保存后同时复制到剪贴板
    pub copy_after_save: bool,
    /// 开机自启
    pub autostart: bool,
    /// 历史记录开关
    pub history_enabled: bool,
    /// 历史记录容量上限
    pub history_limit: u32,
    /// 截图产出是否自动挂到「晾衣绳」
    pub line_enabled: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            hotkey_capture: "Ctrl+Shift+A".into(),
            // 默认留空：常见组合（如 Ctrl+Shift+R）会与浏览器等软件冲突，由用户自选
            hotkey_repeat: String::new(),
            hotkey_pin_clip: String::new(),
            hotkey_full: String::new(),
            hotkey_window: String::new(),
            hotkey_toggle_pins: String::new(),
            save_dir: String::new(),
            file_template: "Snapix_{date}_{time}".into(),
            image_format: "png".into(),
            save_ask: true,
            copy_after_save: false,
            autostart: false,
            history_enabled: true,
            history_limit: 200,
            line_enabled: true,
        }
    }
}

#[derive(Default)]
pub struct SettingsState(pub Mutex<Settings>);

fn config_path(app: &AppHandle) -> Option<PathBuf> {
    app.path()
        .app_config_dir()
        .ok()
        .map(|d| d.join("settings.json"))
}

/// 从磁盘加载（失败回退默认）。
pub fn load(app: &AppHandle) -> Settings {
    config_path(app)
        .and_then(|p| fs::read_to_string(p).ok())
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

/// 原子写设置文件。失败（磁盘满 / 目录只读 / 被杀软拦截）必须回报给调用方——
/// 否则界面显示「已保存」，重启后全部回退，用户无从察觉。
fn persist(app: &AppHandle, s: &Settings) -> Result<(), String> {
    let p = config_path(app).ok_or("无法定位配置目录")?;
    if let Some(dir) = p.parent() {
        fs::create_dir_all(dir).map_err(|e| format!("创建配置目录失败：{e}"))?;
    }
    let json = serde_json::to_string_pretty(s).map_err(|e| e.to_string())?;
    write_atomic(&p, json.as_bytes()).map_err(|e| format!("写入设置失败：{e}"))
}

/// 读取当前设置快照。
pub fn current(app: &AppHandle) -> Settings {
    app.try_state::<SettingsState>()
        .map(|st| lock(&st.0).clone())
        .unwrap_or_default()
}

// ===================== 快捷键 =====================

/// 解析快捷键字符串，兼容 "Ctrl+Shift+A" 与 "Ctrl+Shift+KeyA" 两种写法；
/// 单键（"F1"、"PrintScreen"、"Insert"）与其它 W3C 键名（"Ctrl+Numpad1"、"Alt+PageUp"）
/// 由底层解析器直接支持。
#[cfg(desktop)]
pub fn parse_shortcut(text: &str) -> Result<tauri_plugin_global_shortcut::Shortcut, String> {
    if let Ok(sc) = text.parse() {
        return Ok(sc);
    }
    // 变体 1：单字符键 → W3C code（A → KeyA、1 → Digit1）
    let v1 = map_tokens(text, |t| {
        let mut chars = t.chars();
        match (chars.next(), chars.next()) {
            (Some(c), None) if c.is_ascii_alphabetic() => {
                Some(format!("Key{}", c.to_ascii_uppercase()))
            }
            (Some(c), None) if c.is_ascii_digit() => Some(format!("Digit{c}")),
            _ => None,
        }
    });
    if let Ok(sc) = v1.parse() {
        return Ok(sc);
    }
    // 变体 2：W3C code → 单字符键（KeyA → A、Digit1 → 1）
    let v2 = map_tokens(text, |t| {
        t.strip_prefix("Key")
            .filter(|r| r.len() == 1)
            .or_else(|| t.strip_prefix("Digit").filter(|r| r.len() == 1))
            .map(str::to_string)
    });
    v2.parse().map_err(|_| format!("无法识别快捷键「{text}」"))
}

#[cfg(desktop)]
fn map_tokens(text: &str, f: impl Fn(&str) -> Option<String>) -> String {
    text.split('+')
        .map(|t| {
            let t = t.trim();
            f(t).unwrap_or_else(|| t.to_string())
        })
        .collect::<Vec<_>>()
        .join("+")
}

/// 全部可配置快捷键及其功能名（注册 / 校验 / 报错共用一份清单）。
pub fn hotkey_fields(s: &Settings) -> [(&'static str, &String); 6] {
    [
        ("截图", &s.hotkey_capture),
        ("重复上次区域", &s.hotkey_repeat),
        ("贴图剪贴板图片", &s.hotkey_pin_clip),
        ("全屏即拍", &s.hotkey_full),
        ("窗口即拍", &s.hotkey_window),
        ("隐藏 / 显示全部贴图", &s.hotkey_toggle_pins),
    ]
}

/// 校验全部快捷键：可解析，且两个功能不能共用同一组合键
/// （否则注册第二个时会失败，报出的「被占用」还会误导用户）。
#[cfg(desktop)]
fn validate_hotkeys(s: &Settings) -> Result<(), String> {
    let mut seen: Vec<(&str, tauri_plugin_global_shortcut::Shortcut)> = Vec::new();
    for (name, text) in hotkey_fields(s) {
        if text.trim().is_empty() {
            continue;
        }
        let sc = parse_shortcut(text)?;
        if let Some((other, _)) = seen.iter().find(|(_, o)| *o == sc) {
            return Err(format!(
                "「{name}」与「{other}」不能使用同一个快捷键「{text}」"
            ));
        }
        seen.push((name, sc));
    }
    Ok(())
}

/// 依据设置重新注册全部全局快捷键。
#[cfg(desktop)]
pub fn apply_hotkeys(app: &AppHandle, s: &Settings) -> Result<(), String> {
    use tauri_plugin_global_shortcut::GlobalShortcutExt;

    let gs = app.global_shortcut();
    gs.unregister_all().map_err(|e| e.to_string())?;

    for (name, text) in hotkey_fields(s) {
        if text.trim().is_empty() {
            continue;
        }
        let sc = parse_shortcut(text)?;
        gs.register(sc)
            .map_err(|_| format!("「{name}」快捷键「{text}」注册失败（可能被其他软件占用）"))?;
    }
    Ok(())
}

#[cfg(desktop)]
fn apply_autostart(app: &AppHandle, enable: bool) {
    use tauri_plugin_autostart::ManagerExt;
    let m = app.autolaunch();
    let _ = if enable { m.enable() } else { m.disable() };
}

// ===================== 保存路径 / 命名 =====================

/// 解析默认保存目录（确保存在）。
pub fn resolve_save_dir(app: &AppHandle, s: &Settings) -> PathBuf {
    let dir = if s.save_dir.trim().is_empty() {
        app.path()
            .picture_dir()
            .map(|d| d.join("Snapix"))
            .unwrap_or_else(|_| PathBuf::from("."))
    } else {
        PathBuf::from(s.save_dir.trim())
    };
    let _ = fs::create_dir_all(&dir);
    dir
}

/// Windows 保留设备名（不区分大小写，带扩展名也不允许）。
const RESERVED_NAMES: &[&str] = &[
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
    "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

/// 渲染文件名模板（不含扩展名），并清理非法字符 / 控制字符 / 保留名 / 结尾点空格。
pub fn render_name(template: &str, w: u32, h: u32) -> String {
    let now = chrono::Local::now();
    let name: String = template
        .replace("{date}", &now.format("%Y%m%d").to_string())
        .replace("{time}", &now.format("%H%M%S").to_string())
        .replace("{w}", &w.to_string())
        .replace("{h}", &h.to_string())
        .chars()
        .map(|c| match c {
            '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*' => '-',
            c if c.is_control() => '-',
            c => c,
        })
        .collect();
    // Windows 不允许文件名以点或空格结尾
    let name = name.trim().trim_end_matches(['.', ' ']).trim();
    if name.is_empty() {
        return format!("Snapix_{}", now.format("%Y%m%d_%H%M%S"));
    }
    let base = name.split('.').next().unwrap_or(name).to_ascii_uppercase();
    if RESERVED_NAMES.contains(&base.as_str()) {
        format!("Snapix_{name}")
    } else {
        name.to_string()
    }
}

/// 生成不冲突的完整保存路径。
pub fn unique_path(dir: &Path, stem: &str, ext: &str) -> PathBuf {
    let mut p = dir.join(format!("{stem}.{ext}"));
    let mut i = 1;
    while p.exists() {
        p = dir.join(format!("{stem}_{i}.{ext}"));
        i += 1;
    }
    p
}

// ===================== 命令 =====================

#[tauri::command]
pub fn get_settings(app: AppHandle) -> Settings {
    current(&app)
}

/// 更新设置。快捷键先试注册、成功后才写入内存与磁盘；
/// 注册失败时回滚到旧快捷键并返回错误，避免坏配置落盘导致下次启动也失效。
#[tauri::command]
pub fn update_settings(app: AppHandle, settings: Settings) -> Result<(), String> {
    #[cfg(desktop)]
    validate_hotkeys(&settings)?;

    let old = current(&app);

    #[cfg(desktop)]
    {
        let changed = hotkey_fields(&old)
            .iter()
            .zip(hotkey_fields(&settings).iter())
            .any(|((_, a), (_, b))| a != b);
        if changed {
            if let Err(e) = apply_hotkeys(&app, &settings) {
                let _ = apply_hotkeys(&app, &old);
                return Err(e);
            }
        }
    }

    // 先落盘再改内存：写失败时内存保持旧值，界面与磁盘不会长期不一致
    if let Err(e) = persist(&app, &settings) {
        #[cfg(desktop)]
        let _ = apply_hotkeys(&app, &old);
        return Err(e);
    }
    if let Some(st) = app.try_state::<SettingsState>() {
        *lock(&st.0) = settings.clone();
    }

    #[cfg(desktop)]
    if old.autostart != settings.autostart {
        apply_autostart(&app, settings.autostart);
    }
    Ok(())
}

/// 录制快捷键期间暂停全局热键，避免误触截图。
#[tauri::command]
pub fn hotkeys_suspend(app: AppHandle) {
    #[cfg(desktop)]
    {
        use tauri_plugin_global_shortcut::GlobalShortcutExt;
        let _ = app.global_shortcut().unregister_all();
    }
    #[cfg(not(desktop))]
    let _ = app;
}

/// 录制结束后恢复全局热键。
#[tauri::command]
pub fn hotkeys_resume(app: AppHandle) {
    #[cfg(desktop)]
    {
        let s = current(&app);
        let _ = apply_hotkeys(&app, &s);
    }
    #[cfg(not(desktop))]
    let _ = app;
}

/// 弹出目录选择框（用于设置保存目录）。阻塞等待用户，放到阻塞线程池执行。
#[tauri::command]
pub async fn pick_folder(app: AppHandle) -> Result<Option<String>, String> {
    blocking(move || {
        use tauri_plugin_dialog::DialogExt;
        Ok(app
            .dialog()
            .file()
            .blocking_pick_folder()
            .and_then(|f| f.into_path().ok())
            .map(|p| p.to_string_lossy().to_string()))
    })
    .await
}

#[cfg(test)]
mod tests {
    #[test]
    fn parse_common_hotkeys() {
        assert!(super::parse_shortcut("Ctrl+Shift+A").is_ok());
        assert!(super::parse_shortcut("Ctrl+Shift+KeyA").is_ok());
        assert!(super::parse_shortcut("Alt+F5").is_ok());
        assert!(super::parse_shortcut("Ctrl+Alt+1").is_ok());
        assert!(super::parse_shortcut("随便什么").is_err());
    }

    #[test]
    fn parse_single_and_special_keys() {
        // 前端允许单独使用的键
        for k in ["F1", "F24", "PrintScreen", "ScrollLock", "Pause", "Insert"] {
            assert!(super::parse_shortcut(k).is_ok(), "{k}");
        }
        // 需配合修饰键的其它键，含符号写法
        for k in [
            "Ctrl+Numpad1",
            "Ctrl+NumpadAdd",
            "Alt+PageUp",
            "Ctrl+Shift+ArrowUp",
            "Ctrl+Alt+`",
            "Ctrl+Shift+/",
            "Super+Space",
        ] {
            assert!(super::parse_shortcut(k).is_ok(), "{k}");
        }
        // 只有修饰键不是快捷键
        assert!(super::parse_shortcut("Ctrl").is_err());
        assert!(super::parse_shortcut("Ctrl+Shift").is_err());
    }

    #[test]
    fn render_name_template() {
        let n = super::render_name("Snapix_{date}_{time}_{w}x{h}", 800, 600);
        assert!(n.starts_with("Snapix_"));
        assert!(n.ends_with("_800x600"));
        assert!(!n.contains('{'));
        // 非法字符清理
        assert_eq!(super::render_name("a:b|c", 1, 1), "a-b-c");
    }

    #[test]
    fn render_name_rejects_reserved_and_trailing_dots() {
        assert_eq!(super::render_name("con", 1, 1), "Snapix_con");
        assert_eq!(super::render_name("NUL.shot", 1, 1), "Snapix_NUL.shot");
        assert_eq!(super::render_name("shot... ", 1, 1), "shot");
        assert_eq!(super::render_name("a\u{1}b", 1, 1), "a-b");
        assert!(super::render_name("...", 1, 1).starts_with("Snapix_"));
    }

    #[test]
    fn duplicate_hotkeys_rejected() {
        let mut s = super::Settings {
            hotkey_full: "Ctrl+Shift+A".into(),
            ..Default::default()
        };
        let err = super::validate_hotkeys(&s).unwrap_err();
        assert!(err.contains("全屏即拍"));
        assert!(err.contains("截图"));

        s.hotkey_full = "Ctrl+Shift+KeyA".into(); // 同一键的另一种写法也算重复
        assert!(super::validate_hotkeys(&s).is_err());

        s.hotkey_full = "Ctrl+Shift+F".into();
        assert!(super::validate_hotkeys(&s).is_ok());
    }
}
