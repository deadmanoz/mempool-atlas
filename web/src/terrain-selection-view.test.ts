// @vitest-environment happy-dom

import { describe, expect, it } from "vitest";

import type { BucketTerrainLayout } from "./bucket-terrain";
import { TerrainSelectionView } from "./terrain-selection-view";

const firstTxid = "01".repeat(32);
const secondTxid = "02".repeat(32);

const layout = (): BucketTerrainLayout<"section", "region", null> =>
  ({
    width: 100,
    height: 50,
    glyphs: [
      {
        txid: firstTxid,
        sectionKey: "section",
        regionKey: "region",
        rect: { x: 10, y: 12, width: 8, height: 6 },
      },
      {
        txid: secondTxid,
        sectionKey: "section",
        regionKey: "region",
        rect: { x: 60, y: 30, width: 5, height: 4 },
      },
    ],
  }) as unknown as BucketTerrainLayout<"section", "region", null>;

describe("TerrainSelectionView", () => {
  it("moves one marker without touching the population canvas", () => {
    const canvas = document.createElement("canvas");
    canvas.getBoundingClientRect = () =>
      ({ width: 200, height: 100 }) as DOMRect;
    const marker = document.createElement("div");
    marker.hidden = true;
    const view = new TerrainSelectionView(canvas, marker);
    const current = layout();

    expect(view.capture(current)).toBe(true);
    expect(marker.hidden).toBe(true);

    expect(view.paint(current, firstTxid)).toBe(true);
    expect(marker.hidden).toBe(false);
    expect(marker.style.left).toBe("10%");
    expect(marker.style.top).toBe("24%");
    expect(marker.style.width).toBe("8%");
    expect(marker.style.height).toBe("12%");

    expect(view.paint(current, secondTxid)).toBe(true);
    expect(marker.hidden).toBe(false);
    expect(marker.style.left).toBe("60%");
    expect(marker.style.top).toBe("60%");
    expect(marker.style.width).toBe("5%");
    expect(marker.style.height).toBe("8%");

    expect(view.paint(current, null)).toBe(true);
    expect(marker.hidden).toBe(true);
    expect(marker.style.left).toBe("");
    expect(view.paint(layout(), firstTxid)).toBe(false);
  });
});
