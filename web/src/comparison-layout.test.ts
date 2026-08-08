import { describe, expect, it } from "vitest";

import {
  COMPARISON_PAINT_BATCH_SIZE,
  comparisonPaintBatches,
  createComparisonLayout,
  hitTestComparison,
  paintComparison,
  resolveComparisonGeometry,
} from "./comparison-layout";
import { compareCurrentSnapshots } from "./comparison-model";
import { loadedSource } from "./comparison-test-fixtures";
import { mempoolTransaction, txid } from "./test-fixtures";

const source = (sourceId: string, values: number[]) =>
  loadedSource(
    sourceId,
    values.map((value) =>
      mempoolTransaction(value, { vsize: 100 + value, fee_sats: 200 + value }),
    ),
  );

describe("comparison layout", () => {
  it("allocates positive regions and exactly one glyph per union txid", () => {
    const comparison = compareCurrentSnapshots(
      source("left", [1, 2, 3, 4]),
      source("right", [3, 4, 5]),
    );
    const layout = createComparisonLayout(comparison, 900, 500);

    expect(layout.regions.map(({ key }) => key)).toEqual([
      "left_only",
      "common",
      "right_only",
    ]);
    expect(layout.regions.every(({ rect }) => rect.width > 0)).toBe(true);
    expect(
      layout.regions.reduce(
        (count, region) => count + region.transactionCount,
        0,
      ),
    ).toBe(comparison.totals.union_count);
    expect(
      new Set(
        layout.regions.flatMap((region) =>
          Array.from(region.entries, ({ txid }) => txid),
        ),
      ).size,
    ).toBe(comparison.totals.union_count);
  });

  it("keeps empty source-only regions selectable", () => {
    const comparison = compareCurrentSnapshots(
      source("left", [1, 2]),
      source("right", [1, 2]),
    );
    const layout = createComparisonLayout(comparison, 320, 240);

    const empty = layout.regions.find(({ key }) => key === "left_only");
    expect(empty?.transactionCount).toBe(0);
    expect(empty?.rect.width).toBeGreaterThan(0);
    if (empty === undefined) {
      throw new Error("missing left-only region");
    }
    expect(
      hitTestComparison(
        layout,
        empty.rect.x + empty.rect.width / 2,
        empty.rect.y + 8,
      ),
    ).toMatchObject({ kind: "region", region: { key: "left_only" } });
  });

  it("reuses geometry when only the painted selection changes", () => {
    const comparison = compareCurrentSnapshots(
      source("left", [1, 2, 3]),
      source("right", [2, 3, 4]),
    );
    const first = resolveComparisonGeometry(comparison, 900, 500, 2, null);
    const selectionOnly = resolveComparisonGeometry(
      comparison,
      900,
      500,
      2,
      first.geometry,
    );

    expect(selectionOnly.reusedGeometry).toBe(true);
    expect(selectionOnly.geometry.layout).toBe(first.geometry.layout);
    expect(
      resolveComparisonGeometry(comparison, 901, 500, 2, first.geometry)
        .reusedGeometry,
    ).toBe(false);
    expect(
      resolveComparisonGeometry(comparison, 900, 500, 3, first.geometry)
        .reusedGeometry,
    ).toBe(false);
    const replacement = compareCurrentSnapshots(
      source("left", [1, 2, 3]),
      source("right", [2, 3, 4]),
    );
    expect(
      resolveComparisonGeometry(replacement, 900, 500, 2, first.geometry)
        .reusedGeometry,
    ).toBe(false);
  });

  it("bounds population and source-difference work in progressive paint batches", () => {
    const comparison = compareCurrentSnapshots(
      source(
        "left",
        Array.from({ length: 20_000 }, (_, index) => index + 1),
      ),
      source(
        "right",
        Array.from({ length: 20_000 }, (_, index) => index + 1),
      ),
    );
    const layout = createComparisonLayout(comparison, 900, 500);
    const batches = comparisonPaintBatches(layout);

    expect(
      batches.every(
        ({ start, end }) => end - start <= COMPARISON_PAINT_BATCH_SIZE,
      ),
    ).toBe(true);
    expect(
      batches
        .filter(({ kind }) => kind === "population")
        .reduce((count, { start, end }) => count + end - start, 0),
    ).toBe(comparison.totals.union_count);
    expect(
      batches
        .filter(({ kind }) => kind === "source-difference")
        .reduce((count, { start, end }) => count + end - start, 0),
    ).toBe(comparison.totals.common_count);
  });

  it("paints unfiltered source differences without materializing comparison entries", () => {
    const comparison = compareCurrentSnapshots(
      loadedSource("left", [mempoolTransaction(1)]),
      loadedSource("right", [mempoolTransaction(1, { wtxid: txid(2) })]),
    );
    const layout = createComparisonLayout(comparison, 900, 500);
    const common = layout.regions.find(({ key }) => key === "common");
    if (common === undefined) throw new Error("missing common region");
    let entryReads = 0;
    common.entries = new Proxy(common.entries, {
      get(target, property, receiver) {
        if (
          typeof property === "string" &&
          /^(0|[1-9][0-9]*)$/.test(property)
        ) {
          entryReads += 1;
        }
        return Reflect.get(target, property, receiver);
      },
    });
    const noOp = () => undefined;
    const context = {
      beginPath: noOp,
      clearRect: noOp,
      fill: noOp,
      fillRect: noOp,
      fillText: noOp,
      rect: noOp,
      stroke: noOp,
      strokeRect: noOp,
    } as unknown as CanvasRenderingContext2D;

    paintComparison(context, layout, comparison, "common", "left", {
      kind: "all",
    });

    expect(entryReads).toBe(0);
  });
});
