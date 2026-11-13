mod capture;
mod cursor;
mod editor;
mod fastimg;
mod gifrec;
mod history;
mod line;
mod ocr;
mod pin;
mod scroll;
mod settings;
mod snap;
mod uia;
mod update;
mod util;
mod winfx;

use tauri::{AppHandle, Emitter, Manager};

pub(crate) use util::lock;

/// 所有 WebView 共用的 WebView2 浏览器参数（也写在 tauri.conf.json 的两个预建窗口上）。
///
/// `CalculateNativeWinOcclusion`：Chromium 的原生窗口遮挡检测。它按顶层窗口几何判断页面是否
/// 被完全盖住，被盖住的页面会被挂起。本应用的全屏遮罩窗在显示的一瞬间盖住了主窗口，多屏 +
/// 混合 DPI 下该检测偶发误判，把整个 WebView2 浏览器进程的 IPC 通道一并挂起——表现为遮罩不消失、
/// 贴图不出现、所有按钮和快捷键无响应，只能强杀。禁用后，8 轮连续贴图 + 各路径全部通过。
///
/// 前三个是 wry 的默认值（去掉迷你菜单 / PDF 工具栏 / SmartScreen），自定义参数会覆盖默认，
/// 因此必须一并带上。运行时创建的窗口必须传同一字符串：WebView2 要求共用一个用户数据目录的
/// 实例参数一致，否则后建的窗口创建失败。
pub const BROWSER_ARGS: &str =
    "--disable-features=msWebOOUI,msPdfOOUI,msSmartScreenProtection,CalculateNativeWinOcclusion";

/// 构造 shot 协议 URL（Windows 上自定义协议映射为 http://{scheme}.localhost/）。
pub fn shot_url(path: &str) -> String {
    #[cfg(windows)]
    return format!("http://shot.localhost/{path}");
    #[cfg(not(windows))]
    return format!("shot://localhost/{path}");
}

/// `shot://` 只允许应用自身的 WebView 跨源读取（冻屏帧含整屏内容，不能对任意源开放）。
/// 生产环境前端源为 tauri.localhost；开发环境为 Vite 开发服务器。
fn shot_cors_origin(request_origin: Option<&str>) -> Option<&'static str> {
    const ALLOWED: &[&str] = &[
        "http://tauri.localhost",
        "https://tauri.localhost",
        "tauri://localhost",
    ];
    let origin = request_origin?;
    if let Some(o) = ALLOWED.iter().find(|o| **o == origin) {
        return Some(o);
    }
    if tauri::is_dev() && origin.starts_with("http://localhost:") {
        // 开发服务器端口由 vite.config.ts 固定为 1430
        return Some("http://localhost:1430");
    }
    None
}

// 关于下面这些命令为什么是 `async`：
// 同步命令在 Windows 上直接运行于 WebView2 的事件回调（WebResourceRequested）内部，
// 此时对窗口做 show / hide / 移动会与 WebView2 的进程间调用互锁——不会崩溃、不报错，
// 而是此后所有 WebView 的 IPC 与自定义协议请求全部静默失效（按钮没反应、标题栏拖不动，
// 页面内纯前端逻辑却正常）。Tauri 文档对此有明确警告。`async` 命令在 tokio 线程执行，
// 窗口操作经事件循环代理投递到主线程，脱离回调上下文，从而避开该问题。

#[tauri::command]
async fn start_capture(app: AppHandle) {
    capture::show_overlay(&app);
}

#[tauri::command]
async fn cancel_capture(app: AppHandle) {
    capture::hide_overlay(&app);
}

/// 前端轻量提示（如发现新版本）：复用系统通知通道。
#[tauri::command]
fn notify_user(app: AppHandle, body: String) {
    capture::notify(&app, &body);
}

/// 打开日志目录，方便用户反馈问题时直接取日志。
#[tauri::command]
fn open_log_dir(app: AppHandle) -> Result<(), String> {
    use tauri_plugin_opener::OpenerExt;
    let dir = app.path().app_log_dir().map_err(|e| e.to_string())?;
    app.opener()
        .open_path(dir.to_string_lossy(), None::<&str>)
        .map_err(|e| e.to_string())
}

