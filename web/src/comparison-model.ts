import { violationSignature, type ViolationSignatureKey } from "./terrain";
import {
  comparePackedSnapshotRows,
  packedSnapshotRowCount,
  packedSnapshotRowVsize,
  packedSnapshotRowsSourceDifferenceFlags,
  packedSnapshotTransaction,
  SOURCE_DIFFERENCE_ANCESTOR_PACKAGE,
  SOURCE_DIFFERENCE_REPLACEABILITY,
  SOURCE_DIFFERENCE_WITNESS_VARIANT,
} from "./packed-store";
import type {
  Bip110Status,
  MempoolSnapshot,
  MempoolTransaction,
  RuleId,
  LoadedSourcePublication,
} from "./types";

export type ComparisonSide = "left" | "right";
export type ComparisonRegionKey = "common" | "left_only" | "right_only";
export type ComparisonPolicyStatus = Bip110Status | "unclassified";
export type ComparisonWitnessRelation =
  "one_sided" | "loading" | "same" | "different";

export interface LoadedSourceSnapshot {
  source: LoadedSourcePublication["source"];
  snapshot: MempoolSnapshot;
}

export interface ComparedTransaction {
  txid: string;
  left: MempoolTransaction | null;
  right: MempoolTransaction | null;
  witness_relation: ComparisonWitnessRelation;
}

export const comparisonWitnessVariantDescription = (
  relation: ComparisonWitnessRelation,
): string => {
  switch (relation) {
    case "one_sided":
      return "Observed in one snapshot";
    case "loading":
      return "Witness variants are still loading";
    case "same":
      return "Same witness variant";
    case "different":
      return "Different witness variants";
  }
};

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
  common_source_difference_count: number;
  common_witness_variant_count: number;
  common_ancestor_package_difference_count: number;
  common_replaceability_difference_count: number;
}

export interface CurrentComparison {
  left: LoadedSourceSnapshot;
  right: LoadedSourceSnapshot;
  observed_skew_ms: number;
  earlier_side: ComparisonSide | null;
  common: ComparedTransaction[];
  left_only: ComparedTransaction[];
  right_only: ComparedTransaction[];
  common_source_difference_flags: Uint8Array;
  totals: ComparisonTotals;
}

export type ComparisonPolicyFilter =
  | { kind: "all" }
  | { kind: "rule"; rule: RuleId }
  | { kind: "status"; status: ComparisonPolicyStatus }
  | { kind: "signature"; signature: ViolationSignatureKey };

export const comparisonPolicyFilterMatches = (
  transaction: MempoolTransaction,
  filter: ComparisonPolicyFilter,
): boolean => {
  if (filter.kind === "all") {
    return true;
  }
  const assessment = transaction.bip110;
  if (filter.kind === "status") {
    return (assessment?.status ?? "unclassified") === filter.status;
  }
  if (assessment === null) {
    return false;
  }
  if (filter.kind === "rule") {
    return assessment.violated_rules.includes(filter.rule);
  }
  return violationSignature(assessment)?.key === filter.signature;
};

const COMPARISON_ROW_CACHE_LIMIT = 256;

interface ComparisonMembership {
  common: ComparedTransaction[];
  left_only: ComparedTransaction[];
  right_only: ComparedTransaction[];
  common_source_difference_flags: Uint8Array;
  totals: ComparisonTotals;
}

interface ComparisonMembershipRows {
  commonLeft: Uint32Array;
  commonRight: Uint32Array;
  leftOnly: Uint32Array;
  rightOnly: Uint32Array;
}

interface PackedComparisonMembership extends ComparisonMembership {
  rows: ComparisonMembershipRows;
}

const membershipRowsByComparison = new WeakMap<
  CurrentComparison,
  ComparisonMembershipRows
>();
const EMPTY_COMPARISON_ROWS = new Uint32Array();

