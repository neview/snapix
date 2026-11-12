import { beforeEach, describe, expect, it } from "vitest";
import { useEditor, type Shape } from "./store";

const rect = (id: string, x = 0): Shape => ({
  id,
  type: "rect",
  x,
  y: 0,
  w: 10,
  h: 10,
  stroke: "#fff",
  strokeWidth: 2,
});

const step = (id: string, index: number): Shape => ({
  id,
  type: "step",
  x: 0,
  y: 0,
  index,
  stroke: "#fff",
  strokeWidth: 2,
});

describe("editor store", () => {
  beforeEach(() => {
    useEditor.getState().setImage({ url: "shot://editor/1", width: 100, height: 100 });
  });

  it("setImage resets shapes, crop and history", () => {
    const s = useEditor.getState();
    s.addShape(rect("a"));
    s.setCrop({ x: 0, y: 0, w: 5, h: 5 });
    s.setImage({ url: "shot://editor/2", width: 50, height: 50 });
    const next = useEditor.getState();
    expect(next.shapes).toEqual([]);
    expect(next.crop).toBeNull();
    expect(next.past).toEqual([]);
    expect(next.future).toEqual([]);
  });

  it("undo / redo walk the history stack and clear selection", () => {
    const s = useEditor.getState();
    s.addShape(rect("a"));
    s.addShape(rect("b"));
    s.setSelected("b");
    s.undo();
    expect(useEditor.getState().shapes.map((x) => x.id)).toEqual(["a"]);
    expect(useEditor.getState().selectedId).toBeNull();
    s.redo();
    expect(useEditor.getState().shapes.map((x) => x.id)).toEqual(["a", "b"]);
    // 新操作清空 redo 栈
    s.undo();
    s.addShape(rect("c"));
    expect(useEditor.getState().future).toEqual([]);
    expect(useEditor.getState().shapes.map((x) => x.id)).toEqual(["a", "c"]);
  });

  it("updateShape only records history when asked", () => {
    const s = useEditor.getState();
    s.addShape(rect("a"));
    const before = useEditor.getState().past.length;
    s.updateShape("a", { x: 5 }, false);
    expect(useEditor.getState().past.length).toBe(before);
    s.updateShape("a", { x: 6 }, true);
    expect(useEditor.getState().past.length).toBe(before + 1);
    expect((useEditor.getState().shapes[0] as Extract<Shape, { type: "rect" }>).x).toBe(6);
  });

  it("addShapes is a single undo step", () => {
    const s = useEditor.getState();
    s.addShapes([rect("a"), rect("b"), rect("c")]);
    expect(useEditor.getState().shapes).toHaveLength(3);
    s.undo();
    expect(useEditor.getState().shapes).toHaveLength(0);
  });

  it("nextStepIndex continues after the largest existing number", () => {
    const s = useEditor.getState();
    s.addShape(step("s1", 1));
    s.addShape(step("s2", 2));
    s.addShape(step("s3", 3));
    // 删掉中间的 2 号后，新序号必须是 4 而不是重复的 3
    s.removeShape("s2");
    expect(useEditor.getState().nextStepIndex()).toBe(4);
    s.removeShape("s3");
    expect(useEditor.getState().nextStepIndex()).toBe(2);
  });

  it("removeShape drops selection of the removed shape", () => {
    const s = useEditor.getState();
    s.addShape(rect("a"));
    s.setSelected("a");
    s.removeShape("a");
    expect(useEditor.getState().selectedId).toBeNull();
  });

  it("setColor / setStrokeWidth also patch the selected shape", () => {
    const s = useEditor.getState();
    s.addShape(rect("a"));
    s.setSelected("a");
    s.setColor("#123456");
    s.setStrokeWidth(8);
    const shape = useEditor.getState().shapes[0];
    expect(shape.stroke).toBe("#123456");
    expect(shape.strokeWidth).toBe(8);
  });
});
