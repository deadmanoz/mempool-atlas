import type {
  BucketTerrainLayout,
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

/** A bounded, largest-first sample paired with exact population aggregates. */
export interface ClassifierLabelSamplePopulation extends ClassifierLabelPopulation {}

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
  rows: Uint32Array | null;
  vsize: number;
  population: ClassifierLabelPopulation | null;
  sample: MempoolTransaction[];
}
const classifierLabelPopulationsCache = new WeakMap<
  readonly MempoolTransaction[],
  WeakMap<ClassifierDescriptor, Map<string, ClassifierLabelPopulationIndex>>
>();
const classifierTerrainGroupsCache = new WeakMap<
  ClassifierBucket[],
  ClassifierTerrainGroups
>();

export interface ClassifierBucketPrecomputeOptions extends CooperativeWorkOptions {}

export type ClassifierTerrainLayout = BucketTerrainLayout<
  ClassifierTerrainSectionKey,
  ClassifierBucketKey,
  ClassifierBucketSignature
>;

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
      sampleRows: Array<{ row: number; vsize: number; txid: string }>;
    }
  >;
}

const CLASSIFIER_LABEL_SAMPLE_LIMIT = 8;

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
): ClassifierBucketBuilder => ({
  descriptor,
  bip110Rules:
    descriptor.id === KNOTS_BIP110_CLASSIFIER_ID
      ? createBip110RuleIndexBuilder()
      : null,
  knownLabelKeys: new Set(descriptor.labels.map(({ key }) => key)),
  buckets: new Map(),
  labels: new Map(
    descriptor.labels.map(({ key }) => [
      key,
      { rows: [], vsize: 0, sampleRows: [] },
    ]),
  ),
});

const addClassifierLabelSample = (
  sampleRows: Array<{ row: number; vsize: number; txid: string }>,
  transaction: MempoolTransaction,
  row: number,
): void => {
  const candidate = { row, vsize: transaction.vsize, txid: transaction.txid };
  const compare = (
    left: { vsize: number; txid: string },
    right: { vsize: number; txid: string },
  ): number => right.vsize - left.vsize || left.txid.localeCompare(right.txid);
  if (
    sampleRows.length === CLASSIFIER_LABEL_SAMPLE_LIMIT &&
    compare(candidate, sampleRows[sampleRows.length - 1]!) >= 0
  ) {
    return;
  }
  const insertion = sampleRows.findIndex(
    (existing) => compare(candidate, existing) < 0,
  );
  sampleRows.splice(
    insertion < 0 ? sampleRows.length : insertion,
    0,
    candidate,
  );
  if (sampleRows.length > CLASSIFIER_LABEL_SAMPLE_LIMIT) sampleRows.pop();
};

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
    addClassifierLabelSample(label.sampleRows, transaction, row);
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
  transactions: readonly MempoolTransaction[],
  builder: ClassifierBucketBuilder,
): Map<string, ClassifierLabelPopulationIndex> =>
  new Map(
    [...builder.labels].map(([labelKey, { rows, vsize, sampleRows }]) => [
      labelKey,
      {
        rows: Uint32Array.from(rows),
        vsize,
        population: null,
        sample: transactionIndexView(
          transactions,
          sampleRows.map(({ row }) => row),
        ),
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
    finalizeClassifierLabelPopulations(transactions, builder),
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
  const accumulators = [...builder.buckets.values()].sort((left, right) =>
    compareBuckets(builder.descriptor, left.signature, right.signature),
  );
  const buckets: ClassifierBucket[] = [];
  for (let index = 0; index < accumulators.length; index += 1) {
    options.signal?.throwIfAborted();
    const accumulator = accumulators[index];
    if (accumulator === undefined) continue;
    const { signature, rows, vsize, observedLabelKeys } = accumulator;
    const entries = await sortTransactionViewByVsizeCooperatively(
      transactionIndexView(transactions, rows),
      options,
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
    if (index + 1 < accumulators.length) await yieldCooperatively(options);
  }

  const labelEntries = [...builder.labels];
  const labels = new Map<string, ClassifierLabelPopulationIndex>();
  for (let index = 0; index < labelEntries.length; index += 1) {
    options.signal?.throwIfAborted();
    const entry = labelEntries[index];
    if (entry === undefined) continue;
    const [labelKey, { rows, vsize, sampleRows }] = entry;
    labels.set(labelKey, {
      rows: Uint32Array.from(rows),
      vsize,
      population: null,
      sample: transactionIndexView(
        transactions,
        sampleRows.map(({ row }) => row),
      ),
    });
    if (index + 1 < labelEntries.length) await yieldCooperatively(options);
  }

  options.signal?.throwIfAborted();
  classifierBucketCache(transactions).set(builder.descriptor, buckets);
  classifierLabelPopulationCache(transactions).set(builder.descriptor, labels);
  if (builder.bip110Rules !== null) {
    cacheBip110RuleIndex(transactions, builder.bip110Rules);
  }
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
    .map(createClassifierBucketBuilder);
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
  let byLabel = classifierLabelPopulationCache(transactions).get(descriptor);
  if (byLabel === undefined) {
    classifierBuckets(transactions, descriptor);
    byLabel = classifierLabelPopulationCache(transactions).get(descriptor);
  }
  const index = byLabel?.get(labelKey);
  if (index === undefined) return null;
  if (index.population !== null) return index.population;
  const rows = index.rows;
  if (rows === null) {
    throw new Error(`Classifier label population ${labelKey} is unavailable`);
  }
  const entries = sortPopulation(transactionIndexView(transactions, rows));
  index.population = {
    transactions: entries,
    count: entries.length,
    vsize: index.vsize,
    totalShare:
      transactions.length === 0 ? 0 : entries.length / transactions.length,
  };
  index.rows = null;
  return index.population;
};

/**
 * Return the precomputed largest transactions for one marginal label without
 * materialising and sorting the complete matching population.
 */
export const classifierLabelSamplePopulation = (
  transactions: readonly MempoolTransaction[],
  descriptor: ClassifierDescriptor,
  labelKey: string,
): ClassifierLabelSamplePopulation | null => {
  let byLabel = classifierLabelPopulationCache(transactions).get(descriptor);
  if (byLabel === undefined) {
    classifierBuckets(transactions, descriptor);
    byLabel = classifierLabelPopulationCache(transactions).get(descriptor);
  }
  const index = byLabel?.get(labelKey);
  if (index === undefined) return null;
  const count = index.population?.count ?? index.rows?.length ?? 0;
  return {
    transactions:
      index.population?.transactions.slice(0, CLASSIFIER_LABEL_SAMPLE_LIMIT) ??
      index.sample,
    count,
    vsize: index.vsize,
    totalShare: transactions.length === 0 ? 0 : count / transactions.length,
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
