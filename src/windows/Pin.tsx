import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { getCurrentWindow, PhysicalSize } from "@tauri-apps/api/window";
import {
  ArrowUpToLine,
  Copy,
  Save,
  RotateCcw,
  RotateCw,
  FlipHorizontal,
  PenLine,
  Ghost,
  X,
} from "lucide-react";
import {
  getPinImage,
  pinReady,
  pinCopy,
  pinSave,
  pinRotate,
  pinFlip,
  pinPassthrough,
  pinAnnotate,
  closePin,
  lineReturnPin,
  type ImagePayload,
} from "@/lib/ipc";

type MenuAction =
  "copy" | "save" | "annotate" | "rotate" | "flip" | "reset" | "passthrough" | "return" | "close";

const MENU_ITEMS: {
  action: MenuAction;
  icon: typeof Copy;
  label: string;
  hint: string;
  danger?: boolean;
}[] = [
  { action: "copy", icon: Copy, label: "复制", hint: "Ctrl+C" },
  { action: "save", icon: Save, label: "另存为", hint: "Ctrl+S" },
  { action: "annotate", icon: PenLine, label: "标注", hint: "" },
  { action: "rotate", icon: RotateCw, label: "旋转 90°", hint: "R" },
  { action: "flip", icon: FlipHorizontal, label: "水平翻转", hint: "F" },
  { action: "reset", icon: RotateCcw, label: "重置缩放", hint: "0" },
  { action: "passthrough", icon: Ghost, label: "鼠标穿透", hint: "托盘解除" },
  { action: "close", icon: X, label: "关闭", hint: "Esc", danger: true },
];