const comparedEntry = (
  left: MempoolTransaction | null,
  right: MempoolTransaction | null,
  membershipReady: boolean,
): ComparedTransaction => ({
  txid: left?.txid ?? right?.txid ?? "",
  left,
  right,
  witness_relation:
    left === null || right === null
      ? "one_sided"
      : !membershipReady
        ? "loading"
        : left.wtxid === right.wtxid
          ? "same"
          : "different",
});

const isArrayIndex = (property: PropertyKey): number | null => {
  if (typeof property !== "string" || !/^(0|[1-9][0-9]*)$/.test(property)) {
    return null;
  }
  const index = Number(property);
  return Number.isSafeInteger(index) ? index : null;
};

const lazyComparedEntries = (
  length: number,
  materialize: (index: number) => ComparedTransaction,
): ComparedTransaction[] => {
  const target: ComparedTransaction[] = [];
  const cache = new Map<number, ComparedTransaction>();
  const entryAt = (index: number): ComparedTransaction | undefined => {
    if (index < 0 || index >= length) return undefined;
    const cached = cache.get(index);
    if (cached !== undefined) {
      cache.delete(index);
      cache.set(index, cached);
      return cached;
    }
    const entry = materialize(index);
    cache.set(index, entry);
    if (cache.size > COMPARISON_ROW_CACHE_LIMIT) {
      const oldest = cache.keys().next().value;
      if (oldest !== undefined) cache.delete(oldest);
    }
    return entry;
  };

  return new Proxy(target, {
    get(array, property, receiver) {
      if (property === "length") return length;
      if (property === Symbol.iterator) {
        return function* (): IterableIterator<ComparedTransaction> {
          for (let index = 0; index < length; index += 1) {
            const entry = entryAt(index);
            if (entry !== undefined) yield entry;
          }
        };
      }
      const index = isArrayIndex(property);
      return index === null
        ? Reflect.get(array, property, receiver)
        : entryAt(index);
    },
    has(array, property) {
      const index = isArrayIndex(property);
      return index === null
        ? Reflect.has(array, property)
        : index >= 0 && index < length;
    },
    getOwnPropertyDescriptor(array, property) {
      const index = isArrayIndex(property);
      if (index !== null && index < length) {
        return {
          configurable: true,
          enumerable: true,
          writable: false,
          value: entryAt(index),
        };
      }
      return Reflect.getOwnPropertyDescriptor(array, property);
    },
  });
};

interface PackedMergeVisitor {
  common(leftRow: number, rightRow: number): void;
  leftOnly(leftRow: number): void;
  rightOnly(rightRow: number): void;
}

const visitPackedMerge = (
  leftSnapshot: MempoolSnapshot,
  leftCount: number,
  rightSnapshot: MempoolSnapshot,
  rightCount: number,
  visitor: PackedMergeVisitor,
): void => {
  let leftRow = 0;
  let rightRow = 0;
  while (leftRow < leftCount || rightRow < rightCount) {
    if (leftRow >= leftCount) {
      visitor.rightOnly(rightRow);
      rightRow += 1;
      continue;
    }
    if (rightRow >= rightCount) {
      visitor.leftOnly(leftRow);
      leftRow += 1;
      continue;
    }
    const order = comparePackedSnapshotRows(
      leftSnapshot,
      leftRow,
      rightSnapshot,
      rightRow,
    );
    if (order === 0) {
      visitor.common(leftRow, rightRow);
      leftRow += 1;
      rightRow += 1;
    } else if (order < 0) {
      visitor.leftOnly(leftRow);
      leftRow += 1;
    } else {
      visitor.rightOnly(rightRow);
      rightRow += 1;
    }
  }
};

const requirePackedVsize = (snapshot: MempoolSnapshot, row: number): number => {
  const vsize = packedSnapshotRowVsize(snapshot, row);
  if (vsize === undefined) {
    throw new TypeError("Packed transaction vsize is unavailable");
  }
  return vsize;
};

