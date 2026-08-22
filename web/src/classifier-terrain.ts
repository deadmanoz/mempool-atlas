import type {
  BucketTerrainLayout,
  BucketTerrainPaint,
  BucketTerrainSectionGroup,
} from "./bucket-terrain";
import {
  addTransactionToBip110RuleIndex,
  cacheBip110RuleIndex,
  createBip110RuleIndexBuilder,
  type Bip110RuleIndexBuilder,
} from "./bip110-rule-index";
import { classificationResult } from "./classification-view";
import {
  forEachCooperatively,
  yieldCooperatively,
  type CooperativeWorkOptions,
} from "./cooperative-work";
import type {
  ClassificationResultState,
  ClassifierDescriptor,
  MempoolTransaction,
} from "./types";
import {
  concatenateTransactionViews,
  sortTransactionViewByVsize,
  sortTransactionViewByVsizeCooperatively,
  transactionIndexView,
} from "./transaction-view";

export const KNOTS_BIP110_CLASSIFIER_ID = "knots_bip110";
export const TRANSACTION_PROPERTIES_CLASSIFIER_ID = "transaction_properties";

export type ClassifierTerrainSectionKey =
  "complete" | "partial" | "unavailable";
export type ClassifierBucketKey =
  `complete:${string}` | `partial:${string}` | "unavailable";

interface ClassifierBucketPresentation {
  kind: "summary";
  label: string;
  description: string;
  color: string;
  order: number;
}

export interface ClassifierBucketSignature {
  key: ClassifierBucketKey;
  state: ClassificationResultState | "unavailable";
  labelKeys: string[];
  observedLabelKeys?: string[];
  presentation?: ClassifierBucketPresentation;
}

export interface ClassifierBucket extends ClassifierBucketSignature {
  transactions: MempoolTransaction[];
  count: number;
  vsize: number;
  totalShare: number;
}

export interface ClassifierLabelPopulation {
  transactions: MempoolTransaction[];
  count: number;
  vsize: number;
  totalShare: number;
}

export type ClassifierLabelPopulationSummary = Omit<
  ClassifierLabelPopulation,
  "transactions"
>;

export type ClassifierLabelMatchMode = "any" | "all";

export interface ClassifierLabelQueryPopulation extends ClassifierLabelPopulation {
  labelKeys: string[];
  matchMode: ClassifierLabelMatchMode;
  completeTransactions: MempoolTransaction[];
  partialTransactions: MempoolTransaction[];
}

export interface ClassifierAllMatchCompatibility {
  compatibleLabelKeys: string[];
  matchCount: number;
}

export interface ClassifierTerrainTotals {
  complete: number;
  partial: number;
  unavailable: number;
}

type ClassifierTerrainGroups = BucketTerrainSectionGroup<
  ClassifierTerrainSectionKey,
  ClassifierBucketKey,
  ClassifierBucketSignature
>[];

const classifierBucketsCache = new WeakMap<
  readonly MempoolTransaction[],
  WeakMap<ClassifierDescriptor, ClassifierBucket[]>
>();
interface ClassifierLabelPopulationIndex {
  rows: Uint32Array;
  vsize: number;
  population: ClassifierLabelPopulation | null;
  rowMembership: Uint8Array | null;
}
const classifierLabelPopulationsCache = new WeakMap<
  readonly MempoolTransaction[],
  WeakMap<ClassifierDescriptor, Map<string, ClassifierLabelPopulationIndex>>
>();
const classifierTerrainGroupsCache = new WeakMap<
  ClassifierBucket[],
  ClassifierTerrainGroups
>();

export interface ClassifierBucketPrecomputeOptions extends CooperativeWorkOptions {
  sortTimeBudgetMs?: number;
  aggregateOnly?: boolean;
}

export type ClassifierTerrainLayout = BucketTerrainLayout<
  ClassifierTerrainSectionKey,
  ClassifierBucketKey,
  ClassifierBucketSignature
>;

export interface ClassifierTerrainPaintSelection {
  selectedBucketKey: ClassifierBucketKey | null;
  selectedLabel: string | null;
  selectedLabelMembership: Uint8Array | null;
}

