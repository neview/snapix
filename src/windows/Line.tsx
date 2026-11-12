import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { listen } from "@tauri-apps/api/event";
import { Copy, Pin, Trash2, Check, Save } from "lucide-react";
import {
  lineCancelDetach,
  lineCopy,
  lineDetachTarget,
  lineDragging,
  lineFinishDetach,
  lineHitRects,
  lineItemReady,
  lineList,
  lineMove,
  linePrepareDetach,
  lineRemove,
  lineReturnLanded,
  lineReturnTakeoff,
  lineRevealDetach,
  lineSave,
  type LineItem,
  type LineReturnFlight,
} from "@/lib/ipc";
import { afterPaint, animateFlight, FLIGHT_EASE, physicalToLocal } from "@/lib/flight";

const ROPE_Y = 40;
const HANG = 14;
/** 单段动画的等待上限：正常 420ms，超过就认为这一帧不会再回来，继续收尾 */
const FLIGHT_GRACE_MS = 280;
/** 等两帧上屏的兜底（rAF 被节流时不能把整条链路钉死） */
const PAINT_GRACE_MS = 220;
/** 一次「展开 / 挂回」的总兜底：任何一步没回来都强制收尾，绝不留幽灵图层 */
const FLIGHT_TIMEOUT_MS = 6000;
const sagOf = (width: number) => Math.min(84, Math.max(22, width * 0.028));
const ropeY = (t: number, sag: number) => ROPE_Y + 4 * sag * t * (1 - t);
const clamp = (v: number, lo: number, hi: number) => Math.min(Math.max(v, lo), hi);
const sleep = (ms: number) => new Promise<void>((resolve) => window.setTimeout(resolve, ms));
type Placed = LineItem & { t: number; w: number; h: number };
type Flight = {
  id: string;
  abort: AbortController;
  animation?: Animation;
  ghost?: HTMLImageElement;
  source?: HTMLElement;
};

function fit(item: LineItem) {
  const ratio = item.width / Math.max(1, item.height);
  let h = 146;
  let w = h * ratio;
  if (w > 226) {
    w = 226;
    h = w / ratio;
  }
  if (w < 64) {
    w = 64;
    h = w / ratio;
  }
  if (h > 176) {
    h = 176;
    w = h * ratio;
  }
  return { w, h };
}

