import { check } from "@tauri-apps/plugin-updater";

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
 * 检查更新。拿到元数据后立刻释放 `Update` 资源——
 * 真正安装时会重新 check 一次，避免长期持有原生资源或用到过期签名。
 */
export async function checkUpdate(): Promise<UpdateInfo | null> {
  const update = await check({ timeout: 15000 });
  if (!update) {
    cached = null;
    return null;
  }
  const info: UpdateInfo = {
    version: update.version,
    currentVersion: update.currentVersion,
    notes: update.body?.trim() || undefined,
    date: update.date,
  };
  await update.close().catch(() => {});
  cached = info;
  return info;
}

/**
 * 下载并交给安装器。Windows 上安装器启动后本进程会退出，
 * 所以调用方不要依赖这个 Promise 一定会 resolve。
 */
export async function installUpdate(onProgress: (p: UpdateProgress) => void) {
  const update = await check({ timeout: 15000 });
  if (!update) throw new Error("没有可用的更新");
  let downloaded = 0;
  let total: number | null = null;
  try {
    await update.downloadAndInstall(
      (event) => {
        if (event.event === "Started") {
          total = event.data.contentLength ?? null;
          onProgress({ downloaded: 0, total });
        } else if (event.event === "Progress") {
          downloaded += event.data.chunkLength;
          onProgress({ downloaded, total });
        } else {
          onProgress({ downloaded: total ?? downloaded, total });
        }
      },
      { restartAfterInstall: true },
    );
  } finally {
    await update.close().catch(() => {});
  }
}
