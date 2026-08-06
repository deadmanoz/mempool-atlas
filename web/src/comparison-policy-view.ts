import {
  comparisonRegionEntries,
  policySideForRegion,
  sourceEntry,
  type ComparedTransaction,
  type ComparisonPolicyFilter,
  type ComparisonPolicyStatus,
  type ComparisonRegionKey,
  type ComparisonSide,
  type CurrentComparison,
} from "./comparison-model";
import {
  compareViolationSignatures,
  violationSignature,
  type ExactSignatureKey,
  type PartialSignatureKey,
  type ViolationSignature,
  type ViolationSignatureKey,
} from "./terrain";
import { RULE_IDS, type MempoolTransaction, type RuleId } from "./types";

export const COMPARISON_POLICY_MATRIX_CHIP_LIMIT = 3;
export const COMPARISON_POLICY_SAMPLE_LIMIT = 12;

export type ComparisonPolicyMatrixRowKey =
  "left_only_left" | "common_left" | "common_right" | "right_only_right";

export type ComparisonPolicyMatrixStatus = ComparisonPolicyStatus;

export interface ComparisonPolicyTotals {
  count: number;
  vsize: number;
}

export type ComparisonPolicyStatusTotals = Record<
  ComparisonPolicyStatus,
  ComparisonPolicyTotals
>;

export type ComparisonPolicyRuleTotals = Record<RuleId, ComparisonPolicyTotals>;

export type ExactViolationSignature = Omit<
  ViolationSignature,
  "key" | "completeness"
> & {
  key: ExactSignatureKey;
  completeness: "exact";
};

export type PartialViolationSignature = Omit<
  ViolationSignature,
  "key" | "completeness"
> & {
  key: PartialSignatureKey;
  completeness: "partial";
};

export interface ComparisonPolicyExactSignatureBucket extends ComparisonPolicyTotals {
  signature: ExactViolationSignature;
}

export interface ComparisonPolicyPartialSignatureBucket extends ComparisonPolicyTotals {
  signature: PartialViolationSignature;
}

export interface ComparisonPolicySlice {
  region: ComparisonRegionKey;
  side: ComparisonSide;
  population: ComparisonPolicyTotals;
  statusTotals: ComparisonPolicyStatusTotals;
  ruleTotals: ComparisonPolicyRuleTotals;
  exactSignatures: ComparisonPolicyExactSignatureBucket[];
  partialSignatures: ComparisonPolicyPartialSignatureBucket[];
}

export interface ComparisonPolicyPopulation extends ComparisonPolicyTotals {
  sample: readonly ComparedTransaction[];
}

export interface ComparisonPolicyMatrixStatusCounts {
  compatible: number;
  violating: number;
  indeterminate: number;
  unclassified: number;
}

export interface ComparisonPolicyMatrixExactCombination {
  signature: ExactViolationSignature;
  count: number;
}

export interface ComparisonPolicyMatrixExactOverflow {
  combinationCount: number;
  transactionCount: number;
}

export interface ComparisonPolicyMatrixRow {
  key: ComparisonPolicyMatrixRowKey;
  region: ComparisonRegionKey;
  side: ComparisonSide;
  sourceId: string;
  sourceLabel: string;
  populationCount: number;
  statusCounts: ComparisonPolicyMatrixStatusCounts;
  exactViolationCount: number;
  partialViolationCount: number;
  dominantExactCombinations: ComparisonPolicyMatrixExactCombination[];
  exactCombinationOverflow: ComparisonPolicyMatrixExactOverflow;
}

export interface ComparisonPolicyMatrixRowPresentation {
  label: string;
  detail: string;
  ariaContext: string;
}

export type ComparisonPolicyMatrixSelection =
  | Extract<ComparisonPolicyFilter, { kind: "status" }>
  | { kind: "signature"; signature: ExactSignatureKey };

export interface ComparisonPolicyMatrixTarget {
  region: ComparisonRegionKey;
  side: ComparisonSide;
  filter: ComparisonPolicyMatrixSelection;
  txid: null;
}

