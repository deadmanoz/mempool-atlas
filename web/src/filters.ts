import type { MempoolTransaction } from "./types";
import type { CooperativeWorkOptions } from "./cooperative-work";
import {
  filterTransactionView,
  filterTransactionViewCooperatively,
} from "./transaction-view";

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

export const filtersAreDefault = (filters: Readonly<MempoolFilters>): boolean =>
  filters.minimumFeeRate === DEFAULT_FILTERS.minimumFeeRate &&
  filters.maximumAgeMs === DEFAULT_FILTERS.maximumAgeMs &&
  filters.minimumVsize === DEFAULT_FILTERS.minimumVsize;

const transactionMatchesFilters = (
  transaction: MempoolTransaction,
  filters: Readonly<MempoolFilters>,
  observedAtMs: number,
): boolean => {
  const feeRate = transaction.fee_sats / transaction.vsize;
  const ageMs = Math.max(0, observedAtMs - transaction.entered_at_ms);
  return (
    feeRate >= filters.minimumFeeRate &&
    transaction.vsize >= filters.minimumVsize &&
    (filters.maximumAgeMs === null || ageMs <= filters.maximumAgeMs)
  );
};

export const filterTransactions = (
  transactions: readonly MempoolTransaction[],
  filters: Readonly<MempoolFilters>,
  observedAtMs: number,
): MempoolTransaction[] => {
  if (filtersAreDefault(filters)) return transactions as MempoolTransaction[];
  return filterTransactionView(transactions, (transaction) =>
    transactionMatchesFilters(transaction, filters, observedAtMs),
  );
};

export const filterTransactionsCooperatively = async (
  transactions: readonly MempoolTransaction[],
  filters: Readonly<MempoolFilters>,
  observedAtMs: number,
  options: CooperativeWorkOptions = {},
): Promise<MempoolTransaction[]> => {
  options.signal?.throwIfAborted();
  if (filtersAreDefault(filters)) return transactions as MempoolTransaction[];
  return filterTransactionViewCooperatively(
    transactions,
    (transaction) =>
      transactionMatchesFilters(transaction, filters, observedAtMs),
    options,
  );
};
