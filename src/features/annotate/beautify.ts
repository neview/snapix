import type { Beautify, Ratio, WindowFrame } from "./store";

export type BgPreset = {
  id: string;
  label: string;
  kind: "transparent" | "solid" | "gradient" | "pattern";
  colors: string[];
  angle: number; // CSS 角度（deg）
  /** pattern 专用：底色 + 图案色 */
  pattern?: "dots" | "grid";
};

export const BG_PRESETS: BgPreset[] = [
  { id: "transparent", label: "透明", kind: "transparent", colors: [], angle: 0 },
  { id: "white", label: "白", kind: "solid", colors: ["#FFFFFF"], angle: 0 },
  { id: "dark", label: "深", kind: "solid", colors: ["#11141D"], angle: 0 },
  {
    id: "grad-indigo",
    label: "靛蓝",
    kind: "gradient",
    colors: ["#5B6CFF", "#9B5BFF"],
    angle: 135,
  },
  {
    id: "grad-sunset",
    label: "日落",
    kind: "gradient",
    colors: ["#FF8A65", "#FF3D7F"],
    angle: 135,
  },
  { id: "grad-ocean", label: "海洋", kind: "gradient", colors: ["#21D4FD", "#2152FF"], angle: 135 },
  { id: "grad-mint", label: "薄荷", kind: "gradient", colors: ["#34E89E", "#0F3443"], angle: 135 },
  { id: "grad-peach", label: "蜜桃", kind: "gradient", colors: ["#FFD3A5", "#FD6585"], angle: 135 },
  { id: "grad-slate", label: "石墨", kind: "gradient", colors: ["#3A4050", "#11141D"], angle: 135 },
  {
    id: "grad-aurora",
    label: "极光",
    kind: "gradient",
    colors: ["#00C9FF", "#92FE9D"],
    angle: 135,
  },
  { id: "grad-dusk", label: "暮色", kind: "gradient", colors: ["#2B5876", "#4E4376"], angle: 135 },
  { id: "grad-candy", label: "糖果", kind: "gradient", colors: ["#A18CD1", "#FBC2EB"], angle: 135 },
  {
    id: "pat-dots-light",
    label: "点阵",
    kind: "pattern",
    colors: ["#EEF1F6", "#C3CAD9"],
    angle: 0,
    pattern: "dots",
  },
  {
    id: "pat-grid-dark",
    label: "网格",
    kind: "pattern",
    colors: ["#12151F", "#2A3040"],
    angle: 0,
    pattern: "grid",
  },
];

/** 窗口边框配色。 */
export function frameColors(kind: WindowFrame): { bar: string; text: string } | null {
  if (kind === "light") return { bar: "#ECEEF2", text: "#3A4050" };
  if (kind === "dark") return { bar: "#2A2E38", text: "#C7CCDB" };
  return null;
}

/** 标题栏高度（图片物理像素），随图片宽度自适应并夹紧。 */
export function frameBarHeight(iw: number, kind: WindowFrame): number {
  if (kind === "none") return 0;
  return Math.round(Math.min(Math.max(iw * 0.05, 32), 72));
}

export const RATIOS: { id: Ratio; label: string }[] = [
  { id: "auto", label: "自动" },
  { id: "1:1", label: "1:1" },
  { id: "16:9", label: "16:9" },
  { id: "4:3", label: "4:3" },
  { id: "9:16", label: "9:16" },
];

const RATIO_VALUE: Record<Exclude<Ratio, "auto">, number> = {
  "1:1": 1,
  "16:9": 16 / 9,
  "4:3": 4 / 3,
  "9:16": 9 / 16,
};

export function getPreset(id: string): BgPreset {
  return BG_PRESETS.find((p) => p.id === id) ?? BG_PRESETS[0];
}

export type Layout = {
  outerW: number;
  outerH: number;
  /** 窗口框（标题栏 + 图片）左上角 */
  padX: number;
  padY: number;
  /** 图片左上角（= padY + barH） */
  imgX: number;
  imgY: number;
  /** 窗口框整体尺寸 */
  winW: number;
  winH: number;
  radiusPx: number;
  /** 标题栏高度（0 = 无窗口边框） */
  barH: number;
};

/** 计算美化画布外框尺寸与图片偏移（单位：图片物理像素）。 */
export function computeLayout(iw: number, ih: number, b: Beautify): Layout {
  const short = Math.min(iw, ih);
  const pad = b.enabled ? Math.round(b.padding * short) : 0;
  const barH = b.enabled ? frameBarHeight(iw, b.windowFrame) : 0;

  const winW = iw;
  const winH = ih + barH;

  let outerW = winW + 2 * pad;
  let outerH = winH + 2 * pad;

  if (b.enabled && b.ratio !== "auto") {
    const r = RATIO_VALUE[b.ratio];
    if (outerW / outerH < r) outerW = Math.round(outerH * r);
    else outerH = Math.round(outerW / r);
  }

  const padX = Math.round((outerW - winW) / 2);
  const padY = Math.round((outerH - winH) / 2);

  return {
    outerW,
    outerH,
    padX,
    padY,
    imgX: padX,
    imgY: padY + barH,
    winW,
    winH,
    radiusPx: b.enabled ? Math.round(b.radius * short) : 0,
    barH,
  };
}

