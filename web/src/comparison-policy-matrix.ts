import {
  sourceEntry,
  type ComparedTransaction,
  type ComparisonPolicyFilter,
  type ComparisonPolicyStatus,
  type ComparisonRegionKey,
  type ComparisonSide,
  type CurrentComparison,
} from "./comparison-model";
import {
  violationSignature,
  type ExactSignatureKey,
  type ViolationSignature,
} from "./terrain";
import type { Bip110Assessment, MempoolTransaction } from "./types";

export const COMPARISON_POLICY_MATRIX_CHIP_LIMIT = 3;

export type ComparisonPolicyMatrixRowKey =
  "left_only_left" | "common_left" | "common_right" | "right_only_right";

export type ComparisonPolicyMatrixStatus = ComparisonPolicyStatus;

export interface ComparisonPolicyMatrixStatusCounts {
  compatible: number;
  violating: number;
  indeterminate: number;
  unclassified: number;
}

export type ExactViolationSignature = Omit<
  ViolationSignature,
  "key" | "completeness"
> & {
  key: ExactSignatureKey;
  completeness: "exact";
};

export interface ComparisonPolicyMatrixExactCombination {
  signature: ExactViolationSignature;
  count: number;
}

export interface ComparisonPolicyMatrixExactOverflow {
  combinationCount: number;
  transactionCount: number;
}

export interface ComparisonPolicyMatrixRow {
  key: ComparisonPolicyMatrixRowKey;
  region: ComparisonRegionKey;
  side: ComparisonSide;
  sourceId: string;
  sourceLabel: string;
  populationCount: number;
  statusCounts: ComparisonPolicyMatrixStatusCounts;
  exactViolationCount: number;
  partialViolationCount: number;
  dominantExactCombinations: ComparisonPolicyMatrixExactCombination[];
  exactCombinationOverflow: ComparisonPolicyMatrixExactOverflow;
}

export interface ComparisonPolicyMatrix {
  rows: ComparisonPolicyMatrixRow[];
}

export type ComparisonPolicyMatrixSelection =
  | Extract<ComparisonPolicyFilter, { kind: "status" }>
  | { kind: "signature"; signature: ExactSignatureKey };

export interface ComparisonPolicyMatrixTarget {
  region: ComparisonRegionKey;
  side: ComparisonSide;
  filter: ComparisonPolicyMatrixSelection;
  txid: null;
}

interface RowSpec {
  key: ComparisonPolicyMatrixRowKey;
  region: ComparisonRegionKey;
  side: ComparisonSide;
}

interface ExactCombinationAccumulator {
  signature: ExactViolationSignature;
  count: number;
}

interface RowAccumulator {
  spec: RowSpec;
  populationCount: number;
  statusCounts: ComparisonPolicyMatrixStatusCounts;
  exactViolationCount: number;
  partialViolationCount: number;
  exactCombinations: Map<ExactSignatureKey, ExactCombinationAccumulator>;
}

const ROW_SPECS: readonly RowSpec[] = [
  { key: "left_only_left", region: "left_only", side: "left" },
  { key: "common_left", region: "common", side: "left" },
  { key: "common_right", region: "common", side: "right" },
  { key: "right_only_right", region: "right_only", side: "right" },
];

const emptyStatusCounts = (): ComparisonPolicyMatrixStatusCounts => ({
  compatible: 0,
  violating: 0,
  indeterminate: 0,
  unclassified: 0,
});

const rowAccumulator = (spec: RowSpec): RowAccumulator => ({
  spec,
  populationCount: 0,
  statusCounts: emptyStatusCounts(),
  exactViolationCount: 0,
  partialViolationCount: 0,
  exactCombinations: new Map(),
});

const exactSignature = (
  assessment: Bip110Assessment,
): ExactViolationSignature => {
  const signature = violationSignature(assessment);
  if (
    signature === null ||
    signature.completeness !== "exact" ||
    !signature.key.startsWith("exact:")
  ) {
    throw new Error(
      "Complete violating assessment is missing an exact signature",
    );
  }
  return signature as ExactViolationSignature;
};