const LABEL_COLORS = [
  "#53d9d4",
  "#62a9d9",
  "#7f9be2",
  "#9c8ee0",
  "#b989d0",
  "#d97ab8",
  "#f46f93",
  "#f58a8a",
  "#e1aa4b",
  "#8fcf72",
] as const;

const TRANSACTION_PROPERTY_PROFILES = [
  {
    key: "legacy",
    label: "Legacy / P2SH",
    description:
      "Observed input and output scripts are confined to legacy or P2SH families.",
    color: "#62a9d9",
    labelKeys: ["p2pk", "bare_multisig", "p2pkh", "p2sh"],
  },
  {
    key: "segwit_v0",
    label: "SegWit v0",
    description:
      "Observed input and output scripts are confined to native SegWit v0 families.",
    color: "#53d9d4",
    labelKeys: ["p2wpkh", "p2wsh"],
  },
  {
    key: "taproot",
    label: "Taproot",
    description:
      "Observed input and output scripts are confined to the Taproot family.",
    color: "#9c8ee0",
    labelKeys: ["p2tr"],
  },
  {
    key: "other",
    label: "Anchor / other scripts",
    description:
      "Observed scripts use P2A, another witness version, an unrecognized family, or no recognized payment-script group.",
    color: "#8798a6",
    labelKeys: ["p2a", "unknown_witness_program", "unknown_script"],
  },
] as const;

const MIXED_PROPERTY_PROFILE = {
  key: "mixed",
  label: "Multiple script groups",
  description:
    "Observed inputs and outputs span more than one broad script group.",
  color: "#e1aa4b",
  order: TRANSACTION_PROPERTY_PROFILES.length,
} as const;

const sortPopulation = (
  transactions: readonly MempoolTransaction[],
): MempoolTransaction[] => sortTransactionViewByVsize(transactions);

const labelIndexes = (
  descriptor: ClassifierDescriptor,
  labels: readonly string[],
): number[] => {
  const selected = new Set(labels);
  return descriptor.labels.flatMap(({ key }, index) =>
    selected.has(key) ? [index] : [],
  );
};

const signatureForResult = (
  descriptor: ClassifierDescriptor,
  state: ClassificationResultState,
  labels: readonly string[],
): ClassifierBucketSignature => {
  const indexes = labelIndexes(descriptor, labels);
  return {
    key: `${state}:${indexes.length === 0 ? "none" : indexes.join(".")}`,
    state,
    labelKeys: indexes.map(
      (index) => descriptor.labels[index]?.key ?? `unknown_${index}`,
    ),
  };
};

const transactionPropertySignature = (
  state: ClassificationResultState,
  labels: readonly string[],
): ClassifierBucketSignature => {
  const observed = new Set(labels);
  const matchingProfiles = TRANSACTION_PROPERTY_PROFILES.filter((profile) =>
    profile.labelKeys.some((labelKey) => observed.has(labelKey)),
  );
  const profile =
    matchingProfiles.length === 1
      ? matchingProfiles[0]
      : matchingProfiles.length > 1
        ? MIXED_PROPERTY_PROFILE
        : TRANSACTION_PROPERTY_PROFILES.at(-1);
  if (profile === undefined) {
    throw new Error("Transaction property profiles are not configured");
  }
  const order =
    "order" in profile
      ? profile.order
      : TRANSACTION_PROPERTY_PROFILES.indexOf(profile);
  return {
    key: `${state}:profile_${profile.key}`,
    state,
    labelKeys: [],
    presentation: {
      kind: "summary",
      label: profile.label,
      description: profile.description,
      color: profile.color,
      order,
    },
  };
};

export const classifierBucketForTransaction = (
  transaction: MempoolTransaction,
  descriptor: ClassifierDescriptor,
): ClassifierBucketSignature => {
  const result = classificationResult(transaction, descriptor.id);
  if (result === null) {
    return { key: "unavailable", state: "unavailable", labelKeys: [] };
  }
  return descriptor.id === TRANSACTION_PROPERTIES_CLASSIFIER_ID
    ? transactionPropertySignature(result.state, result.labels)
    : signatureForResult(descriptor, result.state, result.labels);
};

