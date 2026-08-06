import type { MempoolTransaction } from "./types";

export type DistributionMetric = "count" | "vsize";

export interface LogDomain {
  minLog2: number;
  maxLog2: number;
}

export interface AxisTick {
  position: number;
  label: string;
}

export const FEE_RATE_DOMAIN: Readonly<LogDomain> = { minLog2: 0, maxLog2: 9 };
export const VSIZE_DOMAIN: Readonly<LogDomain> = { minLog2: 6, maxLog2: 17 };

export const FEE_RATE_TICKS: readonly AxisTick[] = [
  { position: 0, label: "1" },
  { position: 5 / 9, label: "32" },
  { position: 1, label: "512+" },
];

/** OP_RETURN carried bytes, one byte through 128 KiB. */
export const DATA_BYTES_DOMAIN: Readonly<LogDomain> = {
  minLog2: 0,
  maxLog2: 17,
};

export const DATA_BYTES_TICKS: readonly AxisTick[] = [
  { position: 0, label: "1 B" },
  { position: 9 / 17, label: "512 B" },
  { position: 1, label: "128 KiB+" },
];

/** Input or output counts, one through 1024. */
export const IO_COUNT_DOMAIN: Readonly<LogDomain> = {
  minLog2: 0,
  maxLog2: 10,
};

export const IO_COUNT_TICKS: readonly AxisTick[] = [
  { position: 0, label: "1" },
  { position: 0.5, label: "32" },
  { position: 1, label: "1k+" },
];

/** Total output value in sats, 1k sats through 1000 BTC. */
export const OUTPUT_VALUE_DOMAIN: Readonly<LogDomain> = {
  minLog2: Math.log2(1_000),
  maxLog2: Math.log2(100_000_000_000),
};

export const OUTPUT_VALUE_TICKS: readonly AxisTick[] = [
  { position: 0, label: "1k sat" },
  {
    position:
      (Math.log2(100_000_000) - OUTPUT_VALUE_DOMAIN.minLog2) /
      (OUTPUT_VALUE_DOMAIN.maxLog2 - OUTPUT_VALUE_DOMAIN.minLog2),
    label: "1 BTC",
  },
  { position: 1, label: "1k BTC+" },
];

export const transactionFeeRate = (transaction: MempoolTransaction): number =>
  transaction.vsize > 0 ? transaction.fee_sats / transaction.vsize : 0;

export const logDomainPosition = (
  domain: Readonly<LogDomain>,
  value: number,
): number => {
  if (!(value > 0)) {
    return 0;
  }
  const span = domain.maxLog2 - domain.minLog2;
  const position = (Math.log2(value) - domain.minLog2) / span;
  return Math.min(1, Math.max(0, position));
};

const metricWeight = (
  transaction: MempoolTransaction,
  metric: DistributionMetric,
): number => (metric === "count" ? 1 : transaction.vsize);

/** One classifier bucket (or aggregate) feeding a multi-series panel. */
export interface TransactionGroup {
  key: string;
  label: string;
  color: string;
  transactions: readonly MempoolTransaction[];
}

export interface JointDensity {
  columns: number;
  rows: number;
  /** Row-major weights; row 0 holds the smallest value band. */
  cells: number[];
  columnTotals: number[];
  rowTotals: number[];
  maxCell: number;
  totalWeight: number;
}

export const JOINT_COLUMNS = 22;
export const JOINT_ROWS = 14;

export const buildDensity = (
  transactions: readonly MempoolTransaction[],
  metric: DistributionMetric,
  xDomain: Readonly<LogDomain>,
  xValue: (transaction: MempoolTransaction) => number,
  yDomain: Readonly<LogDomain>,
  yValue: (transaction: MempoolTransaction) => number,
  columns: number = JOINT_COLUMNS,
  rows: number = JOINT_ROWS,
): JointDensity => {
  const cells = new Array<number>(columns * rows).fill(0);
  const columnTotals = new Array<number>(columns).fill(0);
  const rowTotals = new Array<number>(rows).fill(0);
  let totalWeight = 0;
  let maxCell = 0;
  for (const transaction of transactions) {
    const weight = metricWeight(transaction, metric);
    if (weight <= 0) {
      continue;
    }
    const xPosition = logDomainPosition(xDomain, xValue(transaction));
    const yPosition = logDomainPosition(yDomain, yValue(transaction));
    const column = Math.min(columns - 1, Math.floor(xPosition * columns));
    const row = Math.min(rows - 1, Math.floor(yPosition * rows));
    const index = row * columns + column;
    const updated = (cells[index] ?? 0) + weight;
    cells[index] = updated;
    columnTotals[column] = (columnTotals[column] ?? 0) + weight;
    rowTotals[row] = (rowTotals[row] ?? 0) + weight;
    totalWeight += weight;
    if (updated > maxCell) {
      maxCell = updated;
    }
  }
  return {
    columns,
    rows,
    cells,
    columnTotals,
    rowTotals,
    maxCell,
    totalWeight,
  };
};

