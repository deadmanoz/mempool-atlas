import {
  classifierBucketColor,
  classifierBucketLabel,
  classifierBuckets,
} from "./classifier-terrain";
import type { DistributionMetric } from "./fee-distribution";
import type { ClassifierDescriptor, MempoolTransaction } from "./types";

export interface CompositionSegment {
  key: string;
  label: string;
  color: string;
  count: number;
  vsize: number;
  /** Share of the whole population by the requested metric, in [0, 1]. */
  share: number;
  /** Number of exact buckets rolled into this segment (1 for plain buckets). */
  bucketCount: number;
}

export interface CompositionBar {
  classifierId: string;
  title: string;
  segments: CompositionSegment[];
}

export const COMPOSITION_MAX_SEGMENTS = 8;
export const COMPOSITION_OVERFLOW_KEY = "overflow";
const OVERFLOW_COLOR = "#3d4c5a";

const segmentWeight = (
  segment: Pick<CompositionSegment, "count" | "vsize">,
  metric: DistributionMetric,
): number => (metric === "count" ? segment.count : segment.vsize);

export const buildCompositionBar = (
  transactions: readonly MempoolTransaction[],
  descriptor: ClassifierDescriptor,
  metric: DistributionMetric,
  maxSegments: number = COMPOSITION_MAX_SEGMENTS,
): CompositionBar => {
  const totalWeight = transactions.reduce(
    (total, transaction) =>
      total + (metric === "count" ? 1 : transaction.vsize),
    0,
  );
  const buckets = classifierBuckets(transactions, descriptor).map((bucket) => ({
    key: bucket.key,
    label: classifierBucketLabel(descriptor, bucket),
    color: classifierBucketColor(descriptor, bucket),
    count: bucket.count,
    vsize: bucket.vsize,
    share:
      totalWeight === 0
        ? 0
        : (metric === "count" ? bucket.count : bucket.vsize) / totalWeight,
    bucketCount: 1,
  }));
  if (buckets.length <= maxSegments) {
    return {
      classifierId: descriptor.id,
      title: descriptor.title,
      segments: buckets,
    };
  }
  const keep = Math.max(1, maxSegments - 1);
  const retainedKeys = new Set(
    [...buckets]
      .sort(
        (left, right) =>
          segmentWeight(right, metric) - segmentWeight(left, metric),
      )
      .slice(0, keep)
      .map(({ key }) => key),
  );
  const segments: CompositionSegment[] = [];
  let overflow: CompositionSegment | null = null;
  for (const bucket of buckets) {
    if (retainedKeys.has(bucket.key)) {
      segments.push(bucket);
      continue;
    }
    if (overflow === null) {
      overflow = {
        key: COMPOSITION_OVERFLOW_KEY,
        label: "",
        color: OVERFLOW_COLOR,
        count: 0,
        vsize: 0,
        share: 0,
        bucketCount: 0,
      };
    }
    overflow.count += bucket.count;
    overflow.vsize += bucket.vsize;
    overflow.share += bucket.share;
    overflow.bucketCount += 1;
  }
  if (overflow !== null) {
    overflow.label = `${overflow.bucketCount} more buckets`;
    segments.push(overflow);
  }
  return { classifierId: descriptor.id, title: descriptor.title, segments };
};

export const buildCompositionBars = (
  transactions: readonly MempoolTransaction[],
  catalog: readonly ClassifierDescriptor[],
  metric: DistributionMetric,
  maxSegments: number = COMPOSITION_MAX_SEGMENTS,
): CompositionBar[] =>
  catalog.map((descriptor) =>
    buildCompositionBar(transactions, descriptor, metric, maxSegments),
  );

interface RelativeBand {
  key: string;
  label: string;
  color: string;
  /** Exclusive upper bound on the relative count; null for the open band. */
  maximum: number | null;
}

/** Bands over ancestor or descendant counts; a count of 1 is the entry alone. */
const RELATIVE_BANDS: readonly RelativeBand[] = [
  { key: "alone", label: "None", color: "#3d4c5a", maximum: 1 },
  { key: "one", label: "1", color: "#72c8d6", maximum: 2 },
  { key: "few", label: "2\u20134", color: "#4397ac", maximum: 5 },
  { key: "several", label: "5\u20139", color: "#2f6a80", maximum: 10 },
  { key: "many", label: "10+", color: "#9a7735", maximum: null },
];

const relativeBand = (count: number): RelativeBand => {
  for (const band of RELATIVE_BANDS) {
    if (band.maximum === null || count < band.maximum) {
      return band;
    }
  }
  return RELATIVE_BANDS[RELATIVE_BANDS.length - 1] as RelativeBand;
};

const relativeBar = (
  title: string,
  transactions: readonly MempoolTransaction[],
  metric: DistributionMetric,
  countOf: (transaction: MempoolTransaction) => number,
): CompositionBar => {
  const totalWeight = transactions.reduce(
    (total, transaction) =>
      total + (metric === "count" ? 1 : transaction.vsize),
    0,
  );
  const byBand = new Map<
    string,
    { band: RelativeBand; count: number; vsize: number }
  >();
  for (const transaction of transactions) {
    const band = relativeBand(countOf(transaction));
    const entry = byBand.get(band.key) ?? { band, count: 0, vsize: 0 };
    entry.count += 1;
    entry.vsize += transaction.vsize;
    byBand.set(band.key, entry);
  }
  return {
    classifierId: title,
    title,
    segments: RELATIVE_BANDS.flatMap((band) => {
      const entry = byBand.get(band.key);
      if (entry === undefined) {
        return [];
      }
      return [
        {
          key: band.key,
          label: band.label,
          color: band.color,
          count: entry.count,
          vsize: entry.vsize,
          share:
            totalWeight === 0
              ? 0
              : (metric === "count" ? entry.count : entry.vsize) / totalWeight,
          bucketCount: 0,
        },
      ];
    }),
  };
};

/**
 * Two bars over source-reported mempool ancestry: unconfirmed ancestors and
 * unconfirmed descendants, banded by how many relatives each transaction has
 * beyond itself.
 */
export const buildEntanglementBars = (
  transactions: readonly MempoolTransaction[],
  metric: DistributionMetric,
): CompositionBar[] => [
  relativeBar("Unconfirmed ancestors", transactions, metric, (transaction) =>
    Math.max(0, transaction.ancestor_count - 1),
  ),
  relativeBar("Unconfirmed descendants", transactions, metric, (transaction) =>
    Math.max(0, transaction.descendant_count - 1),
  ),
];
