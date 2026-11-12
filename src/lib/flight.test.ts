import { describe, expect, it } from "vitest";
import { flightTransform, physicalToLocal } from "./flight";

describe("flight geometry", () => {
  it("keeps the same image bounds at the handoff", () => {
    expect(
      flightTransform({ x: 100, y: 80, w: 200, h: 100 }, { x: 500, y: 300, w: 800, h: 400 }),
    ).toBe("translate3d(400px, 220px, 0) scale(4, 4)");
  });

  it("does not move an image whose source and destination match", () => {
    const rect = { x: -40, y: 5, w: 800, h: 600 };
    expect(flightTransform(rect, rect)).toBe("translate3d(0px, 0px, 0) scale(1, 1)");
  });

  it.each([1, 1.25, 1.5, 2])("round trips physical bounds at %sx DPI", (dpr) => {
    const origin = { x: -2560, y: 40 };
    const target: [number, number, number, number] = [-2100, 340, 805, 604];
    const local = physicalToLocal(target, origin, dpr);
    expect(local.x * dpr + origin.x).toBeCloseTo(target[0]);
    expect(local.y * dpr + origin.y).toBeCloseTo(target[1]);
    expect(local.w * dpr).toBeCloseTo(target[2]);
    expect(local.h * dpr).toBeCloseTo(target[3]);
  });

  it("preserves separately rounded width and height for very tall captures", () => {
    const to = physicalToLocal([10, 100, 53, 905], { x: 0, y: 0 }, 1.25);
    expect(flightTransform({ x: 0, y: 0, w: 16, h: 176 }, to)).toBe(
      `translate3d(8px, 80px, 0) scale(${42.4 / 16}, ${724 / 176})`,
    );
  });
});