export default function Line() {
  const [items, setItems] = useState<LineItem[]>([]);
  const [width, setWidth] = useState(() => window.innerWidth);
  const [busy, setBusy] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [freshIds, setFreshIds] = useState<Set<string>>(new Set());
  const [menu, setMenu] = useState<{
    item: Placed;
    image: HTMLImageElement | null;
    x: number;
    y: number;
  } | null>(null);
  const [menuConfirm, setMenuConfirm] = useState(false);
  const prevVisible = useRef<Set<string> | null>(null);
  const flightLayer = useRef<HTMLDivElement>(null);
  const active = useRef<Flight | null>(null);
  const acting = useRef(false);
  const reloadVersion = useRef(0);
  const reload = useCallback(async () => {
    const version = ++reloadVersion.current;
    try {
      const list = await lineList();
      if (version === reloadVersion.current) setItems(list);
    } catch (e) {
      setError(`无法加载截图：${String(e)}`);
    }
  }, []);

  useEffect(() => {
    let disposed = false;
    const un = listen("line:changed", () => {
      if (!disposed) void reload();
    });
    void un.then(() => {
      if (!disposed) void reload();
    });
    const onResize = () => setWidth(window.innerWidth);
    const onKey = (e: KeyboardEvent) => {
      const flight = active.current;
      if (e.key !== "Escape" || !flight) return;
      e.preventDefault();
      flight.abort.abort();
      flight.animation?.pause();
      void lineCancelDetach(flight.id).catch(() => {});
    };
    window.addEventListener("resize", onResize);
    window.addEventListener("keydown", onKey);
    return () => {
      disposed = true;
      void un.then((f) => f());
      window.removeEventListener("resize", onResize);
      window.removeEventListener("keydown", onKey);
      const flight = active.current;
      if (flight) {
        flight.abort.abort();
        flight.animation?.cancel();
        flight.ghost?.remove();
        void lineCancelDetach(flight.id).catch(() => {});
      }
    };
  }, [reload]);

  useEffect(() => {
    if (!error) return;
    const timer = window.setTimeout(() => setError(null), 4000);
    return () => window.clearTimeout(timer);
  }, [error]);

  const sag = sagOf(width);
  const placed: Placed[] = useMemo(
    () =>
      items.map((it, i) => ({
        ...it,
        t: it.x ?? (i + 0.5) / Math.max(1, items.length),
        ...fit(it),
      })),
    [items],
  );
  // 新图落地时播一次「从绳上垂下」的入场动画；首次加载不播，避免整排一起掉。
  useEffect(() => {
    const visible = items.filter((it) => !it.pending);
    const ids = new Set(visible.map((it) => it.id));
    if (prevVisible.current === null) {
      prevVisible.current = ids;
      return;
    }
    const fresh = visible.filter((it) => !prevVisible.current!.has(it.id)).map((it) => it.id);
    prevVisible.current = ids;
    if (!fresh.length) return;
    setFreshIds(new Set(fresh));
    const timer = window.setTimeout(() => setFreshIds(new Set()), 700);
    return () => window.clearTimeout(timer);
  }, [items]);

  useEffect(() => {
    const dpr = window.devicePixelRatio || 1;
    const rects = placed
      .filter((p) => !p.pending)
      .map(
        (p) =>
          [
            (p.t * width - p.w / 2 - 10) * dpr,
            (ropeY(p.t, sag) - 10) * dpr,
            (p.w + 20) * dpr,
            (HANG + p.h + 54) * dpr,
          ] as [number, number, number, number],
      );
    void lineHitRects(rects).catch(() => {});
  }, [placed, width, sag]);

  /**
   * 强制结束一次展开：只做本地清理 + 让后端撤掉半成品贴图。
   *
   * 守卫超时、绳子被清空、窗口收起来时都走这里——不管卡在 WAAPI、rAF 还是 IPC，
   * 画面都不会留下一个飞了一半的替身，按钮也不会一直处于 busy 状态。
   */
  const abortFlight = useCallback((id: string) => {
    const flight = active.current;
    if (!flight || flight.id !== id) return;
    active.current = null;
    flight.abort.abort();
    flight.animation?.cancel();
    flight.ghost?.remove();
    if (flight.source?.isConnected) flight.source.style.visibility = "";
    setBusy(null);
    void lineCancelDetach(id).catch(() => {});
  }, []);

  const expand = async (item: Placed, image: HTMLImageElement) => {
    if (active.current || acting.current || !flightLayer.current) return;
    const flight: Flight = { id: item.id, abort: new AbortController() };
    active.current = flight;
    setBusy(item.id);
    setError(null);
    const { signal } = flight.abort;
    const cancelled = new Promise<never>((_, reject) => {
      signal.addEventListener("abort", () => reject(new DOMException("Cancelled", "AbortError")), {
        once: true,
      });
    });
    void cancelled.catch(() => {});
    const watchdog = window.setTimeout(() => abortFlight(item.id), FLIGHT_TIMEOUT_MS);
    try {
      const rect = image.getBoundingClientRect();
      const from = { x: rect.left, y: rect.top, w: rect.width, h: rect.height };
      const origin = await getCurrentWindow().outerPosition();
      signal.throwIfAborted();
      const dpr = window.devicePixelRatio || 1;
      const target = await lineDetachTarget(
        Math.round(origin.x + (from.x + from.w / 2) * dpr),
        Math.round(origin.y + (from.y + from.h / 2) * dpr),
        item.width,
        item.height,
      );
      signal.throwIfAborted();
      const layer = flightLayer.current;
      if (!layer || !image.isConnected) throw new Error("截图已不在晾衣架上");
      const ghost = image.cloneNode() as HTMLImageElement;
      ghost.removeAttribute("class");
      ghost.removeAttribute("id");
      ghost.alt = "";
      Object.assign(ghost.style, {
        position: "absolute",
        left: `${from.x}px`,
        top: `${from.y}px`,
        width: `${from.w}px`,
        height: `${from.h}px`,
        margin: "0",
        display: "block",
        transformOrigin: "top left",
        willChange: "transform, opacity",
        pointerEvents: "none",
        borderRadius: "0",
        objectFit: "fill",
      });
      layer.append(ghost);
      flight.ghost = ghost;
      flight.source = image.parentElement ?? image;
      flight.source.style.visibility = "hidden";
      const preparing = linePrepareDetach(item.id, ...target);
      // 保存 / 建窗还没回来就先挂个空 catch，免得提前 reject 被报成未处理的 Promise
      void preparing.catch(() => {});
      const animation = animateFlight(ghost, from, physicalToLocal(target, origin, dpr));
      flight.animation = animation;
      // 动画卡住（合成器被挂起等）时不能无限等，兜底后直接进入交接
      await Promise.race([animation.finished, cancelled, sleep(FLIGHT_GRACE_MS + 480)]);
      signal.throwIfAborted();
      const label = await Promise.race([preparing, cancelled]);
      signal.throwIfAborted();
      await Promise.race([lineRevealDetach(item.id, label), cancelled]);
      signal.throwIfAborted();
      await Promise.race([afterPaint(signal), sleep(PAINT_GRACE_MS)]);
      signal.throwIfAborted();
      const fade = ghost.animate([{ opacity: 1 }, { opacity: 0 }], {
        duration: 120,
        easing: "ease-out",
        fill: "forwards",
      });
      flight.animation = fade;
      await Promise.race([fade.finished, cancelled, sleep(FLIGHT_GRACE_MS)]);
      signal.throwIfAborted();
      await lineFinishDetach(item.id, label);
      reloadVersion.current++;
      setItems((list) => list.filter((it) => it.id !== item.id));
    } catch (e) {
      await lineCancelDetach(item.id).catch(() => {});
      const ghost = flight.ghost;
      if (ghost?.isConnected && flight.source?.isConnected) {
        const transform = getComputedStyle(ghost).transform;
        ghost.getAnimations().forEach((animation) => animation.cancel());
        await ghost
          .animate(
            [
              { transform, opacity: 1 },
              { transform: "translate3d(0, 0, 0) scale(1, 1)", opacity: 1 },
            ],
            { duration: 220, easing: FLIGHT_EASE, fill: "forwards" },
          )
          .finished.catch(() => {});
      }
      if (!signal.aborted && flightLayer.current) setError(`展开失败，截图已保留：${String(e)}`);
    } finally {
      window.clearTimeout(watchdog);
      flight.ghost?.remove();
      if (flight.source) flight.source.style.visibility = "";
      if (active.current === flight) active.current = null;
      if (flightLayer.current) setBusy(null);
    }
  };

  /** 绳子被清空 / 重新加载成空：任何还在飞的替身都立即收掉，别盖住下一张截图 */
  useEffect(() => {
    if (items.length > 0) return;
    const flight = active.current;
    if (flight) abortFlight(flight.id);
    const layer = flightLayer.current;
    if (layer) layer.querySelectorAll("img").forEach((node) => node.remove());
  }, [items, abortFlight]);

  /**
   * 贴图双击「挂回绳子」：绳窗里画一份从贴图位置起飞的替身，飞回它原来的落点。
   *
   * 贴图也是置顶窗，所以后端先把绳窗抬到它上面再发事件；替身画满两帧后才让后端
   * 关掉贴图窗——首帧与贴图完全重合，切换看不出接缝。
   */
  const playReturn = useCallback(
    async (f: LineReturnFlight) => {
      let ghost: HTMLImageElement | null = null;
      // 兜底：任何一步卡住都要把替身收掉、把条目显出来；后端 4s 超时也会收尾
      const watchdog = window.setTimeout(() => {
        ghost?.remove();
        void lineReturnLanded(f.id, f.label).catch(() => {});
      }, FLIGHT_TIMEOUT_MS);
      try {
        await reload();
        const layer = flightLayer.current;
        if (!layer) throw new Error("飞行层不可用");
        const origin = await getCurrentWindow().outerPosition();
        const dpr = window.devicePixelRatio || 1;
        const from = physicalToLocal(f.from, origin, dpr);
        const to = { x: f.to[0], y: f.to[1], w: f.to[2], h: f.to[3] };

        ghost = document.createElement("img");
        ghost.src = f.url;
        ghost.alt = "";
        ghost.decoding = "async";
        Object.assign(ghost.style, {
          position: "absolute",
          left: `${from.x}px`,
          top: `${from.y}px`,
          width: `${from.w}px`,
          height: `${from.h}px`,
          margin: "0",
          display: "block",
          transformOrigin: "top left",
          willChange: "transform, opacity",
          pointerEvents: "none",
          borderRadius: "0",
          objectFit: "fill",
        });
        // 等图解码完再放进画面，避免第一帧是一块空白（此刻贴图窗还盖在上面）
        await Promise.race([ghost.decode().catch(() => {}), sleep(1200)]);
        layer.append(ghost);
        await Promise.race([afterPaint(), sleep(PAINT_GRACE_MS)]);

        // 替身已经画在贴图原位：此时才关贴图窗，屏幕上不会少一帧
        await lineReturnTakeoff(f.id, f.label).catch(() => {});
        const animation = animateFlight(ghost, from, to);
        await Promise.race([animation.finished, sleep(FLIGHT_GRACE_MS + 480)]);

        await Promise.race([lineReturnLanded(f.id, f.label).catch(() => {}), sleep(3000)]);
        await reload();
        // 真图已经出现在落点上（同一张缩略图），淡出替身即可无缝交接
        await ghost
          .animate([{ opacity: 1 }, { opacity: 0 }], {
            duration: 160,
            easing: "ease-out",
            fill: "forwards",
          })
          .finished.catch(() => {});
      } catch (e) {
        await lineReturnLanded(f.id, f.label).catch(() => {});
        await reload();
        setError(`挂回失败：${String(e)}`);
      } finally {
        window.clearTimeout(watchdog);
        ghost?.remove();
      }
    },
    [reload],
  );

  /** 多张贴图连续挂回时排队播放，飞行层一次只演一张 */
  const returnQueue = useRef<Promise<void>>(Promise.resolve());
  const enqueueReturn = useCallback(
    (f: LineReturnFlight) => {
      returnQueue.current = returnQueue.current.then(() => playReturn(f));
      return returnQueue.current;
    },
    [playReturn],
  );

  useEffect(() => {
    const un = listen<LineReturnFlight>("line:return", (e) => void enqueueReturn(e.payload));
    return () => {
      void un.then((f) => f());
    };
  }, [enqueueReturn]);

  const act = async (id: string, fn: () => Promise<unknown>) => {
    if (active.current || acting.current) return;
    acting.current = true;
    setBusy(id);
    try {
      await fn();
    } catch (e) {
      setError(`操作失败：${String(e)}`);
    } finally {
      acting.current = false;
      setBusy(null);
    }
  };

  /** 拖动结束：先就地更新（回写位置），后端持久化。 */
  const commitX = useCallback((id: string, t: number) => {
    setItems((list) => list.map((it) => (it.id === id ? { ...it, x: t } : it)));
    void lineMove(id, t).catch(() => {});
  }, []);

  const openMenu = useCallback(
    (item: Placed, image: HTMLImageElement | null, e: React.MouseEvent) => {
      e.preventDefault();
      setMenuConfirm(false);
      setMenu({ item, image, x: e.clientX, y: e.clientY });
    },
    [],
  );

  useEffect(() => {
    if (!menu) return;
    const close = () => setMenu(null);
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") close();
    };
    window.addEventListener("pointerdown", close);
    window.addEventListener("keydown", onKey);
    return () => {
      window.removeEventListener("pointerdown", close);
      window.removeEventListener("keydown", onKey);
    };
  }, [menu]);

  const ropePath = `M 0 ${ROPE_Y} Q ${width / 2} ${ROPE_Y + sag * 2} ${width} ${ROPE_Y}`;
  return (
    <div className="relative h-screen w-screen select-none overflow-hidden">
      {items.length > 0 && (
        <svg
          width={width}
          height={ROPE_Y + sag + 26}
          className="pointer-events-none absolute inset-x-0 top-0"
        >
          <defs>
            <linearGradient id="ropeLight" x1="0" x2="1">
              <stop offset="0" stopColor="#ffffff" stopOpacity="0.12" />
              <stop offset="0.5" stopColor="#ffffff" stopOpacity="0.3" />
              <stop offset="1" stopColor="#ffffff" stopOpacity="0.12" />
            </linearGradient>
          </defs>
          <path
            d={ropePath}
            fill="none"
            stroke="#ffffff"
            strokeOpacity={0.07}
            strokeWidth={7}
            strokeLinecap="round"
            transform="translate(0, 2)"
          />
          <path
            d={ropePath}
            fill="none"
            stroke="url(#ropeLight)"
            strokeWidth={1.25}
            strokeLinecap="round"
          />
          <circle cx={2} cy={ROPE_Y} r={2} fill="#ffffff" fillOpacity={0.22} />
          <circle cx={width - 2} cy={ROPE_Y} r={2} fill="#ffffff" fillOpacity={0.22} />
        </svg>
      )}
      {placed.map((item) => (
        <LinePhoto
          key={item.id}
          item={item}
          width={width}
          sag={sag}
          left={item.t * width - item.w / 2}
          top={ropeY(item.t, sag)}
          busy={busy !== null}
          fresh={freshIds.has(item.id)}
          onExpand={(image) => void expand(item, image)}
          onCopy={() => void act(item.id, () => lineCopy(item.id))}
          onRemove={() => void act(item.id, () => lineRemove(item.id))}
          onMove={commitX}
          onContextMenu={openMenu}
        />
      ))}
      <div
        ref={flightLayer}
        aria-hidden="true"
        className="pointer-events-none absolute inset-0 z-40"
      />
      {menu && (
        <div
          className="fixed z-50 w-[150px] overflow-hidden rounded-[10px] border border-white/10 bg-[#17171b]/95 p-1 text-[11.5px] text-white/80 shadow-[0_16px_40px_-12px_rgba(0,0,0,.85)] backdrop-blur"
          style={{
            left: Math.min(menu.x, width - 158),
            top: Math.min(menu.y, window.innerHeight - 174),
          }}
          onPointerDown={(e) => e.stopPropagation()}
          onContextMenu={(e) => e.preventDefault()}
        >
          <LineMenuItem
            icon={Copy}
            label="复制"
            onClick={() => {
              const m = menu;
              setMenu(null);
              void act(m.item.id, () => lineCopy(m.item.id));
            }}
          />
          <LineMenuItem
            icon={Pin}
            label="展开为贴图"
            onClick={() => {
              const m = menu;
              setMenu(null);
              if (m.image) void expand(m.item, m.image);
            }}
          />
          <LineMenuItem
            icon={Save}
            label="保存…"
            onClick={() => {
              const m = menu;
              setMenu(null);
              void act(m.item.id, () => lineSave(m.item.id));
            }}
          />
          <div className="my-1 h-px bg-white/[0.07]" />
          <LineMenuItem
            icon={Trash2}
            label={menuConfirm ? "确认删除" : "删除"}
            danger={menuConfirm}
            onClick={() => {
              const m = menu;
              if (!menuConfirm) {
                setMenuConfirm(true);
                return;
              }
              setMenu(null);
              void act(m.item.id, () => lineRemove(m.item.id));
            }}
          />
        </div>
      )}
      {error && (
        <div
          role="alert"
          className="pointer-events-none absolute inset-x-0 top-4 z-50 flex justify-center"
        >
          <span className="rounded-lg bg-black/85 px-4 py-2 text-xs text-white">{error}</span>
        </div>
      )}
    </div>
  );
}