export const comparisonPolicyMatrixRowPresentation = (
  row: ComparisonPolicyMatrixRow,
  lifecycleLabel: string,
  formatPopulation: (count: number) => string,
): ComparisonPolicyMatrixRowPresentation => {
  const label =
    row.region === "common"
      ? "Present in both snapshots"
      : `Observed only in ${row.sourceLabel}`;
  return {
    label,
    detail: `Policy view for ${row.sourceLabel} · ${formatPopulation(row.populationCount)} · ${lifecycleLabel}`,
    ariaContext: `${label}, policy view for ${row.sourceLabel}`,
  };
};

interface SliceSpec {
  key: ComparisonPolicyMatrixRowKey;
  region: ComparisonRegionKey;
  side: ComparisonSide;
}

interface SignatureAccumulator extends ComparisonPolicyTotals {
  signature: ViolationSignature;
}

interface SliceAccumulator {
  spec: SliceSpec;
  population: ComparisonPolicyTotals;
  statusTotals: ComparisonPolicyStatusTotals;
  ruleTotals: ComparisonPolicyRuleTotals;
  signatures: Map<ViolationSignatureKey, SignatureAccumulator>;
}

interface SliceState {
  slice: ComparisonPolicySlice;
  signatureTotals: Map<ViolationSignatureKey, ComparisonPolicyTotals>;
}

const SLICE_SPECS: readonly SliceSpec[] = [
  { key: "left_only_left", region: "left_only", side: "left" },
  { key: "common_left", region: "common", side: "left" },
  { key: "common_right", region: "common", side: "right" },
  { key: "right_only_right", region: "right_only", side: "right" },
];

const sliceKey = (region: ComparisonRegionKey, side: ComparisonSide): string =>
  `${region}:${policySideForRegion(region, side)}`;

const emptyTotals = (): ComparisonPolicyTotals => ({ count: 0, vsize: 0 });

const emptyStatusTotals = (): ComparisonPolicyStatusTotals => ({
  compatible: emptyTotals(),
  violating: emptyTotals(),
  indeterminate: emptyTotals(),
  unclassified: emptyTotals(),
});

const emptyRuleTotals = (): ComparisonPolicyRuleTotals =>
  Object.fromEntries(
    RULE_IDS.map((rule) => [rule, emptyTotals()]),
  ) as ComparisonPolicyRuleTotals;

const addToTotals = (
  totals: ComparisonPolicyTotals,
  transaction: MempoolTransaction,
): void => {
  totals.count += 1;
  totals.vsize += transaction.vsize;
};

const accumulatorFor = (spec: SliceSpec): SliceAccumulator => ({
  spec,
  population: emptyTotals(),
  statusTotals: emptyStatusTotals(),
  ruleTotals: emptyRuleTotals(),
  signatures: new Map(),
});

const policyStatus = (
  transaction: MempoolTransaction,
): ComparisonPolicyStatus => transaction.bip110?.status ?? "unclassified";

const addTransaction = (
  accumulator: SliceAccumulator,
  transaction: MempoolTransaction,
): void => {
  addToTotals(accumulator.population, transaction);
  addToTotals(accumulator.statusTotals[policyStatus(transaction)], transaction);
  const assessment = transaction.bip110;
  if (assessment === null) {
    return;
  }
  for (const rule of assessment.violated_rules) {
    addToTotals(accumulator.ruleTotals[rule], transaction);
  }
  const signature = violationSignature(assessment);
  if (signature === null) {
    return;
  }
  const totals = accumulator.signatures.get(signature.key);
  if (totals === undefined) {
    accumulator.signatures.set(signature.key, {
      signature,
      count: 1,
      vsize: transaction.vsize,
    });
  } else {
    addToTotals(totals, transaction);
  }
};

const requiredTransaction = (
  entry: ComparedTransaction,
  spec: SliceSpec,
): MempoolTransaction => {
  const transaction = sourceEntry(entry, spec.side);
  if (transaction === null) {
    throw new Error(
      `Comparison policy slice ${spec.region}/${spec.side} is missing its source entry`,
    );
  }
  return transaction;
};

