import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

/** 更新端点返回的版本信息（不含下载资源句柄）。 */
export type UpdateInfo = {
  version: string;
  currentVersion: string;
  notes?: string;
  date?: string;
};

export type UpdateProgress = {
  downloaded: number;
  total: number | null;
};

/**
 * 最近一次检查的结果缓存。启动时的静默检查与设置页共用：
 * 设置页打开时不用再发一次请求，也能直接显示「发现新版本」。
 */
let cached: UpdateInfo | null = null;

export function cachedUpdate() {
  return cached;
}

/**
 * 检查更新。
 *
 * 走 Rust 侧自建命令而不是 `@tauri-apps/plugin-updater` 的 `check()`：
 * 下载链路要能中止（见 `downloadUpdate`），把检查也收拢到同一处，
 * 避免 JS 侧再持有一个原生 `Update` 资源。
 */
export async function checkUpdate(): Promise<UpdateInfo | null> {
  const info = await invoke<UpdateInfo | null>("update_fetch");
  cached = info;
  return info;
}

/** 取消除进行中的下载（abort 任务 → 连接立刻断开，已下载部分丢弃）。 */
export function cancelUpdate() {
  return invoke("update_cancel");
}

/**
 * 下载安装包（带进度）。resolve 表示已下载并通过签名校验，还需要调 `installDownloaded`。
 * 用户取消时 reject 一个 `AbortError` 的 DOMException，调用方据此区分「取消」和「失败」。
 */
export function downloadUpdate(onProgress: (p: UpdateProgress) => void): Promise<void> {
  return new Promise<void>((resolve, reject) => {
    const unlisteners: UnlistenFn[] = [];
    let settled = false;
    const finish = (fn: () => void) => {
      if (settled) return;
      settled = true;
      unlisteners.forEach((f) => f());
      fn();
    };

    void (async () => {
      try {
        unlisteners.push(
          await listen<UpdateProgress>("update:progress", (e) => onProgress(e.payload)),
        );
        unlisteners.push(await listen("update:downloaded", () => finish(resolve)));
        unlisteners.push(
          await listen<string>("update:failed", (e) => finish(() => reject(new Error(e.payload)))),
        );
        unlisteners.push(
          await listen("update:cancelled", () =>
            finish(() => reject(new DOMException("已取消", "AbortError"))),
          ),
        );
        await invoke("update_download");
      } catch (e) {
        finish(() => reject(e));
      }
    })();
  });
}

/** 把下载好的包交给安装器；Windows 上安装器启动后本进程会退出。 */
export function installDownloaded() {
  return invoke("update_install");
}

/**
 * 下载并安装（一步到位）。Windows 上安装器启动后本进程会退出，
 * 所以调用方不要依赖这个 Promise 一定会 resolve。
 */
export async function installUpdate(onProgress: (p: UpdateProgress) => void) {
  await downloadUpdate(onProgress);
  await installDownloaded();
}
