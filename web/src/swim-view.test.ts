import { describe, expect, it } from "vitest";

import {
  AGE_COLUMNS,
  FEE_RATE_LANES,
  ageColumnIndex,
  createSwimLayout,
  feeRateLaneIndex,
  paintMembershipGlyphs,
} from "./swim-view";
import type { MempoolEntry } from "./types";

const MINUTE_MS = 60_000;
const HOUR_MS = 60 * MINUTE_MS;
const DAY_MS = 24 * HOUR_MS;
const NOW_MS = 1_700_000_000_000;

const availableMembership = (
  value: number,
  overrides: Partial<
    Extract<MempoolEntry["facts"], { status: "available" }>
  > = {},
): MempoolEntry => ({
  txid: value.toString(16).padStart(64, "0"),
  facts: {
    status: "available",
    vsize: 250,
    fee_sats: 2_000,
    entered_at_ms: NOW_MS - HOUR_MS,
    ...overrides,
  },
});

const awaitingMembership = (value: number): MempoolEntry => ({
  txid: value.toString(16).padStart(64, "0"),
  facts: { status: "awaiting_rpc" },
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
    const membership = availableMembership(1, {
      fee_sats: 4_000,
      entered_at_ms: NOW_MS - 2 * HOUR_MS,
    });
    const first = createSwimLayout([membership], 1_200, 640, NOW_MS);
    const second = createSwimLayout([membership], 1_200, 640, NOW_MS);
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

  it("keeps awaiting facts out of numeric fee and age cells", () => {
    const layout = createSwimLayout(
      [awaitingMembership(1)],
      1_200,
      640,
      NOW_MS,
    );
    const glyph = layout.awaitingBatch.glyphs[0];

    expect(layout.summary).toMatchObject({
      availableCount: 0,
      awaitingCount: 1,
      totalVsize: 0,
    });
    expect(glyph?.feeLaneIndex).toBeNull();
    expect(glyph?.ageColumnIndex).toBeNull();
    expect(glyph?.y).toBeGreaterThanOrEqual(layout.geometry.awaitingTop);
    expect((glyph?.y ?? 0) + (glyph?.size ?? 0)).toBeLessThanOrEqual(
      layout.geometry.awaitingTop + layout.geometry.awaitingHeight,
    );
  });

  it("keeps glyph sizes monotonic and within rendering bounds", () => {
    const memberships = Array.from({ length: 10_000 }, (_, index) =>
      availableMembership(index + 1),
    );
    memberships.push(availableMembership(10_001, { vsize: 1_000 }));
    const layout = createSwimLayout(memberships, 1_200, 640, NOW_MS);
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
      [
        availableMembership(1, { vsize: 100 }),
        availableMembership(2, { vsize: 1_000 }),
      ],
      1_200,
      640,
      NOW_MS,
    );
    const glyphs = layout.batches.flatMap((batch) => batch.glyphs);

    expect(glyphs).toHaveLength(2);
    expect(glyphs[1]?.size).toBeGreaterThan(glyphs[0]?.size ?? 0);
    expect((glyphs[1]?.size ?? 0) ** 2).toBeCloseTo(
      (glyphs[0]?.size ?? 0) ** 2 * 10,
      8,
    );
  });

  it("paints one Canvas glyph for each of 70,770 memberships", () => {
    const memberships = Array.from({ length: 70_770 }, (_, index) =>
      index % 10 === 0
        ? awaitingMembership(index + 1)
        : availableMembership(index + 1, {
            vsize: 120 + (index % 4_000),
            fee_sats: 120 + (index % 4_000) * (1 + (index % 160)),
            entered_at_ms: NOW_MS - (index % (5 * DAY_MS)),
          }),
    );
    const layout = createSwimLayout(memberships, 1_200, 640, NOW_MS);
    let paintedGlyphs = 0;
    const context: Pick<CanvasRenderingContext2D, "fillRect" | "fillStyle"> = {
      fillStyle: "",
      fillRect: () => {
        paintedGlyphs += 1;
      },
    };

    paintMembershipGlyphs(context, layout);

    expect(paintedGlyphs).toBe(70_770);
    expect(layout.summary.availableCount + layout.summary.awaitingCount).toBe(
      70_770,
    );
    expect(layout.summary.feeLaneCounts).toHaveLength(FEE_RATE_LANES.length);
    expect(layout.summary.ageColumnCounts).toHaveLength(AGE_COLUMNS.length);
  });
});
