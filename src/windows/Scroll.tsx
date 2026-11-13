import { useCallback, useEffect, useRef, useState } from "react";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { listen } from "@tauri-apps/api/event";
import { Check, X, MousePointerClick, Pause, Play, ChevronDown, ChevronRight } from "lucide-react";
import {
  scrollCaptureStop,
  scrollCaptureCancel,
  scrollCaptureSetPaused,
  scrollCaptureStep,
  type ScrollProgress,
} from "@/lib/ipc";
import { cn } from "@/lib/cn";

/**
 * 滚动截图辅助窗口（一套入口两种角色）：
 * - scroll-border：选区描边框（穿透点击，纯视觉）
 * - scroll-ctl：控制条（进度 + 暂停/步进 + 完成 / 取消）
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
  const [paused, setPaused] = useState(false);
  // 步进后短暂高亮，给一帧「按下去有反应」的反馈
  const [stepping, setStepping] = useState(false);
  const stepTimer = useRef<number | null>(null);

  useEffect(() => {
    const un = listen<ScrollProgress>("scroll:progress", (e) => {
      setProgress(e.payload);
      setPaused(e.payload.paused);
    });
    return () => {
      un.then((f) => f());
    };
  }, []);

  useEffect(
    () => () => {
      if (stepTimer.current != null) window.clearTimeout(stepTimer.current);
    },
    [],
  );

  const horizontal = progress?.horizontal ?? false;
  const warn = progress?.warn;

  const togglePause = useCallback(() => {
    setPaused((p) => {
      const next = !p;
      void scrollCaptureSetPaused(next);
      return next;
    });
  }, []);

  const step = useCallback(() => {
    void scrollCaptureStep(1);
    setPaused(true);
    setStepping(true);
    if (stepTimer.current != null) window.clearTimeout(stepTimer.current);
    stepTimer.current = window.setTimeout(() => setStepping(false), 220);
  }, []);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        void scrollCaptureCancel();
      } else if (e.key === "Enter") {
        void scrollCaptureStop();
      } else if (e.code === "Space") {
        e.preventDefault();
        togglePause();
      } else if (e.key === "ArrowDown" || e.key === "ArrowRight") {
        e.preventDefault();
        step();
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [togglePause, step]);

  // 步进方向跟着滚动方向走（纵向 ↓ / 横向 →）；只有暂停时补上操作提示
  const arrow = horizontal ? "→" : "↓";
  const hint = warn
    ? paused
      ? `${warn} · 按 ${arrow} 步进`
      : warn
    : paused
      ? `按 ${arrow} 步进一帧 · 空格继续`
      : horizontal
        ? "自动向右滚动拼接 · 到边自动完成"
        : "自动滚动拼接 · 触底自动完成";

  return (
    <div className="grid h-screen w-screen place-items-center bg-transparent">
      <div
        data-tauri-drag-region
        className="glass flex h-12 w-[376px] items-center gap-2 rounded-toolbar px-2.5 shadow-pop"
      >
        <span className="relative ml-0.5 flex h-2 w-2 shrink-0">
          {!paused && (
            <span className="absolute inline-flex h-full w-full animate-ping rounded-full bg-brand-400 opacity-70" />
          )}
          <span
            className={cn(
              "relative inline-flex h-2 w-2 rounded-full",
              warn ? "bg-amber-400" : paused ? "bg-ink-400" : "bg-brand-500",
            )}
          />
        </span>

        <div data-tauri-drag-region className="min-w-0 flex-1 leading-tight">
          <div
            data-tauri-drag-region
            className="flex items-baseline gap-1.5 text-[12px] font-medium text-ink-100"
          >
            <span className="truncate">
              {paused ? "已暂停" : horizontal ? "横向滚动截图中" : "滚动截图中"}
            </span>
            {progress && (
              <span className="shrink-0 tabular-nums text-brand-300">
                {progress.heightPx.toLocaleString()} px
              </span>
            )}
          </div>
          <div
            data-tauri-drag-region
            className={cn(
              "flex items-center gap-1 truncate text-[10px]",
              warn ? "text-amber-300" : "text-ink-400",
            )}
          >
            {!warn && <MousePointerClick size={10} className="shrink-0" />}
            <span className="truncate">{hint}</span>
          </div>
        </div>

        <button
          onClick={togglePause}
          title={paused ? "继续自动滚动 (空格)" : "暂停，逐帧手动滚动 (空格)"}
          className="grid h-8 w-8 shrink-0 place-items-center rounded-lg text-ink-300 transition-colors hover:bg-white/10 hover:text-ink-50"
        >
          {paused ? <Play size={14} /> : <Pause size={14} />}
        </button>
        <button
          onClick={step}
          disabled={!paused}
          title="手动步进一帧 (↓)"
          className={cn(
            "grid h-8 w-8 shrink-0 place-items-center rounded-lg transition-colors",
            !paused
              ? "text-ink-500 opacity-50"
              : stepping
                ? "bg-white/15 text-ink-50"
                : "text-ink-300 hover:bg-white/10 hover:text-ink-50",
          )}
        >
          {horizontal ? <ChevronRight size={15} /> : <ChevronDown size={15} />}
        </button>
        <button
          onClick={() => void scrollCaptureStop()}
          title="完成 (Enter)"
          className="grid h-8 w-8 shrink-0 place-items-center rounded-lg bg-brand-500 text-white transition-colors hover:bg-brand-400"
        >
          <Check size={15} />
        </button>
        <button
          onClick={() => void scrollCaptureCancel()}
          title="取消 (Esc)"
          className="grid h-8 w-8 shrink-0 place-items-center rounded-lg text-ink-300 transition-colors hover:bg-white/10 hover:text-ink-50"
        >
          <X size={15} />
        </button>
      </div>
    </div>
  );
}
