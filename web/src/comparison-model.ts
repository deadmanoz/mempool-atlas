import type { ViolationSignatureKey } from "./terrain";
import type {
  Bip110Status,
  MempoolSnapshot,
  MempoolTransaction,
  RuleId,
  SourceSnapshotResponse,
} from "./types";

export type ComparisonSide = "left" | "right";
export type ComparisonRegionKey = "common" | "left_only" | "right_only";
export type ComparisonPolicyStatus = Bip110Status | "unclassified";

export interface LoadedSourceSnapshot {
  source: SourceSnapshotResponse["source"];
  snapshot: MempoolSnapshot;
}

export interface ComparedTransaction {
  txid: string;
  left: MempoolTransaction | null;
  right: MempoolTransaction | null;
  same_wtxid: boolean | null;
}

export interface ComparisonTransactionLookup {
  region: ComparisonRegionKey;
  index: number;
  entry: ComparedTransaction;
}

export interface ComparisonTotals {
  union_count: number;
  common_count: number;
  common_left_vsize: number;
  common_right_vsize: number;
  left_only_count: number;
  left_only_vsize: number;
  right_only_count: number;
  right_only_vsize: number;
}

export interface CurrentComparison {
  left: LoadedSourceSnapshot;
  right: LoadedSourceSnapshot;
  observed_skew_ms: number;
  earlier_side: ComparisonSide | null;
  common: ComparedTransaction[];
  left_only: ComparedTransaction[];
  right_only: ComparedTransaction[];
  totals: ComparisonTotals;
}

export type ComparisonPolicyFilter =
  | { kind: "all" }
  | { kind: "rule"; rule: RuleId }
  | { kind: "status"; status: ComparisonPolicyStatus }
  | { kind: "signature"; signature: ViolationSignatureKey };

const addVsize = (total: number, transaction: MempoolTransaction): number =>
  total + transaction.vsize;

const comparedEntry = (
  left: MempoolTransaction | null,
  right: MempoolTransaction | null,
): ComparedTransaction => ({
  txid: left?.txid ?? right?.txid ?? "",
  left,
  right,
  same_wtxid:
    left === null || right === null ? null : left.wtxid === right.wtxid,
});

export const requireLoadedSnapshot = (
  response: SourceSnapshotResponse,
): LoadedSourceSnapshot => {
  if (response.snapshot === null) {
    throw new Error(
      `${response.source.source_label} has no complete mempool snapshot yet`,
    );
  }
  return { source: response.source, snapshot: response.snapshot };
};

/**
 * Merge-joins two independently validated, txid-sorted snapshots. Each txid
 * is placed in exactly one symmetric membership region and common txids retain
 * both source-local witness variants and policy assessments.
 */
export const compareCurrentSnapshots = (
  left: LoadedSourceSnapshot,
  right: LoadedSourceSnapshot,
): CurrentComparison => {
  if (left.snapshot.source_id === right.snapshot.source_id) {
    throw new Error("A comparison requires two distinct sources");
  }

  const common: ComparedTransaction[] = [];
  const leftOnly: ComparedTransaction[] = [];
  const rightOnly: ComparedTransaction[] = [];
  let commonLeftVsize = 0;
  let commonRightVsize = 0;
  let leftOnlyVsize = 0;
  let rightOnlyVsize = 0;
  let leftIndex = 0;
  let rightIndex = 0;

  while (
    leftIndex < left.snapshot.transactions.length ||
    rightIndex < right.snapshot.transactions.length
  ) {
    const leftTransaction = left.snapshot.transactions[leftIndex];
    const rightTransaction = right.snapshot.transactions[rightIndex];
    if (leftTransaction === undefined) {
      if (rightTransaction === undefined) {
        break;
      }
      rightOnly.push(comparedEntry(null, rightTransaction));
      rightOnlyVsize = addVsize(rightOnlyVsize, rightTransaction);
      rightIndex += 1;
      continue;
    }
    if (rightTransaction === undefined) {
      leftOnly.push(comparedEntry(leftTransaction, null));
      leftOnlyVsize = addVsize(leftOnlyVsize, leftTransaction);
      leftIndex += 1;
      continue;
    }
    if (leftTransaction.txid === rightTransaction.txid) {
      common.push(comparedEntry(leftTransaction, rightTransaction));
      commonLeftVsize = addVsize(commonLeftVsize, leftTransaction);
      commonRightVsize = addVsize(commonRightVsize, rightTransaction);
      leftIndex += 1;
      rightIndex += 1;
      continue;
    }
    if (leftTransaction.txid < rightTransaction.txid) {
      leftOnly.push(comparedEntry(leftTransaction, null));
      leftOnlyVsize = addVsize(leftOnlyVsize, leftTransaction);
      leftIndex += 1;
    } else {
      rightOnly.push(comparedEntry(null, rightTransaction));
      rightOnlyVsize = addVsize(rightOnlyVsize, rightTransaction);
      rightIndex += 1;
    }
  }

  const leftObserved = left.snapshot.observed_at_ms;
  const rightObserved = right.snapshot.observed_at_ms;
  return {
    left,
    right,
    observed_skew_ms: Math.abs(leftObserved - rightObserved),
    earlier_side:
      leftObserved === rightObserved
        ? null
        : leftObserved < rightObserved
          ? "left"
          : "right",
    common,
    left_only: leftOnly,
    right_only: rightOnly,
    totals: {
      union_count: common.length + leftOnly.length + rightOnly.length,
      common_count: common.length,
      common_left_vsize: commonLeftVsize,
      common_right_vsize: commonRightVsize,
      left_only_count: leftOnly.length,
      left_only_vsize: leftOnlyVsize,
      right_only_count: rightOnly.length,
      right_only_vsize: rightOnlyVsize,
    },
  };
};

export const comparisonRegionEntries = (
  comparison: CurrentComparison,
  region: ComparisonRegionKey,
): ComparedTransaction[] => comparison[region];

const findComparedTransaction = (
  entries: readonly ComparedTransaction[],
  txid: string,
): { index: number; entry: ComparedTransaction } | null => {
  let low = 0;
  let high = entries.length;
  while (low < high) {
    const middle = low + Math.floor((high - low) / 2);
    const entry = entries[middle];
    if (entry === undefined) {
      return null;
    }
    if (entry.txid < txid) {
      low = middle + 1;
    } else {
      high = middle;
    }
  }
  const candidate = entries[low];
  return candidate?.txid === txid ? { index: low, entry: candidate } : null;
};

/**
 * Locate one txid across the three independently sorted comparison regions.
 * This keeps lookup logarithmic without retaining a second union-sized index.
 */
export const lookupComparisonTransaction = (
  comparison: CurrentComparison,
  txid: string,
): ComparisonTransactionLookup | null => {
  const regions: readonly ComparisonRegionKey[] = [
    "common",
    "left_only",
    "right_only",
  ];
  for (const region of regions) {
    const match = findComparedTransaction(comparison[region], txid);
    if (match !== null) {
      return { region, ...match };
    }
  }
  return null;
};

export const sourceEntry = (
  entry: ComparedTransaction,
  side: ComparisonSide,
): MempoolTransaction | null => entry[side];

export const policySideForRegion = (
  region: ComparisonRegionKey,
  preferred: ComparisonSide,
): ComparisonSide =>
  region === "left_only"
    ? "left"
    : region === "right_only"
      ? "right"
      : preferred;
