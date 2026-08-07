import type { ComparisonSide, CurrentComparison } from "./comparison-model";
import { comparisonRegionTransactionRows } from "./comparison-model";
import { transactionIndexViewFromRetainedRows } from "./transaction-view";
import type { MempoolTransaction } from "./types";

export type ComparisonDistributionScope =
  "all" | "common" | "left_only" | "right_only";

const presentTransactionView = (
  current: CurrentComparison,
  region: "common" | "left_only" | "right_only",
  side: ComparisonSide,
): MempoolTransaction[] =>
  transactionIndexViewFromRetainedRows(
    current[side].snapshot.transactions,
    comparisonRegionTransactionRows(current, region, side),
  );

export const comparisonDistributionTransactions = (
  current: CurrentComparison,
  side: ComparisonSide,
  scope: ComparisonDistributionScope,
): MempoolTransaction[] => {
  if (scope === "all") return current[side].snapshot.transactions;
  if (scope === "common")
    return presentTransactionView(current, "common", side);
  if (scope === "left_only") {
    return side === "left"
      ? presentTransactionView(current, "left_only", side)
      : [];
  }
  return side === "right"
    ? presentTransactionView(current, "right_only", side)
    : [];
};

export const comparisonDistributionScopeSuffix = (
  scope: ComparisonDistributionScope,
): string =>
  scope === "all"
    ? ""
    : scope === "common"
      ? " · present in both"
      : scope === "left_only"
        ? " · only in Source A"
        : " · only in Source B";
