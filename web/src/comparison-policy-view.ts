import {
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
import {
  forEachCooperatively,
  type CooperativeWorkOptions,
} from "./cooperative-work";
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
  samples: Map<string, SampleRow[]>;
}

interface SliceState {
  slice: ComparisonPolicySlice;
  signatureTotals: Map<ViolationSignatureKey, ComparisonPolicyTotals>;
  samples: Map<string, SampleRow[]>;
}

interface SampleRow {
  index: number;
  txid: string;
  vsize: number;
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
  samples: new Map(),
});

const policyStatus = (
  transaction: MempoolTransaction,
): ComparisonPolicyStatus => transaction.bip110?.status ?? "unclassified";

const addTransaction = (
  accumulator: SliceAccumulator,
  transaction: MempoolTransaction,
  entry: ComparedTransaction,
  index: number,
): void => {
  const sampleKeys = ["all", `status:${policyStatus(transaction)}`];
  addToTotals(accumulator.population, transaction);
  addToTotals(accumulator.statusTotals[policyStatus(transaction)], transaction);
  const assessment = transaction.bip110;
  if (assessment !== null) {
    for (const rule of assessment.violated_rules) {
      addToTotals(accumulator.ruleTotals[rule], transaction);
      sampleKeys.push(`rule:${rule}`);
    }
    const signature = violationSignature(assessment);
    if (signature !== null) {
      sampleKeys.push(`signature:${signature.key}`);
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
    }
  }
  for (const key of sampleKeys) {
    const sample = accumulator.samples.get(key) ?? [];
    addToBoundedSample(sample, {
      index,
      txid: entry.txid,
      vsize: transaction.vsize,
    });
    accumulator.samples.set(key, sample);
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
    samples: accumulator.samples,
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

const compareSampleRows = (left: SampleRow, right: SampleRow): number =>
  right.vsize - left.vsize || left.txid.localeCompare(right.txid);

const addToBoundedSample = (sample: SampleRow[], row: SampleRow): void => {
  let index = 0;
  while (index < sample.length && compareSampleRows(sample[index]!, row) <= 0) {
    index += 1;
  }
  if (index >= COMPARISON_POLICY_SAMPLE_LIMIT) {
    return;
  }
  sample.splice(index, 0, row);
  if (sample.length > COMPARISON_POLICY_SAMPLE_LIMIT) {
    sample.pop();
  }
};

const createSliceAccumulators = (): Map<
  ComparisonPolicyMatrixRowKey,
  SliceAccumulator
> => new Map(SLICE_SPECS.map((spec) => [spec.key, accumulatorFor(spec)]));

const requiredAccumulator = (
  accumulators: Map<ComparisonPolicyMatrixRowKey, SliceAccumulator>,
  spec: SliceSpec,
): SliceAccumulator => {
  const accumulator = accumulators.get(spec.key);
  if (accumulator === undefined) {
    throw new Error(`Missing comparison policy slice ${spec.key}`);
  }
  return accumulator;
};

const accumulateComparison = (
  comparison: CurrentComparison,
): Map<ComparisonPolicyMatrixRowKey, SliceAccumulator> => {
  const accumulators = createSliceAccumulators();
  const [leftSpec, commonLeftSpec, commonRightSpec, rightSpec] = SLICE_SPECS;
  if (!leftSpec || !commonLeftSpec || !commonRightSpec || !rightSpec) {
    throw new Error("Comparison policy slice configuration is incomplete");
  }
  const leftAccumulator = requiredAccumulator(accumulators, leftSpec);
  const commonLeftAccumulator = requiredAccumulator(
    accumulators,
    commonLeftSpec,
  );
  const commonRightAccumulator = requiredAccumulator(
    accumulators,
    commonRightSpec,
  );
  const rightAccumulator = requiredAccumulator(accumulators, rightSpec);
  comparison.left_only.forEach((entry, index) => {
    addTransaction(
      leftAccumulator,
      requiredTransaction(entry, leftSpec),
      entry,
      index,
    );
  });
  comparison.common.forEach((entry, index) => {
    addTransaction(
      commonLeftAccumulator,
      requiredTransaction(entry, commonLeftSpec),
      entry,
      index,
    );
    addTransaction(
      commonRightAccumulator,
      requiredTransaction(entry, commonRightSpec),
      entry,
      index,
    );
  });
  comparison.right_only.forEach((entry, index) => {
    addTransaction(
      rightAccumulator,
      requiredTransaction(entry, rightSpec),
      entry,
      index,
    );
  });
  return accumulators;
};

const accumulateComparisonCooperatively = async (
  comparison: CurrentComparison,
  options: CooperativeWorkOptions,
): Promise<Map<ComparisonPolicyMatrixRowKey, SliceAccumulator>> => {
  const accumulators = createSliceAccumulators();
  const [leftSpec, commonLeftSpec, commonRightSpec, rightSpec] = SLICE_SPECS;
  if (!leftSpec || !commonLeftSpec || !commonRightSpec || !rightSpec) {
    throw new Error("Comparison policy slice configuration is incomplete");
  }
  const leftAccumulator = requiredAccumulator(accumulators, leftSpec);
  const commonLeftAccumulator = requiredAccumulator(
    accumulators,
    commonLeftSpec,
  );
  const commonRightAccumulator = requiredAccumulator(
    accumulators,
    commonRightSpec,
  );
  const rightAccumulator = requiredAccumulator(accumulators, rightSpec);
  await forEachCooperatively(
    comparison.left_only,
    (entry, index) => {
      addTransaction(
        leftAccumulator,
        requiredTransaction(entry, leftSpec),
        entry,
        index,
      );
    },
    options,
  );
  await forEachCooperatively(
    comparison.common,
    (entry, index) => {
      addTransaction(
        commonLeftAccumulator,
        requiredTransaction(entry, commonLeftSpec),
        entry,
        index,
      );
      addTransaction(
        commonRightAccumulator,
        requiredTransaction(entry, commonRightSpec),
        entry,
        index,
      );
    },
    options,
  );
  await forEachCooperatively(
    comparison.right_only,
    (entry, index) => {
      addTransaction(
        rightAccumulator,
        requiredTransaction(entry, rightSpec),
        entry,
        index,
      );
    },
    options,
  );
  return accumulators;
};

export class ComparisonPolicyView {
  readonly rows: ComparisonPolicyMatrixRow[];
  private readonly slices = new Map<string, SliceState>();
  private readonly populations = new Map<string, ComparisonPolicyPopulation>();

  constructor(
    private readonly comparison: CurrentComparison,
    accumulators = accumulateComparison(comparison),
  ) {
    for (const spec of SLICE_SPECS) {
      const accumulator = requiredAccumulator(accumulators, spec);
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
    const entries = this.comparison[region];
    const sample = (state.samples.get(filterKey(filter)) ?? []).map((row) => {
      const entry = entries[row.index];
      if (entry === undefined) {
        throw new Error(`Comparison policy sample row ${row.index} is missing`);
      }
      return entry;
    });
    const population = { count: totals.count, vsize: totals.vsize, sample };
    this.populations.set(key, population);
    return population;
  }
}

export const buildComparisonPolicyView = (
  comparison: CurrentComparison,
): ComparisonPolicyView => new ComparisonPolicyView(comparison);

export const buildComparisonPolicyViewCooperatively = async (
  comparison: CurrentComparison,
  options: CooperativeWorkOptions = {},
): Promise<ComparisonPolicyView> =>
  new ComparisonPolicyView(
    comparison,
    await accumulateComparisonCooperatively(comparison, options),
  );

export const comparisonPolicyMatrixTarget = (
  row: Pick<ComparisonPolicyMatrixRow, "region" | "side">,
  filter: ComparisonPolicyMatrixSelection,
): ComparisonPolicyMatrixTarget => ({
  region: row.region,
  side: row.side,
  filter,
  txid: null,
});
