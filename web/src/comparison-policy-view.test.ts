import { describe, expect, it } from "vitest";

import { compareCurrentSnapshots } from "./comparison-model";
import { loadedSource, violating } from "./comparison-test-fixtures";
import { mempoolTransaction } from "./test-fixtures";
import {
  COMPARISON_POLICY_MATRIX_CHIP_LIMIT,
  buildComparisonPolicyView,
  buildComparisonPolicyViewCooperatively,
  comparisonPolicyMatrixRowPresentation,
  comparisonPolicyMatrixTarget,
} from "./comparison-policy-view";
import type { Bip110Assessment, MempoolTransaction } from "./types";

const assessment = (
  status: "compatible" | "indeterminate",
): Bip110Assessment => ({
  status,
  primary_rule: null,
  violated_rules: [],
  unknown_rules: status === "indeterminate" ? ["output_size"] : [],
});

const transaction = (
  value: number,
  bip110: Bip110Assessment | null,
): MempoolTransaction =>
  mempoolTransaction(value, {
    vsize: 100 + value,
    fee_sats: 200 + value,
    entered_at_ms: 1_700_000_000_000 + value,
    bip110,
  });

describe("comparison policy matrix", () => {
  it("derives four fixed per-node rows whose statuses conserve population", () => {
    const comparison = compareCurrentSnapshots(
      loadedSource("core", [
        transaction(1, assessment("compatible")),
        transaction(2, violating(["element_size"])),
        transaction(3, assessment("indeterminate")),
        transaction(5, null),
        transaction(7, violating(["element_size"], ["tapscript_op_if"])),
      ]),
      loadedSource("knots", [
        transaction(1, violating(["tapscript_op_if"])),
        transaction(3, assessment("compatible")),
        transaction(4, null),
        transaction(5, assessment("indeterminate")),
        transaction(7, violating(["element_size", "tapscript_op_if"])),
      ]),
    );

    const matrix = buildComparisonPolicyView(comparison);

    expect(matrix.rows.map(({ key }) => key)).toEqual([
      "left_only_left",
      "common_left",
      "common_right",
      "right_only_right",
    ]);
    expect(
      matrix.rows.map(({ region, side, sourceId, populationCount }) => ({
        region,
        side,
        sourceId,
        populationCount,
      })),
    ).toEqual([
      {
        region: "left_only",
        side: "left",
        sourceId: "core",
        populationCount: 1,
      },
      {
        region: "common",
        side: "left",
        sourceId: "core",
        populationCount: 4,
      },
      {
        region: "common",
        side: "right",
        sourceId: "knots",
        populationCount: 4,
      },
      {
        region: "right_only",
        side: "right",
        sourceId: "knots",
        populationCount: 1,
      },
    ]);
    expect(matrix.rows[1]?.statusCounts).toEqual({
      compatible: 1,
      violating: 1,
      indeterminate: 1,
      unclassified: 1,
    });
    expect(matrix.rows[1]).toMatchObject({
      exactViolationCount: 0,
      partialViolationCount: 1,
    });
    expect(matrix.rows[2]?.statusCounts).toEqual({
      compatible: 1,
      violating: 2,
      indeterminate: 1,
      unclassified: 0,
    });
    for (const row of matrix.rows) {
      expect(
        row.statusCounts.compatible +
          row.statusCounts.violating +
          row.statusCounts.indeterminate +
          row.statusCounts.unclassified,
      ).toBe(row.populationCount);
      expect(row.exactViolationCount + row.partialViolationCount).toBe(
        row.statusCounts.violating,
      );
    }
    expect(
      matrix.rows.reduce((total, row) => total + row.populationCount, 0),
    ).toBe(
      comparison.left.snapshot.transaction_count +
        comparison.right.snapshot.transaction_count,
    );
  });

  it("describes rows as source policy views without claiming every member was assessed", () => {
    const comparison = compareCurrentSnapshots(
      loadedSource("core", [transaction(1, null)]),
      loadedSource("knots", [transaction(1, assessment("compatible"))]),
    );
    const row = buildComparisonPolicyView(comparison).rows[1];
    if (row === undefined) {
      throw new Error("missing common Core row");
    }

    const presentation = comparisonPolicyMatrixRowPresentation(
      row,
      "Paused",
      (count) => `${count} transaction ${count === 1 ? "ID" : "IDs"}`,
    );

    expect(presentation).toEqual({
      label: "Present in both snapshots",
      detail: "Policy view for CORE node · 1 transaction ID · Paused",
      ariaContext: "Present in both snapshots, policy view for CORE node",
    });
    expect(Object.values(presentation).join(" ").toLowerCase()).not.toContain(
      "assessed by",
    );
  });

  it("keeps all four rows explicit when both snapshots are empty", () => {
    const matrix = buildComparisonPolicyView(
      compareCurrentSnapshots(
        loadedSource("core", []),
        loadedSource("knots", []),
      ),
    );

    expect(matrix.rows).toHaveLength(4);
    expect(
      matrix.rows.every(
        (row) =>
          row.populationCount === 0 &&
          Object.values(row.statusCounts).every((count) => count === 0) &&
          row.dominantExactCombinations.length === 0,
      ),
    ).toBe(true);
  });

  it("bounds dominant exact combinations and sorts ties by canonical signature", () => {
    const leftTransactions = [
      transaction(1, violating(["element_size"])),
      transaction(2, violating(["element_size"])),
      transaction(3, violating(["element_size"])),
      transaction(4, violating(["tapscript_op_if"])),
      transaction(5, violating(["tapscript_op_if"])),
      transaction(6, violating(["element_size", "tapscript_op_if"])),
      transaction(7, violating(["tapscript_op_if", "element_size"])),
      transaction(8, violating(["output_size"])),
      transaction(9, violating(["op_success"])),
      transaction(10, violating(["output_size"], ["tapscript_op_if"])),
      transaction(11, violating(["element_size"], ["op_success"])),
    ];
    const comparison = compareCurrentSnapshots(
      loadedSource("core", leftTransactions),
      loadedSource(
        "knots",
        leftTransactions.map((entry) => ({ ...entry })),
      ),
    );

    const row = buildComparisonPolicyView(comparison).rows[1];

    expect(COMPARISON_POLICY_MATRIX_CHIP_LIMIT).toBe(3);
    expect(row?.dominantExactCombinations).toHaveLength(3);
    expect(
      row?.dominantExactCombinations.map(({ signature, count }) => ({
        signature: signature.key,
        count,
      })),
    ).toEqual([
      { signature: "exact:02", count: 3 },
      { signature: "exact:40", count: 2 },
      { signature: "exact:42", count: 2 },
    ]);
    expect(row).toMatchObject({
      populationCount: 11,
      statusCounts: { violating: 11 },
      exactViolationCount: 9,
      partialViolationCount: 2,
      exactCombinationOverflow: {
        combinationCount: 2,
        transactionCount: 2,
      },
    });
    expect(
      row?.dominantExactCombinations.flatMap(
        ({ signature }) => signature.unknownRules,
      ),
    ).toEqual([]);
  });

  it("builds canonical status and signature action targets", () => {
    const comparison = compareCurrentSnapshots(
      loadedSource("core", [transaction(1, violating(["element_size"]))]),
      loadedSource("knots", [transaction(1, assessment("compatible"))]),
    );
    const matrix = buildComparisonPolicyView(comparison);

    expect(
      comparisonPolicyMatrixTarget(matrix.rows[1]!, {
        kind: "status",
        status: "violating",
      }),
    ).toEqual({
      region: "common",
      side: "left",
      filter: { kind: "status", status: "violating" },
      txid: null,
    });
    expect(
      comparisonPolicyMatrixTarget(matrix.rows[2]!, {
        kind: "status",
        status: "compatible",
      }),
    ).toEqual({
      region: "common",
      side: "right",
      filter: { kind: "status", status: "compatible" },
      txid: null,
    });
    expect(
      comparisonPolicyMatrixTarget(matrix.rows[0]!, {
        kind: "signature",
        signature: "exact:02",
      }),
    ).toEqual({
      region: "left_only",
      side: "left",
      filter: { kind: "signature", signature: "exact:02" },
      txid: null,
    });
  });
});