const compareIndexSets = (left: number[], right: number[]): number => {
  for (let index = 0; index < Math.min(left.length, right.length); index += 1) {
    const difference = (left[index] ?? 0) - (right[index] ?? 0);
    if (difference !== 0) {
      return difference;
    }
  }
  return left.length - right.length;
};

const compareBuckets = (
  descriptor: ClassifierDescriptor,
  left: ClassifierBucketSignature,
  right: ClassifierBucketSignature,
): number => {
  if (left.state !== right.state) {
    const order = { complete: 0, partial: 1, unavailable: 2 } as const;
    return order[left.state] - order[right.state];
  }
  if (left.presentation !== undefined || right.presentation !== undefined) {
    return (
      (left.presentation?.order ?? Number.MAX_SAFE_INTEGER) -
      (right.presentation?.order ?? Number.MAX_SAFE_INTEGER)
    );
  }
  return compareIndexSets(
    labelIndexes(descriptor, left.labelKeys),
    labelIndexes(descriptor, right.labelKeys),
  );
};

interface ClassifierBucketAccumulator {
  signature: ClassifierBucketSignature;
  rows: number[];
  vsize: number;
  observedLabelKeys?: Set<string>;
}

interface ClassifierBucketBuilder {
  descriptor: ClassifierDescriptor;
  bip110Rules: Bip110RuleIndexBuilder | null;
  knownLabelKeys: Set<string>;
  buckets: Map<ClassifierBucketKey, ClassifierBucketAccumulator>;
  labels: Map<
    string,
    {
      rows: number[];
      vsize: number;
    }
  >;
}

const classifierBucketCache = (
  transactions: readonly MempoolTransaction[],
): WeakMap<ClassifierDescriptor, ClassifierBucket[]> => {
  let byDescriptor = classifierBucketsCache.get(transactions);
  if (byDescriptor === undefined) {
    byDescriptor = new WeakMap();
    classifierBucketsCache.set(transactions, byDescriptor);
  }
  return byDescriptor;
};

const classifierLabelPopulationCache = (
  transactions: readonly MempoolTransaction[],
): WeakMap<
  ClassifierDescriptor,
  Map<string, ClassifierLabelPopulationIndex>
> => {
  let byDescriptor = classifierLabelPopulationsCache.get(transactions);
  if (byDescriptor === undefined) {
    byDescriptor = new WeakMap();
    classifierLabelPopulationsCache.set(transactions, byDescriptor);
  }
  return byDescriptor;
};

const createClassifierBucketBuilder = (
  descriptor: ClassifierDescriptor,
  retainMarginalIndexes: boolean = true,
): ClassifierBucketBuilder => ({
  descriptor,
  bip110Rules:
    retainMarginalIndexes && descriptor.id === KNOTS_BIP110_CLASSIFIER_ID
      ? createBip110RuleIndexBuilder()
      : null,
  knownLabelKeys: new Set(descriptor.labels.map(({ key }) => key)),
  buckets: new Map(),
  labels: retainMarginalIndexes
    ? new Map(descriptor.labels.map(({ key }) => [key, { rows: [], vsize: 0 }]))
    : new Map(),
});

const addTransactionToClassifierBuckets = (
  builder: ClassifierBucketBuilder,
  transaction: MempoolTransaction,
  row: number,
): void => {
  const { descriptor, knownLabelKeys, buckets, labels } = builder;
  if (builder.bip110Rules !== null) {
    addTransactionToBip110RuleIndex(builder.bip110Rules, transaction, row);
  }
  const signature = classifierBucketForTransaction(transaction, descriptor);
  let accumulator = buckets.get(signature.key);
  if (accumulator === undefined) {
    accumulator = {
      signature,
      rows: [],
      vsize: 0,
      ...(signature.presentation === undefined
        ? {}
        : { observedLabelKeys: new Set<string>() }),
    };
    buckets.set(signature.key, accumulator);
  }
  accumulator.rows.push(row);
  accumulator.vsize += transaction.vsize;
  if (labels.size === 0 && accumulator.observedLabelKeys === undefined) {
    return;
  }
  const classification = classificationResult(transaction, descriptor.id);
  const resultLabels = classification?.labels ?? [];
  for (let index = 0; index < resultLabels.length; index += 1) {
    const labelKey = resultLabels[index];
    if (labelKey === undefined || resultLabels.indexOf(labelKey) !== index) {
      continue;
    }
    if (!knownLabelKeys.has(labelKey)) continue;
    const label = labels.get(labelKey);
    if (label === undefined) continue;
    label.rows.push(row);
    label.vsize += transaction.vsize;
  }
  if (accumulator.observedLabelKeys !== undefined) {
    for (const labelKey of resultLabels) {
      if (knownLabelKeys.has(labelKey)) {
        accumulator.observedLabelKeys.add(labelKey);
      }
    }
  }
};