const finishSlice = (accumulator: SliceAccumulator): SliceState => {
  const signatures = [...accumulator.signatures.values()].sort((left, right) =>
    compareViolationSignatures(left.signature, right.signature),
  );
  const exactSignatures = signatures.filter(
    (bucket): bucket is ComparisonPolicyExactSignatureBucket =>
      bucket.signature.completeness === "exact",
  );
  const partialSignatures = signatures.filter(
    (bucket): bucket is ComparisonPolicyPartialSignatureBucket =>
      bucket.signature.completeness === "partial",
  );
  return {
    slice: {
      region: accumulator.spec.region,
      side: accumulator.spec.side,
      population: accumulator.population,
      statusTotals: accumulator.statusTotals,
      ruleTotals: accumulator.ruleTotals,
      exactSignatures,
      partialSignatures,
    },
    signatureTotals: new Map(
      signatures.map(({ signature, count, vsize }) => [
        signature.key,
        { count, vsize },
      ]),
    ),
  };
};

const matrixRow = (
  comparison: CurrentComparison,
  spec: SliceSpec,
  slice: ComparisonPolicySlice,
): ComparisonPolicyMatrixRow => {
  const combinations = slice.exactSignatures
    .map(({ signature, count }) => ({ signature, count }))
    .sort(
      (left, right) =>
        right.count - left.count ||
        left.signature.key.localeCompare(right.signature.key),
    );
  const dominantExactCombinations = combinations.slice(
    0,
    COMPARISON_POLICY_MATRIX_CHIP_LIMIT,
  );
  const hiddenCombinations = combinations.slice(
    COMPARISON_POLICY_MATRIX_CHIP_LIMIT,
  );
  const source = comparison[spec.side].source;
  return {
    ...spec,
    sourceId: source.source_id,
    sourceLabel: source.source_label,
    populationCount: slice.population.count,
    statusCounts: {
      compatible: slice.statusTotals.compatible.count,
      violating: slice.statusTotals.violating.count,
      indeterminate: slice.statusTotals.indeterminate.count,
      unclassified: slice.statusTotals.unclassified.count,
    },
    exactViolationCount: slice.exactSignatures.reduce(
      (total, bucket) => total + bucket.count,
      0,
    ),
    partialViolationCount: slice.partialSignatures.reduce(
      (total, bucket) => total + bucket.count,
      0,
    ),
    dominantExactCombinations,
    exactCombinationOverflow: {
      combinationCount: hiddenCombinations.length,
      transactionCount: hiddenCombinations.reduce(
        (total, combination) => total + combination.count,
        0,
      ),
    },
  };
};

const filterKey = (filter: ComparisonPolicyFilter): string => {
  if (filter.kind === "all") {
    return "all";
  }
  if (filter.kind === "status") {
    return `status:${filter.status}`;
  }
  if (filter.kind === "rule") {
    return `rule:${filter.rule}`;
  }
  return `signature:${filter.signature}`;
};

const filterMatches = (
  transaction: MempoolTransaction,
  filter: ComparisonPolicyFilter,
): boolean => {
  if (filter.kind === "all") {
    return true;
  }
  if (filter.kind === "status") {
    return policyStatus(transaction) === filter.status;
  }
  if (filter.kind === "rule") {
    return transaction.bip110?.violated_rules.includes(filter.rule) ?? false;
  }
  const assessment = transaction.bip110;
  return (
    assessment !== null &&
    violationSignature(assessment)?.key === filter.signature
  );
};

const compareSampleEntries = (
  left: ComparedTransaction,
  right: ComparedTransaction,
  side: ComparisonSide,
): number => {
  const leftVsize = sourceEntry(left, side)?.vsize ?? 0;
  const rightVsize = sourceEntry(right, side)?.vsize ?? 0;
  return rightVsize - leftVsize || left.txid.localeCompare(right.txid);
};

const addToBoundedSample = (
  sample: ComparedTransaction[],
  entry: ComparedTransaction,
  side: ComparisonSide,
): void => {
  let index = 0;
  while (
    index < sample.length &&
    compareSampleEntries(sample[index]!, entry, side) <= 0
  ) {
    index += 1;
  }
  if (index >= COMPARISON_POLICY_SAMPLE_LIMIT) {
    return;
  }
  sample.splice(index, 0, entry);
  if (sample.length > COMPARISON_POLICY_SAMPLE_LIMIT) {
    sample.pop();
  }
};

export class ComparisonPolicyView {
  readonly rows: ComparisonPolicyMatrixRow[];
  private readonly slices = new Map<string, SliceState>();
  private readonly populations = new Map<string, ComparisonPolicyPopulation>();

