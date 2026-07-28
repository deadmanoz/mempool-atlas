import { describe, expect, it } from "vitest";

import {
  compareCurrentSnapshots,
  type LoadedSourceSnapshot,
} from "./comparison-model";
import {
  COMPARISON_POLICY_MATRIX_CHIP_LIMIT,
  buildComparisonPolicyMatrix,
  comparisonPolicyMatrixTarget,
} from "./comparison-policy-matrix";
import { serializeComparisonViewState } from "./view-state";
import type { ComparisonViewState } from "./view-state";
import type {
  Bip110Assessment,
  Bip110Summary,
  MempoolSnapshot,
  MempoolTransaction,
  RuleId,
} from "./types";

const txid = (value: number): string => value.toString(16).padStart(64, "0");

const assessment = (
  status: "compatible" | "indeterminate",
): Bip110Assessment => ({
  status,
  primary_rule: null,
  violated_rules: [],
  unknown_rules: status === "indeterminate" ? ["output_size"] : [],
});

const violating = (
  violatedRules: RuleId[],
  unknownRules: RuleId[] = [],
): Bip110Assessment => ({
  status: "violating",
  primary_rule: violatedRules[0] ?? null,
  violated_rules: violatedRules,
  unknown_rules: unknownRules,
});

const transaction = (
  value: number,
  bip110: Bip110Assessment | null,
): MempoolTransaction => ({
  txid: txid(value),
  wtxid: txid(value),
  vsize: 100 + value,
  fee_sats: 200 + value,
  entered_at_ms: 1_700_000_000_000 + value,
  bip110,
});

const summary = (
  transactions: readonly MempoolTransaction[],
): Bip110Summary => ({
  evaluator_id: "rdts-rules",
  evaluator_version: "0.1.0",
  scope: "knots_mempool_policy",
  compatible_count: transactions.filter(
    ({ bip110 }) => bip110?.status === "compatible",
  ).length,
  violating_count: transactions.filter(
    ({ bip110 }) => bip110?.status === "violating",
  ).length,
  indeterminate_count: transactions.filter(
    ({ bip110 }) => bip110?.status === "indeterminate",
  ).length,
  unclassified_count: transactions.filter(({ bip110 }) => bip110 === null)
    .length,
});

const loadedSource = (
  sourceId: string,
  transactions: MempoolTransaction[],
): LoadedSourceSnapshot => {
  const snapshot: MempoolSnapshot = {
    source_id: sourceId,
    source_label: `${sourceId.toUpperCase()} node`,
    collection_started_at_ms: 1_700_000_000_000,
    collection_completed_at_ms: 1_700_000_001_000,
    collection_duration_ms: 1_000,
    observed_at_ms: 1_700_000_001_000,
    classification_revision: 1,
    chain_tip: { height: 900_000, hash: "00".repeat(32) },
    transaction_count: transactions.length,
    total_vsize: transactions.reduce((total, entry) => total + entry.vsize, 0),
    bip110_summary: summary(transactions),
    transactions,
  };
  return {
    source: {
      source_id: sourceId,
      source_label: snapshot.source_label,
      availability: "ready",
      poll_interval_seconds: 300,
      last_poll_started_at_ms: snapshot.collection_started_at_ms,
      snapshot_observed_at_ms: snapshot.observed_at_ms,
      chain_tip: snapshot.chain_tip,
      transaction_count: snapshot.transaction_count,
      total_vsize: snapshot.total_vsize,
      last_error: null,
    },
    snapshot,
  };
};

describe("comparison policy matrix", () => {
  it("derives four fixed source-local rows whose statuses conserve population", () => {
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

    const matrix = buildComparisonPolicyMatrix(comparison);

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

  it("keeps all four rows explicit when both snapshots are empty", () => {
    const matrix = buildComparisonPolicyMatrix(
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

    const row = buildComparisonPolicyMatrix(comparison).rows[1];

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
    const matrix = buildComparisonPolicyMatrix(comparison);

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

    const target = comparisonPolicyMatrixTarget(matrix.rows[1]!, {
      kind: "status",
      status: "violating",
    });
    const priorState: ComparisonViewState = {
      left: "core",
      right: "knots",
      region: "right_only",
      side: "right",
      filter: { kind: "all" },
      txid: txid(99),
    };
    expect(
      serializeComparisonViewState({
        ...priorState,
        ...target,
      }),
    ).toBe(
      "left=core&right=knots&region=common&side=left&filter=status%3Aviolating",
    );
  });
});
