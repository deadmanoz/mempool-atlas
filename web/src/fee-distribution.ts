import {
  forEachCooperatively,
  type CooperativeWorkOptions,
} from "./cooperative-work";
import type { MempoolTransaction } from "./types";

export type DistributionMetric = "count" | "vsize";

export interface LogDomain {
  minLog2: number;
  maxLog2: number;
}

export interface AxisTick {
  /** Raw value on the owning logarithmic domain. */
  value: number;
  position: number;
  label: string;
  /** Longer context for an interactive marker or reduced-density label. */
  description?: string;
  /** Higher-priority labels should survive responsive tick reduction. */
  priority: number;
}

export interface AxisTickValue {
  value: number;
  label: string;
  description?: string;
  priority?: number;
}

export interface LogDomainBinBounds {
  /** Null for the first bin, which also receives lower clamped values. */
  lowerInclusive: number | null;
  /** Null for the last bin, which also receives upper clamped values. */
  upperExclusive: number | null;
}

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

/** Inverse of `logDomainPosition` over the unclamped domain. */
export const logDomainValue = (
  domain: Readonly<LogDomain>,
  position: number,
): number => {
  const clampedPosition = Math.min(1, Math.max(0, position));
  return (
    2 ** (domain.minLog2 + clampedPosition * (domain.maxLog2 - domain.minLog2))
  );
};

/**
 * Exact value bounds for one equal-width logarithmic bin. Null open bounds
 * describe the values clamped into the first and last bins.
 */
export const logDomainBinBounds = (
  domain: Readonly<LogDomain>,
  bin: number,
  binCount: number,
): LogDomainBinBounds => {
  if (!Number.isInteger(binCount) || binCount <= 0) {
    throw new RangeError("Log-domain bin count must be a positive integer");
  }
  if (!Number.isInteger(bin) || bin < 0 || bin >= binCount) {
    throw new RangeError("Log-domain bin index is outside the requested range");
  }
  return {
    lowerInclusive: bin === 0 ? null : logDomainValue(domain, bin / binCount),
    upperExclusive:
      bin === binCount - 1
        ? null
        : logDomainValue(domain, (bin + 1) / binCount),
  };
};

/** Build positioned ticks from meaningful raw values rather than fractions. */
export const logDomainTicks = (
  domain: Readonly<LogDomain>,
  ticks: readonly AxisTickValue[],
): AxisTick[] =>
  ticks.map(({ value, label, description, priority = 1 }) => ({
    value,
    position: logDomainPosition(domain, value),
    label,
    ...(description === undefined ? {} : { description }),
    priority,
  }));

export const FEE_RATE_DOMAIN: Readonly<LogDomain> = { minLog2: 0, maxLog2: 9 };
export const VSIZE_DOMAIN: Readonly<LogDomain> = { minLog2: 6, maxLog2: 17 };

export const FEE_RATE_TICKS: readonly AxisTick[] = logDomainTicks(
  FEE_RATE_DOMAIN,
  [
    { value: 1, label: "1", priority: 3 },
    { value: 4, label: "4", priority: 2 },
    { value: 16, label: "16", priority: 2 },
    { value: 64, label: "64", priority: 2 },
    { value: 256, label: "256", priority: 2 },
    { value: 512, label: "512+", priority: 3 },
  ],
);

export const VSIZE_TICKS: readonly AxisTick[] = logDomainTicks(VSIZE_DOMAIN, [
  { value: 64, label: "64 vB", priority: 3 },
  { value: 256, label: "256 vB", priority: 2 },
  { value: 1_024, label: "1 KivB", priority: 2 },
  { value: 4_096, label: "4 KivB", priority: 2 },
  { value: 16_384, label: "16 KivB", priority: 2 },
  { value: 65_536, label: "64 KivB", priority: 2 },
  { value: 131_072, label: "128 KivB+", priority: 3 },
]);

/** OP_RETURN carried bytes, one byte through 128 KiB. */
export const DATA_BYTES_DOMAIN: Readonly<LogDomain> = {
  minLog2: 0,
  maxLog2: 17,
};

export const DATA_BYTES_TICKS: readonly AxisTick[] = logDomainTicks(
  DATA_BYTES_DOMAIN,
  [
    { value: 1, label: "1 B", priority: 3 },
    { value: 8, label: "8 B", priority: 1 },
    {
      value: 40,
      label: "40 B",
      description:
        "Historical payload reference. Bitcoin Core 0.9 and 0.10 allowed at most 40 pushed data bytes in the then-standard single-push OP_RETURN form (42 serialized script bytes with a direct push).",
      priority: 3,
    },
    {
      value: 80,
      label: "80 B",
      description:
        "Payload reference. Bitcoin Core 0.11 defaulted to 80 pushed data bytes. A conventional single-push 80-byte payload serializes to an 83-byte OP_RETURN script, the Core 0.12–29 and BIP-110 script-size reference.",
      priority: 3,
    },
    { value: 512, label: "512 B", priority: 2 },
    { value: 4_096, label: "4 KiB", priority: 2 },
    { value: 32_768, label: "32 KiB", priority: 1 },
    { value: 131_072, label: "128 KiB+", priority: 3 },
  ],
);