const finalizeClassifierBuckets = (
  transactions: readonly MempoolTransaction[],
  builder: ClassifierBucketBuilder,
): ClassifierBucket[] =>
  [...builder.buckets.values()]
    .sort((left, right) =>
      compareBuckets(builder.descriptor, left.signature, right.signature),
    )
    .map(({ signature, rows, vsize, observedLabelKeys }) => {
      const entries = sortPopulation(transactionIndexView(transactions, rows));
      const observed =
        observedLabelKeys === undefined
          ? undefined
          : builder.descriptor.labels.flatMap(({ key }) =>
              observedLabelKeys.has(key) ? [key] : [],
            );
      return {
        ...signature,
        ...(observed === undefined ? {} : { observedLabelKeys: observed }),
        transactions: entries,
        count: entries.length,
        vsize,
        totalShare:
          transactions.length === 0 ? 0 : entries.length / transactions.length,
      };
    });

const finalizeClassifierLabelPopulations = (
  builder: ClassifierBucketBuilder,
): Map<string, ClassifierLabelPopulationIndex> =>
  new Map(
    [...builder.labels].map(([labelKey, { rows, vsize }]) => [
      labelKey,
      {
        rows: Uint32Array.from(rows),
        vsize,
        population: null,
        rowMembership: null,
      },
    ]),
  );

const cacheClassifierBuilder = (
  transactions: readonly MempoolTransaction[],
  builder: ClassifierBucketBuilder,
): ClassifierBucket[] => {
  const buckets = finalizeClassifierBuckets(transactions, builder);
  classifierBucketCache(transactions).set(builder.descriptor, buckets);
  classifierLabelPopulationCache(transactions).set(
    builder.descriptor,
    finalizeClassifierLabelPopulations(builder),
  );
  if (builder.bip110Rules !== null) {
    cacheBip110RuleIndex(transactions, builder.bip110Rules);
  }
  return buckets;
};

const cacheClassifierBuilderCooperatively = async (
  transactions: readonly MempoolTransaction[],
  builder: ClassifierBucketBuilder,
  options: ClassifierBucketPrecomputeOptions,
): Promise<void> => {
  const finalizationTimeBudgetMs = options.sortTimeBudgetMs ?? 4;
  if (
    !Number.isFinite(finalizationTimeBudgetMs) ||
    finalizationTimeBudgetMs <= 0
  ) {
    throw new RangeError(
      "Classifier finalization time budget must be positive",
    );
  }
  let finalizationStartedAt = performance.now();
  const yieldAfterFinalizationSlice = async (): Promise<void> => {
    if (performance.now() - finalizationStartedAt < finalizationTimeBudgetMs) {
      return;
    }
    await yieldCooperatively(options);
    finalizationStartedAt = performance.now();
  };
  const accumulators = [...builder.buckets.values()].sort((left, right) =>
    compareBuckets(builder.descriptor, left.signature, right.signature),
  );
  const buckets: ClassifierBucket[] = [];
  for (let index = 0; index < accumulators.length; index += 1) {
    options.signal?.throwIfAborted();
    const accumulator = accumulators[index];
    if (accumulator === undefined) continue;
    const { signature, rows, vsize, observedLabelKeys } = accumulator;
    const entries = options.aggregateOnly
      ? transactionIndexView(transactions, rows)
      : await sortTransactionViewByVsizeCooperatively(
          transactionIndexView(transactions, rows),
          {
            ...options,
            ...(options.sortTimeBudgetMs === undefined
              ? {}
              : { timeBudgetMs: options.sortTimeBudgetMs }),
          },
        );
    const observed =
      observedLabelKeys === undefined
        ? undefined
        : builder.descriptor.labels.flatMap(({ key }) =>
            observedLabelKeys.has(key) ? [key] : [],
          );
    buckets.push({
      ...signature,
      ...(observed === undefined ? {} : { observedLabelKeys: observed }),
      transactions: entries,
      count: entries.length,
      vsize,
      totalShare:
        transactions.length === 0 ? 0 : entries.length / transactions.length,
    });
    if (index + 1 < accumulators.length) {
      await yieldAfterFinalizationSlice();
    }
  }

  if (!options.aggregateOnly) {
    const labelEntries = [...builder.labels];
    const labels = new Map<string, ClassifierLabelPopulationIndex>();
    for (let index = 0; index < labelEntries.length; index += 1) {
      options.signal?.throwIfAborted();
      const entry = labelEntries[index];
      if (entry === undefined) continue;
      const [labelKey, { rows, vsize }] = entry;
      labels.set(labelKey, {
        rows: Uint32Array.from(rows),
        vsize,
        population: null,
        rowMembership: null,
      });
      if (index + 1 < labelEntries.length) {
        await yieldAfterFinalizationSlice();
      }
    }
    classifierLabelPopulationCache(transactions).set(
      builder.descriptor,
      labels,
    );
    if (builder.bip110Rules !== null) {
      cacheBip110RuleIndex(transactions, builder.bip110Rules);
    }
  }

  options.signal?.throwIfAborted();
  classifierBucketCache(transactions).set(builder.descriptor, buckets);
};