const addTransaction = (
  row: RowAccumulator,
  transaction: MempoolTransaction,
): void => {
  row.populationCount += 1;
  const assessment = transaction.bip110;
  if (assessment === null) {
    row.statusCounts.unclassified += 1;
    return;
  }

  row.statusCounts[assessment.status] += 1;
  if (assessment.status !== "violating") {
    return;
  }
  if (assessment.unknown_rules.length > 0) {
    row.partialViolationCount += 1;
    return;
  }

  row.exactViolationCount += 1;
  const signature = exactSignature(assessment);
  const existing = row.exactCombinations.get(signature.key);
  if (existing === undefined) {
    row.exactCombinations.set(signature.key, { signature, count: 1 });
  } else {
    existing.count += 1;
  }
};

const requiredTransaction = (
  entry: ComparedTransaction,
  region: ComparisonRegionKey,
  side: ComparisonSide,
): MempoolTransaction => {
  const transaction = sourceEntry(entry, side);
  if (transaction === null) {
    throw new Error(
      `Comparison row ${region}/${side} is missing its source entry`,
    );
  }
  return transaction;
};

const finishRow = (
  comparison: CurrentComparison,
  row: RowAccumulator,
): ComparisonPolicyMatrixRow => {
  const combinations = [...row.exactCombinations.values()].sort(
    (left, right) => {
      const countOrder = right.count - left.count;
      if (countOrder !== 0) {
        return countOrder;
      }
      return left.signature.key < right.signature.key
        ? -1
        : left.signature.key > right.signature.key
          ? 1
          : 0;
    },
  );
  const dominantExactCombinations = combinations.slice(
    0,
    COMPARISON_POLICY_MATRIX_CHIP_LIMIT,
  );
  const hiddenCombinations = combinations.slice(
    COMPARISON_POLICY_MATRIX_CHIP_LIMIT,
  );
  const source = comparison[row.spec.side].source;

  return {
    ...row.spec,
    sourceId: source.source_id,
    sourceLabel: source.source_label,
    populationCount: row.populationCount,
    statusCounts: row.statusCounts,
    exactViolationCount: row.exactViolationCount,
    partialViolationCount: row.partialViolationCount,
    dominantExactCombinations,
    exactCombinationOverflow: {
      combinationCount: hiddenCombinations.length,
      transactionCount: hiddenCombinations.reduce(
        (total, combination) => total + combination.count,
        0,
      ),
    },
  };
};

/**
 * Derive the comparison policy matrix in one pass over each disjoint
 * membership region. Common transactions contribute once to each source-local
 * row because their policy assessments remain independent.
 */
export const buildComparisonPolicyMatrix = (
  comparison: CurrentComparison,
): ComparisonPolicyMatrix => {
  const rows = new Map(
    ROW_SPECS.map((spec) => [spec.key, rowAccumulator(spec)]),
  );
  const leftOnly = rows.get("left_only_left");
  const commonLeft = rows.get("common_left");
  const commonRight = rows.get("common_right");
  const rightOnly = rows.get("right_only_right");
  if (
    leftOnly === undefined ||
    commonLeft === undefined ||
    commonRight === undefined ||
    rightOnly === undefined
  ) {
    throw new Error("Comparison policy matrix row definition is incomplete");
  }

  for (const entry of comparison.left_only) {
    addTransaction(leftOnly, requiredTransaction(entry, "left_only", "left"));
  }
  for (const entry of comparison.common) {
    addTransaction(commonLeft, requiredTransaction(entry, "common", "left"));
    addTransaction(commonRight, requiredTransaction(entry, "common", "right"));
  }
  for (const entry of comparison.right_only) {
    addTransaction(
      rightOnly,
      requiredTransaction(entry, "right_only", "right"),
    );
  }

  return {
    rows: ROW_SPECS.map((spec) => finishRow(comparison, rows.get(spec.key)!)),
  };
};

export const comparisonPolicyMatrixTarget = (
  row: Pick<ComparisonPolicyMatrixRow, "region" | "side">,
  filter: ComparisonPolicyMatrixSelection,
): ComparisonPolicyMatrixTarget => ({
  region: row.region,
  side: row.side,
  filter,
  txid: null,
});
