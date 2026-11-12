export type FlightRect = { x: number; y: number; w: number; h: number };

export const FLIGHT_MS = 420;
export const FLIGHT_EASE = "cubic-bezier(.22,1,.36,1)";
/**
 * 幕布（冻屏画面 + 压暗层）淡出的缓动：与飞行等长，但前段偏慢——
 * 起手时屏幕还是原样压暗的，图越飞越远，压暗才逐渐化开，
 * 落到绳上那一刻屏幕刚好全透明，随后隐藏遮罩窗就不会突然闪一下。
 */
export const CURTAIN_EASE = "cubic-bezier(.42,0,.58,1)";

const reducedMotion = () => window.matchMedia("(prefers-reduced-motion: reduce)").matches;

export function flightTransform(from: FlightRect, to: FlightRect) {
  return `translate3d(${to.x - from.x}px, ${to.y - from.y}px, 0) scale(${to.w / from.w}, ${to.h / from.h})`;
}

export function physicalToLocal(
  rect: [number, number, number, number],
  origin: { x: number; y: number },
  dpr: number,
): FlightRect {
  return {
    x: (rect[0] - origin.x) / dpr,
    y: (rect[1] - origin.y) / dpr,
    w: rect[2] / dpr,
    h: rect[3] / dpr,
  };
}

export function animateFlight(element: HTMLElement, from: FlightRect, to: FlightRect) {
  return element.animate(
    [{ transform: "translate3d(0, 0, 0) scale(1, 1)" }, { transform: flightTransform(from, to) }],
    {
      duration: reducedMotion() ? 0 : FLIGHT_MS,
      easing: FLIGHT_EASE,
      fill: "forwards",
    },
  );
}

/** 让一层「幕布」跟着飞行一起淡出（与 `animateFlight` 同长同起）。 */
export function animateCurtain(element: HTMLElement) {
  return element.animate([{ opacity: 1 }, { opacity: 0 }], {
    duration: reducedMotion() ? 0 : FLIGHT_MS,
    easing: CURTAIN_EASE,
    fill: "forwards",
  });
}

export function afterPaint(signal?: AbortSignal): Promise<void> {
  return new Promise((resolve, reject) => {
    let frame = 0;
    const abort = () => {
      cancelAnimationFrame(frame);
      reject(new DOMException("Animation cancelled", "AbortError"));
    };
    if (signal?.aborted) {
      abort();
      return;
    }
    signal?.addEventListener("abort", abort, { once: true });
    frame = requestAnimationFrame(() => {
      frame = requestAnimationFrame(() => {
        signal?.removeEventListener("abort", abort);
        resolve();
      });
    });
  });
}