/**
 * Populate classifier bucket caches without materializing the complete
 * transaction population in one main-thread task. Each uncached descriptor is
 * accumulated from the same bounded row batches, and only row indices and
 * aggregate metadata survive the scan.
 */
export const precomputeClassifierBuckets = async (
  transactions: readonly MempoolTransaction[],
  descriptors: readonly ClassifierDescriptor[],
  options: ClassifierBucketPrecomputeOptions = {},
): Promise<void> => {
  options.signal?.throwIfAborted();
  const byDescriptor = classifierBucketCache(transactions);
  const builders = [...new Set(descriptors)]
    .filter((descriptor) => byDescriptor.get(descriptor) === undefined)
    .map((descriptor) =>
      createClassifierBucketBuilder(descriptor, !options.aggregateOnly),
    );
  if (builders.length === 0) return;

  await forEachCooperatively(
    transactions,
    (transaction, row) => {
      for (const builder of builders) {
        addTransactionToClassifierBuckets(builder, transaction, row);
      }
    },
    options,
  );

  for (let index = 0; index < builders.length; index += 1) {
    options.signal?.throwIfAborted();
    const builder = builders[index];
    if (builder === undefined) continue;
    await cacheClassifierBuilderCooperatively(transactions, builder, options);
    if (index + 1 < builders.length) {
      await yieldCooperatively(options);
    }
  }
  options.signal?.throwIfAborted();
};

export const classifierBuckets = (
  transactions: readonly MempoolTransaction[],
  descriptor: ClassifierDescriptor,
): ClassifierBucket[] => {
  const byDescriptor = classifierBucketCache(transactions);
  const cached = byDescriptor.get(descriptor);
  if (cached !== undefined) {
    return cached;
  }
  const builder = createClassifierBucketBuilder(descriptor);
  for (let row = 0; row < transactions.length; row += 1) {
    const transaction = transactions[row];
    if (transaction === undefined) continue;
    addTransactionToClassifierBuckets(builder, transaction, row);
  }
  return cacheClassifierBuilder(transactions, builder);
};

const ensureClassifierLabelPopulations = (
  transactions: readonly MempoolTransaction[],
  descriptor: ClassifierDescriptor,
): Map<string, ClassifierLabelPopulationIndex> => {
  const cache = classifierLabelPopulationCache(transactions);
  const cached = cache.get(descriptor);
  if (cached !== undefined) return cached;

  const builder = createClassifierBucketBuilder(descriptor);
  for (let row = 0; row < transactions.length; row += 1) {
    const transaction = transactions[row];
    if (transaction !== undefined) {
      addTransactionToClassifierBuckets(builder, transaction, row);
    }
  }
  const labels = finalizeClassifierLabelPopulations(builder);
  cache.set(descriptor, labels);
  if (builder.bip110Rules !== null) {
    cacheBip110RuleIndex(transactions, builder.bip110Rules);
  }
  return labels;
};

