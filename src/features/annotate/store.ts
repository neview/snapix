import { create } from "zustand";

export type ToolKind =
  | "select"
  | "rect"
  | "ellipse"
  | "arrow"
  | "line"
  | "pen"
  | "marker"
  | "text"
  | "step"
  | "sticker"
  | "spotlight"
  | "measure"
  | "mosaic"
  | "blur"
  | "crop";

type ShapeBase = {
  id: string;
  stroke: string;
  strokeWidth: number;
};

export type Shape =
  | (ShapeBase & {
      type: "rect";
      x: number;
      y: number;
      w: number;
      h: number;
      /** true = 半透明填充，false/缺省 = 仅描边 */
      fill?: boolean;
      /** 旋转角度（度），缺省 0 */
      rotation?: number;
    })
  | (ShapeBase & {
      type: "ellipse";
      x: number;
      y: number;
      w: number;
      h: number;
      fill?: boolean;
      rotation?: number;
    })
  | (ShapeBase & { type: "arrow"; points: number[] })
  | (ShapeBase & { type: "line"; points: number[] })
  | (ShapeBase & { type: "measure"; points: number[] })
  | (ShapeBase & { type: "pen"; points: number[] })
  | (ShapeBase & { type: "marker"; points: number[] })
  | (ShapeBase & {
      type: "text";
      x: number;
      y: number;
      text: string;
      fontSize: number;
      rotation?: number;
    })
  | (ShapeBase & { type: "step"; x: number; y: number; index: number })
  | (ShapeBase & {
      type: "sticker";
      x: number;
      y: number;
      emoji: string;
      size: number;
      rotation?: number;
    })
  | (ShapeBase & { type: "spotlight"; x: number; y: number; w: number; h: number })
  | (ShapeBase & {
      type: "mosaic" | "blur";
      x: number;
      y: number;
      w: number;
      h: number;
      strength: number;
    });

export type ImageInfo = { url: string; width: number; height: number };

/** 非破坏式裁剪视口（图像坐标），null = 不裁剪。 */
export type CropRect = { x: number; y: number; w: number; h: number };

export type Ratio = "auto" | "1:1" | "16:9" | "4:3" | "9:16";

/** 窗口拟态边框：无 / 浅色 / 深色标题栏（macOS 风格三色灯）。 */
export type WindowFrame = "none" | "light" | "dark";

export type Beautify = {
  enabled: boolean;
  bgId: string;
  padding: number; // 占图片短边的比例
  radius: number; // 占图片短边的比例
  shadow: boolean;
  ratio: Ratio;
  watermark: string;
  windowFrame: WindowFrame;
};

type State = {
  image: ImageInfo | null;
  crop: CropRect | null;
  setCrop: (c: CropRect | null) => void;
  tool: ToolKind;
  color: string;
  strokeWidth: number;
  fontSize: number;
  /** 矩形 / 椭圆是否半透明填充 */
  fillShape: boolean;
  /** 贴纸工具当前选中的 emoji */
  emoji: string;
  shapes: Shape[];
  selectedId: string | null;
  past: Shape[][];
  future: Shape[][];
  beautify: Beautify;
  setBeautify: (patch: Partial<Beautify>) => void;

  setImage: (image: ImageInfo) => void;
  setTool: (tool: ToolKind) => void;
  setColor: (color: string) => void;
  setStrokeWidth: (w: number) => void;
  setFontSize: (s: number) => void;
  setFillShape: (fill: boolean) => void;
  setEmoji: (emoji: string) => void;
  setSelected: (id: string | null) => void;

  addShape: (shape: Shape) => void;
  /** 批量添加（一次历史快照，一次撤销即可全部还原）。 */
  addShapes: (shapes: Shape[]) => void;
  updateShape: (id: string, patch: Partial<Shape>, history?: boolean) => void;
  removeShape: (id: string) => void;
  /** 显式压入一次历史快照（用于拖拽端点等连续修改开始前）。 */
  markHistory: () => void;
  nextStepIndex: () => number;

  undo: () => void;
  redo: () => void;
  reset: () => void;
};

