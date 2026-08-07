import type {
  ClassificationResult,
  ClassifierDescriptor,
  ClassifierSummary,
  MempoolSnapshot,
  MempoolTransaction,
} from "./types";
import {
  filterTransactionView,
  sortTransactionViewByVsize,
} from "./transaction-view";

export const DEFAULT_CLASSIFIER_ID = "transaction_properties";

export interface ClassificationPopulation {
  count: number;
  vsize: number;
  totalShare: number;
  transactions: MempoolTransaction[];
}

export const classifierDescriptor = (
  snapshot: MempoolSnapshot,
  classifierId: string,
): ClassifierDescriptor | null =>
  snapshot.classifier_catalog.find(({ id }) => id === classifierId) ?? null;

export const classifierSummary = (
  snapshot: MempoolSnapshot,
  classifierId: string,
): ClassifierSummary | null =>
  snapshot.classification_summaries.find(
    ({ classifier_id: id }) => id === classifierId,
  ) ?? null;

export const classificationResult = (
  transaction: MempoolTransaction,
  classifierId: string,
): ClassificationResult | null =>
  transaction.classifications.find(
    ({ classifier_id: id }) => id === classifierId,
  ) ?? null;

export const classificationPopulation = (
  transactions: readonly MempoolTransaction[],
  classifierId: string,
  label: string,
): ClassificationPopulation => {
  const matching = sortTransactionViewByVsize(
    filterTransactionView(
      transactions,
      (transaction) =>
        classificationResult(transaction, classifierId)?.labels.includes(
          label,
        ) ?? false,
    ),
  );
  const vsize = matching.reduce(
    (total, transaction) => total + transaction.vsize,
    0,
  );
  return {
    count: matching.length,
    vsize,
    totalShare:
      transactions.length === 0 ? 0 : matching.length / transactions.length,
    transactions: matching,
  };
};

export const firstPopulatedLabel = (
  descriptor: ClassifierDescriptor,
  summary: ClassifierSummary,
): string =>
  descriptor.labels.reduce(
    (selected, candidate) =>
      (summary.label_counts[candidate.key] ?? 0) >
      (summary.label_counts[selected] ?? 0)
        ? candidate.key
        : selected,
    descriptor.labels[0]?.key ?? "",
  );