/** 拖动中的指针状态（只跟手；摆角另走一套弹簧） */
type DragState = {
  pointer: number;
  startX: number;
  startY: number;
  /** 指针相对图片中心的偏移，拖动时图片不跳 */
  offset: number;
  moved: boolean;
  t: number;
  lastX: number;
  lastAt: number;
};

function LinePhoto({
  item,
  width,
  sag,
  left,
  top,
  busy,
  fresh,
  onExpand,
  onCopy,
  onRemove,
  onMove,
  onContextMenu,
}: {
  item: Placed;
  width: number;
  sag: number;
  left: number;
  top: number;
  busy: boolean;
  fresh: boolean;
  onExpand: (image: HTMLImageElement) => void;
  onCopy: () => void;
  onRemove: () => void;
  onMove: (id: string, t: number) => void;
  onContextMenu: (item: Placed, image: HTMLImageElement | null, e: React.MouseEvent) => void;
}) {
  const image = useRef<HTMLImageElement>(null);
  const root = useRef<HTMLDivElement>(null);
  const swing = useRef<HTMLDivElement>(null);
  const drag = useRef<DragState | null>(null);
  /** 拖动结束的那次 click 要吞掉，不然会误触「展开」 */
  const swallowed = useRef(false);
  const [loaded, setLoaded] = useState(false);
  const [grabbing, setGrabbing] = useState(false);
  const [confirmDelete, setConfirmDelete] = useState(false);
  const confirmTimer = useRef<number | null>(null);

  useEffect(
    () => () => {
      if (confirmTimer.current) window.clearTimeout(confirmTimer.current);
    },
    [],
  );
  // 摆角积分器：拖动中朝「速度反方向」偏，松手后阻尼回摆
  const swingState = useRef({
    angle: 0,
    vel: 0,
    vx: 0,
    pickup: 0,
    last: 0,
    raf: 0,
    dragging: false,
  });

  useEffect(() => {
    if (item.pending) return;
    const abort = new AbortController();
    const ready = async () => {
      try {
        await image.current?.decode();
        await afterPaint(abort.signal);
        await lineItemReady(item.id, true);
      } catch {
        if (!abort.signal.aborted) await lineItemReady(item.id, false).catch(() => {});
      }
    };
    void ready();
    return () => abort.abort();
  }, [item.id, item.url, item.pending]);

  // 自引用的帧循环放不进 useCallback（会被 lint 判成「未声明先访问」），
  // 用 ref 持一份稳定的 tick，事件里只负责把循环踢起来。
  const tickRef = useRef<(now: number) => void>(() => {});

  useEffect(() => {
    const state = swingState.current;
    const tick = (now: number) => {
      const s = state;
      const el = swing.current;
      if (!el) {
        s.raf = 0;
        return;
      }
      const dt = s.last ? Math.min(0.032, Math.max(0, now - s.last) / 1000) : 0.016;
      s.last = now;
      // 手停住后速度自然衰减，摆角回正；否则会一直歪着
      if (!s.dragging) s.vx *= Math.exp(-9 * dt);
      const target = s.dragging ? clamp(-s.vx * 0.016, -16, 16) : 0;
      const stiffness = s.dragging ? 230 : 92;
      const damping = s.dragging ? 18 : 4.6;
      s.vel += ((target - s.angle) * stiffness - s.vel * damping) * dt;
      s.angle += s.vel * dt;
      // 「被拎起来」的轻微放大也走同一帧循环，避免松手时突然缩回去
      s.pickup += ((s.dragging ? 1 : 0) - s.pickup) * Math.min(1, dt * 14);
      el.style.transform = `rotate(${s.angle.toFixed(2)}deg) scale(${(1 + 0.04 * s.pickup).toFixed(
        3,
      )})`;
      if (!s.dragging && s.pickup < 0.01 && Math.abs(s.angle) < 0.05 && Math.abs(s.vel) < 0.6) {
        el.style.transform = "";
        s.angle = 0;
        s.vel = 0;
        s.pickup = 0;
        s.raf = 0;
        return;
      }
      s.raf = requestAnimationFrame(tick);
    };
    tickRef.current = tick;
    return () => {
      if (state.raf) cancelAnimationFrame(state.raf);
      state.raf = 0;
    };
  }, []);

  const kickSwing = useCallback(() => {
    const s = swingState.current;
    if (!s.raf) {
      s.last = 0;
      s.raf = requestAnimationFrame((now) => tickRef.current(now));
    }
  }, []);

  const onPointerDown = (e: React.PointerEvent<HTMLDivElement>) => {
    if (e.button !== 0 || busy || item.pending || !loaded) return;
    swallowed.current = false;
    drag.current = {
      pointer: e.pointerId,
      startX: e.clientX,
      startY: e.clientY,
      offset: e.clientX - (left + item.w / 2),
      moved: false,
      t: (left + item.w / 2) / width,
      lastX: e.clientX,
      lastAt: performance.now(),
    };
    // 指针捕获等真的开始拖再拿：一按下就捕获会把 click 重定向到根节点，
    // 缩略图的「点击展开」就再也收不到了。
  };

  const onPointerMove = (e: React.PointerEvent<HTMLDivElement>) => {
    const d = drag.current;
    if (!d || d.pointer !== e.pointerId) return;
    if (!d.moved) {
      if (Math.hypot(e.clientX - d.startX, e.clientY - d.startY) < 4) return;
      d.moved = true;
      setGrabbing(true);
      root.current?.setPointerCapture(e.pointerId);
      const s = swingState.current;
      s.dragging = true;
      s.vx = 0;
      s.last = 0;
      kickSwing();
      root.current?.classList.add("no-anim");
      if (root.current) root.current.style.zIndex = "20";
      // 拖动期间钉住可点状态：光标跑出图外也不能让窗口变得点不透
      void lineDragging(true).catch(() => {});
    }
    const center = clamp(e.clientX - d.offset, item.w / 2, width - item.w / 2);
    d.t = center / width;
    if (root.current) {
      root.current.style.transform = `translate3d(${center - item.w / 2}px, ${ropeY(d.t, sag)}px, 0)`;
    }
    const s = swingState.current;
    const now = performance.now();
    const dt = Math.max(1, now - d.lastAt);
    const v = ((e.clientX - d.lastX) / dt) * 1000;
    s.vx = s.vx * 0.55 + v * 0.45;
    d.lastX = e.clientX;
    d.lastAt = now;
  };

  const endDrag = (e: React.PointerEvent<HTMLDivElement>, commit: boolean) => {
    const d = drag.current;
    if (!d || d.pointer !== e.pointerId) return;
    drag.current = null;
    const el = root.current;
    if (el?.hasPointerCapture(e.pointerId)) el.releasePointerCapture(e.pointerId);
    if (!d.moved) return;
    swallowed.current = true;
    setGrabbing(false);
    const s = swingState.current;
    s.dragging = false;
    s.vx = 0;
    el?.classList.remove("no-anim");
    if (el) el.style.zIndex = "";
    void lineDragging(false).catch(() => {});
    if (commit) {
      onMove(item.id, d.t);
    } else if (el) {
      el.style.transform = `translate3d(${left}px, ${top}px, 0)`;
    }
  };

  // 悬浮胶囊按屏幕边缘夹一下，避免第一张 / 最后一张的按钮被裁掉
  const capHalf = 58;
  const capCenter = clamp(left + item.w / 2, capHalf + 6, width - capHalf - 6);

  return (
    <div
      ref={root}
      className="line-item group absolute left-0 top-0"
      data-line-id={item.id}
      style={{
        transform: `translate3d(${left}px, ${top}px, 0)`,
        visibility: item.pending ? "hidden" : "visible",
        cursor: grabbing ? "grabbing" : undefined,
      }}
      onPointerDown={onPointerDown}
      onPointerMove={onPointerMove}
      onPointerUp={(e) => endDrag(e, true)}
      onPointerCancel={(e) => endDrag(e, false)}
      onLostPointerCapture={(e) => endDrag(e, false)}
      onContextMenu={(e) => {
        e.preventDefault();
        onContextMenu(item, image.current, e);
      }}
    >
      {/* 吊线 + 图片整组挂在一个锚点上，拖动时绕绳上的挂点摆动 */}
      <div
        ref={swing}
        className="relative"
        style={{ width: item.w, transformOrigin: "50% 0%", willChange: "transform" }}
      >
        <div
          className="relative"
          style={
            fresh ? { animation: "line-drop 320ms cubic-bezier(.22,1,.36,1) both" } : undefined
          }
        >
          <div
            className="pointer-events-none absolute left-1/2 w-px -translate-x-1/2 bg-gradient-to-b from-white/28 to-white/12"
            style={{ height: HANG }}
          />
          <svg
            aria-hidden
            className="pointer-events-none absolute -top-[3px] left-1/2 -translate-x-1/2 drop-shadow-[0_1px_1px_rgba(0,0,0,0.55)]"
            width="9"
            height="17"
            viewBox="0 0 9 17"
            fill="none"
          >
            <rect
              x="0.8"
              y="0.8"
              width="7.4"
              height="15.4"
              rx="2.2"
              fill="#8a6c4e"
              stroke="rgba(0,0,0,0.55)"
            />
            <rect x="2.2" y="2.2" width="4.6" height="12.6" rx="1.4" fill="#a5845f" />
            <path d="M4.5 2.4v12.2" stroke="rgba(255,255,255,0.28)" strokeWidth="0.7" />
          </svg>
          <button
            type="button"
            title="点击展开截图"
            aria-label="展开截图"
            disabled={busy || !!item.pending || !loaded}
            className="block cursor-pointer overflow-hidden rounded-[10px] bg-[#141418] outline-none ring-1 ring-white/15 transition-[outline-color] hover:outline hover:outline-white/25 focus-visible:outline focus-visible:outline-white/60 disabled:cursor-default"
            style={{
              marginTop: HANG,
              cursor: grabbing ? "grabbing" : undefined,
              boxShadow: "0 1px 2px rgba(0,0,0,.5), 0 14px 34px -10px rgba(0,0,0,.72)",
            }}
            onClick={() => {
              if (swallowed.current) {
                swallowed.current = false;
                return;
              }
              if (image.current) onExpand(image.current);
            }}
          >
            <img
              ref={image}
              src={item.url}
              alt=""
              draggable={false}
              onLoad={() => setLoaded(true)}
              className="block object-cover"
              style={{ width: item.w, height: item.h }}
            />
          </button>
        </div>
      </div>
      <div
        className="capsule absolute flex items-center gap-0.5 rounded-full px-1 py-1 opacity-0 transition-opacity duration-150 group-hover:opacity-100 group-focus-within:opacity-100"
        style={{
          left: capCenter - left,
          transform: "translateX(-50%)",
          top: HANG + item.h + 8,
          visibility: busy || grabbing ? "hidden" : undefined,
        }}
        onPointerDown={(e) => e.stopPropagation()}
      >
        <IconBtn label="复制" onClick={onCopy}>
          <Copy size={12} strokeWidth={1.8} />
        </IconBtn>
        <IconBtn label="展开截图" onClick={() => image.current && onExpand(image.current)}>
          <Pin size={12} strokeWidth={1.8} />
        </IconBtn>
        <IconBtn
          label={confirmDelete ? "再次点击确认删除" : "删除（点两次确认）"}
          danger={confirmDelete}
          onClick={() => {
            if (!confirmDelete) {
              setConfirmDelete(true);
              if (confirmTimer.current) window.clearTimeout(confirmTimer.current);
              confirmTimer.current = window.setTimeout(() => setConfirmDelete(false), 2500);
              return;
            }
            setConfirmDelete(false);
            onRemove();
          }}
        >
          {confirmDelete ? (
            <Check size={12} strokeWidth={2.2} />
          ) : (
            <Trash2 size={12} strokeWidth={1.8} />
          )}
        </IconBtn>
      </div>
    </div>
  );
}

function LineMenuItem({
  icon: Icon,
  label,
  onClick,
  danger,
}: {
  icon: React.ComponentType<{ size?: number; strokeWidth?: number }>;
  label: string;
  onClick: () => void;
  danger?: boolean;
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      className={
        "flex w-full items-center gap-2 rounded-[7px] px-2 py-[6px] text-left transition-colors " +
        (danger ? "bg-red-500/15 text-red-200" : "hover:bg-white/[0.07]")
      }
    >
      <Icon size={12} strokeWidth={1.8} />
      {label}
    </button>
  );
}

function IconBtn({
  label,
  children,
  onClick,
  danger,
}: {
  label: string;
  children: React.ReactNode;
  onClick: () => void;
  danger?: boolean;
}) {
  return (
    <button
      type="button"
      title={label}
      onClick={onClick}
      className={
        danger
          ? "grid h-6 w-6 place-items-center rounded-full bg-red-500/80 text-white transition-colors duration-150"
          : "grid h-6 w-6 place-items-center rounded-full text-white/60 transition-colors duration-150 hover:bg-white/10 hover:text-white"
      }
    >
      {children}
    </button>
  );
}
