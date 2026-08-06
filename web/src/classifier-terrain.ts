import type {
  BucketTerrainLayout,
  BucketTerrainSectionGroup,
} from "./bucket-terrain";
import { classificationResult } from "./classification-view";
import type {
  ClassificationResultState,
  ClassifierDescriptor,
  MempoolTransaction,
} from "./types";

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
const classifierTerrainGroupsCache = new WeakMap<
  ClassifierBucket[],
  ClassifierTerrainGroups
>();

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

const sumVsize = (transactions: readonly MempoolTransaction[]): number =>
  transactions.reduce((total, transaction) => total + transaction.vsize, 0);

const sortPopulation = (
  transactions: readonly MempoolTransaction[],
): MempoolTransaction[] =>
  [...transactions].sort(
    (left, right) =>
      right.vsize - left.vsize || left.txid.localeCompare(right.txid),
  );

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

export const classifierBuckets = (
  transactions: readonly MempoolTransaction[],
  descriptor: ClassifierDescriptor,
): ClassifierBucket[] => {
  let byDescriptor = classifierBucketsCache.get(transactions);
  if (byDescriptor === undefined) {
    byDescriptor = new WeakMap();
    classifierBucketsCache.set(transactions, byDescriptor);
  }
  const cached = byDescriptor.get(descriptor);
  if (cached !== undefined) {
    return cached;
  }
  const buckets = new Map<
    ClassifierBucketKey,
    { signature: ClassifierBucketSignature; transactions: MempoolTransaction[] }
  >();
  for (const transaction of transactions) {
    const signature = classifierBucketForTransaction(transaction, descriptor);
    const existing = buckets.get(signature.key);
    if (existing === undefined) {
      buckets.set(signature.key, { signature, transactions: [transaction] });
    } else {
      existing.transactions.push(transaction);
    }
  }
  const result = [...buckets.values()]
    .sort((left, right) =>
      compareBuckets(descriptor, left.signature, right.signature),
    )
    .map(({ signature, transactions: entries }) => {
      const observedLabelKeys =
        signature.presentation === undefined
          ? undefined
          : descriptor.labels.flatMap(({ key }) =>
              entries.some(
                (transaction) =>
                  classificationResult(
                    transaction,
                    descriptor.id,
                  )?.labels.includes(key) ?? false,
              )
                ? [key]
                : [],
            );
      return {
        ...signature,
        ...(observedLabelKeys === undefined ? {} : { observedLabelKeys }),
        transactions: sortPopulation(entries),
        count: entries.length,
        vsize: sumVsize(entries),
        totalShare:
          transactions.length === 0 ? 0 : entries.length / transactions.length,
      };
    });
  byDescriptor.set(descriptor, result);
  return result;
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
      transactions: sectionBuckets.flatMap(
        ({ transactions: entries }) => entries,
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
