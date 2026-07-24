import { describe, expect, it } from "vitest";

import {
  AGE_COLUMNS,
  FEE_RATE_LANES,
  ageColumnIndex,
  createSwimLayout,
  feeRateLaneIndex,
  paintMembershipGlyphs,
} from "./swim-view";
import type { MempoolTransaction } from "./types";

const MINUTE_MS = 60_000;
const HOUR_MS = 60 * MINUTE_MS;
const DAY_MS = 24 * HOUR_MS;
const OBSERVED_AT_MS = 1_700_000_000_000;

const transaction = (
  value: number,
  overrides: Partial<MempoolTransaction> = {},
): MempoolTransaction => ({
  txid: value.toString(16).padStart(64, "0"),
  wtxid: value.toString(16).padStart(64, "0"),
  vsize: 250,
  fee_sats: 2_000,
  entered_at_ms: OBSERVED_AT_MS - HOUR_MS,
  bip110: {
    status: "compatible",
    primary_rule: null,
    violated_rules: [],
    unknown_rules: [],
  },
  ...overrides,
});

describe("feeRateLaneIndex", () => {
  it.each([
    [128, 0],
    [127.999, 1],
    [64, 1],
    [32, 2],
    [2, 6],
    [1, 7],
    [0.999, 8],
    [0, 8],
  ])("places %s sat/vB in lane %s", (feeRate, expectedLane) => {
    expect(feeRateLaneIndex(feeRate)).toBe(expectedLane);
  });
});

describe("ageColumnIndex", () => {
  it.each([
    [3 * DAY_MS, 0],
    [3 * DAY_MS - 1, 1],
    [DAY_MS, 1],
    [6 * HOUR_MS, 2],
    [HOUR_MS, 3],
    [10 * MINUTE_MS, 4],
    [0, 5],
    [-1, 5],
  ])("places age %s in column %s", (ageMs, expectedColumn) => {
    expect(ageColumnIndex(ageMs)).toBe(expectedColumn);
  });
});

describe("createSwimLayout", () => {
  it("uses deterministic positions within the matching fee and age cell", () => {
    const value = transaction(1, {
      fee_sats: 4_000,
      entered_at_ms: OBSERVED_AT_MS - 2 * HOUR_MS,
    });
    const first = createSwimLayout([value], 1_200, 640, OBSERVED_AT_MS);
    const second = createSwimLayout([value], 1_200, 640, OBSERVED_AT_MS);
    const glyph = first.batches.flatMap((batch) => batch.glyphs)[0];
    const repeatedGlyph = second.batches.flatMap((batch) => batch.glyphs)[0];

    expect(glyph).toEqual(repeatedGlyph);
    expect(glyph?.feeLaneIndex).toBe(feeRateLaneIndex(16));
    expect(glyph?.ageColumnIndex).toBe(ageColumnIndex(2 * HOUR_MS));

    const laneIndex = glyph?.feeLaneIndex ?? -1;
    const columnIndex = glyph?.ageColumnIndex ?? -1;
    const cellLeft =
      first.geometry.plotLeft + columnIndex * first.geometry.columnWidth;
    const cellTop =
      first.geometry.plotTop + laneIndex * first.geometry.laneHeight;
    expect(glyph?.x).toBeGreaterThanOrEqual(cellLeft);
    expect((glyph?.x ?? 0) + (glyph?.size ?? 0)).toBeLessThanOrEqual(
      cellLeft + first.geometry.columnWidth,
    );
    expect(glyph?.y).toBeGreaterThanOrEqual(cellTop);
    expect((glyph?.y ?? 0) + (glyph?.size ?? 0)).toBeLessThanOrEqual(
      cellTop + first.geometry.laneHeight,
    );
  });

  it("keeps glyph sizes monotonic and within rendering bounds", () => {
    const transactions = Array.from({ length: 10_000 }, (_, index) =>
      transaction(index + 1),
    );
    transactions.push(transaction(10_001, { vsize: 1_000 }));
    const layout = createSwimLayout(transactions, 1_200, 640, OBSERVED_AT_MS);
    const glyphs = layout.batches.flatMap((batch) => batch.glyphs);
    const regular = glyphs.find((glyph) => glyph.size < 14);
    const large = glyphs.at(-1);

    expect(regular?.size).toBeGreaterThanOrEqual(1.25);
    expect(large?.size).toBeGreaterThan(regular?.size ?? 0);
    expect(Math.max(...glyphs.map((glyph) => glyph.size))).toBeLessThanOrEqual(
      14,
    );
  });

  it("preserves relative vsize in a sparse snapshot", () => {
    const layout = createSwimLayout(
      [transaction(1, { vsize: 100 }), transaction(2, { vsize: 1_000 })],
      1_200,
      640,
      OBSERVED_AT_MS,
    );
    const glyphs = layout.batches.flatMap((batch) => batch.glyphs);

    expect(glyphs).toHaveLength(2);
    expect(glyphs[1]?.size).toBeGreaterThan(glyphs[0]?.size ?? 0);
    expect((glyphs[1]?.size ?? 0) ** 2).toBeCloseTo(
      (glyphs[0]?.size ?? 0) ** 2 * 10,
      8,
    );
  });

  it("paints one Canvas glyph for each transaction at the 200,000-entry limit", () => {
    const transactions = Array.from({ length: 200_000 }, (_, index) =>
      transaction(index + 1, {
        vsize: 120 + (index % 4_000),
        fee_sats: 120 + (index % 4_000) * (1 + (index % 160)),
        entered_at_ms: OBSERVED_AT_MS - (index % (5 * DAY_MS)),
      }),
    );
    const layout = createSwimLayout(transactions, 1_200, 640, OBSERVED_AT_MS);
    let paintedGlyphs = 0;
    const context: Pick<CanvasRenderingContext2D, "fillRect" | "fillStyle"> = {
      fillStyle: "",
      fillRect: () => {
        paintedGlyphs += 1;
      },
    };

    paintMembershipGlyphs(context, layout);

    expect(paintedGlyphs).toBe(200_000);
    expect(layout.summary.transactionCount).toBe(200_000);
    expect(layout.summary.feeLaneCounts).toHaveLength(FEE_RATE_LANES.length);
    expect(layout.summary.ageColumnCounts).toHaveLength(AGE_COLUMNS.length);
  });
});
