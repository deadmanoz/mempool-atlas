import { afterEach, describe, expect, it, vi } from "vitest";

import { prepareCanvasBacking } from "./canvas-backing";

interface CanvasHarness {
  canvas: HTMLCanvasElement;
  context: CanvasRenderingContext2D;
  dimensions: () => { width: number; height: number };
  resizeWrites: () => { width: number; height: number };
  setCssSize: (width: number, height: number) => void;
}

const canvasHarness = (cssWidth: number, cssHeight: number): CanvasHarness => {
  let visibleWidth = cssWidth;
  let visibleHeight = cssHeight;
  let backingWidth = 0;
  let backingHeight = 0;
  let widthWrites = 0;
  let heightWrites = 0;
  const context = {
    setTransform: vi.fn(),
  } as unknown as CanvasRenderingContext2D;
  const canvas = {
    get width() {
      return backingWidth;
    },
    set width(value: number) {
      backingWidth = value;
      widthWrites += 1;
    },
    get height() {
      return backingHeight;
    },
    set height(value: number) {
      backingHeight = value;
      heightWrites += 1;
    },
    getBoundingClientRect: () => ({
      width: visibleWidth,
      height: visibleHeight,
    }),
    getContext: (kind: string) => (kind === "2d" ? context : null),
  } as unknown as HTMLCanvasElement;

  return {
    canvas,
    context,
    dimensions: () => ({ width: backingWidth, height: backingHeight }),
    resizeWrites: () => ({ width: widthWrites, height: heightWrites }),
    setCssSize: (width, height) => {
      visibleWidth = width;
      visibleHeight = height;
    },
  };
};

afterEach(() => {
  vi.unstubAllGlobals();
});

describe("prepareCanvasBacking", () => {
  it("resizes for DPR changes and reapplies the logical-pixel transform", () => {
    const harness = canvasHarness(120, 60);
    vi.stubGlobal("window", { devicePixelRatio: 2 });

    const first = prepareCanvasBacking(harness.canvas, "subpixel");
    expect(first.pixelRatio).toBe(2);
    expect(harness.dimensions()).toEqual({ width: 240, height: 120 });

    vi.stubGlobal("window", { devicePixelRatio: 3 });
    const second = prepareCanvasBacking(harness.canvas, "subpixel");
    expect(second.pixelRatio).toBe(3);
    expect(harness.dimensions()).toEqual({ width: 360, height: 180 });
    expect(harness.context.setTransform).toHaveBeenLastCalledWith(
      3,
      0,
      0,
      3,
      0,
      0,
    );
  });

  it("resizes only the changed backing dimension after a CSS size change", () => {
    const harness = canvasHarness(100.25, 50.5);
    vi.stubGlobal("window", { devicePixelRatio: 2 });
    prepareCanvasBacking(harness.canvas, "subpixel");

    harness.setCssSize(120.25, 50.5);
    const prepared = prepareCanvasBacking(harness.canvas, "subpixel");

    expect(prepared).toMatchObject({
      width: 120.25,
      height: 50.5,
    });
    expect(harness.dimensions()).toEqual({ width: 241, height: 101 });
    expect(harness.resizeWrites()).toEqual({ width: 2, height: 1 });
  });

  it("clamps hidden or zero-sized canvases to one logical pixel", () => {
    const harness = canvasHarness(0, 0);
    vi.stubGlobal("window", { devicePixelRatio: 2 });

    const prepared = prepareCanvasBacking(harness.canvas, "subpixel");

    expect(prepared).toMatchObject({
      width: 1,
      height: 1,
      pixelRatio: 2,
    });
    expect(harness.dimensions()).toEqual({ width: 2, height: 2 });
  });

  it("reuses a matching backing store without redundant resize writes", () => {
    const harness = canvasHarness(100.6, 50.4);
    vi.stubGlobal("window", { devicePixelRatio: 2 });
    const first = prepareCanvasBacking(harness.canvas, "whole-pixel");

    const second = prepareCanvasBacking(harness.canvas, "whole-pixel");

    expect(first).toMatchObject({
      width: 101,
      height: 50,
    });
    expect(second).toMatchObject({
      width: 101,
      height: 50,
    });
    expect(harness.dimensions()).toEqual({ width: 202, height: 100 });
    expect(harness.resizeWrites()).toEqual({ width: 1, height: 1 });
    expect(harness.context.setTransform).toHaveBeenCalledTimes(2);
  });
});
