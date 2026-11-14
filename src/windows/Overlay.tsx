import { useCallback, useEffect, useLayoutEffect, useRef, useState } from "react";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { AnimatePresence, motion } from "motion/react";
import { Copy, Save, PenLine, Pin, Check, X, ScrollText, Film, QrCode } from "lucide-react";
import {
  cancelCapture,
  overlayReady,
  overlayFetch,
  overlayClaim,
  overlayRelease,
  copyRegion,
  copyToLine,
  lineLanded,
  lineDropHint,
  saveRegion,
  openEditor,
  pinRegion,
  ocrRegion,
  qrRegion,
  copyText,
  scrollCaptureStart,
  gifRecordStart,
  elementAt,
  type OverlayPayload,
  type SnapWindow,
  type ElementHit,
  type LineLanding,
} from "@/lib/ipc";
import { animateCurtain, animateFlight, FLIGHT_MS } from "@/lib/flight";

type Rect = { x: number; y: number; w: number; h: number };

type Handle = "nw" | "n" | "ne" | "e" | "se" | "s" | "sw" | "w";

type Drag =
  | { kind: "pending"; sx: number; sy: number }
  | { kind: "draw"; sx: number; sy: number }
  | { kind: "move"; sx: number; sy: number; orig: Rect }
  | { kind: "resize"; handle: Handle; orig: Rect };

const MIN_SIZE = 4;
const CLICK_SLOP = 4;
const MAG_SIZE = 124;
const MAG_ZOOM = 7;
const LAND_SETTLE_MS = 2200;

/**
 * 输出动作的整体兜底：任何一步 IPC 没回来（主线程建窗阻塞、渲染进程 IPC 卡住、
 * 磁盘极慢），到点都必须把遮罩放开。
 *
 * 遮罩是一块整屏置顶窗，卡在 committing 时鼠标键盘全部被它吃掉、连 Esc 都被拦，
 * 用户唯一的出路就是去托盘强杀进程——这种事一次都不能发生。
 */
const COMMIT_TIMEOUT_MS = 6000;
/** 收起遮罩时最多等这么久；剩下的交给二次收起（releaseFrame）与 Rust 侧重试 */
const HIDE_WAIT_MS = 260;
/** 提交超过这么久还没结束就显示「处理中」，别让用户以为已经死了 */
const SLOW_COMMIT_MS = 600;
/** committing 期间按 Esc 的宽限：太快放开会误伤正常（几百毫秒）的提交 */
const ESC_GRACE_MS = 900;

/** 选区顶边进到距屏幕顶端这么近时，把晾衣架举出来；松手即挂上去 */
const DROP_BAND = 92;

const HANDLE_CURSOR: Record<Handle, string> = {
  nw: "nwse-resize",
  se: "nwse-resize",
  ne: "nesw-resize",
  sw: "nesw-resize",
  n: "ns-resize",
  s: "ns-resize",
  e: "ew-resize",
  w: "ew-resize",
};

const clamp = (v: number, lo: number, hi: number) => Math.min(Math.max(v, lo), hi);

let toastSeq = 0;