/**
 * Return aggregate metadata for one marginal classifier label without sorting
 * or materializing its transaction population. Publication preparation already
 * retains the row count and virtual-size total in the compact label index.
 */
export const classifierLabelPopulationSummary = (
  transactions: readonly MempoolTransaction[],
  descriptor: ClassifierDescriptor,
  labelKey: string,
): ClassifierLabelPopulationSummary | null => {
  const index = ensureClassifierLabelPopulations(transactions, descriptor).get(
    labelKey,
  );
  if (index === undefined) return null;
  return {
    count: index.rows.length,
    vsize: index.vsize,
    totalShare:
      transactions.length === 0 ? 0 : index.rows.length / transactions.length,
  };
};

/**
 * Return the cached marginal population for one classifier label. Cooperative
 * precomputation retains compact row indices and aggregate virtual size for
 * every configured label, including labels presented inside summary buckets,
 * so this lookup never scans the complete transaction population again.
 */
export const classifierLabelPopulation = (
  transactions: readonly MempoolTransaction[],
  descriptor: ClassifierDescriptor,
  labelKey: string,
): ClassifierLabelPopulation | null => {
  const index = ensureClassifierLabelPopulations(transactions, descriptor).get(
    labelKey,
  );
  if (index === undefined) return null;
  if (index.population !== null) return index.population;
  const entries = sortPopulation(
    transactionIndexView(transactions, index.rows),
  );
  index.population = {
    transactions: entries,
    count: entries.length,
    vsize: index.vsize,
    totalShare:
      transactions.length === 0 ? 0 : entries.length / transactions.length,
  };
  return index.population;
};

/**
 * Return one bit per root transaction row for a marginal classifier label.
 * Terrain paint can then classify a glyph without a txid lookup or packed-row
 * materialization. The bitset is retained with the existing label-row index.
 */
export const classifierLabelRowMembership = (
  transactions: readonly MempoolTransaction[],
  descriptor: ClassifierDescriptor,
  labelKey: string,
): Uint8Array | null => {
  const index = ensureClassifierLabelPopulations(transactions, descriptor).get(
    labelKey,
  );
  if (index === undefined) return null;
  if (index.rowMembership !== null) return index.rowMembership;
  const membership = new Uint8Array(Math.ceil(transactions.length / 8));
  for (const row of index.rows) {
    membership[row >> 3] = (membership[row >> 3] ?? 0) | (1 << (row & 7));
  }
  index.rowMembership = membership;
  return membership;
};

const normalizedQueryLabels = (
  descriptor: ClassifierDescriptor,
  labelKeys: readonly string[],
): string[] => {
  const selected = new Set(labelKeys);
  return descriptor.labels.flatMap(({ key }) =>
    selected.has(key) ? [key] : [],
  );
};

/**
 * Report which labels can extend the current ALL query without making its
 * intersection empty. Selected labels remain compatible so every query can be
 * reduced, including an impossible combination restored from the URL.
 */
export const classifierAllMatchCompatibility = (
  transactions: readonly MempoolTransaction[],
  descriptor: ClassifierDescriptor,
  labelKeys: readonly string[],
): ClassifierAllMatchCompatibility => {
  const normalized = normalizedQueryLabels(descriptor, labelKeys);
  if (normalized.length === 0) {
    return {
      compatibleLabelKeys: descriptor.labels.map(({ key }) => key),
      matchCount: 0,
    };
  }

  const indexes = ensureClassifierLabelPopulations(transactions, descriptor);
  const membership = new Uint16Array(transactions.length);
  for (const labelKey of normalized) {
    const index = indexes.get(labelKey);
    if (index === undefined) continue;
    for (const row of index.rows) {
      membership[row] = (membership[row] ?? 0) + 1;
    }
  }

  const compatible = new Set(normalized);
  let matchCount = 0;
  for (let row = 0; row < membership.length; row += 1) {
    if (membership[row] !== normalized.length) continue;
    const transaction = transactions[row];
    if (transaction === undefined) continue;
    const result = classificationResult(transaction, descriptor.id);
    if (result === null) continue;
    matchCount += 1;
    for (const labelKey of result.labels) compatible.add(labelKey);
  }

  return {
    compatibleLabelKeys: descriptor.labels.flatMap(({ key }) =>
      compatible.has(key) ? [key] : [],
    ),
    matchCount,
  };
};