describe("comparison policy view", () => {
  const comparison = compareCurrentSnapshots(
    loadedSource("core", [
      transaction(1, violating(["element_size", "tapscript_op_if"])),
      transaction(2, violating(["tapscript_op_if"])),
      transaction(3, violating(["element_size"], ["tapscript_op_if"])),
      transaction(5, assessment("compatible")),
    ]),
    loadedSource("knots", [
      { ...transaction(1, violating(["tapscript_op_if"])), vsize: 501 },
      { ...transaction(2, violating(["tapscript_op_if"])), vsize: 502 },
      { ...transaction(3, null), vsize: 503 },
      { ...transaction(4, assessment("indeterminate")), vsize: 504 },
      { ...transaction(5, assessment("compatible")), vsize: 505 },
    ]),
  );

  it("conserves per-node count and vsize across statuses", () => {
    const view = buildComparisonPolicyView(comparison);

    for (const row of view.rows) {
      const slice = view.slice(row.region, row.side);
      expect(Object.keys(slice.ruleTotals)).toHaveLength(7);
      const statuses = Object.values(slice.statusTotals);
      expect(statuses.reduce((total, status) => total + status.count, 0)).toBe(
        slice.population.count,
      );
      expect(statuses.reduce((total, status) => total + status.vsize, 0)).toBe(
        slice.population.vsize,
      );
      expect(
        [...slice.exactSignatures, ...slice.partialSignatures].reduce(
          (total, bucket) => total + bucket.count,
          0,
        ),
      ).toBe(slice.statusTotals.violating.count);
    }
    expect(
      view.rows.reduce((total, row) => total + row.populationCount, 0),
    ).toBe(
      comparison.left.snapshot.transaction_count +
        comparison.right.snapshot.transaction_count,
    );
  });

  it("keeps common source variants and policy aggregates independent", () => {
    const view = buildComparisonPolicyView(comparison);
    const left = view.slice("common", "left");
    const right = view.slice("common", "right");

    expect(left.population).toEqual({ count: 4, vsize: 411 });
    expect(right.population).toEqual({ count: 4, vsize: 2_011 });
    expect(left.statusTotals).toMatchObject({
      compatible: { count: 1, vsize: 105 },
      violating: { count: 3, vsize: 306 },
      unclassified: { count: 0, vsize: 0 },
    });
    expect(right.statusTotals).toMatchObject({
      compatible: { count: 1, vsize: 505 },
      violating: { count: 2, vsize: 1_003 },
      unclassified: { count: 1, vsize: 503 },
    });
  });

  it("keeps marginal rules overlapping and exact signatures separate from partial signatures", () => {
    const view = buildComparisonPolicyView(comparison);
    const left = view.slice("common", "left");
    const right = view.slice("common", "right");

    expect(left.ruleTotals.element_size).toEqual({ count: 2, vsize: 204 });
    expect(left.ruleTotals.tapscript_op_if).toEqual({ count: 2, vsize: 203 });
    expect(
      left.ruleTotals.element_size.count +
        left.ruleTotals.tapscript_op_if.count,
    ).toBeGreaterThan(left.statusTotals.violating.count);
    expect(right.ruleTotals.element_size).toEqual({ count: 0, vsize: 0 });
    expect(
      left.exactSignatures.map(({ signature, count }) => [
        signature.key,
        count,
      ]),
    ).toEqual([
      ["exact:42", 1],
      ["exact:40", 1],
    ]);
    expect(
      left.partialSignatures.map(({ signature, count }) => [
        signature.key,
        count,
      ]),
    ).toEqual([["partial:02:40", 1]]);
    expect(
      view.population("common", "left", {
        kind: "signature",
        signature: "partial:02:40",
      }),
    ).toEqual({ count: 1, vsize: 103 });
    expect(
      view.population("common", "left", {
        kind: "status",
        status: "violating",
      }),
    ).toMatchObject({ count: 3, vsize: 306 });
    expect(
      view.population("common", "right", {
        kind: "status",
        status: "violating",
      }),
    ).toMatchObject({ count: 2, vsize: 1_003 });
  });

  it("returns aggregate-only totals and caches canonical lookups", () => {
    const leftTransactions = Array.from({ length: 20 }, (_, index) => ({
      ...transaction(index + 1, violating(["element_size"])),
      vsize: ((index + 1) % 5) * 100 + 100,
    }));
    const sampleComparison = compareCurrentSnapshots(
      loadedSource("core", leftTransactions),
      loadedSource(
        "knots",
        leftTransactions.map((entry) => ({ ...entry })),
      ),
    );
    const view = buildComparisonPolicyView(sampleComparison);
    const first = view.population("common", "left", { kind: "all" });
    const repeated = view.population("common", "left", { kind: "all" });
    expect(first).toBe(repeated);
    expect(first.count).toBe(20);
    expect(first.vsize).toBe(
      leftTransactions.reduce((total, entry) => total + entry.vsize, 0),
    );
    expect(first).not.toHaveProperty("entries");
    expect(first).not.toHaveProperty("transactions");
    expect(first).not.toHaveProperty("sample");
  });

  it("builds the same aggregates cooperatively across bounded batches", async () => {
    const synchronous = buildComparisonPolicyView(comparison);
    let yields = 0;
    const cooperative = await buildComparisonPolicyViewCooperatively(
      comparison,
      {
        batchSize: 2,
        yieldBetweenBatches: () => {
          yields += 1;
        },
      },
    );

    expect(cooperative.rows).toEqual(synchronous.rows);
    for (const [region, side] of [
      ["left_only", "left"],
      ["common", "left"],
      ["common", "right"],
      ["right_only", "right"],
    ] as const) {
      expect(cooperative.slice(region, side)).toEqual(
        synchronous.slice(region, side),
      );
      for (const filter of [
        { kind: "all" } as const,
        { kind: "status", status: "violating" } as const,
        { kind: "rule", rule: "element_size" } as const,
      ]) {
        expect(cooperative.population(region, side, filter)).toEqual(
          synchronous.population(region, side, filter),
        );
      }
    }
    expect(yields).toBeGreaterThan(0);
  });

  it("caches zero-total populations without scanning source entries", () => {
    const zeroComparison = compareCurrentSnapshots(
      loadedSource("core", [transaction(1, assessment("compatible"))]),
      loadedSource("knots", [transaction(1, assessment("compatible"))]),
    );
    const view = buildComparisonPolicyView(zeroComparison);
    Object.defineProperty(zeroComparison.common, Symbol.iterator, {
      configurable: true,
      value: () => {
        throw new Error("zero-total population scanned its source region");
      },
    });

    const first = view.population("common", "left", {
      kind: "rule",
      rule: "element_size",
    });
    const repeated = view.population("common", "left", {
      kind: "rule",
      rule: "element_size",
    });

    expect(first).toEqual({ count: 0, vsize: 0 });
    expect(repeated).toBe(first);
  });

  it("canonicalizes the effective side for one-source regions", () => {
    const view = buildComparisonPolicyView(comparison);
    const filter = { kind: "all" } as const;

    expect(view.slice("left_only", "right")).toBe(
      view.slice("left_only", "left"),
    );
    expect(view.population("left_only", "right", filter)).toBe(
      view.population("left_only", "left", filter),
    );
  });
});