const clone = (s: Shape[]) => s.map((x) => ({ ...x }));

export const useEditor = create<State>((set, get) => ({
  image: null,
  crop: null,
  setCrop: (crop) => set({ crop }),
  tool: "select",
  color: "#FF3B5C",
  strokeWidth: 4,
  fontSize: 28,
  fillShape: false,
  emoji: "⭐",
  shapes: [],
  selectedId: null,
  past: [],
  future: [],
  beautify: {
    enabled: false,
    bgId: "grad-indigo",
    padding: 0.07,
    radius: 0.03,
    shadow: true,
    ratio: "auto",
    watermark: "",
    windowFrame: "none",
  },

  setBeautify: (patch) => set((st) => ({ beautify: { ...st.beautify, ...patch } })),

  setImage: (image) =>
    set({ image, crop: null, shapes: [], selectedId: null, past: [], future: [] }),
  setTool: (tool) => set({ tool, selectedId: tool === "select" ? get().selectedId : null }),
  setColor: (color) => {
    set({ color });
    const id = get().selectedId;
    if (id) get().updateShape(id, { stroke: color });
  },
  setStrokeWidth: (strokeWidth) => {
    set({ strokeWidth });
    const id = get().selectedId;
    if (id) get().updateShape(id, { strokeWidth });
  },
  setFontSize: (fontSize) => {
    set({ fontSize });
    const id = get().selectedId;
    const sel = get().shapes.find((s) => s.id === id);
    if (id && sel?.type === "text") get().updateShape(id, { fontSize } as Partial<Shape>);
  },
  setFillShape: (fillShape) => {
    set({ fillShape });
    const id = get().selectedId;
    const sel = get().shapes.find((s) => s.id === id);
    if (id && (sel?.type === "rect" || sel?.type === "ellipse"))
      get().updateShape(id, { fill: fillShape } as Partial<Shape>);
  },
  setEmoji: (emoji) => set({ emoji }),
  setSelected: (selectedId) => set({ selectedId }),

  addShape: (shape) =>
    set((st) => ({
      past: [...st.past, clone(st.shapes)],
      future: [],
      shapes: [...st.shapes, shape],
    })),

  addShapes: (batch) =>
    set((st) => ({
      past: [...st.past, clone(st.shapes)],
      future: [],
      shapes: [...st.shapes, ...batch],
    })),

  updateShape: (id, patch, history = false) =>
    set((st) => ({
      past: history ? [...st.past, clone(st.shapes)] : st.past,
      future: history ? [] : st.future,
      shapes: st.shapes.map((s) => (s.id === id ? ({ ...s, ...patch } as Shape) : s)),
    })),

  removeShape: (id) =>
    set((st) => ({
      past: [...st.past, clone(st.shapes)],
      future: [],
      shapes: st.shapes.filter((s) => s.id !== id),
      selectedId: st.selectedId === id ? null : st.selectedId,
    })),

  markHistory: () => set((st) => ({ past: [...st.past, clone(st.shapes)], future: [] })),

  // 取现有最大序号 + 1 而非计数：删掉中间某个序号后新序号不会与已有的重复
  nextStepIndex: () =>
    get().shapes.reduce((max, s) => (s.type === "step" ? Math.max(max, s.index) : max), 0) + 1,

  undo: () =>
    set((st) => {
      if (!st.past.length) return st;
      const past = [...st.past];
      const prev = past.pop()!;
      return {
        past,
        future: [...st.future, clone(st.shapes)],
        shapes: prev,
        selectedId: null,
      };
    }),

  redo: () =>
    set((st) => {
      if (!st.future.length) return st;
      const future = [...st.future];
      const next = future.pop()!;
      return {
        future,
        past: [...st.past, clone(st.shapes)],
        shapes: next,
        selectedId: null,
      };
    }),

  reset: () => set({ shapes: [], selectedId: null, past: [], future: [], tool: "select" }),
}));

export const uid = () => Math.random().toString(36).slice(2, 10);
