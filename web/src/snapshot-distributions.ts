import {
  buildCompositionBars,
  buildEntanglementBars,
  buildEntanglementBarsCooperatively,
} from "./composition";
import type { CompositionBar } from "./composition";
import { precomputeClassifierBuckets } from "./classifier-terrain";
import {
  combineAbortSignals,
  forEachCooperatively,
  type CooperativeWorkOptions,
} from "./cooperative-work";
import {
  DATA_BYTES_DOMAIN,
  FEE_RATE_DOMAIN,
  OUTPUT_VALUE_DOMAIN,
  buildComplexityDensity,
  buildComplexityDensityCooperatively,
  buildFeeSpectrum,
  buildFeeSpectrumCooperatively,
  buildJointDensity,
  buildJointDensityCooperatively,
  buildSpectrum,
  buildSpectrumCooperatively,
  type DistributionMetric,
  type FeeSpectrum,
  type JointDensity,
  type TransactionGroup,
} from "./fee-distribution";
import {
  buildAgeMosaic,
  buildAgeMosaicCooperatively,
  type AgeMosaic,
} from "./mosaic";
import { panelBucketGroups } from "./panel-groups";
import { ancestorFeeRate } from "./transaction-facts";
import { filterTransactionView } from "./transaction-view";
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
  ancestorFeeSpectrum: FeeSpectrum;
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
      transactions: filterTransactionView(group.transactions, keep),
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
      if (transaction.structure.recognized_carried_bytes > 0) {
        addTransaction(totals.carrier, transaction);
      }
    }
    if (transaction.replaceable) {
      addTransaction(totals.replaceable, transaction);
    }
  }
  return totals;
};