/** Input or output counts, one through 1024. */
export const IO_COUNT_DOMAIN: Readonly<LogDomain> = {
  minLog2: 0,
  maxLog2: 10,
};

export const IO_COUNT_TICKS: readonly AxisTick[] = logDomainTicks(
  IO_COUNT_DOMAIN,
  [
    { value: 1, label: "1", priority: 3 },
    { value: 4, label: "4", priority: 2 },
    { value: 16, label: "16", priority: 2 },
    { value: 64, label: "64", priority: 2 },
    { value: 256, label: "256", priority: 2 },
    { value: 1_024, label: "1k+", priority: 3 },
  ],
);

/** Total output value in sats, 1k sats through 1000 BTC. */
export const OUTPUT_VALUE_DOMAIN: Readonly<LogDomain> = {
  minLog2: Math.log2(1_000),
  maxLog2: Math.log2(100_000_000_000),
};

export const OUTPUT_VALUE_TICKS: readonly AxisTick[] = logDomainTicks(
  OUTPUT_VALUE_DOMAIN,
  [
    { value: 1_000, label: "1k sat", priority: 3 },
    { value: 100_000, label: "100k sat", priority: 2 },
    { value: 10_000_000, label: "10m sat", priority: 2 },
    { value: 100_000_000, label: "1 BTC", priority: 3 },
    { value: 10_000_000_000, label: "100 BTC", priority: 1 },
    { value: 100_000_000_000, label: "1k BTC+", priority: 3 },
  ],
);

export const transactionFeeRate = (transaction: MempoolTransaction): number =>
  transaction.vsize > 0 ? transaction.fee_sats / transaction.vsize : 0;

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

interface DensityAccumulator extends JointDensity {}

const createDensityAccumulator = (
  columns: number,
  rows: number,
): DensityAccumulator => ({
  columns,
  rows,
  cells: new Array<number>(columns * rows).fill(0),
  columnTotals: new Array<number>(columns).fill(0),
  rowTotals: new Array<number>(rows).fill(0),
  maxCell: 0,
  totalWeight: 0,
});

const addDensityTransaction = (
  accumulator: DensityAccumulator,
  transaction: MempoolTransaction,
  metric: DistributionMetric,
  xDomain: Readonly<LogDomain>,
  xValue: (transaction: MempoolTransaction) => number,
  yDomain: Readonly<LogDomain>,
  yValue: (transaction: MempoolTransaction) => number,
): void => {
  const weight = metricWeight(transaction, metric);
  if (weight <= 0) return;
  const xPosition = logDomainPosition(xDomain, xValue(transaction));
  const yPosition = logDomainPosition(yDomain, yValue(transaction));
  const column = Math.min(
    accumulator.columns - 1,
    Math.floor(xPosition * accumulator.columns),
  );
  const row = Math.min(
    accumulator.rows - 1,
    Math.floor(yPosition * accumulator.rows),
  );
  const index = row * accumulator.columns + column;
  const updated = (accumulator.cells[index] ?? 0) + weight;
  accumulator.cells[index] = updated;
  accumulator.columnTotals[column] =
    (accumulator.columnTotals[column] ?? 0) + weight;
  accumulator.rowTotals[row] = (accumulator.rowTotals[row] ?? 0) + weight;
  accumulator.totalWeight += weight;
  if (updated > accumulator.maxCell) accumulator.maxCell = updated;
};

export const buildDensity = (
  transactions: readonly MempoolTransaction[],
  metric: DistributionMetric,
  xDomain: Readonly<LogDomain>,
  xValue: (transaction: MempoolTransaction) => number,
  yDomain: Readonly<LogDomain>,
  yValue: (transaction: MempoolTransaction) => number,
  columns: number = JOINT_COLUMNS,
  rows: number = JOINT_ROWS,
  keep: (transaction: MempoolTransaction) => boolean = () => true,
): JointDensity => {
  const accumulator = createDensityAccumulator(columns, rows);
  for (const transaction of transactions) {
    if (!keep(transaction)) continue;
    addDensityTransaction(
      accumulator,
      transaction,
      metric,
      xDomain,
      xValue,
      yDomain,
      yValue,
    );
  }
  return accumulator;
};

