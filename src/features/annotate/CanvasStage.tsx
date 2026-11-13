import { useEffect, useLayoutEffect, useRef, useState } from "react";
import {
  Stage,
  Layer,
  Image as KImage,
  Rect,
  Ellipse,
  Arrow,
  Line,
  Text as KText,
  Group,
  Circle,
  Label,
  Tag,
  Transformer,
} from "react-konva";
import Konva from "konva";
import { useEditor, uid, type Shape } from "./store";
import { computeLayout, cssBackground, cssBackgroundSize, frameColors } from "./beautify";

/** 可被 Transformer 变换的图形类型（文字仅旋转）。 */
const TRANSFORMABLE = new Set([
  "rect",
  "ellipse",
  "mosaic",
  "blur",
  "spotlight",
  "sticker",
  "text",
]);

/** 支持旋转手柄的图形类型（马赛克/模糊/聚光灯为区域效果，不支持旋转）。 */
const ROTATABLE = new Set(["rect", "ellipse", "sticker", "text"]);

const clampNum = (v: number, lo: number, hi: number) => Math.min(Math.max(v, lo), hi);

/** #RRGGBB → rgba()，用于半透明填充。 */
function hexToRgba(hex: string, alpha: number): string {
  const m = /^#?([0-9a-f]{6})$/i.exec(hex.trim());
  if (!m) return hex;
  const n = parseInt(m[1], 16);
  return `rgba(${(n >> 16) & 255}, ${(n >> 8) & 255}, ${n & 255}, ${alpha})`;
}

