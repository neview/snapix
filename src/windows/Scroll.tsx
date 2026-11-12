import { useEffect, useState } from "react";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { listen } from "@tauri-apps/api/event";
import { Check, X, MousePointerClick } from "lucide-react";
import { scrollCaptureStop, scrollCaptureCancel, type ScrollProgress } from "@/lib/ipc";

/**
 * 滚动截图辅助窗口（一套入口两种角色）：
 * - scroll-border：选区描边框（穿透点击，纯视觉）
 * - scroll-ctl：控制条（进度 + 完成 / 取消）
 */
export default function Scroll() {
  const label = getCurrentWindow().label;
  return label === "scroll-border" ? <Border /> : <Ctl />;
}

function Border() {
  return (
    <div className="h-screen w-screen p-px">
      <div
        className="h-full w-full rounded-[3px] border-2 border-brand-400"
        style={{ boxShadow: "0 0 0 1px rgba(0,0,0,0.35), inset 0 0 0 1px rgba(0,0,0,0.35)" }}
      />
    </div>
  );
}

function Ctl() {
  const [progress, setProgress] = useState<ScrollProgress | null>(null);

  useEffect(() => {
    const un = listen<ScrollProgress>("scroll:progress", (e) => setProgress(e.payload));
    return () => {
      un.then((f) => f());
    };
  }, []);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") scrollCaptureCancel();
      else if (e.key === "Enter") scrollCaptureStop();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

  return (
    <div className="grid h-screen w-screen place-items-center bg-transparent">
      <div
        data-tauri-drag-region
        className="glass flex h-12 w-[304px] items-center gap-2.5 rounded-toolbar px-3 shadow-pop"
      >
        <span className="relative flex h-2 w-2 shrink-0">
          <span className="absolute inline-flex h-full w-full animate-ping rounded-full bg-brand-400 opacity-70" />
          <span className="relative inline-flex h-2 w-2 rounded-full bg-brand-500" />
        </span>

        <div data-tauri-drag-region className="min-w-0 flex-1 leading-tight">
          <div data-tauri-drag-region className="text-[12px] font-medium text-ink-100">
            {progress?.horizontal ? "横向滚动截图中" : "滚动截图中"}
            {progress && (
              <span className="ml-1.5 tabular-nums text-brand-300">
                {progress.heightPx.toLocaleString()} px
              </span>
            )}
          </div>
          <div data-tauri-drag-region className="flex items-center gap-1 text-[10px] text-ink-400">
            <MousePointerClick size={10} />
            {progress?.horizontal
              ? "自动向右滚动拼接 · 到边自动完成"
              : "自动滚动拼接 · 触底自动完成"}
          </div>
        </div>

        <button
          onClick={() => scrollCaptureStop()}
          title="完成 (Enter)"
          className="grid h-8 w-8 shrink-0 place-items-center rounded-lg bg-brand-500 text-white transition-colors hover:bg-brand-400"
        >
          <Check size={15} />
        </button>
        <button
          onClick={() => scrollCaptureCancel()}
          title="取消 (Esc)"
          className="grid h-8 w-8 shrink-0 place-items-center rounded-lg text-ink-300 transition-colors hover:bg-white/10 hover:text-ink-50"
        >
          <X size={15} />
        </button>
      </div>
    </div>
  );
}
