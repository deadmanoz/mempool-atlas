import { describe, expect, it } from "vitest";

import {
  compareCurrentSnapshots,
  lookupComparisonTransaction,
  requireLoadedSnapshot,
} from "./comparison-model";
import { loadedSource } from "./comparison-test-fixtures";
import { mempoolTransaction, txid } from "./test-fixtures";
import type {
  Bip110Assessment,
  MempoolTransaction,
  SourceSnapshotResponse,
} from "./types";

const transaction = (
  value: number,
  vsize: number,
  bip110: Bip110Assessment | null = null,
  wtxid = txid(value),
): MempoolTransaction =>
  mempoolTransaction(value, {
    wtxid,
    vsize,
    fee_sats: vsize * 2,
    entered_at_ms: 1_699_999_000_000 + value,
    bip110,
  });

describe("compareCurrentSnapshots", () => {
  it("merge-joins sorted membership into three disjoint regions", () => {
    const left = loadedSource(
      "core",
      [transaction(1, 100), transaction(2, 200), transaction(4, 300)],
      1_700_000_001_000,
    );
    const right = loadedSource(
      "knots",
      [transaction(1, 110), transaction(3, 400), transaction(4, 350)],
      1_700_000_004_000,
    );

    const comparison = compareCurrentSnapshots(left, right);

    expect(comparison.common.map(({ txid: id }) => id)).toEqual([
      txid(1),
      txid(4),
    ]);
    expect(comparison.left_only.map(({ txid: id }) => id)).toEqual([txid(2)]);
    expect(comparison.right_only.map(({ txid: id }) => id)).toEqual([txid(3)]);
    const regions = [
      ...comparison.common,
      ...comparison.left_only,
      ...comparison.right_only,
    ].map(({ txid: id }) => id);
    expect(new Set(regions).size).toBe(regions.length);
    for (const entries of [
      comparison.common,
      comparison.left_only,
      comparison.right_only,
    ]) {
      expect(entries.map(({ txid: id }) => id)).toEqual(
        [...entries.map(({ txid: id }) => id)].sort(),
      );
    }
    expect(comparison.totals).toEqual({
      union_count: 4,
      common_count: 2,
      common_left_vsize: 400,
      common_right_vsize: 460,
      left_only_count: 1,
      left_only_vsize: 200,
      right_only_count: 1,
      right_only_vsize: 400,
    });
    expect(comparison.observed_skew_ms).toBe(3_000);
    expect(comparison.earlier_side).toBe("left");
    expect(
      comparison.totals.common_left_vsize + comparison.totals.left_only_vsize,
    ).toBe(left.snapshot.total_vsize);
    expect(
      comparison.totals.common_right_vsize + comparison.totals.right_only_vsize,
    ).toBe(right.snapshot.total_vsize);
  });

  it("retains both witness variants for a common txid", () => {
    const left = loadedSource(
      "core",
      [transaction(1, 100, null, txid(10))],
      1_700_000_001_000,
    );
    const right = loadedSource(
      "knots",
      [transaction(1, 120, null, txid(11))],
      1_700_000_001_000,
    );

    const comparison = compareCurrentSnapshots(left, right);

    expect(comparison.common).toHaveLength(1);
    expect(comparison.common[0]).toMatchObject({
      txid: txid(1),
      same_wtxid: false,
      left: { wtxid: txid(10), vsize: 100 },
      right: { wtxid: txid(11), vsize: 120 },
    });
    expect(comparison.totals.common_left_vsize).toBe(100);
    expect(comparison.totals.common_right_vsize).toBe(120);
  });

  it("looks up source-local entries across every sorted membership region", () => {
    const comparison = compareCurrentSnapshots(
      loadedSource(
        "core",
        [
          transaction(1, 100),
          transaction(2, 200),
          transaction(4, 300),
          transaction(8, 800),
        ],
        1_700_000_001_000,
      ),
      loadedSource(
        "knots",
        [
          transaction(1, 110),
          transaction(3, 300),
          transaction(4, 440),
          transaction(9, 900),
        ],
        1_700_000_002_000,
      ),
    );

    expect(lookupComparisonTransaction(comparison, txid(1))).toEqual({
      region: "common",
      index: 0,
      entry: comparison.common[0],
    });
    expect(lookupComparisonTransaction(comparison, txid(8))).toEqual({
      region: "left_only",
      index: 1,
      entry: comparison.left_only[1],
    });
    expect(lookupComparisonTransaction(comparison, txid(9))).toEqual({
      region: "right_only",
      index: 1,
      entry: comparison.right_only[1],
    });
    expect(
      lookupComparisonTransaction(comparison, txid(4))?.entry,
    ).toMatchObject({
      left: { vsize: 300 },
      right: { vsize: 440 },
      same_wtxid: true,
    });
    expect(lookupComparisonTransaction(comparison, txid(5))).toBeNull();
    expect(lookupComparisonTransaction(comparison, txid(0))).toBeNull();
    expect(lookupComparisonTransaction(comparison, txid(10))).toBeNull();
  });

  it("rejects comparing a source with itself or an unavailable snapshot", () => {
    const source = loadedSource("core", [], 1_700_000_001_000);
    expect(() => compareCurrentSnapshots(source, source)).toThrow(
      "two distinct sources",
    );

    const waiting: SourceSnapshotResponse = {
      source: {
        ...source.source,
        availability: "waiting",
        last_poll_started_at_ms: null,
        snapshot_observed_at_ms: null,
        chain_tip: null,
        transaction_count: null,
        total_vsize: null,
        classification: null,
      },
      snapshot: null,
    };
    expect(() => requireLoadedSnapshot(waiting)).toThrow(
      "has no complete mempool snapshot",
    );
  });
});
