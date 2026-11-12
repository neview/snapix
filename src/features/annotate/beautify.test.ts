import { describe, expect, it } from "vitest";
import { BG_PRESETS, computeLayout, cssBackground, frameBarHeight, getPreset } from "./beautify";
import type { Beautify } from "./store";

const base: Beautify = {
  enabled: true,
  bgId: "grad-indigo",
  padding: 0.1,
  radius: 0.05,
  shadow: true,
  ratio: "auto",
  watermark: "",
  windowFrame: "none",
};

describe("computeLayout", () => {
  it("is the identity when beautify is disabled", () => {
    const l = computeLayout(800, 600, { ...base, enabled: false });
    expect(l).toMatchObject({
      outerW: 800,
      outerH: 600,
      padX: 0,
      padY: 0,
      imgX: 0,
      imgY: 0,
      barH: 0,
      radiusPx: 0,
    });
  });

  it("adds symmetric padding scaled by the short side", () => {
    const l = computeLayout(800, 600, base);
    expect(l.outerW).toBe(800 + 2 * 60);
    expect(l.outerH).toBe(600 + 2 * 60);
    expect(l.padX).toBe(60);
    expect(l.padY).toBe(60);
    expect(l.imgX).toBe(l.padX);
    expect(l.imgY).toBe(l.padY);
    expect(l.radiusPx).toBe(30);
  });

  it("expands to the requested aspect ratio without shrinking the image", () => {
    const l = computeLayout(800, 600, { ...base, ratio: "1:1" });
    expect(l.outerW).toBe(l.outerH);
    expect(l.outerW).toBeGreaterThanOrEqual(800 + 120);
    expect(l.imgX + l.winW).toBeLessThanOrEqual(l.outerW);
    expect(l.imgY + l.winH).toBeLessThanOrEqual(l.outerH);

    const wide = computeLayout(600, 800, { ...base, ratio: "16:9" });
    expect(wide.outerW / wide.outerH).toBeCloseTo(16 / 9, 2);
  });

  it("reserves a title bar when a window frame is enabled", () => {
    const l = computeLayout(1000, 500, { ...base, windowFrame: "light" });
    expect(l.barH).toBe(frameBarHeight(1000, "light"));
    expect(l.barH).toBe(50);
    expect(l.winH).toBe(500 + l.barH);
    expect(l.imgY).toBe(l.padY + l.barH);
  });

  it("clamps the title bar height", () => {
    expect(frameBarHeight(100, "dark")).toBe(32);
    expect(frameBarHeight(5000, "dark")).toBe(72);
    expect(frameBarHeight(5000, "none")).toBe(0);
  });
});

describe("presets", () => {
  it("falls back to the first preset for unknown ids", () => {
    expect(getPreset("nope")).toBe(BG_PRESETS[0]);
  });

  it("every preset renders a CSS background", () => {
    for (const p of BG_PRESETS) {
      const css = cssBackground({ ...base, bgId: p.id });
      expect(typeof css).toBe("string");
      expect(css.length).toBeGreaterThan(0);
    }
    expect(cssBackground({ ...base, bgId: "transparent" })).toBe("transparent");
    expect(cssBackground({ ...base, bgId: "white" })).toBe("#FFFFFF");
  });
});