/** 预览用的 CSS 背景（pattern 用多层 background 叠加底色 + 图案）。 */
export function cssBackground(b: Beautify): string {
  const p = getPreset(b.bgId);
  if (p.kind === "transparent") return "transparent";
  if (p.kind === "solid") return p.colors[0];
  if (p.kind === "pattern") {
    const [base, ink] = p.colors;
    if (p.pattern === "grid") {
      return `linear-gradient(${ink} 1px, transparent 1px), linear-gradient(90deg, ${ink} 1px, transparent 1px), ${base}`;
    }
    return `radial-gradient(${ink} 1.5px, transparent 1.6px), ${base}`;
  }
  return `linear-gradient(${p.angle}deg, ${p.colors[0]}, ${p.colors[1]})`;
}

/** pattern 背景的 CSS background-size（与绘制保持一致）。 */
export function cssBackgroundSize(b: Beautify): string | undefined {
  const p = getPreset(b.bgId);
  if (p.kind !== "pattern") return undefined;
  return p.pattern === "grid" ? "26px 26px, 26px 26px, auto" : "22px 22px, auto";
}

/** 导出时把背景画到 canvas。 */
export function paintBackground(ctx: CanvasRenderingContext2D, w: number, h: number, b: Beautify) {
  const p = getPreset(b.bgId);
  if (p.kind === "transparent") return;
  if (p.kind === "solid") {
    ctx.fillStyle = p.colors[0];
    ctx.fillRect(0, 0, w, h);
    return;
  }
  if (p.kind === "pattern") {
    const [base, ink] = p.colors;
    ctx.fillStyle = base;
    ctx.fillRect(0, 0, w, h);
    // 图案密度随画布短边缩放，避免大图上点/线过密
    const cell = Math.max(22, Math.round(Math.min(w, h) * 0.03));
    ctx.fillStyle = ink;
    ctx.strokeStyle = ink;
    if (p.pattern === "grid") {
      ctx.globalAlpha = 0.5;
      ctx.lineWidth = Math.max(1, cell * 0.04);
      for (let x = 0; x <= w; x += cell) {
        ctx.beginPath();
        ctx.moveTo(x, 0);
        ctx.lineTo(x, h);
        ctx.stroke();
      }
      for (let y = 0; y <= h; y += cell) {
        ctx.beginPath();
        ctx.moveTo(0, y);
        ctx.lineTo(w, y);
        ctx.stroke();
      }
      ctx.globalAlpha = 1;
    } else {
      const r = Math.max(1.2, cell * 0.07);
      for (let y = cell / 2; y < h; y += cell) {
        for (let x = cell / 2; x < w; x += cell) {
          ctx.beginPath();
          ctx.arc(x, y, r, 0, Math.PI * 2);
          ctx.fill();
        }
      }
    }
    return;
  }
  // 依据 CSS 角度计算渐变起止点
  const rad = ((p.angle - 90) * Math.PI) / 180;
  const cx = w / 2;
  const cy = h / 2;
  const len = Math.abs(w * Math.cos(rad)) + Math.abs(h * Math.sin(rad));
  const dx = (Math.cos(rad) * len) / 2;
  const dy = (Math.sin(rad) * len) / 2;
  const g = ctx.createLinearGradient(cx - dx, cy - dy, cx + dx, cy + dy);
  g.addColorStop(0, p.colors[0]);
  g.addColorStop(1, p.colors[1]);
  ctx.fillStyle = g;
  ctx.fillRect(0, 0, w, h);
}

/** 导出时绘制窗口标题栏（macOS 风格三色灯），画在窗口框顶部。 */
export function paintWindowBar(ctx: CanvasRenderingContext2D, layout: Layout, b: Beautify) {
  if (layout.barH <= 0) return;
  const fc = frameColors(b.windowFrame);
  if (!fc) return;

  ctx.save();
  ctx.fillStyle = fc.bar;
  ctx.fillRect(layout.padX, layout.padY, layout.winW, layout.barH);

  const cy = layout.padY + layout.barH / 2;
  const r = Math.max(4, layout.barH * 0.13);
  const gap = r * 3.2;
  const startX = layout.padX + gap;
  const dots = ["#FF5F57", "#FEBC2E", "#28C840"];
  dots.forEach((c, i) => {
    ctx.beginPath();
    ctx.fillStyle = c;
    ctx.arc(startX + i * gap, cy, r, 0, Math.PI * 2);
    ctx.fill();
  });
  ctx.restore();
}

export function roundRectPath(
  ctx: CanvasRenderingContext2D,
  x: number,
  y: number,
  w: number,
  h: number,
  r: number,
) {
  const radius = Math.min(r, w / 2, h / 2);
  ctx.beginPath();
  ctx.moveTo(x + radius, y);
  ctx.arcTo(x + w, y, x + w, y + h, radius);
  ctx.arcTo(x + w, y + h, x, y + h, radius);
  ctx.arcTo(x, y + h, x, y, radius);
  ctx.arcTo(x, y, x + w, y, radius);
  ctx.closePath();
}
