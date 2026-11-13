import { listen } from "@tauri-apps/api/event";
import { getSettings } from "@/lib/ipc";

/** 主题模式：深色 / 浅色 / 跟随系统。 */
export type ThemeMode = "dark" | "light" | "system";

const KEY = "snapix.theme";

function systemIsLight() {
  return window.matchMedia("(prefers-color-scheme: light)").matches;
}

function resolve(mode: ThemeMode): "dark" | "light" {
  if (mode === "system") return systemIsLight() ? "light" : "dark";
  return mode;
}

/** 立即套用（同步，挂载前调用不会闪）。 */
export function applyTheme(mode: ThemeMode) {
  const real = resolve(mode);
  document.documentElement.dataset.theme = real;
  document.documentElement.style.colorScheme = real;
  try {
    localStorage.setItem(KEY, mode);
  } catch {
    // 隐私模式 / 存储被禁用：只影响下次启动的初值，不影响本次
  }
}

/**
 * 窗口挂载前调用：先用本地缓存上色（同步，无闪），
 * 再与后端设置对齐，并订阅其它窗口改主题的广播。
 *
 * 只给「主窗口 / 标注编辑器」用；截图遮罩、贴图、晾衣绳始终深色，
 * 它们叠在画面或桌面上，跟着变亮反而看不清内容。
 */
export function initTheme() {
  applyTheme((localStorage.getItem(KEY) as ThemeMode | null) ?? "dark");

  // 跟随系统：系统主题变化时实时切换
  window.matchMedia("(prefers-color-scheme: light)").addEventListener("change", () => {
    if ((localStorage.getItem(KEY) as ThemeMode | null) === "system") applyTheme("system");
  });

  // 设置页改主题：Rust 广播给所有窗口
  void listen<string>("theme:changed", (e) => {
    const mode = e.payload as ThemeMode;
    if (mode === "dark" || mode === "light" || mode === "system") applyTheme(mode);
  }).catch(() => {});

  // 首次启动 / 缓存过期：以后端设置为准（系统模式下也顺便按当前系统状态重算）
  getSettings()
    .then((s) => applyTheme((s.theme as ThemeMode) ?? "dark"))
    .catch(() => {});
}
