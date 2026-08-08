import { afterEach, describe, expect, it, vi } from "vitest";

import type { BucketTerrainLayout } from "./bucket-terrain";
import { TerrainSelectionView } from "./terrain-selection-view";

const layout = (): BucketTerrainLayout<"section", "region", null> =>
  ({
    width: 100,
    height: 50,
    glyphs: [
      {
        txid: "01".repeat(32),
        sectionKey: "section",
        regionKey: "region",
        rect: { x: 10, y: 12, width: 8, height: 6 },
      },
    ],
  }) as unknown as BucketTerrainLayout<"section", "region", null>;

afterEach(() => {
  vi.unstubAllGlobals();
});

describe("TerrainSelectionView", () => {
  it("restores one captured terrain and paints only the selected glyph", () => {
    const baseContext = {
      globalAlpha: 1,
      setTransform: vi.fn(),
      drawImage: vi.fn(),
    } as unknown as CanvasRenderingContext2D;
    const baseCanvas = {
      width: 0,
      height: 0,
      getContext: () => baseContext,
    } as unknown as HTMLCanvasElement;
    vi.stubGlobal("document", {
      createElement: vi.fn(() => baseCanvas),
    });
    const context = {
      globalAlpha: 1,
      fillStyle: "",
      strokeStyle: "",
      lineWidth: 1,
      shadowColor: "",
      shadowBlur: 0,
      save: vi.fn(),
      restore: vi.fn(),
      setTransform: vi.fn(),
      drawImage: vi.fn(),
      fillRect: vi.fn(),
      strokeRect: vi.fn(),
    } as unknown as CanvasRenderingContext2D;
    const canvas = {
      width: 200,
      height: 100,
      getContext: () => context,
    } as unknown as HTMLCanvasElement;
    const view = new TerrainSelectionView(canvas);
    const current = layout();

    expect(view.capture(current)).toBe(true);
    expect(baseContext.drawImage).toHaveBeenCalledWith(canvas, 0, 0);
    expect(view.paint(current, current.glyphs[0]!.txid)).toBe(true);
    expect(context.drawImage).toHaveBeenCalledWith(baseCanvas, 0, 0);
    expect(context.fillRect).toHaveBeenCalledWith(10, 12, 8, 6);
    expect(context.strokeRect).toHaveBeenCalledWith(10, 12, 8, 6);
    expect(context.setTransform).toHaveBeenLastCalledWith(2, 0, 0, 2, 0, 0);

    expect(view.paint(layout(), current.glyphs[0]!.txid)).toBe(false);
  });
});
