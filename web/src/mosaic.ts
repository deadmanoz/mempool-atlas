import type { DistributionMetric, TransactionGroup } from "./fee-distribution";

export interface AgeBand {
  key: string;
  label: string;
  color: string;
  /** Exclusive upper bound in milliseconds; null for the open last band. */
  maxAgeMs: number | null;
}

export const AGE_BANDS: readonly AgeBand[] = [
  { key: "under_10m", label: "< 10 min", color: "#b9eff3", maxAgeMs: 600_000 },
  {
    key: "under_1h",
    label: "10–60 min",
    color: "#72c8d6",
    maxAgeMs: 3_600_000,
  },
  { key: "under_6h", label: "1–6 h", color: "#4397ac", maxAgeMs: 21_600_000 },
  { key: "under_24h", label: "6–24 h", color: "#2f6a80", maxAgeMs: 86_400_000 },
  { key: "over_24h", label: "> 24 h", color: "#28495c", maxAgeMs: null },
];

export interface MosaicCell {
  bandKey: string;
  bandLabel: string;
  bandColor: string;
  count: number;
  weight: number;
  /** Share of the owning column's weight, in [0, 1]. */
  share: number;
}

export interface MosaicColumn {
  key: string;
  label: string;
  color: string;
  count: number;
  weight: number;
  /** Share of the whole population's weight, in [0, 1]. */
  share: number;
  cells: MosaicCell[];
}

export interface AgeMosaic {
  columns: MosaicColumn[];
  totalWeight: number;
}

const bandIndex = (ageMs: number): number => {
  for (const [index, band] of AGE_BANDS.entries()) {
    if (band.maxAgeMs === null || ageMs < band.maxAgeMs) {
      return index;
    }
  }
  return AGE_BANDS.length - 1;
};

/**
 * Two-way composition of one classifier's buckets against observation-relative
 * age bands: column width is the bucket's population share, cell height is the
 * band's share within that bucket. Ages never use the browser clock.
 */
export const buildAgeMosaic = (
  groups: readonly TransactionGroup[],
  observedAtMs: number,
  metric: DistributionMetric,
): AgeMosaic => {
  const columns: MosaicColumn[] = [];
  let totalWeight = 0;
  for (const group of groups) {
    const bandWeights = AGE_BANDS.map(() => ({ count: 0, weight: 0 }));
    let groupWeight = 0;
    for (const transaction of group.transactions) {
      const weight = metric === "count" ? 1 : transaction.vsize;
      if (weight <= 0) {
        continue;
      }
      const ageMs = Math.max(0, observedAtMs - transaction.entered_at_ms);
      const entry = bandWeights[bandIndex(ageMs)];
      if (entry === undefined) {
        continue;
      }
      entry.count += 1;
      entry.weight += weight;
      groupWeight += weight;
    }
    if (groupWeight === 0) {
      continue;
    }
    totalWeight += groupWeight;
    columns.push({
      key: group.key,
      label: group.label,
      color: group.color,
      count: group.transactions.length,
      weight: groupWeight,
      share: 0,
      cells: AGE_BANDS.flatMap((band, index) => {
        const entry = bandWeights[index];
        if (entry === undefined || entry.weight === 0) {
          return [];
        }
        return [
          {
            bandKey: band.key,
            bandLabel: band.label,
            bandColor: band.color,
            count: entry.count,
            weight: entry.weight,
            share: entry.weight / groupWeight,
          },
        ];
      }),
    });
  }
  for (const column of columns) {
    column.share = totalWeight === 0 ? 0 : column.weight / totalWeight;
  }
  return { columns, totalWeight };
};
