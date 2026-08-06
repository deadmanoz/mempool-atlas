import {
  classifierBucketColor,
  classifierBucketLabel,
  classifierBuckets,
  type ClassifierBucketKey,
} from "./classifier-terrain";
import type { DistributionMetric, TransactionGroup } from "./fee-distribution";
import { countFormat } from "./format";
import type { ClassifierDescriptor, MempoolTransaction } from "./types";

export const PANEL_GROUP_LIMIT = 6;
export const OVERFLOW_GROUP_COLOR = "#3d4c5a";
export const ALL_TRANSACTIONS_COLOR = "#93a7b8";

export interface PanelBucketGroup extends TransactionGroup {
  /** Exact bucket behind this group; null for aggregates. */
  bucketKey: ClassifierBucketKey | null;
}

const bucketWeight = (
  bucket: { count: number; vsize: number },
  metric: DistributionMetric,
): number => (metric === "count" ? bucket.count : bucket.vsize);

/**
 * The largest buckets of one classifier as panel series, with every remaining
 * bucket rolled into a single labelled aggregate so the groups always
 * partition the whole population. Partial buckets carry a "· partial" suffix.
 */
export const panelBucketGroups = (
  transactions: readonly MempoolTransaction[],
  descriptor: ClassifierDescriptor | null,
  metric: DistributionMetric,
  limit: number = PANEL_GROUP_LIMIT,
): PanelBucketGroup[] => {
  if (descriptor === null) {
    return [
      {
        key: "all",
        label: "All transactions",
        color: ALL_TRANSACTIONS_COLOR,
        transactions,
        bucketKey: null,
      },
    ];
  }
  const buckets = classifierBuckets(transactions, descriptor);
  const largest = new Set(
    [...buckets]
      .sort(
        (left, right) =>
          bucketWeight(right, metric) - bucketWeight(left, metric),
      )
      .slice(0, limit),
  );
  const groups: PanelBucketGroup[] = [];
  const rest: MempoolTransaction[] = [];
  let restCount = 0;
  for (const bucket of buckets) {
    if (largest.has(bucket)) {
      const bucketLabel = classifierBucketLabel(descriptor, bucket);
      groups.push({
        key: bucket.key,
        label:
          bucket.state === "partial" ? `${bucketLabel} · partial` : bucketLabel,
        color: classifierBucketColor(descriptor, bucket),
        transactions: bucket.transactions,
        bucketKey: bucket.key,
      });
    } else {
      rest.push(...bucket.transactions);
      restCount += 1;
    }
  }
  if (restCount > 0) {
    groups.push({
      key: "overflow",
      label: `${countFormat.format(restCount)} more buckets`,
      color: OVERFLOW_GROUP_COLOR,
      transactions: rest,
      bucketKey: null,
    });
  }
  return groups;
};