export default function CanvasStage({
  stageRef,
}: {
  stageRef: React.RefObject<Konva.Stage | null>;
}) {
  const image = useEditor((s) => s.image);
  const tool = useEditor((s) => s.tool);
  const color = useEditor((s) => s.color);
  const strokeWidth = useEditor((s) => s.strokeWidth);
  const fontSize = useEditor((s) => s.fontSize);
  const fillShape = useEditor((s) => s.fillShape);
  const emoji = useEditor((s) => s.emoji);
  const shapes = useEditor((s) => s.shapes);
  const selectedId = useEditor((s) => s.selectedId);
  const addShape = useEditor((s) => s.addShape);
  const updateShape = useEditor((s) => s.updateShape);
  const removeShape = useEditor((s) => s.removeShape);
  const markHistory = useEditor((s) => s.markHistory);
  const setSelected = useEditor((s) => s.setSelected);
  const setTool = useEditor((s) => s.setTool);
  const nextStepIndex = useEditor((s) => s.nextStepIndex);
  const beautify = useEditor((s) => s.beautify);

  const crop = useEditor((s) => s.crop);
  const setCrop = useEditor((s) => s.setCrop);

  const wrapRef = useRef<HTMLDivElement>(null);
  const trRef = useRef<Konva.Transformer>(null);
  const [imgEl, setImgEl] = useState<HTMLImageElement | null>(null);
  const [box, setBox] = useState({ w: 800, h: 600 });
  const [zoom, setZoom] = useState(1);
  const [draft, setDraft] = useState<Shape | null>(null);
  const [cropDraft, setCropDraft] = useState<{ x: number; y: number; w: number; h: number } | null>(
    null,
  );
  const [editing, setEditing] = useState<string | null>(null);
  const drawingRef = useRef(false);
  const cropStartRef = useRef<{ x: number; y: number } | null>(null);

  // 换图时把缩放归 1（「随 prop 变化调整 state」模式：在渲染期而非 effect 里做，避免多渲染一帧）
  const [zoomForImage, setZoomForImage] = useState(image);
  if (image !== zoomForImage) {
    setZoomForImage(image);
    setZoom(1);
  }

  useEffect(() => {
    if (!image) return;
    let live = true;
    const el = new window.Image();
    el.crossOrigin = "anonymous";
    el.onload = () => {
      if (live) setImgEl(el);
    };
    el.src = image.url;
    return () => {
      live = false;
      el.onload = null;
      // 换图 / 编辑器关闭（image 置空）时把已解码的位图从状态里摘掉，
      // 否则窗口隐藏了图还留在内存里（4K 一张 33MB）
      setImgEl((cur) => (cur === el ? null : cur));
    };
  }, [image]);

  useLayoutEffect(() => {
    const measure = () => {
      const r = wrapRef.current?.getBoundingClientRect();
      if (r) setBox({ w: r.width, h: r.height });
    };
    measure();
    window.addEventListener("resize", measure);
    return () => window.removeEventListener("resize", measure);
  }, []);

  // 有效视口：裁剪后为裁剪区，否则整图（非破坏式，标注坐标始终基于原图）
  const effW = crop?.w ?? image?.width ?? 0;
  const effH = crop?.h ?? image?.height ?? 0;

  const layout = computeLayout(effW, effH, beautify);
  // 可用视口（扣除内边距）
  const availW = Math.max(120, box.w - 48);
  const availH = Math.max(120, box.h - 48);
  // 超长图（滚动截图产物）：按宽度适配 + 垂直滚动查看；普通图：整体适配
  const isTall = image ? layout.outerH > layout.outerW * 2.2 : false;
  const baseScale = image
    ? isTall
      ? Math.min(availW / layout.outerW, 1, 16000 / layout.outerH)
      : Math.min(availW / layout.outerW, availH / layout.outerH, 1.5)
    : 1;
  // 画布缩放（Ctrl+滚轮）；夹紧避免超出 canvas 尺寸上限
  const maxZoom = image
    ? Math.max(
        1,
        Math.min(6, 15000 / (layout.outerW * baseScale), 15000 / (layout.outerH * baseScale)),
      )
    : 1;
  const scale = image ? baseScale * clampNum(zoom, 0.2, maxZoom) : 1;

  const stageW = image ? effW * scale : box.w;
  const stageH = image ? effH * scale : box.h;
  const frameW = image ? layout.outerW * scale : box.w;
  const frameH = image ? layout.outerH * scale : box.h;
  const padXpx = layout.padX * scale;
  const padYpx = layout.padY * scale;
  const barHpx = layout.barH * scale;
  const radiusDisp = layout.radiusPx * scale;
  const frameCol = frameColors(beautify.windowFrame);

  // Ctrl+滚轮缩放画布（非被动监听才能 preventDefault）；上限随布局变化，用 ref 供事件回调读取
  const maxZoomRef = useRef(maxZoom);
  useEffect(() => {
    maxZoomRef.current = maxZoom;
  }, [maxZoom]);
  useEffect(() => {
    const el = wrapRef.current;
    if (!el) return;
    const onWheel = (e: WheelEvent) => {
      if (!e.ctrlKey) return;
      e.preventDefault();
      setZoom((z) => clampNum(z * (e.deltaY < 0 ? 1.12 : 1 / 1.12), 0.2, maxZoomRef.current));
    };
    el.addEventListener("wheel", onWheel, { passive: false });
    return () => el.removeEventListener("wheel", onWheel);
  }, []);

  // Ctrl+0 重置 / Ctrl+= 放大 / Ctrl+- 缩小
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (!(e.ctrlKey || e.metaKey)) return;
      const tag = document.activeElement?.tagName;
      if (tag === "TEXTAREA" || tag === "INPUT") return;
      if (e.key === "0") {
        e.preventDefault();
        setZoom(1);
      } else if (e.key === "=" || e.key === "+") {
        e.preventDefault();
        setZoom((z) => clampNum(z * 1.2, 0.2, maxZoomRef.current));
      } else if (e.key === "-") {
        e.preventDefault();
        setZoom((z) => clampNum(z / 1.2, 0.2, maxZoomRef.current));
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

  // 绑定 / 解绑变换器
  useEffect(() => {
    const tr = trRef.current;
    const stage = stageRef.current;
    if (!tr || !stage) return;
    const sel = shapes.find((s) => s.id === selectedId);
    if (tool === "select" && sel && TRANSFORMABLE.has(sel.type)) {
      const node = stage.findOne(`#${selectedId}`);
      tr.nodes(node ? [node] : []);
    } else {
      tr.nodes([]);
    }
    tr.getLayer()?.batchDraw();
  }, [selectedId, shapes, tool, stageRef]);

  const pointer = (stage: Konva.Stage) => stage.getRelativePointerPosition()!;

  const onMouseDown = (e: Konva.KonvaEventObject<MouseEvent>) => {
    const stage = e.target.getStage();
    if (!stage) return;
    const clickedEmpty = e.target === stage || e.target.hasName("bg");

    if (tool === "select") {
      if (clickedEmpty) setSelected(null);
      return;
    }
    if (!clickedEmpty) return;

    const p = pointer(stage);

    if (tool === "crop") {
      drawingRef.current = true;
      cropStartRef.current = { x: p.x, y: p.y };
      setCropDraft({ x: p.x, y: p.y, w: 0, h: 0 });
      return;
    }

    const id = uid();

    if (tool === "text") {
      addShape({
        id,
        type: "text",
        x: p.x,
        y: p.y,
        text: "",
        fontSize,
        stroke: color,
        strokeWidth,
      });
      setTool("select");
      setSelected(id);
      setEditing(id);
      return;
    }
    if (tool === "step") {
      addShape({
        id,
        type: "step",
        x: p.x,
        y: p.y,
        index: nextStepIndex(),
        stroke: color,
        strokeWidth,
      });
      return;
    }
    if (tool === "sticker") {
      const size = fontSize * 2;
      addShape({
        id,
        type: "sticker",
        x: p.x - size / 2,
        y: p.y - size / 2,
        emoji,
        size,
        stroke: color,
        strokeWidth,
      });
      return;
    }

    drawingRef.current = true;
    if (tool === "rect" || tool === "ellipse") {
      setDraft({
        id,
        type: tool,
        x: p.x,
        y: p.y,
        w: 0,
        h: 0,
        fill: fillShape,
        stroke: color,
        strokeWidth,
      });
    } else if (tool === "mosaic" || tool === "blur") {
      setDraft({
        id,
        type: tool,
        x: p.x,
        y: p.y,
        w: 0,
        h: 0,
        strength: 14,
        stroke: color,
        strokeWidth,
      });
    } else if (tool === "spotlight") {
      setDraft({ id, type: "spotlight", x: p.x, y: p.y, w: 0, h: 0, stroke: color, strokeWidth });
    } else if (tool === "arrow" || tool === "line" || tool === "measure") {
      setDraft({ id, type: tool, points: [p.x, p.y, p.x, p.y], stroke: color, strokeWidth });
    } else if (tool === "pen" || tool === "marker") {
      setDraft({ id, type: tool, points: [p.x, p.y], stroke: color, strokeWidth });
    }
  };

  const onMouseMove = (e: Konva.KonvaEventObject<MouseEvent>) => {
    if (!drawingRef.current) return;
    const stage = e.target.getStage();
    if (!stage) return;
    const p = pointer(stage);

    if (tool === "crop" && cropStartRef.current) {
      const s = cropStartRef.current;
      setCropDraft({
        x: Math.min(s.x, p.x),
        y: Math.min(s.y, p.y),
        w: Math.abs(p.x - s.x),
        h: Math.abs(p.y - s.y),
      });
      return;
    }

    if (!draft) return;

    if (
      draft.type === "rect" ||
      draft.type === "ellipse" ||
      draft.type === "mosaic" ||
      draft.type === "blur" ||
      draft.type === "spotlight"
    ) {
      setDraft({ ...draft, w: p.x - draft.x, h: p.y - draft.y });
    } else if (draft.type === "arrow" || draft.type === "line" || draft.type === "measure") {
      setDraft({ ...draft, points: [draft.points[0], draft.points[1], p.x, p.y] });
    } else if (draft.type === "pen" || draft.type === "marker") {
      setDraft({ ...draft, points: [...draft.points, p.x, p.y] });
    }
  };

  const onMouseUp = () => {
    drawingRef.current = false;

    if (tool === "crop") {
      cropStartRef.current = null;
      if (cropDraft && image && cropDraft.w >= 12 && cropDraft.h >= 12) {
        // 相对原图夹紧（cropDraft 已是原图坐标）
        const x = Math.max(0, Math.round(cropDraft.x));
        const y = Math.max(0, Math.round(cropDraft.y));
        const w = Math.min(image.width - x, Math.round(cropDraft.w));
        const h = Math.min(image.height - y, Math.round(cropDraft.h));
        if (w >= 12 && h >= 12) setCrop({ x, y, w, h });
        setTool("select");
      }
      setCropDraft(null);
      return;
    }

    if (!draft) return;
    let commit: Shape = draft;
    if ("w" in commit && "h" in commit) {
      const x = commit.w < 0 ? commit.x + commit.w : commit.x;
      const y = commit.h < 0 ? commit.y + commit.h : commit.y;
      const w = Math.abs(commit.w);
      const h = Math.abs(commit.h);
      if (w < 4 || h < 4) return setDraft(null);
      commit = { ...commit, x, y, w, h };
    }
    if ((commit.type === "pen" || commit.type === "marker") && commit.points.length < 4)
      return setDraft(null);
    if (commit.type === "measure") {
      const [ax, ay, bx, by] = commit.points;
      if (Math.hypot(bx - ax, by - ay) < 6) return setDraft(null);
    }
    addShape(commit);
    setDraft(null);
  };

  const update = (id: string, patch: Partial<Shape>) => updateShape(id, patch, true);

  const selectedShape = shapes.find((s) => s.id === selectedId);
  const showEndpoints =
    tool === "select" &&
    selectedShape &&
    (selectedShape.type === "arrow" ||
      selectedShape.type === "line" ||
      selectedShape.type === "measure");

  const editingShape = editing ? shapes.find((s) => s.id === editing) : null;

  // 文字编辑框挂在画框容器内，随内容一起滚动；位置 = 画框内边距 + 标题栏 + 图像坐标 × 缩放
  const editLeft =
    editingShape?.type === "text" ? padXpx + (editingShape.x - (crop?.x ?? 0)) * scale : 0;
  const editTop =
    editingShape?.type === "text" ? padYpx + barHpx + (editingShape.y - (crop?.y ?? 0)) * scale : 0;

  // 聚光灯：压暗全图 + destination-out 挖亮窗口（含拖拽草稿实时预览）
  const spotlights = shapes.filter(
    (s): s is Extract<Shape, { type: "spotlight" }> => s.type === "spotlight",
  );
  const draftSpot = draft?.type === "spotlight" ? draft : null;
  const allSpots = draftSpot ? [...spotlights, draftSpot] : spotlights;
  const others = [
    ...shapes.filter((s) => s.type !== "spotlight"),
    ...(draft && draft.type !== "spotlight" ? [draft] : []),
  ];

  const trIsSticker = selectedShape?.type === "sticker";
  const trRotatable = !!selectedShape && ROTATABLE.has(selectedShape.type);
  // 贴纸：等比四角；文字：仅旋转（字号走属性栏）；其余：全向
  const trAnchors = trIsSticker
    ? ["top-left", "top-right", "bottom-left", "bottom-right"]
    : selectedShape?.type === "text"
      ? []
      : undefined;

  return (
    <div className="relative h-full w-full">
      <div ref={wrapRef} className="flex h-full w-full overflow-auto p-6">
        <div
          className="relative m-auto shrink-0"
          style={{
            width: frameW,
            height: frameH,
            background: beautify.enabled ? cssBackground(beautify) : undefined,
            backgroundSize: beautify.enabled ? cssBackgroundSize(beautify) : undefined,
            borderRadius: beautify.enabled ? Math.max(12, radiusDisp * 0.7) : 0,
          }}
        >
          {/* 窗口框：标题栏 + 截图共用圆角与投影 */}
          <div
            className="absolute overflow-hidden"
            style={{
              left: padXpx,
              top: padYpx,
              width: stageW,
              height: stageH + barHpx,
              borderRadius: beautify.enabled ? radiusDisp : 8,
              boxShadow: beautify.enabled
                ? beautify.shadow
                  ? "0 22px 55px -12px rgba(0,0,0,.45)"
                  : "none"
                : "var(--shadow-pop, 0 12px 30px rgba(0,0,0,.18))",
            }}
          >
            {barHpx > 0 && frameCol && (
              <div
                className="flex items-center gap-2"
                style={{ height: barHpx, background: frameCol.bar, paddingLeft: barHpx * 0.42 }}
              >
                {["#FF5F57", "#FEBC2E", "#28C840"].map((c) => (
                  <span
                    key={c}
                    className="rounded-full"
                    style={{ width: barHpx * 0.26, height: barHpx * 0.26, background: c }}
                  />
                ))}
              </div>
            )}
            <Stage
              ref={stageRef}
              width={stageW}
              height={stageH}
              scaleX={scale}
              scaleY={scale}
              offsetX={crop?.x ?? 0}
              offsetY={crop?.y ?? 0}
              onMouseDown={onMouseDown}
              onMouseMove={onMouseMove}
              onMouseUp={onMouseUp}
              style={{ cursor: tool === "select" ? "default" : "crosshair" }}
            >
              <Layer>
                {imgEl && (
                  <KImage image={imgEl} width={image!.width} height={image!.height} name="bg" />
                )}
              </Layer>

              <Layer>
                {/* 聚光灯：先铺暗幕，再逐个挖亮 */}
                {allSpots.length > 0 && image && (
                  <Rect
                    x={0}
                    y={0}
                    width={image.width}
                    height={image.height}
                    fill="rgba(8, 10, 16, 0.58)"
                    listening={false}
                  />
                )}
                {allSpots.map((s) => (
                  <Rect
                    key={s.id}
                    id={s.id}
                    x={s.x}
                    y={s.y}
                    width={s.w}
                    height={s.h}
                    fill="#fff"
                    globalCompositeOperation="destination-out"
                    listening={tool === "select"}
                    draggable={tool === "select"}
                    onClick={() => tool === "select" && setSelected(s.id)}
                    onTap={() => tool === "select" && setSelected(s.id)}
                    onDragEnd={(e) => update(s.id, { x: e.target.x(), y: e.target.y() })}
                    onTransformEnd={(e) => {
                      const n = e.target;
                      const sx = n.scaleX();
                      const sy = n.scaleY();
                      n.scaleX(1);
                      n.scaleY(1);
                      update(s.id, {
                        x: n.x(),
                        y: n.y(),
                        w: Math.max(4, n.width() * sx),
                        h: Math.max(4, n.height() * sy),
                      } as Partial<Shape>);
                    }}
                  />
                ))}
                {/* 仅选中 / 拖拽中的聚光灯显示虚线描边（导出前会清空选中，成图无描边） */}
                {allSpots
                  .filter((s) => s.id === selectedId || s.id === draftSpot?.id)
                  .map((s) => (
                    <Rect
                      key={`sp-edge-${s.id}`}
                      x={s.x}
                      y={s.y}
                      width={s.w}
                      height={s.h}
                      stroke="rgba(255,255,255,0.45)"
                      strokeWidth={1.2 / scale}
                      dash={[6 / scale, 4 / scale]}
                      listening={false}
                    />
                  ))}

                {others.map((s) => (
                  <ShapeNode
                    key={s.id}
                    s={s}
                    imgEl={imgEl}
                    draggable={tool === "select"}
                    onSelect={() => tool === "select" && setSelected(s.id)}
                    onChange={(patch) => update(s.id, patch)}
                    onEdit={() => {
                      setSelected(s.id);
                      setEditing(s.id);
                    }}
                  />
                ))}
                <Transformer
                  ref={trRef}
                  rotateEnabled={trRotatable}
                  rotationSnaps={[0, 45, 90, 135, 180, 225, 270, 315]}
                  rotationSnapTolerance={6}
                  keepRatio={trIsSticker}
                  enabledAnchors={trAnchors}
                  borderStroke="#5B6CFF"
                  anchorStroke="#5B6CFF"
                  anchorFill="#fff"
                  anchorSize={9}
                  ignoreStroke
                  boundBoxFunc={(oldB, newB) => (newB.width < 8 || newB.height < 8 ? oldB : newB)}
                />

                {/* 裁剪拖拽预览 */}
                {cropDraft && (
                  <Rect
                    x={cropDraft.x}
                    y={cropDraft.y}
                    width={cropDraft.w}
                    height={cropDraft.h}
                    stroke="#5B6CFF"
                    strokeWidth={2 / scale}
                    dash={[8 / scale, 5 / scale]}
                    fill="rgba(91,108,255,0.08)"
                    listening={false}
                  />
                )}

                {/* 箭头 / 直线端点编辑手柄 */}
                {showEndpoints &&
                  selectedShape &&
                  "points" in selectedShape &&
                  [0, 1].map((i) => (
                    <Circle
                      key={`ep-${i}`}
                      x={selectedShape.points[i * 2]}
                      y={selectedShape.points[i * 2 + 1]}
                      radius={7 / scale}
                      fill="#fff"
                      stroke="#5B6CFF"
                      strokeWidth={2 / scale}
                      draggable
                      onDragStart={() => markHistory()}
                      onDragMove={(e) => {
                        const pts = [...selectedShape.points];
                        pts[i * 2] = e.target.x();
                        pts[i * 2 + 1] = e.target.y();
                        updateShape(selectedShape.id, { points: pts } as Partial<Shape>, false);
                      }}
                    />
                  ))}
              </Layer>
            </Stage>
          </div>

          {beautify.enabled && beautify.watermark && (
            <div
              className="pointer-events-none absolute bottom-2 right-3 select-none font-semibold text-white/85 drop-shadow"
              style={{ fontSize: Math.max(11, 13 * scale) }}
            >
              {beautify.watermark}
            </div>
          )}

          {/* 文字内联编辑（挂在画框内，随内容滚动） */}
          {editingShape?.type === "text" && (
            <textarea
              autoFocus
              defaultValue={editingShape.text}
              onBlur={(e) => {
                const v = e.currentTarget.value;
                if (!v.trim()) removeShape(editingShape.id);
                else update(editingShape.id, { text: v } as Partial<Shape>);
                setEditing(null);
              }}
              onKeyDown={(e) => {
                if ((e.key === "Enter" && !e.shiftKey) || e.key === "Escape") {
                  e.preventDefault();
                  e.currentTarget.blur();
                }
              }}
              className="absolute z-30 resize-none overflow-hidden rounded border border-brand-400 bg-white/95 px-1 font-bold leading-tight shadow-pop outline-none"
              style={{
                left: editLeft,
                top: editTop,
                fontSize: editingShape.fontSize * scale,
                minWidth: 100,
                color: editingShape.stroke,
              }}
            />
          )}
        </div>
      </div>

      {/* 缩放 HUD */}
      {image && Math.abs(zoom - 1) > 0.001 && (
        <div className="pointer-events-none absolute bottom-3 right-3 rounded-md bg-black/65 px-2 py-1 text-[11px] font-medium tabular-nums text-white backdrop-blur">
          {Math.round(zoom * 100)}% · Ctrl+0 重置
        </div>
      )}
    </div>
  );
}

type NodeProps = {
  s: Shape;
  imgEl: HTMLImageElement | null;
  draggable: boolean;
  onSelect: () => void;
  onChange: (patch: Partial<Shape>) => void;
  onEdit: () => void;
};

function bakePoints(node: Konva.Node, pts: number[]) {
  const dx = node.x();
  const dy = node.y();
  node.position({ x: 0, y: 0 });
  return pts.map((v, i) => (i % 2 === 0 ? v + dx : v + dy));
}

function ShapeNode({ s, imgEl, draggable, onSelect, onChange, onEdit }: NodeProps) {
  // 绘制类工具激活时关闭命中（listening），避免已有图形挡住新标注的落笔
  const common = {
    id: s.id,
    draggable,
    listening: draggable,
    onClick: onSelect,
    onTap: onSelect,
  };

  switch (s.type) {
    case "rect":
      return (
        <Rect
          {...common}
          x={s.x}
          y={s.y}
          width={s.w}
          height={s.h}
          rotation={s.rotation ?? 0}
          stroke={s.stroke}
          strokeWidth={s.strokeWidth}
          fill={s.fill ? hexToRgba(s.stroke, 0.22) : undefined}
          cornerRadius={4}
          onDragEnd={(e) => onChange({ x: e.target.x(), y: e.target.y() })}
          onTransformEnd={(e) => {
            const n = e.target;
            const sx = n.scaleX();
            const sy = n.scaleY();
            n.scaleX(1);
            n.scaleY(1);
            onChange({
              x: n.x(),
              y: n.y(),
              w: Math.max(4, n.width() * sx),
              h: Math.max(4, n.height() * sy),
              rotation: n.rotation(),
            } as Partial<Shape>);
          }}
        />
      );
    case "ellipse":
      return (
        <Ellipse
          {...common}
          x={s.x + s.w / 2}
          y={s.y + s.h / 2}
          radiusX={Math.abs(s.w / 2)}
          radiusY={Math.abs(s.h / 2)}
          rotation={s.rotation ?? 0}
          stroke={s.stroke}
          strokeWidth={s.strokeWidth}
          fill={s.fill ? hexToRgba(s.stroke, 0.22) : undefined}
          onDragEnd={(e) => onChange({ x: e.target.x() - s.w / 2, y: e.target.y() - s.h / 2 })}
          onTransformEnd={(e) => {
            const n = e.target;
            const sx = n.scaleX();
            const sy = n.scaleY();
            n.scaleX(1);
            n.scaleY(1);
            const w = Math.max(4, Math.abs(s.w) * sx);
            const h = Math.max(4, Math.abs(s.h) * sy);
            onChange({
              x: n.x() - w / 2,
              y: n.y() - h / 2,
              w,
              h,
              rotation: n.rotation(),
            } as Partial<Shape>);
          }}
        />
      );
    case "arrow":
      return (
        <Arrow
          {...common}
          points={s.points}
          stroke={s.stroke}
          fill={s.stroke}
          strokeWidth={s.strokeWidth}
          pointerLength={Math.max(8, s.strokeWidth * 2.5)}
          pointerWidth={Math.max(8, s.strokeWidth * 2.5)}
          hitStrokeWidth={Math.max(12, s.strokeWidth)}
          onDragEnd={(e) => onChange({ points: bakePoints(e.target, s.points) } as Partial<Shape>)}
        />
      );
    case "line":
      return (
        <Line
          {...common}
          points={s.points}
          stroke={s.stroke}
          strokeWidth={s.strokeWidth}
          lineCap="round"
          hitStrokeWidth={Math.max(12, s.strokeWidth)}
          onDragEnd={(e) => onChange({ points: bakePoints(e.target, s.points) } as Partial<Shape>)}
        />
      );
    case "measure": {
      const [x1, y1, x2, y2] = s.points;
      const len = Math.hypot(x2 - x1, y2 - y1);
      const iw = imgEl?.naturalWidth ?? 1200;
      // 标签字号随图像宽度自适应（导出为图像像素，观感稳定）
      const fs = Math.min(44, Math.max(13, Math.round(iw * 0.011)));
      const tick = fs * 0.5 + s.strokeWidth;
      const ux = len > 0 ? (x2 - x1) / len : 1;
      const uy = len > 0 ? (y2 - y1) / len : 0;
      const px = -uy;
      const py = ux;
      const mx = (x1 + x2) / 2;
      const my = (y1 + y2) / 2;
      const label = `${Math.round(len)} px`;
      return (
        <Group
          {...common}
          onDragEnd={(e) => onChange({ points: bakePoints(e.target, s.points) } as Partial<Shape>)}
        >
          <Line
            points={s.points}
            stroke={s.stroke}
            strokeWidth={s.strokeWidth}
            hitStrokeWidth={Math.max(14, s.strokeWidth * 3)}
          />
          {/* 两端垂直刻度线 */}
          <Line
            points={[x1 + px * tick, y1 + py * tick, x1 - px * tick, y1 - py * tick]}
            stroke={s.stroke}
            strokeWidth={s.strokeWidth}
          />
          <Line
            points={[x2 + px * tick, y2 + py * tick, x2 - px * tick, y2 - py * tick]}
            stroke={s.stroke}
            strokeWidth={s.strokeWidth}
          />
          {/* 像素距离标签（沿法线偏移出线体） */}
          <Label
            x={mx + px * (fs + tick)}
            y={my + py * (fs + tick)}
            offsetX={label.length * fs * 0.31}
            offsetY={fs * 0.8}
            listening={false}
          >
            <Tag fill="rgba(15,17,24,0.85)" cornerRadius={4} />
            <KText text={label} fontSize={fs} fill="#fff" padding={fs * 0.28} fontStyle="bold" />
          </Label>
        </Group>
      );
    }
    case "pen":
      return (
        <Line
          {...common}
          points={s.points}
          stroke={s.stroke}
          strokeWidth={s.strokeWidth}
          lineCap="round"
          lineJoin="round"
          tension={0.4}
          hitStrokeWidth={Math.max(12, s.strokeWidth)}
          onDragEnd={(e) => onChange({ points: bakePoints(e.target, s.points) } as Partial<Shape>)}
        />
      );
    case "marker":
      return (
        <Line
          {...common}
          points={s.points}
          stroke={s.stroke}
          strokeWidth={s.strokeWidth * 4}
          opacity={0.42}
          lineCap="round"
          lineJoin="round"
          tension={0.4}
          hitStrokeWidth={Math.max(16, s.strokeWidth * 4)}
          onDragEnd={(e) => onChange({ points: bakePoints(e.target, s.points) } as Partial<Shape>)}
        />
      );
    case "text":
      return (
        <KText
          {...common}
          x={s.x}
          y={s.y}
          text={s.text || " "}
          fontSize={s.fontSize}
          rotation={s.rotation ?? 0}
          fill={s.stroke}
          fontStyle="bold"
          onDblClick={onEdit}
          onDblTap={onEdit}
          onDragEnd={(e) => onChange({ x: e.target.x(), y: e.target.y() })}
          onTransformEnd={(e) => {
            const n = e.target;
            n.scaleX(1);
            n.scaleY(1);
            onChange({
              x: n.x(),
              y: n.y(),
              rotation: n.rotation(),
            } as Partial<Shape>);
          }}
        />
      );
    case "step":
      return <StepBadge s={s} common={common} onChange={onChange} />;
    case "sticker":
      return (
        <KText
          {...common}
          x={s.x}
          y={s.y}
          text={s.emoji}
          fontSize={s.size}
          rotation={s.rotation ?? 0}
          onDragEnd={(e) => onChange({ x: e.target.x(), y: e.target.y() })}
          onTransformEnd={(e) => {
            const n = e.target;
            const sx = n.scaleX();
            n.scaleX(1);
            n.scaleY(1);
            onChange({
              x: n.x(),
              y: n.y(),
              size: Math.max(14, s.size * sx),
              rotation: n.rotation(),
            } as Partial<Shape>);
          }}
        />
      );
    case "spotlight":
      // 聚光灯在画布层单独渲染（需要 destination-out 合成）
      return null;
    case "mosaic":
    case "blur":
      return <FxImage s={s} common={common} imgEl={imgEl} onChange={onChange} />;
  }
}

function StepBadge({
  s,
  common,
  onChange,
}: {
  s: Extract<Shape, { type: "step" }>;
  common: Record<string, unknown>;
  onChange: (patch: Partial<Shape>) => void;
}) {
  const r = 16 + s.strokeWidth;
  return (
    <Group
      {...common}
      x={s.x}
      y={s.y}
      onDragEnd={(e: Konva.KonvaEventObject<DragEvent>) =>
        onChange({ x: e.target.x(), y: e.target.y() })
      }
    >
      <Circle radius={r} fill={s.stroke} />
      <KText
        text={String(s.index)}
        fontSize={r}
        fontStyle="bold"
        fill="#fff"
        width={r * 2}
        height={r * 2}
        offsetX={r}
        offsetY={r}
        align="center"
        verticalAlign="middle"
      />
    </Group>
  );
}

function FxImage({
  s,
  common,
  imgEl,
  onChange,
}: {
  s: Extract<Shape, { type: "mosaic" | "blur" }>;
  common: Record<string, unknown>;
  imgEl: HTMLImageElement | null;
  onChange: (patch: Partial<Shape>) => void;
}) {
  const ref = useRef<Konva.Image>(null);
  useEffect(() => {
    const node = ref.current;
    if (!node || !imgEl) return;
    node.cache();
    node.getLayer()?.batchDraw();
  }, [s.x, s.y, s.w, s.h, s.strength, s.type, imgEl]);

  if (!imgEl) return null;
  return (
    <KImage
      ref={ref}
      {...common}
      image={imgEl}
      x={s.x}
      y={s.y}
      width={s.w}
      height={s.h}
      crop={{ x: s.x, y: s.y, width: s.w, height: s.h }}
      filters={s.type === "mosaic" ? [Konva.Filters.Pixelate] : [Konva.Filters.Blur]}
      pixelSize={s.strength}
      blurRadius={s.strength}
      onDragEnd={(e) => onChange({ x: e.target.x(), y: e.target.y() })}
      onTransformEnd={(e) => {
        const n = e.target;
        const sx = n.scaleX();
        const sy = n.scaleY();
        n.scaleX(1);
        n.scaleY(1);
        onChange({
          x: n.x(),
          y: n.y(),
          w: Math.max(4, n.width() * sx),
          h: Math.max(4, n.height() * sy),
        } as Partial<Shape>);
      }}
    />
  );
}