export const buildDensityCooperatively = async (
  transactions: readonly MempoolTransaction[],
  metric: DistributionMetric,
  xDomain: Readonly<LogDomain>,
  xValue: (transaction: MempoolTransaction) => number,
  yDomain: Readonly<LogDomain>,
  yValue: (transaction: MempoolTransaction) => number,
  columns: number = JOINT_COLUMNS,
  rows: number = JOINT_ROWS,
  options: CooperativeWorkOptions = {},
  keep: (transaction: MempoolTransaction) => boolean = () => true,
): Promise<JointDensity> => {
  const accumulator = createDensityAccumulator(columns, rows);
  await forEachCooperatively(
    transactions,
    (transaction) => {
      if (!keep(transaction)) return;
      addDensityTransaction(
        accumulator,
        transaction,
        metric,
        xDomain,
        xValue,
        yDomain,
        yValue,
      );
    },
    options,
  );
  return accumulator;
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

export const buildJointDensityCooperatively = (
  transactions: readonly MempoolTransaction[],
  metric: DistributionMetric,
  options: CooperativeWorkOptions = {},
  columns: number = JOINT_COLUMNS,
  rows: number = JOINT_ROWS,
): Promise<JointDensity> =>
  buildDensityCooperatively(
    transactions,
    metric,
    FEE_RATE_DOMAIN,
    transactionFeeRate,
    VSIZE_DOMAIN,
    ({ vsize }) => vsize,
    columns,
    rows,
    options,
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
    transactions,
    metric,
    IO_COUNT_DOMAIN,
    ({ structure }) => structure?.input_count ?? 0,
    IO_COUNT_DOMAIN,
    ({ structure }) => structure?.output_count ?? 0,
    columns,
    rows,
    ({ structure }) => structure !== null,
  );

export const buildComplexityDensityCooperatively = (
  transactions: readonly MempoolTransaction[],
  metric: DistributionMetric,
  options: CooperativeWorkOptions = {},
  columns: number = JOINT_COLUMNS,
  rows: number = JOINT_ROWS,
): Promise<JointDensity> =>
  buildDensityCooperatively(
    transactions,
    metric,
    IO_COUNT_DOMAIN,
    ({ structure }) => structure?.input_count ?? 0,
    IO_COUNT_DOMAIN,
    ({ structure }) => structure?.output_count ?? 0,
    columns,
    rows,
    options,
    ({ structure }) => structure !== null,
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

interface SpectrumAccumulator extends FeeSpectrum {}

const createSpectrumAccumulator = (binCount: number): SpectrumAccumulator => ({
  bins: Array.from({ length: binCount }, () => ({
    total: 0,
    segments: [],
  })),
  maxBin: 0,
  totalWeight: 0,
});

const addSpectrumTransaction = (
  weights: number[],
  transaction: MempoolTransaction,
  metric: DistributionMetric,
  domain: Readonly<LogDomain>,
  value: (transaction: MempoolTransaction) => number,
): number => {
  const weight = metricWeight(transaction, metric);
  if (weight <= 0) return 0;
  const position = logDomainPosition(domain, value(transaction));
  const bin = Math.min(
    weights.length - 1,
    Math.floor(position * weights.length),
  );
  weights[bin] = (weights[bin] ?? 0) + weight;
  return weight;
};

const finishSpectrumGroup = (
  accumulator: SpectrumAccumulator,
  group: TransactionGroup,
  weights: readonly number[],
  groupWeight: number,
): void => {
  if (groupWeight === 0) return;
  accumulator.totalWeight += groupWeight;
  for (let bin = 0; bin < weights.length; bin += 1) {
    const weight = weights[bin] ?? 0;
    if (weight <= 0) continue;
    const entry = accumulator.bins[bin];
    if (entry === undefined) continue;
    entry.total += weight;
    entry.segments.push({
      key: group.key,
      label: group.label,
      color: group.color,
      weight,
    });
    if (entry.total > accumulator.maxBin) accumulator.maxBin = entry.total;
  }
};

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
  const accumulator = createSpectrumAccumulator(binCount);
  for (const group of groups) {
    const weights = new Array<number>(binCount).fill(0);
    let groupWeight = 0;
    for (const transaction of group.transactions) {
      groupWeight += addSpectrumTransaction(
        weights,
        transaction,
        metric,
        domain,
        value,
      );
    }
    finishSpectrumGroup(accumulator, group, weights, groupWeight);
  }
  return accumulator;
};

export const buildSpectrumCooperatively = async (
  groups: readonly TransactionGroup[],
  metric: DistributionMetric,
  domain: Readonly<LogDomain>,
  value: (transaction: MempoolTransaction) => number,
  options: CooperativeWorkOptions = {},
  binCount: number = SPECTRUM_BINS,
  keep: (transaction: MempoolTransaction) => boolean = () => true,
): Promise<FeeSpectrum> => {
  const accumulator = createSpectrumAccumulator(binCount);
  for (const group of groups) {
    const weights = new Array<number>(binCount).fill(0);
    let groupWeight = 0;
    await forEachCooperatively(
      group.transactions,
      (transaction) => {
        if (!keep(transaction)) return;
        groupWeight += addSpectrumTransaction(
          weights,
          transaction,
          metric,
          domain,
          value,
        );
      },
      options,
    );
    finishSpectrumGroup(accumulator, group, weights, groupWeight);
  }
  options.signal?.throwIfAborted();
  return accumulator;
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

export const buildFeeSpectrumCooperatively = (
  groups: readonly TransactionGroup[],
  metric: DistributionMetric,
  options: CooperativeWorkOptions = {},
  binCount: number = SPECTRUM_BINS,
): Promise<FeeSpectrum> =>
  buildSpectrumCooperatively(
    groups,
    metric,
    FEE_RATE_DOMAIN,
    transactionFeeRate,
    options,
    binCount,
  );