export default function Overlay() {
  const [frame, setFrame] = useState<OverlayPayload | null>(null);
  const [sel, setSel] = useState<Rect | null>(null);
  const [drag, setDrag] = useState<Drag | null>(null);
  const [cursor, setCursor] = useState<{ x: number; y: number } | null>(null);
  const [hex, setHex] = useState<string>("#000000");
  const [rgb, setRgb] = useState<[number, number, number]>([0, 0, 0]);
  const [ocr, setOcr] = useState<{
    loading: boolean;
    text: string;
    qr?: string[];
    error?: string;
  } | null>(null);
  const [toasts, setToasts] = useState<{ id: number; text: string }[]>([]);
  const [elemMode, setElemMode] = useState(false);
  const [elem, setElem] = useState<ElementHit | null>(null);
  // 输出动作进行中：保留选区画面（去掉工具栏 / 手柄 / 边框），等后端收起遮罩后再重置。
  // 贴图尤其依赖这一点——贴图窗在选区原位显示后遮罩才隐藏，两者像素一致，切换无感。
  const [committing, setCommitting] = useState(false);
  /** 保存对话框打开中：冻屏画面 / 选区 / 工具条全部原地不动，键盘鼠标一并冻结 */
  const [saving, setSaving] = useState(false);
  /** 提交迟迟不返回时显示的「处理中」，只为了告诉用户没死机 */
  const [slowCommit, setSlowCommit] = useState(false);
  /** 正在飞向晾衣绳：从选区矩形过渡到绳上的落点 */
  const [flight, setFlight] = useState<{ from: Rect; to: Rect } | null>(null);
  const flightResolve = useRef<(() => void) | null>(null);
  const flightElement = useRef<HTMLDivElement>(null);
  /** 飞行期间的整屏压暗层：和冻屏画面一起淡出，别等窗户收起时才消失 */
  const flightMask = useRef<HTMLDivElement>(null);
  const imgRef = useRef<HTMLImageElement | null>(null);
  useLayoutEffect(() => {
    if (!flight || !flightElement.current) return;
    const animation = animateFlight(flightElement.current, flight.from, flight.to);
    void animation.finished.then(
      () => flightResolve.current?.(),
      () => {},
    );
    // 两件事同起同落：图飞向绳子，同时「冻屏画面 + 压暗」整层化开。
    // 落点到位时屏幕已经透明，之后隐藏遮罩窗完全没有视觉跳变。
    const curtain = ([imgRef.current, flightMask.current].filter(Boolean) as HTMLElement[]).map(
      (el) => animateCurtain(el),
    );
    return () => {
      animation.cancel();
      // 飞行中途取消（失败 / 下一张）时把幕布恢复出来，否则遮罩会变成半透明空壳
      curtain.forEach((c) => c.cancel());
    };
  }, [flight]);
  /** 选区顶边进入顶部带时把晾衣架举出来：只做一次，避免每次 mousemove 都发 IPC */
  const dropArmed = useRef(false);

  const canvasRef = useRef<HTMLCanvasElement | null>(null);

  // 物理像素 / CSS 像素 比（遮罩窗铺满显示器，即显示器缩放系数）
  const scale = frame?.scale ?? 1;
  // 以冻屏帧推导逻辑视口，不依赖 window.innerWidth（跨 DPI 屏时窗口几何可能短暂失准）
  const cssW = frame ? frame.width / scale : window.innerWidth;
  const cssH = frame ? frame.height / scale : window.innerHeight;

  const reset = useCallback(() => {
    setSel(null);
    setDrag(null);
    setOcr(null);
    setElem(null);
  }, []);

  /** 遮罩收起后延迟释放冻屏大位图（4K 约 33MB），避免隐藏前闪黑与常驻内存。 */
  const releaseTimer = useRef<number | null>(null);
  /** 二次收起：第一次 hide 可能静默失败，隔一会儿再补一刀 */
  const rehideTimer = useRef<number | null>(null);
  /** 当前是否有可用的冻屏帧（判断二次收起时是不是已经开了新一轮截图） */
  const frameLive = useRef(false);
  /** 当前帧号：载荷与收尾都以它为准，跨轮次的操作一律作废 */
  const frameIdRef = useRef(0);
  const releaseFrame = useCallback(() => {
    // 遮罩要收了：解除「晾衣架投放提示」的钉住状态，否则绳子会一直点不透
    dropArmed.current = false;
    lineDropHint(false).catch(() => {});
    frameLive.current = false;
    if (releaseTimer.current) window.clearTimeout(releaseTimer.current);
    if (rehideTimer.current) window.clearTimeout(rehideTimer.current);
    releaseTimer.current = window.setTimeout(() => {
      setFrame(null);
      setFlight(null);
      const c = canvasRef.current;
      if (c) {
        c.width = 0;
        c.height = 0;
      }
    }, 300);
    // 兜底收起：`hide()` 是从 IPC 回调派发到主线程执行的，主线程正在建 WebView /
    // 弹原生对话框时会超时返回 Err，而我们只看到一句被忽略的日志——窗口其实还留在
    // 屏幕上。过了这一刻前端已经没有任何输入入口，所以这里必须再补一次。
    // 用 overlayRelease 而不是 cancelCapture：带帧号校验，新一轮截图已经接管时
    // 是空操作，不会把用户刚唤起的那一次误收掉。
    const releasedFrame = frameIdRef.current;
    rehideTimer.current = window.setTimeout(() => {
      rehideTimer.current = null;
      if (!frameLive.current) overlayRelease(releasedFrame).catch(() => {});
    }, 1200);
  }, []);

  const toast = useCallback((text: string) => {
    const id = ++toastSeq;
    setToasts((t) => [...t, { id, text }]);
    setTimeout(() => setToasts((t) => t.filter((x) => x.id !== id)), 1800);
  }, []);

  /** 提交动作的兜底计时：到点无条件把遮罩放开（可能已经出图，只是回执丢了） */
  const commitTimer = useRef<number | null>(null);
  const slowTimer = useRef<number | null>(null);
  const commitStart = useRef(0);

  const clearCommitWatchdog = useCallback(() => {
    if (commitTimer.current) window.clearTimeout(commitTimer.current);
    if (slowTimer.current) window.clearTimeout(slowTimer.current);
    commitTimer.current = null;
    slowTimer.current = null;
    setSlowCommit(false);
  }, []);

  /**
   * 强制收尾：不看后端回执，直接把本地状态放掉并收起遮罩。
   * 用于「等太久了」与 committing 期间按 Esc——此时截图多半已经落盘 / 进剪贴板，
   * 再把它扣在屏幕上是更坏的结局。
   */
  const busyRef = useRef(false);
  const forceClose = useCallback(
    (message?: string) => {
      busyRef.current = false;
      clearCommitWatchdog();
      setCommitting(false);
      setSaving(false);
      reset();
      releaseFrame();
      if (message) toast(message);
    },
    [clearCommitWatchdog, reset, releaseFrame, toast],
  );

  const armCommitWatchdog = useCallback(() => {
    clearCommitWatchdog();
    commitStart.current = performance.now();
    slowTimer.current = window.setTimeout(() => setSlowCommit(true), SLOW_COMMIT_MS);
    commitTimer.current = window.setTimeout(() => {
      commitTimer.current = null;
      forceClose("这一步响应超时，已自动收起截图");
    }, COMMIT_TIMEOUT_MS);
  }, [clearCommitWatchdog, forceClose]);

  /** 收遮罩但不让 UI 陪着一起等：IPC 卡住时最多等 HIDE_WAIT_MS */
  const hideOverlayNow = useCallback(async () => {
    await Promise.race([
      cancelCapture().catch(() => {}),
      new Promise<void>((resolve) => window.setTimeout(resolve, HIDE_WAIT_MS)),
    ]);
  }, []);

  const exitCapture = useCallback(() => {
    clearCommitWatchdog();
    reset();
    releaseFrame();
    cancelCapture();
  }, [clearCommitWatchdog, reset, releaseFrame]);

  // 遮罩打开：接收冻屏元数据，图像经 shot:// 协议加载。
  // 事件 + 挂载时主动拉取双通道（防 emit 早于监听器注册），按帧号去重。
  const applyPayload = useCallback(
    (p: OverlayPayload) => {
      // 只接受发给本窗口的载荷（多屏防串扰），并按帧号去重：
      // 帧号单调递增，迟到的旧载荷不能把新一帧顶掉。
      if (p.label && p.label !== getCurrentWindow().label) return;
      if (p.frameId <= frameIdRef.current) return;
      frameIdRef.current = p.frameId;
      if (releaseTimer.current) window.clearTimeout(releaseTimer.current);
      if (rehideTimer.current) window.clearTimeout(rehideTimer.current);
      frameLive.current = true;
      // 新一轮截图：上一次提交可能还留着飞行层 / 兜底计时，全部作废
      clearCommitWatchdog();
      busyRef.current = false;
      setCommitting(false);
      setSaving(false);
      setFlight(null);
      reset();
      setCursor(null);
      setFrame(p);
      if (p.preselect) {
        const r = p.preselect;
        setSel({ x: r.x / p.scale, y: r.y / p.scale, w: r.w / p.scale, h: r.h / p.scale });
      }
    },
    [clearCommitWatchdog, reset],
  );

  useEffect(() => {
    // 必须用窗口作用域监听：全局 listen 会收到发往其他遮罩窗的帧载荷
    const un = getCurrentWindow().listen<OverlayPayload>("overlay:open", (e) =>
      applyPayload(e.payload),
    );
    overlayFetch()
      .then((p) => p && applyPayload(p))
      .catch(() => {});
    return () => {
      un.then((f) => f());
    };
  }, [applyPayload]);

  // 其他屏的遮罩开始交互：清空本屏选区（保留冻屏帧）
  useEffect(() => {
    const un = getCurrentWindow().listen("overlay:standby", () => reset());
    return () => {
      un.then((f) => f());
    };
  }, [reset]);

  // 冻屏图加载完成：画进取色 canvas，并通知后端显示遮罩窗（防旧帧闪烁）
  const onFrameLoad = () => {
    if (!frame) return;
    const img = imgRef.current;
    const c = canvasRef.current;
    if (img && c) {
      c.width = frame.width;
      c.height = frame.height;
      c.getContext("2d", { willReadFrequently: true })?.drawImage(
        img,
        0,
        0,
        frame.width,
        frame.height,
      );
    }
    overlayReady(frame.frameId).catch(() => {});
  };

  // 冻屏图拉取失败（帧已被新一轮截图替换、协议响应异常）：不能让一块看不见的遮罩挂着
  const onFrameError = () => {
    if (!frame) return;
    // 只在还是当前帧时放弃；旧帧的 onError 属于正常替换
    if (frameIdRef.current !== frame.frameId) return;
    frameIdRef.current = 0;
    exitCapture();
  };

  const sampleColor = (cssX: number, cssY: number) => {
    const c = canvasRef.current;
    if (!c || !frame) return;
    const px = clamp(Math.round(cssX * scale), 0, frame.width - 1);
    const py = clamp(Math.round(cssY * scale), 0, frame.height - 1);
    try {
      const d = c.getContext("2d", { willReadFrequently: true })?.getImageData(px, py, 1, 1).data;
      if (!d) return;
      const toHexByte = (n: number) => n.toString(16).padStart(2, "0");
      setHex(`#${toHexByte(d[0])}${toHexByte(d[1])}${toHexByte(d[2])}`.toUpperCase());
      setRgb([d[0], d[1], d[2]]);
    } catch {
      // canvas 被污染时忽略取色
    }
  };

  // ===================== 窗口吸附 =====================

  const cssWindows: (SnapWindow & { cx: number; cy: number; cw: number; ch: number })[] = (
    frame?.windows ?? []
  ).map((w) => ({
    ...w,
    cx: w.x / scale,
    cy: w.y / scale,
    cw: w.w / scale,
    ch: w.h / scale,
  }));

  const idle = !sel && !drag;
  const snapActive = !sel && (!drag || drag.kind === "pending");
  const hoverWin =
    snapActive && cursor
      ? (cssWindows.find(
          (w) =>
            cursor.x >= w.cx &&
            cursor.x <= w.cx + w.cw &&
            cursor.y >= w.cy &&
            cursor.y <= w.cy + w.ch,
        ) ?? null)
      : null;

  // 元素吸附：Tab 切换后轮询光标下 UI 元素（物理坐标 → CSS）。
  // 轮询开始前先清掉旧命中，避免切回时闪一帧过期的框。
  const polling = elemMode && snapActive && !!frame;
  useEffect(() => {
    if (!polling) return;
    let live = true;
    let busy = false;
    const timer = window.setInterval(async () => {
      if (busy) return;
      busy = true;
      const hit = await elementAt().catch(() => null);
      busy = false;
      if (live) setElem(hit);
    }, 90);
    return () => {
      live = false;
      window.clearInterval(timer);
    };
  }, [polling]);

  // 统一的吸附高亮目标（元素优先）
  const snapRect =
    polling && elem
      ? {
          cx: elem.x / scale,
          cy: elem.y / scale,
          cw: elem.w / scale,
          ch: elem.h / scale,
          label: elem.name || "元素",
          pw: elem.w,
          ph: elem.h,
        }
      : hoverWin
        ? {
            cx: hoverWin.cx,
            cy: hoverWin.cy,
            cw: hoverWin.cw,
            ch: hoverWin.ch,
            label: hoverWin.title === "Program Manager" ? "桌面" : hoverWin.title || "窗口",
            pw: hoverWin.w,
            ph: hoverWin.h,
          }
        : null;

  const clampRectToView = (r: Rect): Rect => {
    const x = clamp(r.x, 0, cssW);
    const y = clamp(r.y, 0, cssH);
    return { x, y, w: clamp(r.w - (x - r.x), 0, cssW - x), h: clamp(r.h - (y - r.y), 0, cssH - y) };
  };

  const selectSnap = () => {
    if (!snapRect) return;
    setSel(clampRectToView({ x: snapRect.cx, y: snapRect.cy, w: snapRect.cw, h: snapRect.ch }));
  };

  // ===================== 选区拖拽 =====================

  const onRootMouseDown = (e: React.MouseEvent) => {
    if (e.button !== 0 || committing || saving) return;
    overlayClaim();
    setDrag({ kind: "pending", sx: e.clientX, sy: e.clientY });
  };

  const onSelMouseDown = (e: React.MouseEvent) => {
    if (e.button !== 0 || !sel || committing || saving) return;
    e.stopPropagation();
    setDrag({ kind: "move", sx: e.clientX, sy: e.clientY, orig: sel });
  };

  const startResize = (handle: Handle) => (e: React.MouseEvent) => {
    if (e.button !== 0 || !sel || committing || saving) return;
    e.stopPropagation();
    setDrag({ kind: "resize", handle, orig: sel });
  };

  const onMouseMove = (e: React.MouseEvent) => {
    const x = e.clientX;
    const y = e.clientY;
    setCursor({ x, y });
    sampleColor(x, y);
    if (!drag) return;

    // 鼠标已在窗外释放（如拖到另一显示器）：结束拖拽，避免"粘手"
    if (e.buttons === 0) {
      onMouseUp();
      return;
    }

    if (drag.kind === "pending") {
      if (Math.abs(x - drag.sx) > CLICK_SLOP || Math.abs(y - drag.sy) > CLICK_SLOP) {
        setDrag({ kind: "draw", sx: drag.sx, sy: drag.sy });
        setSel({
          x: Math.min(drag.sx, x),
          y: Math.min(drag.sy, y),
          w: Math.abs(x - drag.sx),
          h: Math.abs(y - drag.sy),
        });
      }
      return;
    }

    if (drag.kind === "draw") {
      // Shift = 正方形约束
      let w = Math.abs(x - drag.sx);
      let h = Math.abs(y - drag.sy);
      if (e.shiftKey) {
        const side = Math.max(w, h);
        w = side;
        h = side;
      }
      setSel({
        x: x < drag.sx ? drag.sx - w : drag.sx,
        y: y < drag.sy ? drag.sy - h : drag.sy,
        w,
        h,
      });
    } else if (drag.kind === "move") {
      setSel({
        ...drag.orig,
        x: clamp(drag.orig.x + (x - drag.sx), 0, cssW - drag.orig.w),
        y: clamp(drag.orig.y + (y - drag.sy), 0, cssH - drag.orig.h),
      });
    } else {
      // resize：对侧边固定，允许翻越
      const o = drag.orig;
      const h = drag.handle;
      let x0 = o.x;
      let x1 = o.x + o.w;
      let y0 = o.y;
      let y1 = o.y + o.h;
      if (h.includes("w")) x0 = x;
      if (h.includes("e")) x1 = x;
      if (h.includes("n")) y0 = y;
      if (h.includes("s")) y1 = y;
      // Shift + 角点手柄 = 以对角为锚的正方形
      if (e.shiftKey && h.length === 2) {
        const fx = h.includes("w") ? o.x + o.w : o.x;
        const fy = h.includes("n") ? o.y + o.h : o.y;
        const mx = h.includes("w") ? x0 : x1;
        const my = h.includes("n") ? y0 : y1;
        const side = Math.max(Math.abs(mx - fx), Math.abs(my - fy));
        const nmx = fx + (mx < fx ? -side : side);
        const nmy = fy + (my < fy ? -side : side);
        x0 = Math.min(fx, nmx);
        x1 = Math.max(fx, nmx);
        y0 = Math.min(fy, nmy);
        y1 = Math.max(fy, nmy);
      }
      setSel({
        x: Math.min(x0, x1),
        y: Math.min(y0, y1),
        w: Math.abs(x1 - x0),
        h: Math.abs(y1 - y0),
      });
    }
  };

  const onMouseUp = () => {
    if (!drag) return;
    if (drag.kind === "pending") {
      // 无位移的单击：吸附选中悬停窗口 / 元素
      if (!sel) selectSnap();
      setDrag(null);
      return;
    }
    // 拖到屏幕顶端松手 = 挂到晾衣架上，走的是和点对勾同一条链路（飞行 → 落地）
    if (sel && sel.y <= DROP_BAND && sel.w >= MIN_SIZE && sel.h >= MIN_SIZE) {
      setDrag(null);
      void doCopyToLine();
      return;
    }
    if (drag.kind === "draw" && sel && (sel.w < MIN_SIZE || sel.h < MIN_SIZE)) {
      setSel(null);
    }
    setDrag(null);
  };

  const onContextMenu = (e: React.MouseEvent) => {
    e.preventDefault();
    if (ocr || committing || saving) return;
    if (sel) {
      setSel(null);
      setDrag(null);
    } else {
      exitCapture();
    }
  };

  // ===================== 输出动作 =====================

  const toPhysical = (r: Rect) => {
    if (!frame) return { x: 0, y: 0, w: 0, h: 0 };
    const x = clamp(Math.round(r.x * scale), 0, frame.width - 1);
    const y = clamp(Math.round(r.y * scale), 0, frame.height - 1);
    return {
      x,
      y,
      w: clamp(Math.round(r.w * scale), 1, frame.width - x),
      h: clamp(Math.round(r.h * scale), 1, frame.height - y),
    };
  };

  /**
   * 带选区执行一次输出动作。进行中保留选区画面（committing），成功返回时 Rust 侧
   * 已隐藏遮罩，这里再重置并释放冻屏帧；失败则保留选区并提示——否则前端已清空、
   * 窗口却仍置顶可见，用户面对的是一块看不见但吃掉所有点击的全屏透明窗。
   *
   */
  const withSel = (fn: (p: { x: number; y: number; w: number; h: number }) => Promise<unknown>) => {
    if (busyRef.current || !sel || sel.w < MIN_SIZE || sel.h < MIN_SIZE) return;
    const p = toPhysical(sel);
    // 回执可能在新一轮截图之后才回来（超时兜底 / 主线程排队）：只认自己那一帧，
    // 否则会把用户刚框好的新选区一起 reset 掉。
    const frameId = frameIdRef.current;
    busyRef.current = true;
    setDrag(null);
    setCommitting(true);
    armCommitWatchdog();
    fn(p)
      .then(() => {
        if (frameIdRef.current !== frameId) return;
        reset();
        releaseFrame();
      })
      .catch((e) => {
        if (frameIdRef.current === frameId) toast(`操作失败：${String(e)}`);
      })
      .finally(() => {
        if (frameIdRef.current !== frameId) return;
        busyRef.current = false;
        setCommitting(false);
        clearCommitWatchdog();
      });
  };

  const doCopy = () => withSel((p) => copyRegion(p.x, p.y, p.w, p.h));

  const doCopyToLine = async () => {
    if (busyRef.current || !sel || sel.w < MIN_SIZE || sel.h < MIN_SIZE || !frame) return;
    const p = toPhysical(sel);
    const from: Rect = { ...sel };
    const frameId = frame.frameId;
    busyRef.current = true;
    setDrag(null);
    setCommitting(true);
    armCommitWatchdog();
    let startedId = "";
    let saved = false;
    let flightDone = Promise.resolve();
    let flightTimer: number | undefined;
    let landingTimer: number | undefined;
    let unlisten: (() => void) | undefined;
    const startFlight = (landing: LineLanding) => {
      if (startedId || frameIdRef.current !== frameId) return;
      startedId = landing.id;
      flightDone = new Promise<void>((resolve) => {
        const finish = () => {
          window.clearTimeout(flightTimer);
          flightResolve.current = null;
          resolve();
        };
        flightResolve.current = finish;
        flightTimer = window.setTimeout(finish, FLIGHT_MS + 500);
      });
      setFlight({ from, to: { x: landing.x, y: landing.y, w: landing.w, h: landing.h } });
    };
    try {
      unlisten = await getCurrentWindow().listen<LineLanding>("line:flight", (e) =>
        startFlight(e.payload),
      );
      const landing = await copyToLine(p.x, p.y, p.w, p.h);
      saved = true;
      if (!landing || frameIdRef.current !== frameId) return;
      startFlight(landing);
      await flightDone;
      // 后端 `land` 内部有 save / 主线程 / 绘制三段等待，最坏可能超过这里的上限；
      // 超时后它仍会自己把那一条从 pending 摘掉，所以放弃等待是安全的。
      // 先挂 catch：race 输掉之后它再 reject 就是一条没人处理的异常。
      const landingPromise = lineLanded(landing.id);
      landingPromise.catch(() => {});
      await Promise.race([
        landingPromise,
        new Promise<never>((_, reject) => {
          landingTimer = window.setTimeout(
            () => reject(new Error("晾衣架显示超时")),
            LAND_SETTLE_MS,
          );
        }),
      ]);
    } catch (e) {
      if (frameIdRef.current === frameId) toast(`操作失败：${String(e)}`);
    } finally {
      unlisten?.();
      window.clearTimeout(flightTimer);
      window.clearTimeout(landingTimer);
      flightResolve.current?.();
      if (frameIdRef.current === frameId) {
        if (saved) {
          // 不能无限等：hide 的 IPC 一旦排不上主线程，这里就会把 committing
          // 永远扣在屏幕上（Esc 也被拦着），只剩强杀一条路。
          await hideOverlayNow();
          reset();
          releaseFrame();
        }
        setFlight(null);
        busyRef.current = false;
        setCommitting(false);
        clearCommitWatchdog();
      }
    }
  };

  const doAnnotate = () => withSel((p) => openEditor(p.x, p.y, p.w, p.h));
  const doPin = () => withSel((p) => pinRegion(p.x, p.y, p.w, p.h));

  /**
   * 保存：整个原生对话框期间，遮罩、选区框线、工具条全部原地不动——
   * 用户点「保存」不该看到截图界面先被拆掉。存盘成功才收起；取消/失败则
   * 回到选区，可以换块区域或再点一次保存。
   *
   * `saving` 期间必须冻结键盘：原生对话框是另一线程上的窗口，Esc 会同时
   * 漏到本窗口（WebView2 收键），一次 Esc 会把对话框和整个截图流程一起关掉。
   */
  const doSave = async () => {
    if (busyRef.current || !sel || sel.w < MIN_SIZE || sel.h < MIN_SIZE) return;
    const p = toPhysical(sel);
    const frameId = frameIdRef.current;
    busyRef.current = true;
    setDrag(null);
    setSaving(true);
    try {
      const saved = await saveRegion(p.x, p.y, p.w, p.h);
      if (saved && frameIdRef.current === frameId) {
        reset();
        releaseFrame();
      }
    } catch (e) {
      if (frameIdRef.current === frameId) toast(`保存失败：${String(e)}`);
    } finally {
      busyRef.current = false;
      setSaving(false);
    }
  };

  const doScroll = async (horizontal = false) => {
    if (!sel || sel.w < MIN_SIZE || sel.h < MIN_SIZE) return;
    const p = toPhysical(sel);
    try {
      // 成功后 Rust 侧接管（隐藏遮罩、弹控制条），失败则保留选区提示原因
      await scrollCaptureStart(p.x, p.y, p.w, p.h, horizontal);
      reset();
      releaseFrame();
    } catch (e) {
      toast(String(e));
    }
  };

  const doGif = async () => {
    if (!sel || sel.w < MIN_SIZE || sel.h < MIN_SIZE) return;
    const p = toPhysical(sel);
    try {
      // 成功后 Rust 侧接管（隐藏遮罩、弹录制控制条），失败保留选区提示原因
      await gifRecordStart(p.x, p.y, p.w, p.h);
      reset();
      releaseFrame();
    } catch (e) {
      toast(String(e));
    }
  };

  const doOcr = async () => {
    if (!sel) return;
    const p = toPhysical(sel);
    setOcr({ loading: true, text: "" });
    // 文字与二维码并行识别；任一成功即出结果
    const [textRes, qrRes] = await Promise.allSettled([
      ocrRegion(p.x, p.y, p.w, p.h),
      qrRegion(p.x, p.y, p.w, p.h),
    ]);
    const text = textRes.status === "fulfilled" ? textRes.value : "";
    const qr = qrRes.status === "fulfilled" ? qrRes.value : [];
    if (textRes.status === "rejected" && qr.length === 0) {
      setOcr({ loading: false, text: "", error: String(textRes.reason) });
    } else {
      setOcr({ loading: false, text, qr });
    }
  };

  const closeOcr = () => {
    exitCapture();
  };

  // ===================== 键盘 =====================

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      // 保存对话框开着：全部按键交给对话框自己处理。原生对话框在另一个线程上，
      // Esc 会同时漏进本窗口——这里必须先拦住，否则一次 Esc 会把对话框和整个
      // 截图流程一起关掉，用户刚框好的选区就白框了。
      if (saving) return;

      if (ocr) {
        if (e.key === "Escape") closeOcr();
        return;
      }

      // 输出动作进行中：选区画面必须保持原样，屏蔽其余按键。
      // 唯一例外是「真的卡住了」——超过宽限时间还按 Esc，必须放用户走，
      // 否则这块全屏置顶窗只能靠托盘强杀才能收掉。
      if (committing) {
        if (e.key === "Escape" && performance.now() - commitStart.current > ESC_GRACE_MS) {
          forceClose("已强制收起截图");
        }
        return;
      }

      if (e.key === "Escape") {
        exitCapture();
        return;
      }
      if (e.key === "Tab") {
        e.preventDefault();
        if (!sel) {
          const next = !elemMode;
          setElemMode(next);
          setElem(null);
          toast(next ? "已切换：元素吸附" : "已切换：窗口吸附");
        }
        return;
      }
      if (e.key === "Enter" && sel) {
        e.preventDefault();
        doCopy();
        return;
      }
      if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === "a") {
        e.preventDefault();
        overlayClaim();
        setSel({ x: 0, y: 0, w: cssW, h: cssH });
        setDrag(null);
        return;
      }
      if ((e.ctrlKey || e.metaKey) && sel) {
        const k = e.key.toLowerCase();
        if (k === "c") return void (e.preventDefault(), doCopy());
        if (k === "s") return void (e.preventDefault(), doSave());
        if (k === "t") return void (e.preventDefault(), doPin());
        if (k === "e") return void (e.preventDefault(), doAnnotate());
      }
      if (!e.ctrlKey && !e.metaKey && !e.altKey && e.key.toLowerCase() === "c" && !sel) {
        const value = e.shiftKey ? `rgb(${rgb[0]}, ${rgb[1]}, ${rgb[2]})` : hex;
        copyText(value);
        toast(`已复制颜色 ${value}`);
        return;
      }

      // 方向键：移动选区；Ctrl+方向键：调整大小；Shift 步进 ×10
      if (sel && e.key.startsWith("Arrow")) {
        e.preventDefault();
        const step = (e.shiftKey ? 10 : 1) / scale;
        const dx = e.key === "ArrowLeft" ? -step : e.key === "ArrowRight" ? step : 0;
        const dy = e.key === "ArrowUp" ? -step : e.key === "ArrowDown" ? step : 0;
        setSel((r) => {
          if (!r) return r;
          if (e.ctrlKey || e.metaKey) {
            return {
              ...r,
              w: clamp(r.w + dx, MIN_SIZE, cssW - r.x),
              h: clamp(r.h + dy, MIN_SIZE, cssH - r.y),
            };
          }
          return {
            ...r,
            x: clamp(r.x + dx, 0, cssW - r.w),
            y: clamp(r.y + dy, 0, cssH - r.h),
          };
        });
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [sel, ocr, hex, rgb, frame, scale, elemMode, committing, saving, forceClose]);

  // ===================== 派生渲染量 =====================

  const hasSelection = !!sel && sel.w >= MIN_SIZE && sel.h >= MIN_SIZE;
  const adjusting = drag?.kind === "draw" || drag?.kind === "resize";
  const showMagnifier = !!frame && !!cursor && (idle || adjusting);
  const showToolbar = hasSelection && !drag && !ocr && !committing;
  // 进行中只留冻屏画面 + 选区外压暗，边框 / 手柄 / 标签全部隐去
  const showSelChrome = !committing;

  /**
   * 选区顶边贴近屏幕顶端 → 把晾衣架举到遮罩之上。
   *
   * 只动 z 序与显隐、不动几何，所以正在进行的拖选一帧都不会掉。晾衣架本来就在
   * 冻屏画面里（截图时它是可见的），举起来的那一份和底图完全重合，看不出切换。
   */
  useEffect(() => {
    if (!drag || !sel || sel.y > DROP_BAND || dropArmed.current) return;
    dropArmed.current = true;
    lineDropHint(true).catch(() => {});
  }, [drag, sel]);

  const physSel = sel ? toPhysical(sel) : null;

  const handlePos: Record<Handle, React.CSSProperties> = sel
    ? {
        nw: { left: -7, top: -7 },
        n: { left: sel.w / 2 - 7, top: -7 },
        ne: { left: sel.w - 7, top: -7 },
        e: { left: sel.w - 7, top: sel.h / 2 - 7 },
        se: { left: sel.w - 7, top: sel.h - 7 },
        s: { left: sel.w / 2 - 7, top: sel.h - 7 },
        sw: { left: -7, top: sel.h - 7 },
        w: { left: -7, top: sel.h / 2 - 7 },
      }
    : ({} as Record<Handle, React.CSSProperties>);

  return (
    <div
      className="relative h-screen w-screen cursor-crosshair overflow-hidden select-none"
      style={committing ? { cursor: "default" } : undefined}
      onMouseDown={onRootMouseDown}
      onMouseMove={onMouseMove}
      onMouseUp={onMouseUp}
      onContextMenu={onContextMenu}
      onMouseEnter={() => {
        // 多屏：鼠标移入本屏遮罩时接管键盘焦点
        getCurrentWindow()
          .setFocus()
          .catch(() => {});
      }}
    >
      <canvas ref={canvasRef} className="hidden" />

      {/* 冻屏背景图（shot:// 零压缩 BMP） */}
      {frame && (
        <img
          ref={imgRef}
          src={frame.frameUrl}
          crossOrigin="anonymous"
          onLoad={onFrameLoad}
          onError={onFrameError}
          alt=""
          draggable={false}
          className="pointer-events-none absolute inset-0 h-full w-full"
        />
      )}

      {/* 未选择：压暗 + 窗口吸附高亮 / 提示 */}
      {!sel && (
        <>
          {snapRect ? (
            <div
              className="pointer-events-none absolute border border-white/85"
              style={{
                left: snapRect.cx,
                top: snapRect.cy,
                width: snapRect.cw,
                height: snapRect.ch,
                boxShadow: "0 0 0 1px rgba(0,0,0,0.35), 0 0 0 100vmax rgba(0,0,0,0.45)",
              }}
            >
              <div
                className="absolute flex max-w-[60vw] items-center gap-1.5 rounded-[5px] bg-white/92 px-1.5 py-[3px] text-[10.5px] font-medium text-black/80 shadow-soft backdrop-blur-sm"
                style={{
                  left: Math.max(4, -Math.min(0, snapRect.cx) + 4),
                  top: snapRect.cy < 30 ? 6 : -26,
                }}
              >
                {elemMode && (
                  <span className="shrink-0 rounded-[3px] bg-black/8 px-1 text-[9.5px]">元素</span>
                )}
                <span className="truncate">{snapRect.label}</span>
                <span className="shrink-0 tabular-nums opacity-55">
                  {snapRect.pw} × {snapRect.ph}
                </span>
              </div>
            </div>
          ) : (
            <div className="pointer-events-none absolute inset-0 bg-black/45" />
          )}

          {/* 十字辅助线 */}
          {cursor && !drag && (
            <>
              <div
                className="pointer-events-none absolute inset-x-0 h-px bg-white/25"
                style={{ top: cursor.y }}
              />
              <div
                className="pointer-events-none absolute inset-y-0 w-px bg-white/25"
                style={{ left: cursor.x }}
              />
            </>
          )}

          {idle && !snapRect && (
            <div className="pointer-events-none absolute inset-x-0 top-[12%] grid place-items-center">
              <motion.div
                initial={{ opacity: 0, y: 8 }}
                animate={{ opacity: 1, y: 0 }}
                transition={{ duration: 0.25 }}
                className="glass flex items-center gap-3 rounded-xl px-5 py-3 text-sm text-ink-100 shadow-float"
              >
                <span className="text-ink-200">拖拽选区 · 单击吸附窗口</span>
                <span className="text-ink-500">·</span>
                <span className="text-ink-400">
                  Tab 切换{elemMode ? "窗口" : "元素"}吸附 · Shift 正方形 · Enter 复制 · Esc 取消
                </span>
              </motion.div>
            </div>
          )}
        </>
      )}

      {/* 飞向晾衣绳：整块选区画面从原位等比缩到绳上的落点。
          期间改用整屏压暗 + 飞行层替代「选区中间挖洞」的画法，否则图飞走后
          原地会留一块亮着的空洞。 */}
      {flight && (
        <div ref={flightMask} className="pointer-events-none absolute inset-0 bg-black/45" />
      )}
      {flight && frame && (
        <div
          ref={flightElement}
          className="pointer-events-none absolute origin-top-left overflow-hidden"
          style={{
            left: flight.from.x,
            top: flight.from.y,
            width: flight.from.w,
            height: flight.from.h,
            backgroundImage: `url(${frame.frameUrl})`,
            backgroundSize: `${cssW}px ${cssH}px`,
            backgroundPosition: `${-flight.from.x}px ${-flight.from.y}px`,
            backgroundRepeat: "no-repeat",
            willChange: "transform",
            transformOrigin: "top left",
          }}
        />
      )}

      {/* 提交久等不回：给一个「它还活着」的信号。正常路径几百毫秒就结束，
          不会闪出这个胶囊；只有真的慢（后端写盘 / 主线程忙）才看得到。 */}
      {committing && slowCommit && !flight && (
        <div className="pointer-events-none absolute inset-0 z-30 grid place-items-center">
          <div className="glass flex items-center gap-2 rounded-full px-3.5 py-1.5 text-[12px] text-ink-100 shadow-pop">
            <span className="h-3 w-3 animate-spin rounded-full border-2 border-white/25 border-t-white/85" />
            处理中…
          </div>
        </div>
      )}

      {/* 拖到屏幕顶端：告诉用户「松手就挂上去了」 */}
      {drag && sel && sel.y <= DROP_BAND && (
        <div
          className="pointer-events-none absolute inset-x-0 flex justify-center"
          style={{ top: 12 }}
        >
          <div
            className="rounded-full bg-white/92 px-3 py-1 text-[11.5px] font-medium text-black/80 shadow-soft"
            style={{ animation: "line-hint 160ms cubic-bezier(.22,1,.36,1) both" }}
          >
            松手挂到晾衣架
          </div>
        </div>
      )}

      {/* 选区 */}
      {sel && !flight && (
        <div
          className={showSelChrome ? "absolute border border-white/90" : "absolute"}
          style={{
            left: sel.x,
            top: sel.y,
            width: sel.w,
            height: sel.h,
            boxShadow: "0 0 0 1px rgba(0,0,0,0.32), 0 0 0 100vmax rgba(0,0,0,0.45)",
            cursor: committing ? "default" : drag?.kind === "move" ? "grabbing" : "move",
          }}
          onMouseDown={onSelMouseDown}
          onDoubleClick={doCopy}
        >
          {/* 尺寸 / 坐标标签 */}
          {physSel && showSelChrome && (
            <div
              className="pointer-events-none absolute left-0 flex items-center gap-1.5 whitespace-nowrap rounded-[5px] bg-white/92 px-1.5 py-[3px] text-[10.5px] font-medium text-black/80 shadow-soft backdrop-blur-sm"
              style={{ top: sel.y < 30 ? 4 : -26 }}
            >
              <span className="tabular-nums">
                {physSel.w} × {physSel.h}
              </span>
              <span className="tabular-nums opacity-50">
                ({physSel.x}, {physSel.y})
              </span>
            </div>
          )}

          {/* 边缘拖拽条（不可见，扩大热区） */}
          {hasSelection && !drag && showSelChrome && (
            <>
              <div
                className="absolute -top-1 left-2 right-2 h-2 cursor-ns-resize"
                onMouseDown={startResize("n")}
              />
              <div
                className="absolute -bottom-1 left-2 right-2 h-2 cursor-ns-resize"
                onMouseDown={startResize("s")}
              />
              <div
                className="absolute -left-1 top-2 bottom-2 w-2 cursor-ew-resize"
                onMouseDown={startResize("w")}
              />
              <div
                className="absolute -right-1 top-2 bottom-2 w-2 cursor-ew-resize"
                onMouseDown={startResize("e")}
              />
            </>
          )}

          {/* 8 个缩放手柄 */}
          {hasSelection &&
            showSelChrome &&
            (Object.keys(handlePos) as Handle[]).map((h) => (
              <span
                key={h}
                className="absolute z-10 grid h-3.5 w-3.5 place-items-center"
                style={{ ...handlePos[h], cursor: HANDLE_CURSOR[h], width: 14, height: 14 }}
                onMouseDown={startResize(h)}
              >
                <span className="h-2 w-2 rounded-full border border-black/25 bg-white shadow-soft" />
              </span>
            ))}
        </div>
      )}

      {/* 放大镜 + 像素坐标 + 取色 */}
      {showMagnifier && cursor && frame && (
        <div
          className="pointer-events-none absolute z-20"
          style={{
            left: cursor.x + MAG_SIZE + 28 > cssW ? cursor.x - MAG_SIZE - 18 : cursor.x + 18,
            top: cursor.y + MAG_SIZE + 52 > cssH ? cursor.y - MAG_SIZE - 52 : cursor.y + 18,
          }}
        >
          <div
            className="relative overflow-hidden rounded-lg border border-white/15 shadow-pop"
            style={{ width: MAG_SIZE, height: MAG_SIZE }}
          >
            <div
              className="absolute inset-0 bg-ink-950"
              style={{
                backgroundImage: `url(${frame.frameUrl})`,
                backgroundRepeat: "no-repeat",
                backgroundSize: `${(frame.width / scale) * MAG_ZOOM}px ${(frame.height / scale) * MAG_ZOOM}px`,
                backgroundPosition: `${-(cursor.x * MAG_ZOOM - MAG_SIZE / 2)}px ${-(cursor.y * MAG_ZOOM - MAG_SIZE / 2)}px`,
                imageRendering: "pixelated",
              }}
            />
            <div className="absolute inset-x-0 top-1/2 h-px -translate-y-1/2 bg-white/70" />
            <div className="absolute inset-y-0 left-1/2 w-px -translate-x-1/2 bg-white/70" />
          </div>
          <div className="glass mt-1.5 flex items-center gap-2 rounded-md px-2 py-1 text-[11px] text-ink-100">
            <span
              className="h-3 w-3 rounded-sm border border-white/20"
              style={{ background: hex }}
            />
            <span className="tabular-nums">{hex}</span>
            <span className="text-ink-500">·</span>
            <span className="tabular-nums text-ink-300">
              {Math.round(cursor.x * scale)}, {Math.round(cursor.y * scale)}
            </span>
          </div>
          {idle && (
            <div className="mt-1 w-fit rounded bg-black/55 px-1.5 py-0.5 text-[10px] text-ink-300">
              C 复制 HEX · Shift+C 复制 RGB
            </div>
          )}
        </div>
      )}

      {/* OCR 结果面板 */}
      <AnimatePresence>
        {ocr && (
          <motion.div
            initial={{ opacity: 0 }}
            animate={{ opacity: 1 }}
            exit={{ opacity: 0 }}
            className="absolute inset-0 z-40 grid place-items-center bg-black/40"
            onMouseDown={(e) => e.stopPropagation()}
          >
            <motion.div
              initial={{ scale: 0.96, y: 8 }}
              animate={{ scale: 1, y: 0 }}
              className="glass flex max-h-[70vh] w-[min(560px,80vw)] flex-col rounded-panel p-4 shadow-pop"
            >
              <div className="mb-2 flex items-center justify-between">
                <span className="flex items-center gap-2 text-sm font-medium text-ink-100">
                  <ScanIcon size={16} />
                  文字 / 二维码识别
                </span>
                <button
                  onClick={closeOcr}
                  className="grid h-7 w-7 place-items-center rounded-lg text-ink-300 transition-colors hover:bg-white/10 hover:text-ink-50"
                >
                  <X size={15} />
                </button>
              </div>

              {ocr.loading ? (
                <div className="grid h-24 place-items-center text-sm text-ink-300">正在识别…</div>
              ) : ocr.error ? (
                <div className="rounded-lg bg-red-500/10 p-3 text-sm text-red-300">
                  识别失败：{ocr.error}
                </div>
              ) : (
                <>
                  {(ocr.qr?.length ?? 0) > 0 && (
                    <div className="mb-2 space-y-1.5">
                      {ocr.qr!.map((code, i) => (
                        <div
                          key={i}
                          className="flex items-center gap-2 rounded-lg border border-white/10 bg-white/[0.04] px-3 py-2"
                        >
                          <QrCode size={14} className="shrink-0 text-white/55" />
                          <span
                            className="min-w-0 flex-1 truncate text-sm text-ink-100"
                            title={code}
                          >
                            {code}
                          </span>
                          <button
                            onClick={async () => {
                              await copyText(code);
                              toast("已复制二维码内容");
                            }}
                            className="shrink-0 rounded-[6px] bg-white px-2 py-0.5 text-[11px] font-medium text-black transition-colors hover:bg-white/85"
                          >
                            复制
                          </button>
                        </div>
                      ))}
                    </div>
                  )}
                  <textarea
                    readOnly
                    value={ocr.text || "（未识别到文字）"}
                    className="min-h-[120px] flex-1 resize-none rounded-lg border border-white/10 bg-ink-950/60 p-3 text-sm leading-relaxed text-ink-100 outline-none"
                  />
                </>
              )}

              <div className="mt-3 flex justify-end gap-2">
                <button
                  onClick={closeOcr}
                  className="rounded-lg px-3 py-1.5 text-sm text-ink-300 transition-colors hover:bg-white/8 hover:text-ink-50"
                >
                  关闭
                </button>
                <button
                  disabled={ocr.loading || !!ocr.error || !ocr.text}
                  onClick={async () => {
                    await copyText(ocr.text);
                    closeOcr();
                  }}
                  className="rounded-full bg-white px-3.5 py-1.5 text-[12.5px] font-semibold text-black transition-colors hover:bg-white/85 disabled:opacity-35"
                >
                  复制文字
                </button>
              </div>
            </motion.div>
          </motion.div>
        )}
      </AnimatePresence>

      {/* 悬浮工具条。提交动作期间整个 AnimatePresence 卸载，工具条立即消失而不播退场动画：
          此时画面必须保持静止，直到贴图窗在原位显示、遮罩隐藏 */}
      {!committing && (
        <AnimatePresence>
          {showToolbar && sel && (
            <motion.div
              initial={{ opacity: 0, y: 6, scale: 0.96 }}
              animate={{ opacity: 1, y: 0, scale: 1 }}
              exit={{ opacity: 0, y: 6, scale: 0.96 }}
              transition={{ duration: 0.18, ease: [0.22, 1, 0.36, 1] }}
              onMouseDown={(e) => e.stopPropagation()}
              className="glass absolute z-10 flex items-center gap-1 rounded-toolbar p-1.5 shadow-pop"
              style={{
                left: Math.max(8, Math.min(sel.x + sel.w - 408, cssW - 420)),
                top: sel.y + sel.h + 52 > cssH ? Math.max(8, sel.y - 52) : sel.y + sel.h + 10,
              }}
            >
              <ToolButton icon={PenLine} label="标注 (Ctrl+E)" onClick={doAnnotate} />
              <ToolButton icon={Pin} label="贴图置顶 (Ctrl+T)" onClick={doPin} />
              <ToolButton icon={ScanIcon} label="OCR / 二维码识别" onClick={doOcr} />
              <ToolButton
                icon={ScrollText}
                label="滚动长截图（按住 Shift = 横向）"
                onClick={(e) => void doScroll(e.shiftKey)}
              />
              <ToolButton icon={Film} label="录制 GIF（10fps · 停止自动保存）" onClick={doGif} />
              <div className="mx-1 h-5 w-px bg-white/10" />
              <ToolButton icon={Copy} label="复制 (Ctrl+C)" onClick={doCopy} />
              <ToolButton icon={Save} label="保存 (Ctrl+S)" onClick={doSave} />
              <button
                onMouseDown={(e) => {
                  e.stopPropagation();
                  if (e.button === 0) exitCapture();
                }}
                className="grid h-9 w-9 place-items-center rounded-lg text-ink-300 transition-colors hover:bg-white/10 hover:text-ink-50"
                title="取消 (Esc)"
              >
                <X size={17} />
              </button>
              <button
                onMouseDown={(e) => {
                  e.stopPropagation();
                  if (e.button === 0) doCopyToLine();
                }}
                className="ml-0.5 grid h-9 w-9 place-items-center rounded-full bg-white text-black transition-colors hover:bg-white/85"
                title="完成并挂到晾衣绳 (Enter / 双击 = 仅复制)"
              >
                <Check size={17} />
              </button>
            </motion.div>
          )}
        </AnimatePresence>
      )}

      {/* Toast */}
      <div className="pointer-events-none absolute inset-x-0 bottom-10 z-50 flex flex-col items-center gap-2">
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

function ScanIcon(props: { size?: number }) {
  return (
    <svg
      width={props.size ?? 17}
      height={props.size ?? 17}
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth="2"
      strokeLinecap="round"
      strokeLinejoin="round"
    >
      <path d="M3 7V5a2 2 0 0 1 2-2h2M17 3h2a2 2 0 0 1 2 2v2M21 17v2a2 2 0 0 1-2 2h-2M7 21H5a2 2 0 0 1-2-2v-2" />
      <path d="M7 9h7M7 13h10M7 17h5" />
    </svg>
  );
}

function ToolButton({
  icon: Icon,
  label,
  onClick,
  disabled,
}: {
  icon: React.ComponentType<{ size?: number }>;
  label: string;
  onClick?: (e: React.MouseEvent) => void;
  disabled?: boolean;
}) {
  return (
    <button
      disabled={disabled}
      onMouseDown={(e) => {
        // 用 mousedown 而非 click 是为了即时响应，但必须只认左键：右键/中键不能触发保存等动作
        e.stopPropagation();
        if (e.button === 0) onClick?.(e);
      }}
      className="group relative grid h-9 w-9 place-items-center rounded-lg text-ink-200 transition-colors hover:bg-white/10 hover:text-ink-0 disabled:cursor-not-allowed disabled:opacity-35 disabled:hover:bg-transparent"
      title={label}
    >
      <Icon size={17} />
    </button>
  );
}