export default function Pin() {
  const [img, setImg] = useState<ImagePayload | null>(null);
  const [ver, setVer] = useState(0);
  const [hover, setHover] = useState(false);
  const [opacity, setOpacity] = useState(1);
  const [hud, setHud] = useState<string | null>(null);
  const [menu, setMenu] = useState<{ x: number; y: number } | null>(null);
  /** 这张贴图是从晾衣绳取下的：双击 = 挂回绳子（其余贴图仍保持双击关闭） */
  const [canReturn, setCanReturn] = useState(false);

  const label = getCurrentWindow().label;
  const baseRef = useRef<{ w: number; h: number } | null>(null);
  const zoomRef = useRef(1);
  const opacityRef = useRef(1);
  const hudTimer = useRef<number | null>(null);
  // 窗口是隐藏创建的：首张图渲染完才让后端显示，避免空窗 / 错位闪现
  const shownRef = useRef(false);

  useEffect(() => {
    let disposed = false;
    let version = 0;
    const standby = new URLSearchParams(window.location.search).get("preload") === "1";
    const load = async () => {
      const request = ++version;
      try {
        const p = await getPinImage(label);
        if (disposed || request !== version) return;
        if (p) {
          shownRef.current = false;
          setImg(p);
          setCanReturn(!!p.fromLine);
          baseRef.current = { w: p.width, h: p.height };
          zoomRef.current = (p.displayWidth ?? p.width) / p.width;
        } else if (!standby) {
          await closePin(label);
        }
      } catch {
        if (!disposed) void closePin(label).catch(() => {});
      }
    };
    const unlisten = getCurrentWindow().listen("pin:load", () => void load());
    void unlisten
      .then(() => {
        if (!disposed) return load();
      })
      .catch(() => closePin(label).catch(() => {}));
    return () => {
      disposed = true;
      void unlisten.then((un) => un()).catch(() => {});
    };
  }, [label]);

  const onImageLoad = useCallback(
    (event: React.SyntheticEvent<HTMLImageElement>) => {
      if (shownRef.current) return;
      shownRef.current = true;
      const image = event.currentTarget;
      // 隐藏的 WebView 可能暂停 rAF；交接帧由可见的飞行层保留。
      image
        .decode()
        .then(() => pinReady())
        .catch(() => closePin(label).catch(() => {}));
    },
    [label],
  );

  const onImageError = useCallback(() => {
    // 首张图都拿不到就没有可显示的内容，收掉隐藏窗以免留下看不见的僵尸窗口
    if (!shownRef.current) closePin(label).catch(() => {});
  }, [label]);

  const flashHud = useCallback((text: string) => {
    setHud(text);
    if (hudTimer.current) window.clearTimeout(hudTimer.current);
    hudTimer.current = window.setTimeout(() => setHud(null), 900);
  }, []);

  // 滚轮缩放；Ctrl+滚轮调透明度
  useEffect(() => {
    const onWheel = (e: WheelEvent) => {
      e.preventDefault();
      if (e.ctrlKey) {
        const next = Math.min(
          1,
          Math.max(0.15, opacityRef.current + (e.deltaY < 0 ? 0.08 : -0.08)),
        );
        opacityRef.current = next;
        setOpacity(next);
        flashHud(`不透明度 ${Math.round(next * 100)}%`);
        return;
      }
      const base = baseRef.current;
      if (!base) return;
      const next = Math.min(5, Math.max(0.15, zoomRef.current * (e.deltaY < 0 ? 1.08 : 0.92)));
      zoomRef.current = next;
      getCurrentWindow().setSize(
        new PhysicalSize(Math.round(base.w * next), Math.round(base.h * next)),
      );
      flashHud(`${Math.round(next * 100)}%`);
    };
    window.addEventListener("wheel", onWheel, { passive: false });
    return () => window.removeEventListener("wheel", onWheel);
  }, [flashHud]);

  const resetZoom = useCallback(() => {
    const base = baseRef.current;
    if (!base) return;
    zoomRef.current = 1;
    getCurrentWindow().setSize(new PhysicalSize(base.w, base.h));
    opacityRef.current = 1;
    setOpacity(1);
    flashHud("100%");
  }, [flashHud]);

  const onCopy = useCallback(() => {
    pinCopy(label)
      .then(() => flashHud("已复制"))
      .catch((e) => flashHud(`复制失败：${String(e)}`));
  }, [label, flashHud]);
  const onSave = useCallback(() => {
    pinSave(label)
      .then((p) => p && flashHud("已保存"))
      .catch((e) => flashHud(`保存失败：${String(e)}`));
  }, [label, flashHud]);
  const onClose = useCallback(() => {
    closePin(label).catch(() => {});
  }, [label]);

  /** 挂回晾衣绳：后端把图写回绳上，并让绳窗播「飞回去」的动画。 */
  const onReturn = useCallback(() => {
    setMenu(null);
    lineReturnPin(label).catch((e) => flashHud(`挂回失败：${String(e)}`));
  }, [label, flashHud]);

  const menuItems = useMemo(() => {
    if (!canReturn) return MENU_ITEMS;
    const at = MENU_ITEMS.findIndex((m) => m.action === "close");
    return [
      ...MENU_ITEMS.slice(0, at),
      { action: "return" as const, icon: ArrowUpToLine, label: "挂回晾衣绳", hint: "双击" },
      ...MENU_ITEMS.slice(at),
    ];
  }, [canReturn]);

  /** 顺时针旋转 90°：Rust 侧改位图，前端同步窗口尺寸并强刷图像。 */
  const onRotate = useCallback(async () => {
    try {
      const dims = await pinRotate(label);
      baseRef.current = { w: dims.width, h: dims.height };
      const z = zoomRef.current;
      await getCurrentWindow().setSize(
        new PhysicalSize(Math.round(dims.width * z), Math.round(dims.height * z)),
      );
      setVer((v) => v + 1);
      flashHud("旋转 90°");
    } catch (e) {
      flashHud(`旋转失败：${String(e)}`);
    }
  }, [label, flashHud]);

  const onFlip = useCallback(async () => {
    try {
      await pinFlip(label);
      setVer((v) => v + 1);
      flashHud("水平翻转");
    } catch (e) {
      flashHud(`翻转失败：${String(e)}`);
    }
  }, [label, flashHud]);

  const onAnnotate = useCallback(() => {
    pinAnnotate(label).catch((e) => flashHud(`无法打开编辑器：${String(e)}`));
  }, [label, flashHud]);

  const onPassthrough = useCallback(() => {
    setMenu(null);
    flashHud("已开启鼠标穿透");
    // 稍等 HUD 渲染后再穿透（此后本窗不再接收鼠标事件，解除走托盘菜单）
    window.setTimeout(() => {
      pinPassthrough(label).catch((e) => flashHud(`开启穿透失败：${String(e)}`));
    }, 60);
  }, [label, flashHud]);

  // 键盘：Esc 关闭 · Ctrl+C 复制 · Ctrl+S 另存 · 0 重置 · R 旋转 · F 翻转
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        if (menu) setMenu(null);
        else onClose();
      } else if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === "c") {
        onCopy();
      } else if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === "s") {
        e.preventDefault();
        onSave();
      } else if (e.key === "0") {
        resetZoom();
      } else if (!e.ctrlKey && !e.metaKey && e.key.toLowerCase() === "r") {
        onRotate();
      } else if (!e.ctrlKey && !e.metaKey && e.key.toLowerCase() === "f") {
        onFlip();
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [menu, onClose, onCopy, onSave, resetZoom, onRotate, onFlip]);

  const runMenu = (action: MenuAction) => {
    switch (action) {
      case "copy":
        return onCopy();
      case "save":
        return onSave();
      case "annotate":
        return onAnnotate();
      case "rotate":
        return onRotate();
      case "flip":
        return onFlip();
      case "reset":
        return resetZoom();
      case "passthrough":
        return onPassthrough();
      case "return":
        return onReturn();
      case "close":
        return onClose();
    }
  };

  return (
    <div
      className="group relative h-screen w-screen overflow-hidden bg-transparent"
      onMouseEnter={() => setHover(true)}
      onMouseLeave={() => setHover(false)}
      onDoubleClick={() => (canReturn ? onReturn() : onClose())}
      onContextMenu={(e) => {
        e.preventDefault();
        setMenu({
          x: Math.min(e.clientX, Math.max(0, window.innerWidth - 168)),
          y: Math.min(e.clientY, Math.max(0, window.innerHeight - 252)),
        });
      }}
      onMouseDown={() => menu && setMenu(null)}
    >
      {img && (
        <img
          data-tauri-drag-region
          src={ver ? `${img.imageUrl}?v=${ver}` : img.imageUrl}
          alt=""
          draggable={false}
          decoding="async"
          onLoad={onImageLoad}
          onError={onImageError}
          style={{ opacity }}
          className="h-full w-full select-none"
        />
      )}
      {/* 描边只在悬停时出现：贴图刚显示时必须与遮罩里的选区像素级一致，
          常驻的 1px 描边会让切换瞬间多出一圈线 */}
      <div
        className="pointer-events-none absolute inset-0 ring-1 ring-inset ring-black/25 transition-opacity"
        style={{ opacity: hover ? 1 : 0 }}
      />

      {/* 悬浮控制条 */}
      <div
        className="absolute right-1.5 top-1.5 flex items-center gap-1 rounded-lg border border-white/10 bg-ink-900/80 p-1 backdrop-blur transition-opacity"
        style={{ opacity: hover && !menu ? 1 : 0 }}
      >
        {canReturn && (
          <button
            onClick={onReturn}
            className="grid h-6 w-6 place-items-center rounded text-ink-200 transition-colors hover:bg-white/10 hover:text-ink-0"
            title="挂回晾衣绳（双击）"
          >
            <ArrowUpToLine size={13} />
          </button>
        )}
        <button
          onClick={onCopy}
          className="grid h-6 w-6 place-items-center rounded text-ink-200 transition-colors hover:bg-white/10 hover:text-ink-0"
          title="复制 (Ctrl+C)"
        >
          <Copy size={13} />
        </button>
        <button
          onClick={onClose}
          className="grid h-6 w-6 place-items-center rounded text-ink-200 transition-colors hover:bg-red-500/80 hover:text-white"
          title={canReturn ? "关闭（Esc）" : "关闭（双击 / Esc）"}
        >
          <X size={13} />
        </button>
      </div>

      {/* HUD：缩放 / 透明度 */}
      {hud && (
        <div className="pointer-events-none absolute bottom-1.5 left-1.5 rounded-md bg-black/65 px-2 py-0.5 text-[11px] font-medium tabular-nums text-white backdrop-blur">
          {hud}
        </div>
      )}

      {/* 右键菜单 */}
      {menu && (
        <div
          className="absolute z-50 w-40 overflow-hidden rounded-lg border border-white/10 bg-ink-900/95 py-1 shadow-pop backdrop-blur"
          style={{ left: menu.x, top: menu.y }}
          onMouseDown={(e) => e.stopPropagation()}
        >
          {menuItems.map((m) => (
            <button
              key={m.action}
              onClick={() => {
                setMenu(null);
                runMenu(m.action);
              }}
              className={
                "flex w-full items-center gap-2 px-3 py-1.5 text-left text-xs text-ink-100 transition-colors hover:bg-white/10 " +
                (m.danger ? "hover:text-red-300" : "")
              }
            >
              <m.icon size={13} className="text-ink-400" />
              <span className="flex-1">{m.label}</span>
              <span className="text-[10px] text-ink-500">{m.hint}</span>
            </button>
          ))}
        </div>
      )}
    </div>
  );
}
