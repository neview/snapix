import { describe, expect, it } from "vitest";
import { pickFreshIds } from "./lineFresh";

const set = (...ids: string[]) => new Set(ids);

describe("pickFreshIds", () => {
  it("只挑这一轮新出现的可见条目", () => {
    expect(pickFreshIds(set("a"), [{ id: "a" }, { id: "b" }], set())).toEqual(["b"]);
  });

  it("飞过来的那张不播入场动画（落地弹跳的根因）", () => {
    // b 是从遮罩飞过来的：位置已经精确对准，再播一次就会看到弹跳
    expect(pickFreshIds(set("a"), [{ id: "a" }, { id: "b" }], set("b"))).toEqual([]);
  });

  it("pending 的条目不参与（还在飞，没落地）", () => {
    expect(pickFreshIds(set(), [{ id: "a", pending: true }], set())).toEqual([]);
  });

  it("刚落地的那张 pending 被摘掉、又在 landing 集合里 → 依然不播", () => {
    expect(pickFreshIds(set("a"), [{ id: "a" }, { id: "b", pending: false }], set("b"))).toEqual(
      [],
    );
  });

  it("与落地集合无关的新条目照旧播（例如编辑器导出的图）", () => {
    expect(pickFreshIds(set("a"), [{ id: "a" }, { id: "b" }, { id: "c" }], set("b"))).toEqual([
      "c",
    ]);
  });

  it("首次加载时调用方不传 prev，这里只验证它不会把已有条目当新条目", () => {
    const all = [{ id: "a" }, { id: "b" }];
    expect(pickFreshIds(set("a", "b"), all, set())).toEqual([]);
  });
});
