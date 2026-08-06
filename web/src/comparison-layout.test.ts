import { describe, expect, it } from "vitest";

import {
  createComparisonLayout,
  hitTestComparison,
  resolveComparisonGeometry,
} from "./comparison-layout";
import {
  compareCurrentSnapshots,
  type LoadedSourceSnapshot,
} from "./comparison-model";
import { mempoolTransaction } from "./test-fixtures";
import type { MempoolSnapshot, MempoolTransaction } from "./types";

const source = (sourceId: string, values: number[]): LoadedSourceSnapshot => {
  const transactions: MempoolTransaction[] = values.map((value) =>
    mempoolTransaction(value, { vsize: 100 + value, fee_sats: 200 + value }),
  );
  const snapshot: MempoolSnapshot = {
    source_id: sourceId,
    source_label: sourceId,
    collection_started_at_ms: 1_700_000_000_000,
    collection_completed_at_ms: 1_700_000_001_000,
    collection_duration_ms: 1_000,
    observed_at_ms: 1_700_000_001_000,
    classification_revision: 0,
    chain_tip: { height: 900_000, hash: "00".repeat(32) },
    transaction_count: transactions.length,
    total_vsize: transactions.reduce((total, entry) => total + entry.vsize, 0),
    classifier_catalog: [],
    classification_summaries: [],
    bip110_summary: {
      evaluator_id: "rdts-rules",
      evaluator_version: "0.1.0",
      scope: "knots_mempool_policy",
      compatible_count: 0,
      violating_count: 0,
      indeterminate_count: 0,
      unclassified_count: transactions.length,
    },
    transactions,
  };
  return {
    source: {
      source_id: sourceId,
      source_label: sourceId,
      availability: "ready",
      poll_interval_seconds: 300,
      last_poll_started_at_ms: snapshot.collection_started_at_ms,
      snapshot_observed_at_ms: snapshot.observed_at_ms,
      chain_tip: snapshot.chain_tip,
      transaction_count: snapshot.transaction_count,
      total_vsize: snapshot.total_vsize,
      classification: {
        state: "complete",
        revision: snapshot.classification_revision,
        classified_count: 0,
        unclassified_count: snapshot.transaction_count,
      },
      last_error: null,
    },
    snapshot,
  };
};

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
    expect(layout.glyphs).toHaveLength(comparison.totals.union_count);
    expect(new Set(layout.glyphs.map(({ txid: id }) => id)).size).toBe(
      comparison.totals.union_count,
    );
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
});
