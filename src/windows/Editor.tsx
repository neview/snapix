import { useEffect, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { AnimatePresence, motion } from "motion/react";
import type Konva from "konva";
import {
  MousePointer2,
  Square,
  Circle as CircleIcon,
  ArrowUpRight,
  Minus,
  Pencil,
  Highlighter,
  Type,
  ListOrdered,
  Grid2x2,
  Droplets,
  Undo2,
  Redo2,
  Copy,
  Save,
  X,
  Trash2,
  Sparkles,
  ShieldCheck,
  Crop,
  Smile,
  Lightbulb,
  Pin,
  Ruler,
} from "lucide-react";
import TitleBar from "@/components/TitleBar";
import CanvasStage from "@/features/annotate/CanvasStage";
import { useEditor, uid, type Shape, type ToolKind } from "@/features/annotate/store";
import {
  BG_PRESETS,
  RATIOS,
  computeLayout,
  paintBackground,
  paintWindowBar,
  roundRectPath,
} from "@/features/annotate/beautify";
import { cn } from "@/lib/cn";
import {
  copyImage,
  saveImage,
  closeEditor,
  detectPrivacy,
  pinData,
  editClipboard,
  type ImagePayload,
} from "@/lib/ipc";

const TOOLS: { kind: ToolKind; icon: typeof Square; label: string; key: string }[] = [
  { kind: "select", icon: MousePointer2, label: "选择 / 移动", key: "V" },
  { kind: "rect", icon: Square, label: "矩形", key: "R" },
  { kind: "ellipse", icon: CircleIcon, label: "椭圆", key: "O" },
  { kind: "arrow", icon: ArrowUpRight, label: "箭头", key: "A" },
  { kind: "line", icon: Minus, label: "直线", key: "L" },
  { kind: "pen", icon: Pencil, label: "画笔", key: "P" },
  { kind: "marker", icon: Highlighter, label: "荧光笔", key: "H" },
  { kind: "text", icon: Type, label: "文字", key: "T" },
  { kind: "step", icon: ListOrdered, label: "步骤序号", key: "N" },
  { kind: "sticker", icon: Smile, label: "Emoji 贴纸", key: "E" },
  { kind: "spotlight", icon: Lightbulb, label: "聚光灯（压暗四周，突出重点）", key: "F" },
  { kind: "measure", icon: Ruler, label: "测量标尺（像素距离）", key: "D" },
  { kind: "mosaic", icon: Grid2x2, label: "马赛克", key: "M" },
  { kind: "blur", icon: Droplets, label: "模糊", key: "B" },
  { kind: "crop", icon: Crop, label: "裁剪（拖出保留区域）", key: "C" },
];

const EMOJIS = [
  "👍",
  "👎",
  "❤️",
  "🔥",
  "⭐",
  "🎯",
  "✅",
  "❌",
  "⚠️",
  "❓",
  "💡",
  "🚀",
  "🎉",
  "👀",
  "😂",
  "😱",
  "📌",
  "🏆",
];

const PALETTE = [
  "#FF3B5C",
  "#FF9F0A",
  "#FFD60A",
  "#32D74B",
  "#5B6CFF",
  "#0A84FF",
  "#FFFFFF",
  "#111318",
];

const WIDTHS = [
  { v: 2, label: "细" },
  { v: 4, label: "中" },
  { v: 8, label: "粗" },
];

const FONTS = [
  { v: 20, label: "小" },
  { v: 28, label: "中" },
  { v: 40, label: "大" },
];

let toastSeq = 0;

export default function Editor() {
  const stageRef = useRef<Konva.Stage | null>(null);
  const [toasts, setToasts] = useState<{ id: number; text: string }[]>([]);
  const [detecting, setDetecting] = useState(false);
  /** 出图（保存 / 复制）进行中：原生保存对话框期间冻结快捷键，避免 Esc 同时关掉编辑器 */
  const exportingRef = useRef(false);
  const tool = useEditor((s) => s.tool);
  const color = useEditor((s) => s.color);
  const strokeWidth = useEditor((s) => s.strokeWidth);
  const fontSize = useEditor((s) => s.fontSize);
  const fillShape = useEditor((s) => s.fillShape);
  const emoji = useEditor((s) => s.emoji);
  const selectedId = useEditor((s) => s.selectedId);
  const shapes = useEditor((s) => s.shapes);
  const past = useEditor((s) => s.past);
  const future = useEditor((s) => s.future);
  const beautify = useEditor((s) => s.beautify);
  const setBeautify = useEditor((s) => s.setBeautify);
  const crop = useEditor((s) => s.crop);
  const setCrop = useEditor((s) => s.setCrop);

  const setTool = useEditor((s) => s.setTool);
  const setColor = useEditor((s) => s.setColor);
  const setStrokeWidth = useEditor((s) => s.setStrokeWidth);
  const setFontSize = useEditor((s) => s.setFontSize);
  const setFillShape = useEditor((s) => s.setFillShape);
  const setEmoji = useEditor((s) => s.setEmoji);
  const setImage = useEditor((s) => s.setImage);
  const setSelected = useEditor((s) => s.setSelected);
  const removeShape = useEditor((s) => s.removeShape);
  const updateShape = useEditor((s) => s.updateShape);
  const markHistory = useEditor((s) => s.markHistory);
  const addShapes = useEditor((s) => s.addShapes);
  const undo = useEditor((s) => s.undo);
  const redo = useEditor((s) => s.redo);

  const toast = (text: string) => {
    const id = ++toastSeq;
    setToasts((t) => [...t, { id, text }]);
    setTimeout(() => setToasts((t) => t.filter((x) => x.id !== id)), 2200);
  };

  /** 自动隐私打码：OCR 定位手机号/邮箱/身份证/卡号 → 批量加马赛克。 */
  const doPrivacy = async () => {
    if (detecting) return;
    setDetecting(true);
    try {
      const rects = await detectPrivacy();
      if (!rects.length) {
        toast("未检测到隐私内容");
        return;
      }
      const pad = 4;
      addShapes(
        rects.map((r): Shape => ({
          id: uid(),
          type: "mosaic",
          x: r.x - pad,
          y: r.y - pad,
          w: r.w + pad * 2,
          h: r.h + pad * 2,
          strength: 14,
          stroke: color,
          strokeWidth,
        })),
      );
      toast(`已打码 ${rects.length} 处，可选中调整（Ctrl+Z 撤销）`);
    } catch (e) {
      toast(String(e));
    } finally {
      setDetecting(false);
    }
  };

  const selectedShape = shapes.find((s) => s.id === selectedId);
  const showFont = tool === "text" || tool === "sticker" || selectedShape?.type === "text";
  const showFill =
    tool === "rect" ||
    tool === "ellipse" ||
    selectedShape?.type === "rect" ||
    selectedShape?.type === "ellipse";
  const fxSelected =
    selectedShape?.type === "mosaic" || selectedShape?.type === "blur" ? selectedShape : null;

  // 接收待标注图像（shot:// 协议）
  useEffect(() => {
    const un = listen<ImagePayload>("editor:open", (e) => {
      setImage({
        url: e.payload.imageUrl,
        width: e.payload.width,
        height: e.payload.height,
      });
    });
    return () => {
      un.then((f) => f());
    };
  }, [setImage]);

  const exportUrl = (cb: (url: string) => void) => {
    const stage = stageRef.current;
    const img = useEditor.getState().image;
    const b = useEditor.getState().beautify;
    const cr = useEditor.getState().crop;
    if (!stage || !img) return;
    // 有效导出尺寸：裁剪视口或整图
    const outW = cr?.w ?? img.width;
    const outH = cr?.h ?? img.height;
    setSelected(null);
    requestAnimationFrame(() =>
      requestAnimationFrame(() => {
        const shotUrl = stage.toDataURL({
          pixelRatio: outW / stage.width(),
          mimeType: "image/png",
        });
        if (!b.enabled) {
          cb(shotUrl);
          return;
        }

        const layout = computeLayout(outW, outH, b);
        const canvas = document.createElement("canvas");
        canvas.width = layout.outerW;
        canvas.height = layout.outerH;
        const ctx = canvas.getContext("2d");
        if (!ctx) {
          cb(shotUrl);
          return;
        }
        paintBackground(ctx, layout.outerW, layout.outerH, b);

        const shot = new window.Image();
        shot.onload = () => {
          const r = layout.radiusPx;
          // 阴影投在整个窗口框（含标题栏）下
          if (b.shadow) {
            ctx.save();
            ctx.shadowColor = "rgba(0,0,0,0.42)";
            ctx.shadowBlur = layout.outerW * 0.025;
            ctx.shadowOffsetY = layout.outerW * 0.012;
            roundRectPath(ctx, layout.padX, layout.padY, layout.winW, layout.winH, r);
            ctx.fillStyle = "#000";
            ctx.fill();
            ctx.restore();
          }
          // 窗口框整体圆角裁剪：标题栏 + 截图共用一个圆角矩形
          ctx.save();
          roundRectPath(ctx, layout.padX, layout.padY, layout.winW, layout.winH, r);
          ctx.clip();
          paintWindowBar(ctx, layout, b);
          ctx.drawImage(shot, layout.imgX, layout.imgY, outW, outH);
          ctx.restore();

          if (b.watermark) {
            const fs = Math.max(14, layout.outerW * 0.02);
            ctx.font = `600 ${fs}px system-ui, -apple-system, sans-serif`;
            ctx.fillStyle = "rgba(255,255,255,0.88)";
            ctx.textAlign = "right";
            ctx.textBaseline = "bottom";
            ctx.shadowColor = "rgba(0,0,0,0.45)";
            ctx.shadowBlur = 4;
            ctx.fillText(b.watermark, layout.outerW - 12, layout.outerH - 8);
          }
          cb(canvas.toDataURL("image/png"));
        };
        shot.src = shotUrl;
      }),
    );
  };

  /** 出图动作统一包装：失败时留在编辑器并提示，避免静默丢图。 */
  const exportWith = (
    action: (url: string) => Promise<unknown>,
    okText: string | null,
    keepOpen: boolean,
  ) =>
    exportUrl(async (url) => {
      if (exportingRef.current) return;
      exportingRef.current = true;
      try {
        const res = await action(url);
        // 保存对话框被取消：后端返回 null，遮罩/编辑器都还在，别把这一轮标注关掉
        if (res === null) {
          toast("已取消保存");
          return;
        }
        if (keepOpen) {
          if (okText) toast(okText);
        } else {
          closeEditor();
        }
      } catch (e) {
        toast(`操作失败：${String(e)}`);
      } finally {
        exportingRef.current = false;
      }
    });

  /** keepOpen（Shift+点击 / Ctrl+Shift+快捷键）：出图后不关闭，连续标注。 */
  const doCopy = (keepOpen = false) => exportWith(copyImage, "已复制，可继续编辑", keepOpen);
  const doSave = (keepOpen = false) => exportWith(saveImage, "已保存，可继续编辑", keepOpen);
  const doPin = () => exportWith(pinData, null, false);

  // 快捷键：Ctrl+Z/Y 撤销重做 · Ctrl+C/S 复制/保存 · 单键切换工具 · Esc 取消选中/关闭
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      const editingText =
        document.activeElement?.tagName === "TEXTAREA" ||
        document.activeElement?.tagName === "INPUT";
      // 保存对话框打开中：Esc 会同时漏到本窗口，此时不能让它关掉编辑器
      if (editingText || exportingRef.current) return;

      if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === "z") {
        e.preventDefault();
        if (e.shiftKey) redo();
        else undo();
      } else if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === "y") {
        e.preventDefault();
        redo();
      } else if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === "c") {
        e.preventDefault();
        doCopy(e.shiftKey);
      } else if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === "s") {
        e.preventDefault();
        doSave(e.shiftKey);
      } else if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === "v") {
        // 粘贴剪贴板图片，开启新一轮标注
        e.preventDefault();
        editClipboard().catch((err) => toast(String(err)));
      } else if ((e.key === "Delete" || e.key === "Backspace") && selectedId) {
        e.preventDefault();
        removeShape(selectedId);
      } else if (e.key === "Escape") {
        if (selectedId) setSelected(null);
        else closeEditor();
      } else if (!e.ctrlKey && !e.metaKey && !e.altKey) {
        const t = TOOLS.find((x) => x.key.toLowerCase() === e.key.toLowerCase());
        if (t) setTool(t.kind);
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [undo, redo, removeShape, selectedId, setSelected, setTool]);

  return (
    <div className="relative flex h-full flex-col overflow-hidden rounded-2xl border border-white/8 bg-ink-900 text-ink-100">
      <TitleBar title="Snapix · 标注" onClose={() => closeEditor()} />

      {/* 属性 / 操作栏 */}
      <div className="flex items-center justify-between gap-3 border-b border-white/6 px-4 py-2">
        <div className="flex items-center gap-3">
          {/* 调色板 */}
          <div className="flex items-center gap-1.5">
            {PALETTE.map((c) => (
              <button
                key={c}
                onClick={() => setColor(c)}
                className={cn(
                  "h-5 w-5 rounded-full border transition-transform hover:scale-110",
                  color.toUpperCase() === c
                    ? "border-white ring-2 ring-brand-500"
                    : "border-white/20",
                )}
                style={{ background: c }}
                title={c}
              />
            ))}
            {/* 自定义颜色 */}
            <label
              title="自定义颜色"
              className={cn(
                "relative h-5 w-5 cursor-pointer rounded-full border transition-transform hover:scale-110",
                !PALETTE.includes(color.toUpperCase())
                  ? "border-white ring-2 ring-brand-500"
                  : "border-white/20",
              )}
              style={{
                background: !PALETTE.includes(color.toUpperCase())
                  ? color
                  : "conic-gradient(#FF3B5C, #FF9F0A, #FFD60A, #32D74B, #0A84FF, #9B5BFF, #FF3B5C)",
              }}
            >
              <input
                type="color"
                value={color}
                onChange={(e) => setColor(e.target.value)}
                className="absolute inset-0 h-full w-full cursor-pointer opacity-0"
              />
            </label>
          </div>

          <div className="h-5 w-px bg-white/10" />

          {/* 线宽 */}
          <div className="flex items-center gap-1">
            {WIDTHS.map((w) => (
              <button
                key={w.v}
                onClick={() => setStrokeWidth(w.v)}
                className={cn(
                  "rounded-md px-2 py-1 text-xs transition-colors",
                  strokeWidth === w.v ? "bg-brand-500 text-white" : "text-ink-300 hover:bg-white/8",
                )}
              >
                {w.label}
              </button>
            ))}
          </div>

          {showFont && (
            <>
              <div className="h-5 w-px bg-white/10" />
              <div className="flex items-center gap-1">
                <Type size={13} className="text-ink-400" />
                {FONTS.map((f) => (
                  <button
                    key={f.v}
                    onClick={() => setFontSize(f.v)}
                    className={cn(
                      "rounded-md px-2 py-1 text-xs transition-colors",
                      fontSize === f.v
                        ? "bg-brand-500 text-white"
                        : "text-ink-300 hover:bg-white/8",
                    )}
                  >
                    {f.label}
                  </button>
                ))}
              </div>
            </>
          )}

          {showFill && (
            <>
              <div className="h-5 w-px bg-white/10" />
              <div className="flex items-center gap-1">
                {(
                  [
                    { v: false, label: "描边" },
                    { v: true, label: "填充" },
                  ] as const
                ).map((o) => (
                  <button
                    key={String(o.v)}
                    onClick={() => setFillShape(o.v)}
                    className={cn(
                      "rounded-md px-2 py-1 text-xs transition-colors",
                      fillShape === o.v
                        ? "bg-brand-500 text-white"
                        : "text-ink-300 hover:bg-white/8",
                    )}
                  >
                    {o.label}
                  </button>
                ))}
              </div>
            </>
          )}

          {tool === "sticker" && (
            <>
              <div className="h-5 w-px bg-white/10" />
              <div className="flex max-w-[340px] items-center gap-0.5 overflow-x-auto">
                {EMOJIS.map((em) => (
                  <button
                    key={em}
                    onClick={() => setEmoji(em)}
                    className={cn(
                      "shrink-0 rounded-md px-1 py-0.5 text-base leading-none transition-colors",
                      emoji === em ? "bg-brand-500/40 ring-1 ring-brand-400" : "hover:bg-white/8",
                    )}
                  >
                    {em}
                  </button>
                ))}
              </div>
            </>
          )}

          {fxSelected && (
            <>
              <div className="h-5 w-px bg-white/10" />
              <div className="flex items-center gap-2">
                <span className="text-xs text-ink-400">强度</span>
                <input
                  type="range"
                  min={6}
                  max={30}
                  step={2}
                  value={fxSelected.strength}
                  onMouseDown={() => markHistory()}
                  onChange={(e) =>
                    updateShape(
                      fxSelected.id,
                      { strength: Number(e.target.value) } as Partial<Shape>,
                      false,
                    )
                  }
                  className="w-24 accent-brand-500"
                />
              </div>
            </>
          )}

          {crop && (
            <>
              <div className="h-5 w-px bg-white/10" />
              <button
                onClick={() => setCrop(null)}
                title="取消裁剪，恢复整图"
                className="flex items-center gap-1.5 rounded-md bg-brand-500/15 px-2 py-1 text-xs text-brand-200 transition-colors hover:bg-brand-500/25"
              >
                <Crop size={12} />
                {crop.w}×{crop.h}
                <X size={11} />
              </button>
            </>
          )}
        </div>

        <div className="flex items-center gap-1">
          <button
            onClick={doPrivacy}
            disabled={detecting}
            title="自动检测手机号 / 邮箱 / 身份证 / 卡号并打码"
            className="mr-1 flex items-center gap-1.5 rounded-lg px-3 py-1.5 text-sm text-ink-300 transition-colors hover:bg-white/8 hover:text-ink-50 disabled:opacity-50"
          >
            <ShieldCheck size={15} />
            {detecting ? "检测中…" : "自动打码"}
          </button>

          <button
            onClick={() => setBeautify({ enabled: !beautify.enabled })}
            title="出图美化"
            className={cn(
              "mr-1 flex items-center gap-1.5 rounded-lg px-3 py-1.5 text-sm transition-colors",
              beautify.enabled
                ? "bg-brand-500/90 text-white shadow-soft"
                : "text-ink-300 hover:bg-white/8 hover:text-ink-50",
            )}
          >
            <Sparkles size={15} />
            美化
          </button>

          <IconBtn onClick={undo} disabled={!past.length} title="撤销 (Ctrl+Z)">
            <Undo2 size={16} />
          </IconBtn>
          <IconBtn onClick={redo} disabled={!future.length} title="重做 (Ctrl+Y)">
            <Redo2 size={16} />
          </IconBtn>
          <IconBtn
            onClick={() => selectedId && removeShape(selectedId)}
            disabled={!selectedId}
            title="删除所选 (Delete)"
          >
            <Trash2 size={16} />
          </IconBtn>

          <div className="mx-1 h-5 w-px bg-white/10" />

          <button
            onClick={() => {
              closeEditor();
            }}
            className="flex items-center gap-1.5 rounded-lg px-3 py-1.5 text-sm text-ink-300 transition-colors hover:bg-white/8 hover:text-ink-50"
          >
            <X size={15} />
            取消
          </button>
          <button
            onClick={doPin}
            title="把成图钉到屏幕上"
            className="flex items-center gap-1.5 rounded-lg px-3 py-1.5 text-sm text-ink-100 transition-colors hover:bg-white/8"
          >
            <Pin size={15} />
            贴图
          </button>
          <button
            onClick={(e) => doSave(e.shiftKey)}
            title="Ctrl+S（Shift+点击 = 保存后不关闭）"
            className="flex items-center gap-1.5 rounded-lg px-3 py-1.5 text-sm text-ink-100 transition-colors hover:bg-white/8"
          >
            <Save size={15} />
            保存
          </button>
          <button
            onClick={(e) => doCopy(e.shiftKey)}
            title="Ctrl+C（Shift+点击 = 复制后不关闭）"
            className="flex items-center gap-1.5 rounded-full bg-white px-4 py-1.5 text-[13px] font-semibold text-black transition-colors hover:bg-white/85"
          >
            <Copy size={15} />
            复制
          </button>
        </div>
      </div>

      {/* 工具栏 + 画布 */}
      <div className="flex flex-1 overflow-hidden">
        <div className="flex w-14 shrink-0 flex-col items-center gap-1 overflow-y-auto border-r border-white/6 py-3">
          {TOOLS.map((t) => (
            <button
              key={t.kind}
              onClick={() => setTool(t.kind)}
              title={`${t.label} (${t.key})`}
              className={cn(
                "grid h-9 w-9 place-items-center rounded-lg transition-colors",
                tool === t.kind
                  ? "bg-brand-500 text-white shadow-soft"
                  : "text-ink-300 hover:bg-white/8 hover:text-ink-50",
              )}
            >
              <t.icon size={18} />
            </button>
          ))}
        </div>

        <div className="flex-1 overflow-hidden bg-ink-950">
          <CanvasStage stageRef={stageRef} />
        </div>

        {beautify.enabled && <BeautifyPanel />}
      </div>

      {/* Toast */}
      <div className="pointer-events-none absolute inset-x-0 bottom-6 z-50 flex flex-col items-center gap-2">
        <AnimatePresence>
          {toasts.map((t) => (
            <motion.div
              key={t.id}
              initial={{ opacity: 0, y: 10, scale: 0.96 }}
              animate={{ opacity: 1, y: 0, scale: 1 }}
              exit={{ opacity: 0, y: -6 }}
              className="glass rounded-lg px-3.5 py-2 text-sm text-ink-50 shadow-pop"
            >
              {t.text}
            </motion.div>
          ))}
        </AnimatePresence>
      </div>
    </div>
  );
}