const buildTotalsCooperatively = async (
  transactions: readonly MempoolTransaction[],
  options: CooperativeWorkOptions,
): Promise<SnapshotDistributionTotals> => {
  const totals: SnapshotDistributionTotals = {
    population: { count: 0, vsize: 0 },
    structured: { count: 0, vsize: 0 },
    carrier: { count: 0, vsize: 0 },
    replaceable: { count: 0, vsize: 0 },
  };
  await forEachCooperatively(
    transactions,
    (transaction) => {
      addTransaction(totals.population, transaction);
      if (transaction.structure !== null) {
        addTransaction(totals.structured, transaction);
        if (transaction.structure.recognized_carried_bytes > 0) {
          addTransaction(totals.carrier, transaction);
        }
      }
      if (transaction.replaceable) {
        addTransaction(totals.replaceable, transaction);
      }
    },
    options,
  );
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
  const carriageDescriptor =
    classifierCatalog.find(({ id }) => id === "data_carriage_shape") ?? null;
  const carrierGroups = filteredGroups(
    panelBucketGroups(transactions, carriageDescriptor, metric, dataGroupLimit),
    (transaction) => (transaction.structure?.recognized_carried_bytes ?? 0) > 0,
  );
  const structuredGroups = filteredGroups(
    groups,
    ({ structure }) => structure !== null,
  );

  return {
    composition: buildCompositionBars(transactions, classifierCatalog, metric),
    feeSpectrum: buildFeeSpectrum(groups, metric),
    ancestorFeeSpectrum: buildSpectrum(
      groups,
      metric,
      FEE_RATE_DOMAIN,
      ancestorFeeRate,
    ),
    jointDensity: buildJointDensity(transactions, metric),
    ageMosaic: buildAgeMosaic(groups, observedAtMs, metric),
    dataSpectrum: buildSpectrum(
      carrierGroups,
      metric,
      DATA_BYTES_DOMAIN,
      (transaction) => transaction.structure?.recognized_carried_bytes ?? 0,
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

export const buildSnapshotDistributionModelCooperatively = async (
  {
    transactions,
    classifierCatalog,
    selectedClassifier,
    observedAtMs,
    metric,
    groupLimit,
    dataGroupLimit,
  }: SnapshotDistributionInput,
  options: CooperativeWorkOptions & {
    classifierSortTimeBudgetMs?: number;
    aggregateOnlyClassifierBuckets?: boolean;
  } = {},
): Promise<SnapshotDistributionModel> => {
  options.signal?.throwIfAborted();
  await precomputeClassifierBuckets(transactions, classifierCatalog, {
    ...options,
    ...(options.classifierSortTimeBudgetMs === undefined
      ? {}
      : { sortTimeBudgetMs: options.classifierSortTimeBudgetMs }),
    ...(options.aggregateOnlyClassifierBuckets === undefined
      ? {}
      : { aggregateOnly: options.aggregateOnlyClassifierBuckets }),
  });
  options.signal?.throwIfAborted();

  const groups = panelBucketGroups(
    transactions,
    selectedClassifier,
    metric,
    groupLimit,
  );
  const carriageDescriptor =
    classifierCatalog.find(({ id }) => id === "data_carriage_shape") ?? null;
  const carrierGroups = panelBucketGroups(
    transactions,
    carriageDescriptor,
    metric,
    dataGroupLimit,
  );

  const composition = buildCompositionBars(
    transactions,
    classifierCatalog,
    metric,
  );
  const feeSpectrum = await buildFeeSpectrumCooperatively(
    groups,
    metric,
    options,
  );
  const ancestorFeeSpectrum = await buildSpectrumCooperatively(
    groups,
    metric,
    FEE_RATE_DOMAIN,
    ancestorFeeRate,
    options,
  );
  const jointDensity = await buildJointDensityCooperatively(
    transactions,
    metric,
    options,
  );
  const ageMosaic = await buildAgeMosaicCooperatively(
    groups,
    observedAtMs,
    metric,
    options,
  );
  const dataSpectrum = await buildSpectrumCooperatively(
    carrierGroups,
    metric,
    DATA_BYTES_DOMAIN,
    (transaction) => transaction.structure?.recognized_carried_bytes ?? 0,
    options,
    undefined,
    (transaction) => (transaction.structure?.recognized_carried_bytes ?? 0) > 0,
  );
  const complexityDensity = await buildComplexityDensityCooperatively(
    transactions,
    metric,
    options,
  );
  const entanglement = await buildEntanglementBarsCooperatively(
    transactions,
    metric,
    options,
  );
  const valueSpectrum = await buildSpectrumCooperatively(
    groups,
    metric,
    OUTPUT_VALUE_DOMAIN,
    (transaction) => transaction.structure?.output_sats ?? 0,
    options,
    undefined,
    ({ structure }) => structure !== null,
  );
  const totals = await buildTotalsCooperatively(transactions, options);
  options.signal?.throwIfAborted();
  return {
    composition,
    feeSpectrum,
    ancestorFeeSpectrum,
    jointDensity,
    ageMosaic,
    dataSpectrum,
    complexityDensity,
    entanglement,
    valueSpectrum,
    totals,
  };
};

export class SnapshotDistributionCache {
  private owner: object | null = null;
  private readonly variants = new Map<string, SnapshotDistributionModel>();
  private readonly pending = new Map<
    string,
    Promise<SnapshotDistributionModel>
  >();
  private ownerController: AbortController | null = null;
  private ownerEpoch = 0;

  replaceOwner(owner: object | null): void {
    if (this.owner === owner) {
      return;
    }
    this.ownerController?.abort();
    this.owner = owner;
    this.ownerEpoch += 1;
    this.variants.clear();
    this.pending.clear();
    this.ownerController = owner === null ? null : new AbortController();
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

  adopt(
    owner: object,
    variant: string,
    model: SnapshotDistributionModel,
  ): SnapshotDistributionModel {
    this.replaceOwner(owner);
    this.variants.set(variant, model);
    return model;
  }

  getAsync(
    owner: object,
    variant: string,
    build: (signal: AbortSignal) => Promise<SnapshotDistributionModel>,
    externalSignal?: AbortSignal,
  ): Promise<SnapshotDistributionModel> {
    this.replaceOwner(owner);
    const cached = this.variants.get(variant);
    if (cached !== undefined) return Promise.resolve(cached);
    const inFlight = this.pending.get(variant);
    if (inFlight !== undefined && externalSignal === undefined) return inFlight;
    const epoch = this.ownerEpoch;
    const controller = this.ownerController;
    if (controller === null) {
      return Promise.reject(new Error("Distribution cache has no owner"));
    }
    const combinedSignal =
      externalSignal === undefined
        ? null
        : combineAbortSignals([controller.signal, externalSignal]);
    const signal = combinedSignal?.signal ?? controller.signal;
    const trackPending = externalSignal === undefined;
    let buildPromise: Promise<SnapshotDistributionModel>;
    try {
      buildPromise = build(signal);
    } catch (error) {
      combinedSignal?.dispose();
      return Promise.reject(error);
    }
    const pending = buildPromise
      .then((model) => {
        signal.throwIfAborted();
        if (this.owner === owner && this.ownerEpoch === epoch) {
          this.variants.set(variant, model);
        }
        return model;
      })
      .finally(() => {
        combinedSignal?.dispose();
        if (trackPending && this.pending.get(variant) === pending) {
          this.pending.delete(variant);
        }
      });
    if (trackPending) this.pending.set(variant, pending);
    return pending;
  }

  reset(): void {
    this.replaceOwner(null);
  }
}
