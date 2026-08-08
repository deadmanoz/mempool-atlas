// @vitest-environment happy-dom

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import {
  ComparisonCanvasView,
  renderLatestComparisonCanvas,
  type ComparisonCanvasRenderStatus,
} from "./comparison-canvas-view";
import {
  compareCurrentSnapshots,
  type ComparisonRegionKey,
} from "./comparison-model";
import { loadedSource } from "./comparison-test-fixtures";
import { mempoolTransaction, txid } from "./test-fixtures";

const comparison = compareCurrentSnapshots(
  loadedSource("left", [mempoolTransaction(1), mempoolTransaction(2)]),
  loadedSource("right", [mempoolTransaction(2), mempoolTransaction(3)]),
);

describe("ComparisonCanvasView", () => {
  const context = {
    beginPath: vi.fn(),
    clearRect: vi.fn(),
    drawImage: vi.fn(),
    fill: vi.fn(),
    fillRect: vi.fn(),
    fillText: vi.fn(),
    rect: vi.fn(),
    restore: vi.fn(),
    save: vi.fn(),
    setTransform: vi.fn(),
    stroke: vi.fn(),
    strokeRect: vi.fn(),
    fillStyle: "",
    font: "",
    globalAlpha: 1,
    lineWidth: 1,
    strokeStyle: "",
    textBaseline: "alphabetic",
  } as unknown as CanvasRenderingContext2D;

  beforeEach(() => {
    vi.clearAllMocks();
    vi.spyOn(HTMLCanvasElement.prototype, "getContext").mockImplementation(
      () => context,
    );
    let frame = 0;
    vi.spyOn(window, "requestAnimationFrame").mockImplementation((callback) => {
      const handle = ++frame;
      queueMicrotask(() => callback(performance.now()));
      return handle;
    });
    vi.spyOn(window, "cancelAnimationFrame").mockImplementation(() => {});
  });

  afterEach(() => {
    vi.restoreAllMocks();
  });

  it("reuses the population base for transaction-only transitions and repaints explicit policy focus", async () => {
    const canvas = document.createElement("canvas");
    canvas.getBoundingClientRect = () =>
      ({ width: 900, height: 500 }) as DOMRect;
    const view = new ComparisonCanvasView(canvas);

    await view.render(comparison, "common", null, "left", { kind: "all" });
    const populationRects = vi.mocked(context.rect).mock.calls.length;
    const baseCopies = vi.mocked(context.drawImage).mock.calls.length;

    await view.render(comparison, "common", txid(2), "left", { kind: "all" });
    expect(vi.mocked(context.rect).mock.calls).toHaveLength(populationRects);
    expect(vi.mocked(context.drawImage).mock.calls).toHaveLength(
      baseCopies + 1,
    );

    await view.render(comparison, "common", txid(2), "left", { kind: "all" });
    expect(vi.mocked(context.rect).mock.calls).toHaveLength(populationRects);
    expect(vi.mocked(context.drawImage).mock.calls).toHaveLength(
      baseCopies + 1,
    );

    await view.render(comparison, "common", txid(2), "left", {
      kind: "status",
      status: "compatible",
    });
    expect(vi.mocked(context.rect).mock.calls.length).toBeGreaterThan(
      populationRects,
    );
    const focusedPopulationRects = vi.mocked(context.rect).mock.calls.length;

    await view.render(comparison, "common", txid(1), "left", {
      kind: "status",
      status: "compatible",
    });
    expect(vi.mocked(context.rect).mock.calls).toHaveLength(
      focusedPopulationRects,
    );
  });

  it("reports an aborted population render as superseded", async () => {
    const frames: FrameRequestCallback[] = [];
    vi.mocked(window.requestAnimationFrame).mockImplementation((callback) => {
      frames.push(callback);
      return frames.length;
    });
    const canvas = document.createElement("canvas");
    canvas.getBoundingClientRect = () =>
      ({ width: 900, height: 500 }) as DOMRect;
    const view = new ComparisonCanvasView(canvas);

    const stale = view.render(comparison, "common", null, "left", {
      kind: "all",
    });
    const current = view.render(comparison, "left_only", null, "left", {
      kind: "all",
    });

    await expect(stale).resolves.toBe("superseded");
    while (frames.length > 0) {
      frames.shift()?.(performance.now());
      await Promise.resolve();
    }
    await expect(current).resolves.toBe("rendered");
  });

  it("settles the latest region after repeated same-publication churn", async () => {
    let region: ComparisonRegionKey = "common";
    const pending: Array<{
      region: ComparisonRegionKey;
      resolve: (status: ComparisonCanvasRenderStatus) => void;
    }> = [];
    const render = vi.fn(
      () =>
        new Promise<ComparisonCanvasRenderStatus>((resolve) => {
          pending.push({ region, resolve });
        }),
    );
    const settled = renderLatestComparisonCanvas(
      () => true,
      () => region,
      render,
    );
    const changes: ComparisonRegionKey[] = [
      "left_only",
      "common",
      "right_only",
      "left_only",
      "common",
    ];

    for (let index = 0; index < changes.length; index += 1) {
      await vi.waitFor(() => expect(pending).toHaveLength(index + 1));
      region = changes[index]!;
      pending[index]!.resolve("superseded");
    }
    await vi.waitFor(() => expect(pending).toHaveLength(changes.length + 1));
    expect(pending.at(-1)?.region).toBe("common");
    pending.at(-1)!.resolve("rendered");

    await expect(settled).resolves.toBeUndefined();
    expect(render).toHaveBeenCalledTimes(changes.length + 1);
  });
});
