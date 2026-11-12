import { getCurrentWindow } from "@tauri-apps/api/window";
import { Minus, X } from "lucide-react";

/**
 * 无边框窗口的自定义标题栏：左侧可拖拽，右侧窗口控制。
 * 视觉上刻意「消失」——只有一枚发丝线图标和极低对比度的标题，
 * 让整个窗口看起来是一块整体玻璃，而不是「带标题栏的软件」。
 */
export default function TitleBar({ title, onClose }: { title?: string; onClose?: () => void }) {
  const win = getCurrentWindow();

  return (
    <div
      data-tauri-drag-region
      className="flex h-12 shrink-0 items-center justify-between px-5 select-none"
    >
      <div data-tauri-drag-region className="flex items-center gap-2.5">
        {/* 应用标记：一块带高光边的圆角方块，单色 */}
        <div className="relative h-[18px] w-[18px] rounded-[6px] border border-white/12 bg-white/[0.07]">
          <div className="absolute inset-[3px] rounded-[3px] border border-white/18" />
        </div>
        <span
          data-tauri-drag-region
          className="text-[12.5px] font-medium tracking-[0.02em] text-white/45"
        >
          {title ?? "Snapix"}
        </span>
      </div>

      <div className="flex items-center gap-0.5">
        <button
          onClick={() => win.minimize()}
          className="grid h-7 w-7 place-items-center rounded-full text-white/35 transition-colors duration-150 hover:bg-white/[0.08] hover:text-white/80"
          aria-label="最小化"
        >
          <Minus size={14} strokeWidth={1.75} />
        </button>
        <button
          onClick={() => (onClose ? onClose() : win.hide())}
          className="grid h-7 w-7 place-items-center rounded-full text-white/35 transition-colors duration-150 hover:bg-white/[0.08] hover:text-white/80"
          aria-label="关闭到托盘"
          title="关闭到托盘（仍可用快捷键截图）"
        >
          <X size={14} strokeWidth={1.75} />
        </button>
      </div>
    </div>
  );
}