/**
 * Resolve a multi-label query from the compact marginal row indexes built with
 * the classifier buckets. Each matching transaction appears once, and proven
 * labels in partial results remain selectable without treating unavailable
 * results as matches.
 */
export const classifierLabelQueryPopulation = (
  transactions: readonly MempoolTransaction[],
  descriptor: ClassifierDescriptor,
  labelKeys: readonly string[],
  matchMode: ClassifierLabelMatchMode,
): ClassifierLabelQueryPopulation => {
  const normalized = normalizedQueryLabels(descriptor, labelKeys);
  if (normalized.length === 0) {
    return {
      labelKeys: [],
      matchMode,
      transactions: [],
      completeTransactions: [],
      partialTransactions: [],
      count: 0,
      vsize: 0,
      totalShare: 0,
    };
  }

  const indexes = ensureClassifierLabelPopulations(transactions, descriptor);
  const membership = new Uint16Array(transactions.length);
  for (const labelKey of normalized) {
    const index = indexes.get(labelKey);
    if (index === undefined) continue;
    for (const row of index.rows) {
      membership[row] = (membership[row] ?? 0) + 1;
    }
  }

  const completeRows: number[] = [];
  const partialRows: number[] = [];
  let vsize = 0;
  for (let row = 0; row < membership.length; row += 1) {
    const matches =
      matchMode === "all"
        ? membership[row] === normalized.length
        : (membership[row] ?? 0) > 0;
    if (!matches) continue;
    const transaction = transactions[row];
    if (transaction === undefined) continue;
    const result = classificationResult(transaction, descriptor.id);
    if (result === null) continue;
    (result.state === "complete" ? completeRows : partialRows).push(row);
    vsize += transaction.vsize;
  }

  const completeTransactions = transactionIndexView(transactions, completeRows);
  const partialTransactions = transactionIndexView(transactions, partialRows);
  const entries = concatenateTransactionViews(transactions, [
    completeTransactions,
    partialTransactions,
  ]);
  return {
    labelKeys: normalized,
    matchMode,
    transactions: entries,
    completeTransactions,
    partialTransactions,
    count: entries.length,
    vsize,
    totalShare:
      transactions.length === 0 ? 0 : entries.length / transactions.length,
  };
};

export const classifierBucketPopulation = (
  transactions: readonly MempoolTransaction[],
  descriptor: ClassifierDescriptor,
  bucketKey: ClassifierBucketKey,
): ClassifierBucket | null =>
  classifierBuckets(transactions, descriptor).find(
    ({ key }) => key === bucketKey,
  ) ?? null;

export const classifierTerrainGroups = (
  transactions: readonly MempoolTransaction[],
  descriptor: ClassifierDescriptor,
): ClassifierTerrainGroups => {
  const buckets = classifierBuckets(transactions, descriptor);
  const cached = classifierTerrainGroupsCache.get(buckets);
  if (cached !== undefined) {
    return cached;
  }
  const groups: ClassifierTerrainGroups = [];
  for (const sectionKey of ["complete", "partial", "unavailable"] as const) {
    const sectionBuckets = buckets.filter(({ state }) => state === sectionKey);
    if (sectionBuckets.length === 0) {
      continue;
    }
    groups.push({
      key: sectionKey,
      transactions: concatenateTransactionViews(
        transactions,
        sectionBuckets.map(({ transactions: entries }) => entries),
      ),
      regions: sectionBuckets.map((bucket) => ({
        key: bucket.key,
        sectionKey,
        signature: {
          key: bucket.key,
          state: bucket.state,
          labelKeys: bucket.labelKeys,
          ...(bucket.observedLabelKeys === undefined
            ? {}
            : { observedLabelKeys: bucket.observedLabelKeys }),
          ...(bucket.presentation === undefined
            ? {}
            : { presentation: bucket.presentation }),
        },
        transactions: bucket.transactions,
      })),
      nested: sectionBuckets.length > 1,
    });
  }
  classifierTerrainGroupsCache.set(buckets, groups);
  return groups;
};

