import { useEffect, useState } from "react";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { listen } from "@tauri-apps/api/event";
import { Square, X } from "lucide-react";
import { gifRecordStop, gifRecordCancel, type GifProgress } from "@/lib/ipc";

/**
 * GIF 录制辅助窗口（一套入口两种角色）：
 * - gif-border：录制区红色描边框（穿透点击，纯视觉）
 * - gif-ctl：控制条（时长/帧数 + 停止保存 / 取消）
 */
export default function Gif() {
  const label = getCurrentWindow().label;
  return label === "gif-border" ? <Border /> : <Ctl />;
}

function Border() {
  return (
    <div className="h-screen w-screen p-px">
      <div
        className="h-full w-full rounded-[3px] border-2 border-red-500"
        style={{ boxShadow: "0 0 0 1px rgba(0,0,0,0.35), inset 0 0 0 1px rgba(0,0,0,0.35)" }}
      />
    </div>
  );
}

function Ctl() {
  const [progress, setProgress] = useState<GifProgress | null>(null);

  useEffect(() => {
    const un = listen<GifProgress>("gif:progress", (e) => setProgress(e.payload));
    return () => {
      un.then((f) => f());
    };
  }, []);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") gifRecordCancel();
      else if (e.key === "Enter") gifRecordStop();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

  return (
    <div className="grid h-screen w-screen place-items-center bg-transparent">
      <div
        data-tauri-drag-region
        className="glass flex h-12 w-[284px] items-center gap-2.5 rounded-toolbar px-3 shadow-pop"
      >
        <span className="relative flex h-2 w-2 shrink-0">
          <span className="absolute inline-flex h-full w-full animate-ping rounded-full bg-red-400 opacity-70" />
          <span className="relative inline-flex h-2 w-2 rounded-full bg-red-500" />
        </span>

        <div data-tauri-drag-region className="min-w-0 flex-1 leading-tight">
          <div data-tauri-drag-region className="text-[12px] font-medium text-ink-100">
            GIF 录制中
            <span className="ml-1.5 tabular-nums text-red-300">
              {(progress?.secs ?? 0).toFixed(1)}s
            </span>
          </div>
          <div data-tauri-drag-region className="text-[10px] text-ink-400">
            {progress ? `${progress.frames} 帧 · ` : ""}10 fps · 上限 60s · 停止后自动保存
          </div>
        </div>

        <button
          onClick={() => gifRecordStop()}
          title="停止并保存 (Enter)"
          className="grid h-8 w-8 shrink-0 place-items-center rounded-lg bg-red-500 text-white transition-colors hover:bg-red-400"
        >
          <Square size={13} fill="currentColor" />
        </button>
        <button
          onClick={() => gifRecordCancel()}
          title="取消 (Esc)"
          className="grid h-8 w-8 shrink-0 place-items-center rounded-lg text-ink-300 transition-colors hover:bg-white/10 hover:text-ink-50"
        >
          <X size={15} />
        </button>
      </div>
    </div>
  );
}