  constructor(private readonly comparison: CurrentComparison) {
    const accumulators = new Map(
      SLICE_SPECS.map((spec) => [spec.key, accumulatorFor(spec)]),
    );
    const requiredAccumulator = (spec: SliceSpec): SliceAccumulator => {
      const accumulator = accumulators.get(spec.key);
      if (accumulator === undefined) {
        throw new Error(`Missing comparison policy slice ${spec.key}`);
      }
      return accumulator;
    };
    const leftOnlySpec = SLICE_SPECS[0]!;
    const commonLeftSpec = SLICE_SPECS[1]!;
    const commonRightSpec = SLICE_SPECS[2]!;
    const rightOnlySpec = SLICE_SPECS[3]!;
    const leftOnly = requiredAccumulator(leftOnlySpec);
    const commonLeft = requiredAccumulator(commonLeftSpec);
    const commonRight = requiredAccumulator(commonRightSpec);
    const rightOnly = requiredAccumulator(rightOnlySpec);
    for (const entry of comparison.left_only) {
      addTransaction(leftOnly, requiredTransaction(entry, leftOnlySpec));
    }
    for (const entry of comparison.common) {
      addTransaction(commonLeft, requiredTransaction(entry, commonLeftSpec));
      addTransaction(commonRight, requiredTransaction(entry, commonRightSpec));
    }
    for (const entry of comparison.right_only) {
      addTransaction(rightOnly, requiredTransaction(entry, rightOnlySpec));
    }

    for (const spec of SLICE_SPECS) {
      const accumulator = accumulators.get(spec.key);
      if (accumulator === undefined) {
        throw new Error(`Missing comparison policy accumulator ${spec.key}`);
      }
      this.slices.set(
        sliceKey(spec.region, spec.side),
        finishSlice(accumulator),
      );
    }
    this.rows = SLICE_SPECS.map((spec) =>
      matrixRow(comparison, spec, this.slice(spec.region, spec.side)),
    );
  }

  slice(
    region: ComparisonRegionKey,
    side: ComparisonSide,
  ): ComparisonPolicySlice {
    const state = this.slices.get(sliceKey(region, side));
    if (state === undefined) {
      throw new Error(`Missing comparison policy slice ${region}/${side}`);
    }
    return state.slice;
  }

  population(
    region: ComparisonRegionKey,
    side: ComparisonSide,
    filter: ComparisonPolicyFilter,
  ): ComparisonPolicyPopulation {
    const effectiveSide = policySideForRegion(region, side);
    const key = `${sliceKey(region, effectiveSide)}:${filterKey(filter)}`;
    const cached = this.populations.get(key);
    if (cached !== undefined) {
      return cached;
    }
    const state = this.slices.get(sliceKey(region, effectiveSide));
    if (state === undefined) {
      throw new Error(
        `Missing comparison policy slice ${region}/${effectiveSide}`,
      );
    }
    const totals =
      filter.kind === "all"
        ? state.slice.population
        : filter.kind === "status"
          ? state.slice.statusTotals[filter.status]
          : filter.kind === "rule"
            ? state.slice.ruleTotals[filter.rule]
            : (state.signatureTotals.get(filter.signature) ?? emptyTotals());
    if (totals.count === 0) {
      const population: ComparisonPolicyPopulation = {
        count: 0,
        vsize: 0,
        sample: [],
      };
      this.populations.set(key, population);
      return population;
    }
    const sample: ComparedTransaction[] = [];
    for (const entry of comparisonRegionEntries(this.comparison, region)) {
      const transaction = sourceEntry(entry, effectiveSide);
      if (transaction !== null && filterMatches(transaction, filter)) {
        addToBoundedSample(sample, entry, effectiveSide);
      }
    }
    const population = { count: totals.count, vsize: totals.vsize, sample };
    this.populations.set(key, population);
    return population;
  }
}

export const buildComparisonPolicyView = (
  comparison: CurrentComparison,
): ComparisonPolicyView => new ComparisonPolicyView(comparison);

export const comparisonPolicyMatrixTarget = (
  row: Pick<ComparisonPolicyMatrixRow, "region" | "side">,
  filter: ComparisonPolicyMatrixSelection,
): ComparisonPolicyMatrixTarget => ({
  region: row.region,
  side: row.side,
  filter,
  txid: null,
});
