import type { MempoolTransaction } from "./types";

export interface MempoolFilters {
  minimumFeeRate: number;
  maximumAgeMs: number | null;
  minimumVsize: number;
}

export const DEFAULT_FILTERS: Readonly<MempoolFilters> = {
  minimumFeeRate: 0,
  maximumAgeMs: null,
  minimumVsize: 0,
};

export const filterTransactions = (
  transactions: readonly MempoolTransaction[],
  filters: Readonly<MempoolFilters>,
  observedAtMs: number,
): MempoolTransaction[] =>
  transactions.filter((transaction) => {
    const feeRate = transaction.fee_sats / transaction.vsize;
    const ageMs = Math.max(0, observedAtMs - transaction.entered_at_ms);
    return (
      feeRate >= filters.minimumFeeRate &&
      transaction.vsize >= filters.minimumVsize &&
      (filters.maximumAgeMs === null || ageMs <= filters.maximumAgeMs)
    );
  });
