import { describe, expect, it } from "vitest";

import {
  comparisonPolicyFilterMatches,
  compareCurrentSnapshots,
  lookupComparisonTransaction,
  type ComparedTransaction,
  type CurrentComparison,
} from "./comparison-model";
import { loadedSource } from "./comparison-test-fixtures";
import { mempoolTransaction, txid } from "./test-fixtures";
import type { Bip110Assessment, MempoolTransaction } from "./types";

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

describe("comparisonPolicyFilterMatches", () => {
  const violating: Bip110Assessment = {
    status: "violating",
    primary_rule: "element_size",
    violated_rules: ["element_size"],
    unknown_rules: ["output_size"],
  };
  const assessed = transaction(1, 100, violating);
  const unassessed = transaction(2, 100, null);

  it("matches status, marginal rule, and signature filters", () => {
    expect(comparisonPolicyFilterMatches(assessed, { kind: "all" })).toBe(true);
    expect(
      comparisonPolicyFilterMatches(assessed, {
        kind: "status",
        status: "violating",
      }),
    ).toBe(true);
    expect(
      comparisonPolicyFilterMatches(assessed, {
        kind: "rule",
        rule: "element_size",
      }),
    ).toBe(true);
    expect(
      comparisonPolicyFilterMatches(assessed, {
        kind: "rule",
        rule: "output_size",
      }),
    ).toBe(false);
    expect(
      comparisonPolicyFilterMatches(assessed, {
        kind: "signature",
        signature: "partial:02:01",
      }),
    ).toBe(true);
    expect(
      comparisonPolicyFilterMatches(unassessed, {
        kind: "status",
        status: "unclassified",
      }),
    ).toBe(true);
  });
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
    expect(comparison.common_differing_wtxids).toEqual(Uint8Array.of(1));
    expect(comparison.totals.common_left_vsize).toBe(100);
    expect(comparison.totals.common_right_vsize).toBe(120);
  });

  it("defers witness-variant comparison until membership is ready", () => {
    const comparison = compareCurrentSnapshots(
      loadedSource(
        "core",
        [transaction(1, 100, null, txid(10))],
        1_700_000_001_000,
      ),
      loadedSource(
        "knots",
        [transaction(1, 120, null, txid(11))],
        1_700_000_001_000,
      ),
      false,
    );

    expect(comparison.common[0]?.same_wtxid).toBeNull();
    expect(comparison.common_differing_wtxids).toEqual(Uint8Array.of(0));
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
    expect(lookupComparisonTransaction(comparison, txid(1))?.region).toBe(
      "common",
    );
  });

  it("looks up a rendered region logarithmically without iterating it", () => {
    const entries = Array.from(
      { length: 1_024 },
      (_, index): ComparedTransaction => ({
        txid: txid(index),
        left: null,
        right: null,
        same_wtxid: null,
      }),
    );
    let indexedReads = 0;
    const guardedEntries = new Proxy(entries, {
      get(target, property, receiver) {
        if (property === Symbol.iterator || property === "find") {
          throw new Error("comparison lookup must not iterate a region");
        }
        if (
          typeof property === "string" &&
          /^(0|[1-9][0-9]*)$/.test(property)
        ) {
          indexedReads += 1;
        }
        return Reflect.get(target, property, receiver);
      },
    });
    const comparison = {
      common: guardedEntries,
      left_only: [],
      right_only: [],
    } as unknown as CurrentComparison;

    expect(lookupComparisonTransaction(comparison, txid(777))).toMatchObject({
      region: "common",
      entry: { txid: txid(777) },
    });
    expect(indexedReads).toBeLessThan(20);
  });

  it("rejects comparing a source with itself", () => {
    const source = loadedSource("core", [], 1_700_000_001_000);
    expect(() => compareCurrentSnapshots(source, source)).toThrow(
      "two distinct sources",
    );
  });
});
