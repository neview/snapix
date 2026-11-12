//! 跨模块的小工具：毒化安全的互斥锁、原子写文件、阻塞任务下放。

use std::path::Path;
use std::sync::{Mutex, MutexGuard};

/// 获取互斥锁；若持锁线程曾 panic 导致锁毒化，取回内部数据继续使用而不是让整个应用连锁崩溃。
/// 本项目的锁内数据都是简单容器（HashMap / Vec / Option），不存在半更新的不变量。
pub fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// 先写临时文件再重命名覆盖，避免写入中途崩溃留下半截 JSON。
pub fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })
}

/// 把阻塞型工作（对话框、OCR、图像编解码、COM 调用）放到 tokio 的阻塞线程池执行，
/// 供 `async` 命令调用，避免占住主 UI 线程或 async worker。
/// 任务 panic 时转成 `Err`，而不是让命令悬挂或整个运行时崩溃。
pub async fn blocking<T, E>(f: impl FnOnce() -> Result<T, E> + Send + 'static) -> Result<T, E>
where
    T: Send + 'static,
    E: Send + From<String> + 'static,
{
    match tauri::async_runtime::spawn_blocking(f).await {
        Ok(r) => r,
        Err(e) => Err(E::from(format!("后台任务异常终止：{e}"))),
    }
}
