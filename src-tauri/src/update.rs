//! 自动更新：自建下载链路，只为拿到「真正的取消」。
//!
//! `tauri-plugin-updater` 的 `downloadAndInstall` 没有中止接口，下载一旦开始只能等它结束；
//! 断网时也只会报错，用户既停不掉也没法续。
//!
//! 这里改成两段式：`Update::download` 在可 abort 的异步任务里跑，
//! 取消 = abort 任务 → 丢掉响应流，HTTP 连接立刻断开。
//! 签名校验仍然由插件完成（`download` 返回前会验证 minisign 签名），
//! 安装用 `Update::install`，与原来的行为一致。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_updater::UpdaterExt;

use crate::lock;

#[derive(Default)]
pub struct UpdateState {
    /// 已下载并通过签名校验的安装包
    bytes: Mutex<Option<Vec<u8>>>,
    /// 下载任务句柄，取消时 abort
    task: Mutex<Option<tauri::async_runtime::JoinHandle<()>>>,
    running: AtomicBool,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateInfo {
    pub version: String,
    pub current_version: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub date: Option<String>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct Progress {
    downloaded: u64,
    total: Option<u64>,
}

/// 检查更新。结果里只有元数据，真正的下载要再调 `update_download`。
#[tauri::command]
pub async fn update_fetch(app: AppHandle) -> Result<Option<UpdateInfo>, String> {
    let updater = app.updater().map_err(|e| e.to_string())?;
    let found = updater.check().await.map_err(|e| e.to_string())?;
    if let Some(s) = app.try_state::<UpdateState>() {
        *lock(&s.bytes) = None;
    }
    Ok(found.map(|u| UpdateInfo {
        version: u.version.clone(),
        current_version: u.current_version.clone(),
        notes: u
            .body
            .clone()
            .map(|b| b.trim().to_string())
            .filter(|b| !b.is_empty()),
        date: u.date.map(|d| d.to_string()),
    }))
}

/// 开始下载（带进度事件）。重复调用直接返回，避免并发下载同一份包。
#[tauri::command]
pub fn update_download(app: AppHandle) -> Result<(), String> {
    let state = app.state::<UpdateState>();
    if state.running.swap(true, Ordering::SeqCst) {
        return Err("正在下载".into());
    }
    if lock(&state.bytes).is_some() {
        state.running.store(false, Ordering::SeqCst);
        let _ = app.emit("update:downloaded", ());
        return Ok(());
    }

    let app2 = app.clone();
    let handle = tauri::async_runtime::spawn(async move {
        let result = async {
            let updater = app2.updater().map_err(|e| e.to_string())?;
            let update = updater
                .check()
                .await
                .map_err(|e| e.to_string())?
                .ok_or("没有可用的更新")?;
            let reporter = app2.clone();
            let mut downloaded: u64 = 0;
            update
                .download(
                    move |chunk, total| {
                        downloaded += chunk as u64;
                        let _ = reporter.emit("update:progress", Progress { downloaded, total });
                    },
                    || {},
                )
                .await
                .map_err(|e| e.to_string())
        }
        .await;

        let st = app2.state::<UpdateState>();
        match result {
            Ok(bytes) => {
                *lock(&st.bytes) = Some(bytes);
                let _ = app2.emit("update:downloaded", ());
            }
            Err(e) => {
                let _ = app2.emit("update:failed", e);
            }
        }
        st.running.store(false, Ordering::SeqCst);
    });

    *lock(&state.task) = Some(handle);
    Ok(())
}

/// 取消下载：abort 任务即断开连接，已下载的部分直接丢弃。
#[tauri::command]
pub fn update_cancel(app: AppHandle) {
    let state = app.state::<UpdateState>();
    if let Some(handle) = lock(&state.task).take() {
        handle.abort();
    }
    state.running.store(false, Ordering::SeqCst);
    *lock(&state.bytes) = None;
    let _ = app.emit("update:cancelled", ());
}

/// 把已下载的包交给安装器（Windows 上安装器启动后本进程会退出）。
/// `async`：内部要重新 check 一次拿安装句柄，同步命令会卡住主线程。
#[tauri::command]
pub async fn update_install(app: AppHandle) -> Result<(), String> {
    let state = app.state::<UpdateState>();
    let bytes = lock(&state.bytes).take().ok_or("安装包尚未下载完成")?;

    let updater = app.updater().map_err(|e| e.to_string())?;
    // 重新 check 拿一个 Update 句柄来执行安装（下载时已经校验过签名）。
    let update = updater
        .check()
        .await
        .map_err(|e| e.to_string())?
        .ok_or("没有可用的更新")?;
    update.install(bytes).map_err(|e| e.to_string())
}