fn show_main(app: &AppHandle) {
    if let Some(win) = app.get_webview_window("main") {
        let _ = win.show();
        let _ = win.unminimize();
        let _ = win.set_focus();
    }
}

/// 主窗口默认位置与屏幕底边（任务栏之上）的间距，逻辑像素。
/// 无边框窗口的 outer_size 已含约 8px 阴影边距，可见间隙约为此值再加 8px。
const MAIN_BOTTOM_MARGIN: f64 = 8.0;

/// 首次显示主窗口：放在光标所在显示器（多屏时即用户正在用的那块，退回主显示器）
/// 工作区的水平居中、贴近底边处。配置里主窗口为隐藏创建，摆好位置再显示，
/// 避免先居中出现再跳一下。之后由用户拖动决定位置，托盘再打开时保持不变。
/// 主窗口上次的几何（独立小文件，不跟设置一起重置）。
#[derive(Clone, Copy, serde::Serialize, serde::Deserialize)]
struct MainBounds {
    x: i32,
    y: i32,
    w: u32,
    h: u32,
}

fn main_bounds_path(app: &AppHandle) -> Option<std::path::PathBuf> {
    app.path()
        .app_config_dir()
        .ok()
        .map(|d| d.join("main-window.json"))
}

fn load_main_bounds(app: &AppHandle) -> Option<MainBounds> {
    let b: MainBounds =
        serde_json::from_slice(&std::fs::read(main_bounds_path(app)?).ok()?).ok()?;
    // 显示器被拔掉 / 分辨率变了：不要还原到屏幕外
    let visible = app.available_monitors().ok().is_some_and(|ms| {
        ms.iter().any(|m| {
            let p = m.position();
            let s = m.size();
            b.x >= p.x && b.y >= p.y && b.x < p.x + s.width as i32 && b.y < p.y + s.height as i32
        })
    });
    visible.then_some(b)
}

fn save_main_bounds(app: &AppHandle) {
    let Some(win) = app.get_webview_window("main") else {
        return;
    };
    let (Ok(pos), Ok(size)) = (win.outer_position(), win.inner_size()) else {
        return;
    };
    let Some(path) = main_bounds_path(app) else {
        return;
    };
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let b = MainBounds {
        x: pos.x,
        y: pos.y,
        w: size.width,
        h: size.height,
    };
    if let Ok(json) = serde_json::to_vec(&b) {
        let _ = crate::util::write_atomic(&path, &json);
    }
}

fn place_main_bottom_center(app: &AppHandle) {
    let Some(win) = app.get_webview_window("main") else {
        return;
    };
    let monitor = app
        .cursor_position()
        .ok()
        .and_then(|p| app.monitor_from_point(p.x, p.y).ok().flatten())
        .or_else(|| app.primary_monitor().ok().flatten());
    let Some(monitor) = monitor else {
        return;
    };

    // 窗口此刻还在创建时所在显示器的 DPI 下；按目标显示器的缩放换算物理尺寸，
    // 跨 DPI 移动后系统会按逻辑尺寸重算，与这里的结果一致
    let (Ok(outer), Ok(cur_scale)) = (win.outer_size(), win.scale_factor()) else {
        return;
    };
    let target: tauri::PhysicalSize<i32> = outer
        .to_logical::<f64>(cur_scale)
        .to_physical(monitor.scale_factor());
    let area = monitor.work_area();
    let margin = (MAIN_BOTTOM_MARGIN * monitor.scale_factor()).round() as i32;

    let x = area.position.x + (area.size.width as i32 - target.width) / 2;
    let y = area.position.y + area.size.height as i32 - target.height - margin;
    let _ = win.set_position(tauri::PhysicalPosition::new(x, y.max(area.position.y)));
}

/// 打开主窗口并导航到指定页（home / history / settings）。
fn show_main_page(app: &AppHandle, page: &str) {
    show_main(app);
    let _ = app.emit_to("main", "nav", page.to_string());
}

