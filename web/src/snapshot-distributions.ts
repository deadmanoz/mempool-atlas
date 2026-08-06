import { buildCompositionBars, buildEntanglementBars } from "./composition";
import type { CompositionBar } from "./composition";
import {
  DATA_BYTES_DOMAIN,
  FEE_RATE_DOMAIN,
  OUTPUT_VALUE_DOMAIN,
  buildComplexityDensity,
  buildFeeSpectrum,
  buildJointDensity,
  buildSpectrum,
  type DistributionMetric,
  type FeeSpectrum,
  type JointDensity,
  type TransactionGroup,
} from "./fee-distribution";
import { buildAgeMosaic, type AgeMosaic } from "./mosaic";
import { panelBucketGroups } from "./panel-groups";
import { packageFeeRate } from "./transaction-facts";
import type { ClassifierDescriptor, MempoolTransaction } from "./types";

export interface DistributionPopulationTotals {
  count: number;
  vsize: number;
}

export interface SnapshotDistributionTotals {
  population: DistributionPopulationTotals;
  structured: DistributionPopulationTotals;
  carrier: DistributionPopulationTotals;
  replaceable: DistributionPopulationTotals;
}

export interface SnapshotDistributionInput {
  transactions: readonly MempoolTransaction[];
  classifierCatalog: readonly ClassifierDescriptor[];
  selectedClassifier: ClassifierDescriptor | null;
  observedAtMs: number;
  metric: DistributionMetric;
  groupLimit: number;
  dataGroupLimit: number;
}

/**
 * Aggregate-only inputs for the nine snapshot distribution panels. This model
 * deliberately retains no source transactions or classifier partitions.
 */
export interface SnapshotDistributionModel {
  composition: CompositionBar[];
  feeSpectrum: FeeSpectrum;
  packageSpectrum: FeeSpectrum;
  jointDensity: JointDensity;
  ageMosaic: AgeMosaic;
  dataSpectrum: FeeSpectrum;
  complexityDensity: JointDensity;
  entanglement: CompositionBar[];
  valueSpectrum: FeeSpectrum;
  totals: SnapshotDistributionTotals;
}

const filteredGroups = (
  groups: readonly TransactionGroup[],
  keep: (transaction: MempoolTransaction) => boolean,
): TransactionGroup[] =>
  groups
    .map((group) => ({
      key: group.key,
      label: group.label,
      color: group.color,
      transactions: group.transactions.filter(keep),
    }))
    .filter((group) => group.transactions.length > 0);

const addTransaction = (
  totals: DistributionPopulationTotals,
  transaction: MempoolTransaction,
): void => {
  totals.count += 1;
  totals.vsize += transaction.vsize;
};

const buildTotals = (
  transactions: readonly MempoolTransaction[],
): SnapshotDistributionTotals => {
  const totals: SnapshotDistributionTotals = {
    population: { count: 0, vsize: 0 },
    structured: { count: 0, vsize: 0 },
    carrier: { count: 0, vsize: 0 },
    replaceable: { count: 0, vsize: 0 },
  };
  for (const transaction of transactions) {
    addTransaction(totals.population, transaction);
    if (transaction.structure !== null) {
      addTransaction(totals.structured, transaction);
      if (transaction.structure.op_return_bytes > 0) {
        addTransaction(totals.carrier, transaction);
      }
    }
    if (transaction.replaceable) {
      addTransaction(totals.replaceable, transaction);
    }
  }
  return totals;
};

export const buildSnapshotDistributionModel = ({
  transactions,
  classifierCatalog,
  selectedClassifier,
  observedAtMs,
  metric,
  groupLimit,
  dataGroupLimit,
}: SnapshotDistributionInput): SnapshotDistributionModel => {
  const groups = panelBucketGroups(
    transactions,
    selectedClassifier,
    metric,
    groupLimit,
  );
  const dataDescriptor =
    classifierCatalog.find(({ id }) => id === "data_protocols") ?? null;
  const carrierGroups = filteredGroups(
    panelBucketGroups(transactions, dataDescriptor, metric, dataGroupLimit),
    (transaction) => (transaction.structure?.op_return_bytes ?? 0) > 0,
  );
  const structuredGroups = filteredGroups(
    groups,
    ({ structure }) => structure !== null,
  );

  return {
    composition: buildCompositionBars(transactions, classifierCatalog, metric),
    feeSpectrum: buildFeeSpectrum(groups, metric),
    packageSpectrum: buildSpectrum(
      groups,
      metric,
      FEE_RATE_DOMAIN,
      packageFeeRate,
    ),
    jointDensity: buildJointDensity(transactions, metric),
    ageMosaic: buildAgeMosaic(groups, observedAtMs, metric),
    dataSpectrum: buildSpectrum(
      carrierGroups,
      metric,
      DATA_BYTES_DOMAIN,
      (transaction) => transaction.structure?.op_return_bytes ?? 0,
    ),
    complexityDensity: buildComplexityDensity(transactions, metric),
    entanglement: buildEntanglementBars(transactions, metric),
    valueSpectrum: buildSpectrum(
      structuredGroups,
      metric,
      OUTPUT_VALUE_DOMAIN,
      (transaction) => transaction.structure?.output_sats ?? 0,
    ),
    totals: buildTotals(transactions),
  };
};

export class SnapshotDistributionCache {
  private owner: object | null = null;
  private readonly variants = new Map<string, SnapshotDistributionModel>();

  replaceOwner(owner: object | null): void {
    if (this.owner === owner) {
      return;
    }
    this.owner = owner;
    this.variants.clear();
  }

  get(
    owner: object,
    variant: string,
    build: () => SnapshotDistributionModel,
  ): SnapshotDistributionModel {
    this.replaceOwner(owner);
    const cached = this.variants.get(variant);
    if (cached !== undefined) {
      return cached;
    }
    const model = build();
    this.variants.set(variant, model);
    return model;
  }

  reset(): void {
    this.owner = null;
    this.variants.clear();
  }
}