const requirePackedTransaction = (
  snapshot: MempoolSnapshot,
  row: number,
): MempoolTransaction => {
  const transaction = packedSnapshotTransaction(snapshot, row);
  if (transaction === undefined) {
    throw new TypeError("Packed transaction row is unavailable");
  }
  return transaction;
};

const comparePackedMembership = (
  left: LoadedSourceSnapshot,
  right: LoadedSourceSnapshot,
  membershipReady: boolean,
): PackedComparisonMembership => {
  const leftCount = packedSnapshotRowCount(left.snapshot);
  const rightCount = packedSnapshotRowCount(right.snapshot);
  if (leftCount > 0xffff_ffff || rightCount > 0xffff_ffff) {
    throw new RangeError("Packed comparison row count exceeds Uint32 capacity");
  }

  let commonCount = 0;
  let leftOnlyCount = 0;
  let rightOnlyCount = 0;
  let commonLeftVsize = 0;
  let commonRightVsize = 0;
  let leftOnlyVsize = 0;
  let rightOnlyVsize = 0;
  visitPackedMerge(left.snapshot, leftCount, right.snapshot, rightCount, {
    common: (leftRow, rightRow) => {
      commonCount += 1;
      commonLeftVsize += requirePackedVsize(left.snapshot, leftRow);
      commonRightVsize += requirePackedVsize(right.snapshot, rightRow);
    },
    leftOnly: (leftRow) => {
      leftOnlyCount += 1;
      leftOnlyVsize += requirePackedVsize(left.snapshot, leftRow);
    },
    rightOnly: (rightRow) => {
      rightOnlyCount += 1;
      rightOnlyVsize += requirePackedVsize(right.snapshot, rightRow);
    },
  });

  const commonLeftRows = new Uint32Array(commonCount);
  const commonRightRows = new Uint32Array(commonCount);
  const commonSourceDifferenceFlags = new Uint8Array(commonCount);
  const leftOnlyRows = new Uint32Array(leftOnlyCount);
  const rightOnlyRows = new Uint32Array(rightOnlyCount);
  let commonIndex = 0;
  let leftOnlyIndex = 0;
  let rightOnlyIndex = 0;
  let commonSourceDifferenceCount = 0;
  let commonWitnessVariantCount = 0;
  let commonAncestorPackageDifferenceCount = 0;
  let commonReplaceabilityDifferenceCount = 0;
  visitPackedMerge(left.snapshot, leftCount, right.snapshot, rightCount, {
    common: (leftRow, rightRow) => {
      commonLeftRows[commonIndex] = leftRow;
      commonRightRows[commonIndex] = rightRow;
      if (membershipReady) {
        const flags = packedSnapshotRowsSourceDifferenceFlags(
          left.snapshot,
          leftRow,
          right.snapshot,
          rightRow,
        );
        if (flags === null) {
          throw new TypeError("Packed source-fact comparison is unavailable");
        }
        commonSourceDifferenceFlags[commonIndex] = flags;
        if (flags !== 0) commonSourceDifferenceCount += 1;
        if ((flags & SOURCE_DIFFERENCE_WITNESS_VARIANT) !== 0) {
          commonWitnessVariantCount += 1;
        }
        if ((flags & SOURCE_DIFFERENCE_ANCESTOR_PACKAGE) !== 0) {
          commonAncestorPackageDifferenceCount += 1;
        }
        if ((flags & SOURCE_DIFFERENCE_REPLACEABILITY) !== 0) {
          commonReplaceabilityDifferenceCount += 1;
        }
      }
      commonIndex += 1;
    },
    leftOnly: (leftRow) => {
      leftOnlyRows[leftOnlyIndex] = leftRow;
      leftOnlyIndex += 1;
    },
    rightOnly: (rightRow) => {
      rightOnlyRows[rightOnlyIndex] = rightRow;
      rightOnlyIndex += 1;
    },
  });

  return {
    common: lazyComparedEntries(commonCount, (index) => {
      const leftRow = commonLeftRows[index];
      const rightRow = commonRightRows[index];
      if (leftRow === undefined || rightRow === undefined) {
        throw new RangeError("Packed common comparison index is unavailable");
      }
      return comparedEntry(
        requirePackedTransaction(left.snapshot, leftRow),
        requirePackedTransaction(right.snapshot, rightRow),
        membershipReady,
      );
    }),
    left_only: lazyComparedEntries(leftOnlyCount, (index) => {
      const row = leftOnlyRows[index];
      if (row === undefined) {
        throw new RangeError(
          "Packed left-only comparison index is unavailable",
        );
      }
      return comparedEntry(
        requirePackedTransaction(left.snapshot, row),
        null,
        membershipReady,
      );
    }),
    right_only: lazyComparedEntries(rightOnlyCount, (index) => {
      const row = rightOnlyRows[index];
      if (row === undefined) {
        throw new RangeError(
          "Packed right-only comparison index is unavailable",
        );
      }
      return comparedEntry(
        null,
        requirePackedTransaction(right.snapshot, row),
        membershipReady,
      );
    }),
    common_source_difference_flags: commonSourceDifferenceFlags,
    rows: {
      commonLeft: commonLeftRows,
      commonRight: commonRightRows,
      leftOnly: leftOnlyRows,
      rightOnly: rightOnlyRows,
    },
    totals: {
      union_count: commonCount + leftOnlyCount + rightOnlyCount,
      common_count: commonCount,
      common_left_vsize: commonLeftVsize,
      common_right_vsize: commonRightVsize,
      left_only_count: leftOnlyCount,
      left_only_vsize: leftOnlyVsize,
      right_only_count: rightOnlyCount,
      right_only_vsize: rightOnlyVsize,
      common_source_difference_count: commonSourceDifferenceCount,
      common_witness_variant_count: commonWitnessVariantCount,
      common_ancestor_package_difference_count:
        commonAncestorPackageDifferenceCount,
      common_replaceability_difference_count:
        commonReplaceabilityDifferenceCount,
    },
  };
};