function BeautifyPanel() {
  const beautify = useEditor((s) => s.beautify);
  const setBeautify = useEditor((s) => s.setBeautify);

  return (
    <div className="flex w-60 shrink-0 flex-col gap-4 overflow-y-auto border-l border-white/6 bg-ink-900 px-4 py-4">
      <div className="flex items-center gap-1.5 text-sm font-medium text-ink-100">
        <Sparkles size={14} className="text-brand-400" />
        出图美化
      </div>

      {/* 背景 */}
      <div className="space-y-2">
        <div className="text-xs text-ink-400">背景</div>
        <div className="grid grid-cols-5 gap-2">
          {BG_PRESETS.map((p) => (
            <button
              key={p.id}
              onClick={() => setBeautify({ bgId: p.id })}
              title={p.label}
              className={cn(
                "h-7 w-full rounded-md border transition-transform hover:scale-105",
                beautify.bgId === p.id ? "border-white ring-2 ring-brand-500" : "border-white/15",
              )}
              style={{
                background:
                  p.kind === "transparent"
                    ? "repeating-conic-gradient(#888 0% 25%, #ccc 0% 50%) 50% / 8px 8px"
                    : p.kind === "solid"
                      ? p.colors[0]
                      : p.kind === "pattern"
                        ? p.pattern === "grid"
                          ? `linear-gradient(${p.colors[1]} 1px, transparent 1px), linear-gradient(90deg, ${p.colors[1]} 1px, transparent 1px), ${p.colors[0]}`
                          : `radial-gradient(${p.colors[1]} 1.5px, transparent 1.6px), ${p.colors[0]}`
                        : `linear-gradient(${p.angle}deg, ${p.colors[0]}, ${p.colors[1]})`,
                backgroundSize:
                  p.kind === "pattern"
                    ? p.pattern === "grid"
                      ? "7px 7px, 7px 7px, auto"
                      : "7px 7px, auto"
                    : undefined,
              }}
            />
          ))}
        </div>
      </div>

      {/* 窗口边框 */}
      <div className="space-y-2">
        <div className="text-xs text-ink-400">窗口边框</div>
        <div className="flex gap-1.5">
          {(
            [
              { v: "none", label: "无" },
              { v: "light", label: "浅色" },
              { v: "dark", label: "深色" },
            ] as const
          ).map((o) => (
            <button
              key={o.v}
              onClick={() => setBeautify({ windowFrame: o.v })}
              className={cn(
                "flex-1 rounded-md px-2 py-1 text-xs transition-colors",
                beautify.windowFrame === o.v
                  ? "bg-brand-500 text-white"
                  : "text-ink-300 hover:bg-white/8",
              )}
            >
              {o.label}
            </button>
          ))}
        </div>
      </div>

      {/* 比例 */}
      <div className="space-y-2">
        <div className="text-xs text-ink-400">画幅比例</div>
        <div className="flex flex-wrap gap-1.5">
          {RATIOS.map((r) => (
            <button
              key={r.id}
              onClick={() => setBeautify({ ratio: r.id })}
              className={cn(
                "rounded-md px-2 py-1 text-xs transition-colors",
                beautify.ratio === r.id
                  ? "bg-brand-500 text-white"
                  : "text-ink-300 hover:bg-white/8",
              )}
            >
              {r.label}
            </button>
          ))}
        </div>
      </div>

      {/* 内边距 */}
      <Slider
        label="边距"
        value={beautify.padding}
        min={0}
        max={0.2}
        step={0.005}
        onChange={(v) => setBeautify({ padding: v })}
        format={(v) => `${Math.round(v * 100)}%`}
      />

      {/* 圆角 */}
      <Slider
        label="圆角"
        value={beautify.radius}
        min={0}
        max={0.1}
        step={0.0025}
        onChange={(v) => setBeautify({ radius: v })}
        format={(v) => `${Math.round(v * 100)}%`}
      />

      {/* 阴影 */}
      <label className="flex cursor-pointer items-center justify-between text-xs text-ink-300">
        <span>投影</span>
        <input
          type="checkbox"
          checked={beautify.shadow}
          onChange={(e) => setBeautify({ shadow: e.target.checked })}
          className="h-4 w-4 accent-brand-500"
        />
      </label>

      {/* 水印 */}
      <div className="space-y-2">
        <div className="text-xs text-ink-400">水印文字</div>
        <input
          value={beautify.watermark}
          onChange={(e) => setBeautify({ watermark: e.target.value })}
          placeholder="可选，右下角显示"
          className="w-full rounded-md border border-white/10 bg-white/5 px-2 py-1.5 text-xs text-ink-100 outline-none placeholder:text-ink-500 focus:border-brand-400"
        />
      </div>
    </div>
  );
}

function Slider({
  label,
  value,
  min,
  max,
  step,
  onChange,
  format,
}: {
  label: string;
  value: number;
  min: number;
  max: number;
  step: number;
  onChange: (v: number) => void;
  format: (v: number) => string;
}) {
  return (
    <div className="space-y-1.5">
      <div className="flex items-center justify-between text-xs text-ink-400">
        <span>{label}</span>
        <span className="tabular-nums text-ink-300">{format(value)}</span>
      </div>
      <input
        type="range"
        min={min}
        max={max}
        step={step}
        value={value}
        onChange={(e) => onChange(Number(e.target.value))}
        className="w-full accent-brand-500"
      />
    </div>
  );
}

function IconBtn({
  children,
  onClick,
  disabled,
  title,
}: {
  children: React.ReactNode;
  onClick: () => void;
  disabled?: boolean;
  title?: string;
}) {
  return (
    <button
      onClick={onClick}
      disabled={disabled}
      title={title}
      className="grid h-8 w-8 place-items-center rounded-lg text-ink-300 transition-colors hover:bg-white/8 hover:text-ink-50 disabled:cursor-not-allowed disabled:opacity-30 disabled:hover:bg-transparent"
    >
      {children}
    </button>
  );
}