#[cfg(desktop)]
fn setup_tray(app: &tauri::App) -> tauri::Result<()> {
    use tauri::menu::{Menu, MenuItem, PredefinedMenuItem, Submenu};
    use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};

    let capture_i = MenuItem::with_id(app, "capture", "截图", true, None::<&str>)?;
    let full_i = MenuItem::with_id(app, "capture-full", "全屏即拍", true, None::<&str>)?;
    let repeat_i = MenuItem::with_id(app, "repeat", "重复上次区域", true, None::<&str>)?;
    let pin_clip_i = MenuItem::with_id(app, "pin-clip", "贴图剪贴板图片", true, None::<&str>)?;
    let edit_clip_i = MenuItem::with_id(app, "edit-clip", "标注剪贴板图片", true, None::<&str>)?;
    let toggle_pins_i = MenuItem::with_id(
        app,
        "toggle-pins",
        "隐藏 / 显示全部贴图",
        true,
        None::<&str>,
    )?;
    let unthrough_i = MenuItem::with_id(
        app,
        "pins-unthrough",
        "解除贴图鼠标穿透",
        true,
        None::<&str>,
    )?;
    let delay_menu = Submenu::with_items(
        app,
        "延时截图",
        true,
        &[
            &MenuItem::with_id(app, "delay-3", "3 秒后", true, None::<&str>)?,
            &MenuItem::with_id(app, "delay-5", "5 秒后", true, None::<&str>)?,
            &MenuItem::with_id(app, "delay-10", "10 秒后", true, None::<&str>)?,
        ],
    )?;
    let line_i = MenuItem::with_id(app, "line-toggle", "显示 / 隐藏晾衣绳", true, None::<&str>)?;
    let line_clear_i = MenuItem::with_id(app, "line-clear", "清空晾衣绳", true, None::<&str>)?;
    let history_i = MenuItem::with_id(app, "history", "历史记录", true, None::<&str>)?;
    let settings_i = MenuItem::with_id(app, "settings", "设置", true, None::<&str>)?;
    let check_update_i = MenuItem::with_id(app, "check-update", "检查更新", true, None::<&str>)?;
    let show_i = MenuItem::with_id(app, "show", "打开主窗口", true, None::<&str>)?;
    let quit_i = MenuItem::with_id(app, "quit", "退出 Snapix", true, None::<&str>)?;
    let menu = Menu::with_items(
        app,
        &[
            &capture_i,
            &full_i,
            &repeat_i,
            &delay_menu,
            &PredefinedMenuItem::separator(app)?,
            &pin_clip_i,
            &edit_clip_i,
            &toggle_pins_i,
            &unthrough_i,
            &PredefinedMenuItem::separator(app)?,
            &line_i,
            &line_clear_i,
            &history_i,
            &settings_i,
            &check_update_i,
            &PredefinedMenuItem::separator(app)?,
            &show_i,
            &quit_i,
        ],
    )?;

    let mut builder = TrayIconBuilder::new()
        .tooltip("Snapix — 全能截图")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id.as_ref() {
            "capture" => capture::show_overlay(app),
            "capture-full" => capture::capture_full_impl(app),
            "repeat" => capture::repeat_last_capture(app),
            "delay-3" => capture::capture_delayed_impl(app.clone(), 3),
            "delay-5" => capture::capture_delayed_impl(app.clone(), 5),
            "delay-10" => capture::capture_delayed_impl(app.clone(), 10),
            "pin-clip" => {
                if let Err(e) = pin::pin_clipboard_impl(app) {
                    capture::notify(app, &e);
                }
            }
            "edit-clip" => {
                if let Err(e) = pin::edit_clipboard_impl(app) {
                    capture::notify(app, &e);
                }
            }
            "toggle-pins" => pin::toggle_pins(app.clone()),
            "pins-unthrough" => pin::pins_passthrough_off(app.clone()),
            "line-toggle" => line::toggle(app),
            "line-clear" => line::clear_all(app),
            "history" => show_main_page(app, "history"),
            "settings" => show_main_page(app, "settings"),
            "check-update" => {
                show_main_page(app, "settings");
                let _ = app.emit_to("main", "update:check", ());
            }
            "show" => show_main(app),
            "quit" => app.exit(0),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                show_main(tray.app_handle());
            }
        });

    if let Some(icon) = app.default_window_icon() {
        builder = builder.icon(icon.clone());
    }

    builder.build(app)?;
    Ok(())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let mut builder = tauri::Builder::default();
    // 单实例必须最先注册：否则第二次双击图标会起第二个进程，两个进程各自持有一份
    // 历史索引，任一方截图后整份回写 index.json，另一方的条目就被静默抹掉。
    #[cfg(desktop)]
    {
        builder = builder.plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
            // 二次启动视为「打开主窗口」，与托盘「打开主窗口」一致
            show_main(app);
            let _ = app.emit_to("main", "nav", "home".to_string());
        }));
    }
    let mut builder = builder
        .plugin(
            tauri_plugin_log::Builder::new()
                .clear_targets()
                .target(tauri_plugin_log::Target::new(
                    tauri_plugin_log::TargetKind::LogDir {
                        file_name: Some("snapix".into()),
                    },
                ))
                .target(tauri_plugin_log::Target::new(
                    tauri_plugin_log::TargetKind::Stdout,
                ))
                .level(log::LevelFilter::Warn)
                // 依赖库只记 warning，自己的模块保留 info，日志文件才不会被噪声塞满
                .level_for("tauri_app_lib", log::LevelFilter::Info)
                .level_for("tauri_app", log::LevelFilter::Info)
                .max_file_size(2_000_000)
                .rotation_strategy(tauri_plugin_log::RotationStrategy::KeepAll)
                .build(),
        )
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_clipboard_manager::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .manage(capture::CaptureState::default())
        .manage(pin::PinState::default())
        .manage(editor::EditorState::default())
        .manage(settings::SettingsState::default())
        .manage(history::HistoryState::default())
        .manage(line::LineState::default())
        .manage(scroll::ScrollState::default())
        .manage(update::UpdateState::default())
        .manage(gifrec::GifState::default())
        // shot:// 协议：零压缩位图直出（冻屏帧 / 编辑图 / 贴图 / 历史缩略）。
        // 关键：用异步版，在后台线程做 BMP 编码。同步协议处理器运行在主 UI 线程，
        // 4K 整屏 33MB 的编码会卡死主线程（曾导致 AppHang / 快捷键与按钮无响应）。
        .register_asynchronous_uri_scheme_protocol("shot", |ctx, request, responder| {
            let app = ctx.app_handle().clone();
            std::thread::spawn(move || {
                let path = request.uri().path().trim_start_matches('/').to_string();
                let (kind, rest) = match path.split_once('/') {
                    Some((k, r)) => (k.to_string(), r.to_string()),
                    None => (path.clone(), String::new()),
                };
                let cors = shot_cors_origin(
                    request
                        .headers()
                        .get("origin")
                        .and_then(|v| v.to_str().ok()),
                );

                let (body, mime): (Option<Vec<u8>>, &str) = match kind.as_str() {
                    "frame" => (
                        rest.parse()
                            .ok()
                            .and_then(|id| capture::frame_bmp(&app, id)),
                        "image/bmp",
                    ),
                    "editor" => (
                        rest.parse()
                            .ok()
                            .and_then(|id| editor::editor_bmp(&app, id)),
                        "image/bmp",
                    ),
                    "pin" => (pin::pin_bmp(&app, &rest), "image/bmp"),
                    "history" => (history::preview_bytes(&app, &rest), "image/png"),
                    "line" => (line::preview_bytes(&app, &rest), "image/png"),
                    _ => (None, "application/octet-stream"),
                };

                let mut builder = tauri::http::Response::builder()
                    .header("Vary", "Origin")
                    .header("X-Content-Type-Options", "nosniff");
                if let Some(origin) = cors {
                    builder = builder.header("Access-Control-Allow-Origin", origin);
                }
                let response = match body {
                    // URL 均带唯一 id，可安全缓存（放大镜 CSS 背景与 <img> 共用同一帧）
                    Some(bytes) => builder
                        .header("Content-Type", mime)
                        .header("Cache-Control", "private, max-age=300")
                        .body(bytes)
                        .unwrap(),
                    None => builder.status(404).body(Vec::new()).unwrap(),
                };
                responder.respond(response);
            });
        })
        // 无边框窗口仍会收到 Alt+F4 / 任务栏关闭：main 是预创建的单实例窗口，
        // editor 是首次标注时创建后复用的；真正销毁后托盘「打开主窗口」与「标注」
        // 都会静默失效（editor 销毁还能重建，但会丢掉当前标注图），因此一律拦截为隐藏。
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                match window.label() {
                    "main" => {
                        // 收进托盘前记住位置/大小，下次启动还原
                        save_main_bounds(window.app_handle());
                        api.prevent_close();
                        let _ = window.hide();
                    }
                    "editor" => {
                        api.prevent_close();
                        editor::close_editor_impl(window.app_handle());
                    }
                    // 滚动 / 录制控制条被 Alt+F4 或任务栏关闭：必须同时结束会话。
                    // 否则后台线程会继续往屏幕上注入滚轮、持续抓帧直到自然超时，
                    // 而 `active` 门闩在此期间会让所有截图入口静默失效，用户已没有停止入口。
                    "scroll-ctl" => scroll::scroll_capture_cancel(window.app_handle().clone()),
                    "gif-ctl" => gifrec::gif_record_cancel(window.app_handle().clone()),
                    _ => {}
                }
            }
        });

    #[cfg(desktop)]
    {
        use tauri_plugin_global_shortcut::ShortcutState;

        builder = builder
            .plugin(tauri_plugin_autostart::init(
                tauri_plugin_autostart::MacosLauncher::LaunchAgent,
                Some(vec!["--autostart"]),
            ))
            .plugin(
                tauri_plugin_global_shortcut::Builder::new()
                    .with_handler(move |app, shortcut, event| {
                        if event.state() != ShortcutState::Pressed {
                            return;
                        }
                        let s = settings::current(app);
                        if let Ok(sc) = settings::parse_shortcut(&s.hotkey_capture) {
                            if shortcut == &sc {
                                capture::show_overlay(app);
                                return;
                            }
                        }
                        if let Ok(sc) = settings::parse_shortcut(&s.hotkey_repeat) {
                            if shortcut == &sc {
                                capture::repeat_last_capture(app);
                                return;
                            }
                        }
                        if let Ok(sc) = settings::parse_shortcut(&s.hotkey_pin_clip) {
                            if shortcut == &sc {
                                if let Err(e) = pin::pin_clipboard_impl(app) {
                                    capture::notify(app, &e);
                                }
                                return;
                            }
                        }
                        if let Ok(sc) = settings::parse_shortcut(&s.hotkey_full) {
                            if shortcut == &sc {
                                capture::capture_full_impl(app);
                                return;
                            }
                        }
                        if let Ok(sc) = settings::parse_shortcut(&s.hotkey_window) {
                            if shortcut == &sc {
                                capture::capture_window_impl(app);
                                return;
                            }
                        }
                        if let Ok(sc) = settings::parse_shortcut(&s.hotkey_toggle_pins) {
                            if shortcut == &sc {
                                pin::toggle_pins(app.clone());
                            }
                        }
                    })
                    .build(),
            );

        builder = builder.setup(move |app| {
            let handle = app.handle();
            let launched_by_autostart = std::env::args().any(|a| a == "--autostart");
            log::info!(
                "Snapix {} 启动（自启={launched_by_autostart}）",
                env!("CARGO_PKG_VERSION")
            );

            // 加载设置并注册全局快捷键
            let loaded = settings::load(handle);
            *lock(&app.state::<settings::SettingsState>().0) = loaded.clone();
            if let Err(e) = settings::apply_hotkeys(handle, &loaded) {
                // 被其他软件占用等情况：不能静默，否则用户只会觉得「快捷键坏了」
                log::warn!("快捷键注册失败: {e}");
                capture::notify(handle, &format!("{e}，请到「设置 → 快捷键」重新设置"));
            }
            // 老版本注册的自启项没有 --autostart 参数，这里重写一次
            #[cfg(desktop)]
            if loaded.autostart {
                settings::apply_autostart(handle, true);
            }

            history::load(handle);
            line::load(handle);
            line::spawn_watcher(handle.clone());
            setup_tray(app)?;

            // 预创建各显示器的遮罩窗（隐藏），首次截图免 WebView 冷启动
            let monitor_count = xcap::Monitor::all().map(|m| m.len()).unwrap_or(1);
            capture::ensure_overlay_windows(handle, monitor_count.max(1));

            // 有保存过的主窗几何就还原；否则首次运行放屏幕底部居中。
            match load_main_bounds(handle) {
                Some(b) => {
                    if let Some(win) = handle.get_webview_window("main") {
                        let _ = win.set_position(tauri::PhysicalPosition::new(b.x, b.y));
                        let _ = win.set_size(tauri::PhysicalSize::new(b.w, b.h));
                    }
                }
                None => place_main_bottom_center(handle),
            }
            // 开机自启静默进托盘，不抢焦点弹窗
            if !launched_by_autostart {
                show_main(handle);
            }
            Ok(())
        });
    }

    let app = builder
        .invoke_handler(tauri::generate_handler![
            start_capture,
            cancel_capture,
            notify_user,
            open_log_dir,
            capture::overlay_ready,
            capture::overlay_fetch,
            capture::overlay_claim,
            capture::capture_delayed,
            capture::capture_full,
            capture::capture_window,
            capture::repeat_capture,
            capture::copy_region,
            capture::copy_to_line,
            capture::save_region,
            editor::open_editor,
            editor::copy_image,
            editor::save_image,
            editor::close_editor,
            pin::pin_region,
            pin::pin_ready,
            pin::get_pin_image,
            pin::pin_copy,
            pin::pin_save,
            pin::pin_rotate,
            pin::pin_flip,
            pin::pin_passthrough,
            pin::pins_passthrough_off,
            pin::pin_annotate,
            pin::pin_data,
            pin::close_pin,
            pin::pin_clipboard,
            pin::edit_clipboard,
            pin::toggle_pins,
            capture::copy_text,
            capture::ocr_region,
            capture::qr_region,
            editor::editor_detect_privacy,
            editor::editor_ready,
            editor::editor_pending,
            uia::element_at,
            update::update_fetch,
            update::update_download,
            update::update_cancel,
            update::update_install,
            scroll::scroll_capture_start,
            scroll::scroll_capture_stop,
            scroll::scroll_capture_cancel,
            gifrec::gif_record_start,
            gifrec::gif_record_stop,
            gifrec::gif_record_cancel,
            settings::get_settings,
            settings::update_settings,
            settings::reset_settings,
            settings::pick_folder,
            settings::hotkeys_suspend,
            settings::hotkeys_resume,
            history::history_list,
            history::history_delete,
            history::history_clear,
            history::history_open_dir,
            history::history_copy,
            history::history_save_as,
            history::history_pin,
            history::history_edit,
            line::line_list,
            line::line_move,
            line::line_remove,
            line::line_clear,
            line::line_copy,
            line::line_save,
            line::line_pin,
            line::line_hit_rects,
            line::line_dragging,
            line::line_landed,
            line::line_item_ready,
            line::line_prepare_detach,
            line::line_reveal_detach,
            line::line_finish_detach,
            line::line_cancel_detach,
            line::line_detach_target,
            line::line_drop_hint,
            line::line_return_pin,
            line::line_return_takeoff,
            line::line_return_landed
        ])
        .build(tauri::generate_context!())
        .expect("error while building tauri application");

    // 记录退出原因（诊断偶发的非零退出码）
    app.run(|app, event| match &event {
        tauri::RunEvent::ExitRequested { code, .. } => {
            log::info!("exit requested, code={code:?}");
        }
        tauri::RunEvent::Exit => {
            save_main_bounds(app);
            log::info!("event loop exited");
        }
        _ => {}
    });
}