export const buildJointDensity = (
  transactions: readonly MempoolTransaction[],
  metric: DistributionMetric,
  columns: number = JOINT_COLUMNS,
  rows: number = JOINT_ROWS,
): JointDensity =>
  buildDensity(
    transactions,
    metric,
    FEE_RATE_DOMAIN,
    (transaction) => transactionFeeRate(transaction),
    VSIZE_DOMAIN,
    ({ vsize }) => vsize,
    columns,
    rows,
  );

/**
 * Input-count by output-count density over transactions with structure facts.
 * Transactions whose structure has not arrived are skipped.
 */
export const buildComplexityDensity = (
  transactions: readonly MempoolTransaction[],
  metric: DistributionMetric,
  columns: number = JOINT_COLUMNS,
  rows: number = JOINT_ROWS,
): JointDensity =>
  buildDensity(
    transactions.filter(({ structure }) => structure !== null),
    metric,
    IO_COUNT_DOMAIN,
    ({ structure }) => structure?.input_count ?? 0,
    IO_COUNT_DOMAIN,
    ({ structure }) => structure?.output_count ?? 0,
    columns,
    rows,
  );

export interface SpectrumBinSegment {
  key: string;
  label: string;
  color: string;
  weight: number;
}

export interface SpectrumBin {
  total: number;
  segments: SpectrumBinSegment[];
}

export interface FeeSpectrum {
  bins: SpectrumBin[];
  maxBin: number;
  totalWeight: number;
}

export const SPECTRUM_BINS = 22;

/**
 * Stacked fee-rate histogram: per log-scale fee bin, one segment per group in
 * group order. Groups must partition their population; bins cover the whole
 * clamped fee-rate domain.
 */
export const buildSpectrum = (
  groups: readonly TransactionGroup[],
  metric: DistributionMetric,
  domain: Readonly<LogDomain>,
  value: (transaction: MempoolTransaction) => number,
  binCount: number = SPECTRUM_BINS,
): FeeSpectrum => {
  const bins: SpectrumBin[] = Array.from({ length: binCount }, () => ({
    total: 0,
    segments: [],
  }));
  let totalWeight = 0;
  let maxBin = 0;
  for (const group of groups) {
    const weights = new Array<number>(binCount).fill(0);
    let groupWeight = 0;
    for (const transaction of group.transactions) {
      const weight = metricWeight(transaction, metric);
      if (weight <= 0) {
        continue;
      }
      const position = logDomainPosition(domain, value(transaction));
      const bin = Math.min(binCount - 1, Math.floor(position * binCount));
      weights[bin] = (weights[bin] ?? 0) + weight;
      groupWeight += weight;
    }
    if (groupWeight === 0) {
      continue;
    }
    totalWeight += groupWeight;
    for (let bin = 0; bin < binCount; bin += 1) {
      const weight = weights[bin] ?? 0;
      if (weight <= 0) {
        continue;
      }
      const entry = bins[bin];
      if (entry === undefined) {
        continue;
      }
      entry.total += weight;
      entry.segments.push({
        key: group.key,
        label: group.label,
        color: group.color,
        weight,
      });
      if (entry.total > maxBin) {
        maxBin = entry.total;
      }
    }
  }
  return { bins, maxBin, totalWeight };
};

export const buildFeeSpectrum = (
  groups: readonly TransactionGroup[],
  metric: DistributionMetric,
  binCount: number = SPECTRUM_BINS,
): FeeSpectrum =>
  buildSpectrum(
    groups,
    metric,
    FEE_RATE_DOMAIN,
    (transaction) => transactionFeeRate(transaction),
    binCount,
  );