export const classifierTerrainTotals = (
  transactions: readonly MempoolTransaction[],
  descriptor: ClassifierDescriptor,
): ClassifierTerrainTotals => {
  const totals: ClassifierTerrainTotals = {
    complete: 0,
    partial: 0,
    unavailable: 0,
  };
  for (const transaction of transactions) {
    totals[classifierBucketForTransaction(transaction, descriptor).state] += 1;
  }
  return totals;
};

export const classifierBucketLabel = (
  descriptor: ClassifierDescriptor,
  signature: ClassifierBucketSignature,
): string => {
  if (signature.state === "unavailable") {
    return "Result unavailable";
  }
  if (signature.presentation !== undefined) {
    return signature.presentation.label;
  }
  if (signature.labelKeys.length === 0) {
    return "No labels";
  }
  const labelsByKey = new Map(
    descriptor.labels.map(({ key, label }) => [key, label]),
  );
  return signature.labelKeys
    .map((key) => labelsByKey.get(key) ?? key)
    .join(" + ");
};

export const classifierBucketColor = (
  descriptor: ClassifierDescriptor,
  signature: ClassifierBucketSignature,
): string => {
  if (signature.state === "unavailable") {
    return "#697988";
  }
  if (signature.presentation !== undefined) {
    return signature.presentation.color;
  }
  if (signature.labelKeys.length === 0) {
    return "#8798a6";
  }
  const firstIndex = descriptor.labels.findIndex(
    ({ key }) => key === signature.labelKeys[0],
  );
  return LABEL_COLORS[Math.max(0, firstIndex) % LABEL_COLORS.length] as string;
};

export const classifierTerrainPaint = (
  descriptor: ClassifierDescriptor,
  selection: ClassifierTerrainPaintSelection,
): BucketTerrainPaint<
  ClassifierTerrainSectionKey,
  ClassifierBucketKey,
  ClassifierBucketSignature
> => {
  const { selectedBucketKey, selectedLabel, selectedLabelMembership } =
    selection;
  const hasFilter = selectedBucketKey !== null || selectedLabel !== null;
  const glyphMatchesSelectedLabel = (sourceRow: number): boolean =>
    selectedLabelMembership !== null &&
    ((selectedLabelMembership[sourceRow >> 3] ?? 0) &
      (1 << (sourceRow & 7))) !==
      0;
  return {
    color: (region) => classifierBucketColor(descriptor, region.signature),
    selected: (region) =>
      selectedBucketKey !== null
        ? region.key === selectedBucketKey
        : selectedLabel !== null &&
          !classifierBucketIsSummary(region.signature) &&
          classifierBucketContainsLabel(region.signature, selectedLabel),
    partial: (region) => region.signature.state === "partial",
    glyphOpacity: (region, selected, glyph) =>
      region.signature.state === "unavailable"
        ? hasFilter
          ? 0.2
          : 0.82
        : selected
          ? 1
          : selectedBucketKey !== null
            ? 0.46
            : selectedLabel !== null
              ? glyphMatchesSelectedLabel(glyph.sourceRow)
                ? 1
                : 0.2
              : 0.82,
  };
};

export const classifierBucketContainsLabel = (
  signature: ClassifierBucketSignature,
  labelKey: string,
): boolean =>
  (signature.observedLabelKeys ?? signature.labelKeys).includes(labelKey);

export const classifierBucketIsSummary = (
  signature: ClassifierBucketSignature,
): boolean => signature.presentation?.kind === "summary";

export const classifierBucketDescription = (
  signature: ClassifierBucketSignature,
): string | null => signature.presentation?.description ?? null;

export const classifierUsesSummaryBuckets = (
  descriptor: ClassifierDescriptor,
): boolean => descriptor.id === TRANSACTION_PROPERTIES_CLASSIFIER_ID;