export const requireLoadedSnapshot = (
  response: LoadedSourcePublication,
): LoadedSourceSnapshot => {
  return { source: response.source, snapshot: response.publication };
};

/**
 * Merge-joins two independently validated, txid-sorted snapshots. Each txid
 * is placed in exactly one symmetric membership region and common txids retain
 * both source-local witness variants and policy assessments.
 */
export const compareCurrentSnapshots = (
  left: LoadedSourceSnapshot,
  right: LoadedSourceSnapshot,
  membershipReady = true,
): CurrentComparison => {
  if (left.snapshot.source_id === right.snapshot.source_id) {
    throw new Error("A comparison requires two distinct sources");
  }

  const { rows, ...membership } = comparePackedMembership(
    left,
    right,
    membershipReady,
  );

  const leftObserved = left.snapshot.observed_at_ms;
  const rightObserved = right.snapshot.observed_at_ms;
  const comparison: CurrentComparison = {
    left,
    right,
    observed_skew_ms: Math.abs(leftObserved - rightObserved),
    earlier_side:
      leftObserved === rightObserved
        ? null
        : leftObserved < rightObserved
          ? "left"
          : "right",
    ...membership,
  };
  membershipRowsByComparison.set(comparison, rows);
  return comparison;
};

export const comparisonRegionTransactionRows = (
  comparison: CurrentComparison,
  region: ComparisonRegionKey,
  side: ComparisonSide,
): Uint32Array => {
  const rows = membershipRowsByComparison.get(comparison);
  if (rows === undefined) {
    throw new TypeError("Comparison membership rows are unavailable");
  }
  if (region === "common") {
    return side === "left" ? rows.commonLeft : rows.commonRight;
  }
  if (region === "left_only") {
    return side === "left" ? rows.leftOnly : EMPTY_COMPARISON_ROWS;
  }
  return side === "right" ? rows.rightOnly : EMPTY_COMPARISON_ROWS;
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
