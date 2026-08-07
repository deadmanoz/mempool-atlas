import {
  createBucketTerrainLayout,
  hitTestBucketTerrain,
  paintBucketTerrain,
  type BucketTerrainGlyph,
  type BucketTerrainHit,
  type BucketTerrainLayout,
  type BucketTerrainMode,
  type BucketTerrainRect,
  type BucketTerrainRegion,
  type BucketTerrainSection,
  type BucketTerrainSectionGroup,
} from "./bucket-terrain";
import { prepareCanvasBacking } from "./canvas-backing";
import type { Bip110Assessment, MempoolTransaction, RuleId } from "./types";
import { RULE_IDS } from "./types";
import {
  concatenateTransactionViews,
  filterTransactionView,
  sortTransactionViewByVsize,
  transactionIndexView,
} from "./transaction-view";

export type TerrainMode = BucketTerrainMode;
export type RuleMask = number;
export type ExactSignatureKey = `exact:${string}`;
export type PartialSignatureKey = `partial:${string}:${string}`;
export type ViolationSignatureKey = ExactSignatureKey | PartialSignatureKey;
export type StatusRegionKey = "compatible" | "indeterminate" | "unclassified";
export type TerrainSectionKey =
  | "compatible"
  | "indeterminate"
  | "violating_exact"
  | "violating_incomplete"
  | "unclassified";
export type TerrainRegionKey = StatusRegionKey | ViolationSignatureKey;

export type TerrainSelection =
  | { kind: "rule"; rule: RuleId }
  | { kind: "region"; regionKey: TerrainRegionKey };

export interface TerrainRule {
  id: RuleId;
  number: number;
  shortLabel: string;
  label: string;
  description: string;
  color: string;
}

export const TERRAIN_RULES: readonly TerrainRule[] = [
  {
    id: "output_size",
    number: 1,
    shortLabel: "Output size",
    label: "Output script size",
    description:
      "Checks output script size, including the policy exception for bounded OP_RETURN outputs.",
    color: "#f58a8a",
  },
  {
    id: "element_size",
    number: 2,
    shortLabel: "Element size",
    label: "Serialized element size",
    description:
      "Checks serialized script and witness elements against the policy size limit.",
    color: "#f46f93",
  },
  {
    id: "undefined_version",
    number: 3,
    shortLabel: "Undefined version",
    label: "Undefined witness version",
    description:
      "Checks undefined witness and Tapleaf versions, including the required P2A witness form.",
    color: "#d97ab8",
  },
  {
    id: "taproot_annex",
    number: 4,
    shortLabel: "Taproot annex",
    label: "Taproot annex",
    description: "Checks whether a Taproot witness carries an annex.",
    color: "#b989d0",
  },
  {
    id: "control_block_size",
    number: 5,
    shortLabel: "Control block",
    label: "Control-block size",
    description: "Checks Taproot control blocks against the policy size limit.",
    color: "#9c8ee0",
  },
  {
    id: "op_success",
    number: 6,
    shortLabel: "OP_SUCCESS",
    label: "OP_SUCCESS",
    description:
      "Checks tapscript for OP_SUCCESS opcodes, including unreachable script.",
    color: "#7f9be2",
  },
  {
    id: "tapscript_op_if",
    number: 7,
    shortLabel: "Tapscript OP_IF",
    label: "Tapscript OP_IF",
    description: "Checks tapscript execution for OP_IF or OP_NOTIF.",
    color: "#62a9d9",
  },
] as const;

export const terrainRule = (ruleId: RuleId): TerrainRule => {
  const rule = TERRAIN_RULES.find(({ id }) => id === ruleId);
  if (rule === undefined) {
    throw new Error(`Missing terrain metadata for ${ruleId}`);
  }
  return rule;
};

export interface ViolationSignature {
  key: ViolationSignatureKey;
  completeness: "exact" | "partial";
  violatedMask: RuleMask;
  unknownMask: RuleMask;
  violatedRules: RuleId[];
  unknownRules: RuleId[];
  foundationRule: RuleId | null;
}

export type TerrainRect = BucketTerrainRect;
export type TerrainSection = BucketTerrainSection<TerrainSectionKey>;
export type TerrainRegion = BucketTerrainRegion<
  TerrainSectionKey,
  TerrainRegionKey,
  ViolationSignature | null
>;
export type TerrainGlyph = BucketTerrainGlyph<
  TerrainSectionKey,
  TerrainRegionKey
>;

export interface ClassificationTotals {
  compatible: { count: number; vsize: number };
  violating: { count: number; vsize: number };
  indeterminate: { count: number; vsize: number };
  unclassified: { count: number; vsize: number };
  completeCoverage: { count: number; vsize: number };
  incompleteCoverage: { count: number; vsize: number };
}

export interface RulePopulation {
  rule: RuleId;
  transactions: MempoolTransaction[];
  count: number;
  vsize: number;
  totalShare: number;
}

export interface SignaturePopulation {
  signature: ViolationSignature;
  transactions: MempoolTransaction[];
  count: number;
  vsize: number;
  totalShare: number;
}

export interface StatusPopulation {
  transactions: MempoolTransaction[];
  count: number;
  vsize: number;
  totalShare: number;
}

export interface TerrainLayout extends BucketTerrainLayout<
  TerrainSectionKey,
  TerrainRegionKey,
  ViolationSignature | null
> {
  totals: ClassificationTotals;
}

export type TerrainHit = BucketTerrainHit<
  TerrainSectionKey,
  TerrainRegionKey,
  ViolationSignature | null
>;

const RULE_MASK_LIMIT = (1 << RULE_IDS.length) - 1;

const RULE_BITS = new Map<RuleId, RuleMask>(
  RULE_IDS.map((rule, index) => [rule, 1 << index]),
);

const emptyTotal = (): { count: number; vsize: number } => ({
  count: 0,
  vsize: 0,
});

const addToTotal = (
  total: { count: number; vsize: number },
  transaction: MempoolTransaction,
): void => {
  total.count += 1;
  total.vsize += transaction.vsize;
};

const populationShare = (
  transactions: readonly MempoolTransaction[],
  count: number,
): number => (transactions.length === 0 ? 0 : count / transactions.length);

const sortTransactions = (
  transactions: readonly MempoolTransaction[],
): MempoolTransaction[] => sortTransactionViewByVsize(transactions);

const sumVsize = (transactions: readonly MempoolTransaction[]): number =>
  transactions.reduce((total, transaction) => total + transaction.vsize, 0);

export const classificationTotals = (
  transactions: readonly MempoolTransaction[],
): ClassificationTotals => {
  const totals: ClassificationTotals = {
    compatible: emptyTotal(),
    violating: emptyTotal(),
    indeterminate: emptyTotal(),
    unclassified: emptyTotal(),
    completeCoverage: emptyTotal(),
    incompleteCoverage: emptyTotal(),
  };

  for (const transaction of transactions) {
    const assessment = transaction.bip110;
    if (assessment === null) {
      addToTotal(totals.unclassified, transaction);
      continue;
    }
    addToTotal(totals[assessment.status], transaction);
    if (assessment.unknown_rules.length === 0) {
      addToTotal(totals.completeCoverage, transaction);
    } else {
      addToTotal(totals.incompleteCoverage, transaction);
    }
  }
  return totals;
};

export const ruleMask = (rules: readonly RuleId[]): RuleMask => {
  let mask = 0;
  for (const rule of rules) {
    mask |= RULE_BITS.get(rule) ?? 0;
  }
  return mask & RULE_MASK_LIMIT;
};

export const rulesForMask = (mask: RuleMask): RuleId[] =>
  RULE_IDS.filter((rule) => ((RULE_BITS.get(rule) ?? 0) & mask) !== 0);

const encodeMask = (mask: RuleMask): string =>
  (mask & RULE_MASK_LIMIT).toString(16).padStart(2, "0");

export const violationSignature = (
  assessment: Bip110Assessment,
): ViolationSignature | null => {
  if (assessment.status !== "violating") {
    return null;
  }
  const violatedMask = ruleMask(assessment.violated_rules);
  const unknownMask = ruleMask(assessment.unknown_rules);
  const completeness = unknownMask === 0 ? "exact" : "partial";
  const key: ViolationSignatureKey =
    completeness === "exact"
      ? `exact:${encodeMask(violatedMask)}`
      : `partial:${encodeMask(violatedMask)}:${encodeMask(unknownMask)}`;
  const violatedRules = rulesForMask(violatedMask);
  const unknownRules = rulesForMask(unknownMask);
  return {
    key,
    completeness,
    violatedMask,
    unknownMask,
    violatedRules,
    unknownRules,
    foundationRule: violatedRules.at(-1) ?? null,
  };
};

/**
 * Return the canonical terrain region for one source-local transaction.
 * Status regions remain explicit, while violating assessments are keyed by
 * their complete proven and unknown rule sets.
 */
export const terrainRegionKey = (
  transaction: MempoolTransaction,
): TerrainRegionKey => {
  const assessment = transaction.bip110;
  if (assessment === null) {
    return "unclassified";
  }
  if (assessment.status !== "violating") {
    return assessment.status;
  }
  const signature = violationSignature(assessment);
  if (signature === null) {
    throw new Error("Violating assessment is missing its rule signature");
  }
  return signature.key;
};

export const signatureLabel = (signature: ViolationSignature): string => {
  const rules = signature.violatedRules
    .map((rule) => `R${terrainRule(rule).number}`)
    .join(" + ");
  if (rules.length === 0) {
    return signature.completeness === "exact"
      ? "Violation"
      : "Confirmed violation";
  }
  if (signature.completeness === "partial") {
    return `Proven ${rules}`;
  }
  return signature.violatedRules.length === 1 ? `${rules} only` : rules;
};

export const unknownRulesLabel = (signature: ViolationSignature): string =>
  signature.unknownRules
    .map((rule) => `R${terrainRule(rule).number}`)
    .join(" + ");

const inverseGrayRank = (mask: RuleMask): number => {
  let gray = mask & RULE_MASK_LIMIT;
  let rank = gray;
  while (gray > 0) {
    gray >>= 1;
    rank ^= gray;
  }
  return rank & RULE_MASK_LIMIT;
};

export const compareViolationSignatures = (
  left: ViolationSignature,
  right: ViolationSignature,
): number => {
  if (left.completeness !== right.completeness) {
    return left.completeness === "exact" ? -1 : 1;
  }
  const violatedOrder =
    inverseGrayRank(left.violatedMask) - inverseGrayRank(right.violatedMask);
  if (violatedOrder !== 0) {
    return violatedOrder;
  }
  const unknownOrder =
    inverseGrayRank(left.unknownMask) - inverseGrayRank(right.unknownMask);
  return unknownOrder !== 0 ? unknownOrder : left.key.localeCompare(right.key);
};

export const rulePopulation = (
  transactions: readonly MempoolTransaction[],
  rule: RuleId,
): RulePopulation => {
  const selected = sortTransactions(
    filterTransactionView(
      transactions,
      (transaction) =>
        transaction.bip110?.violated_rules.includes(rule) ?? false,
    ),
  );
  return {
    rule,
    transactions: selected,
    count: selected.length,
    vsize: sumVsize(selected),
    totalShare: populationShare(transactions, selected.length),
  };
};

export const signaturePopulations = (
  transactions: readonly MempoolTransaction[],
): SignaturePopulation[] => {
  const groups = new Map<
    ViolationSignatureKey,
    { signature: ViolationSignature; rows: number[] }
  >();
  for (let row = 0; row < transactions.length; row += 1) {
    const transaction = transactions[row];
    if (transaction === undefined) continue;
    const assessment = transaction.bip110;
    if (assessment === null) {
      continue;
    }
    const signature = violationSignature(assessment);
    if (signature === null) {
      continue;
    }
    const existing = groups.get(signature.key);
    if (existing === undefined) {
      groups.set(signature.key, { signature, rows: [row] });
    } else {
      existing.rows.push(row);
    }
  }
  return [...groups.values()]
    .sort((left, right) =>
      compareViolationSignatures(left.signature, right.signature),
    )
    .map(({ signature, rows }) => {
      const sorted = sortTransactions(transactionIndexView(transactions, rows));
      return {
        signature,
        transactions: sorted,
        count: sorted.length,
        vsize: sumVsize(sorted),
        totalShare: populationShare(transactions, sorted.length),
      };
    });
};

export const signaturePopulation = (
  transactions: readonly MempoolTransaction[],
  key: ViolationSignatureKey,
): SignaturePopulation | null =>
  signaturePopulations(transactions).find(
    ({ signature }) => signature.key === key,
  ) ?? null;

export const statusPopulation = (
  transactions: readonly MempoolTransaction[],
  key: StatusRegionKey,
): StatusPopulation => {
  const selected = sortTransactions(
    filterTransactionView(transactions, (transaction) =>
      key === "unclassified"
        ? transaction.bip110 === null
        : transaction.bip110?.status === key,
    ),
  );
  return {
    transactions: selected,
    count: selected.length,
    vsize: sumVsize(selected),
    totalShare: populationShare(transactions, selected.length),
  };
};

export const incompleteViolationPopulation = (
  transactions: readonly MempoolTransaction[],
): StatusPopulation => {
  const selected = sortTransactions(
    filterTransactionView(
      transactions,
      (transaction) =>
        transaction.bip110?.status === "violating" &&
        transaction.bip110.unknown_rules.length > 0,
    ),
  );
  return {
    transactions: selected,
    count: selected.length,
    vsize: sumVsize(selected),
    totalShare: populationShare(transactions, selected.length),
  };
};

const terrainGroups = (
  transactions: readonly MempoolTransaction[],
): BucketTerrainSectionGroup<
  TerrainSectionKey,
  TerrainRegionKey,
  ViolationSignature | null
>[] => {
  const compatibleRows: number[] = [];
  const indeterminateRows: number[] = [];
  const unclassifiedRows: number[] = [];
  const signatures = new Map<
    ViolationSignatureKey,
    {
      key: ViolationSignatureKey;
      sectionKey: TerrainSectionKey;
      signature: ViolationSignature;
      rows: number[];
    }
  >();

  for (let row = 0; row < transactions.length; row += 1) {
    const transaction = transactions[row];
    if (transaction === undefined) continue;
    const assessment = transaction.bip110;
    if (assessment === null) {
      unclassifiedRows.push(row);
      continue;
    }
    if (assessment.status === "compatible") {
      compatibleRows.push(row);
      continue;
    }
    if (assessment.status === "indeterminate") {
      indeterminateRows.push(row);
      continue;
    }
    const signature = violationSignature(assessment);
    if (signature === null) {
      continue;
    }
    const sectionKey =
      signature.completeness === "exact"
        ? "violating_exact"
        : "violating_incomplete";
    const existing = signatures.get(signature.key);
    if (existing === undefined) {
      signatures.set(signature.key, {
        key: signature.key,
        sectionKey,
        signature,
        rows: [row],
      });
    } else {
      existing.rows.push(row);
    }
  }

  const signatureGroups = [...signatures.values()]
    .sort((left, right) =>
      compareViolationSignatures(left.signature, right.signature),
    )
    .map(({ rows, ...group }) => ({
      ...group,
      transactions: transactionIndexView(transactions, rows),
    }));
  const exact = signatureGroups.filter(
    ({ sectionKey }) => sectionKey === "violating_exact",
  );
  const incomplete = signatureGroups.filter(
    ({ sectionKey }) => sectionKey === "violating_incomplete",
  );
  const groups: BucketTerrainSectionGroup<
    TerrainSectionKey,
    TerrainRegionKey,
    ViolationSignature | null
  >[] = [];
  const pushStatus = (
    key: "compatible" | "indeterminate" | "unclassified",
    entries: MempoolTransaction[],
  ): void => {
    if (entries.length === 0) {
      return;
    }
    groups.push({
      key,
      transactions: entries,
      regions: [
        {
          key,
          sectionKey: key,
          signature: null,
          transactions: entries,
        },
      ],
      nested: false,
    });
  };

  pushStatus("compatible", transactionIndexView(transactions, compatibleRows));
  pushStatus(
    "indeterminate",
    transactionIndexView(transactions, indeterminateRows),
  );
  if (exact.length > 0) {
    groups.push({
      key: "violating_exact",
      transactions: concatenateTransactionViews(
        transactions,
        exact.map(({ transactions: entries }) => entries),
      ),
      regions: exact,
      nested: true,
    });
  }
  if (incomplete.length > 0) {
    groups.push({
      key: "violating_incomplete",
      transactions: concatenateTransactionViews(
        transactions,
        incomplete.map(({ transactions: entries }) => entries),
      ),
      regions: incomplete,
      nested: true,
    });
  }
  pushStatus(
    "unclassified",
    transactionIndexView(transactions, unclassifiedRows),
  );
  return groups;
};

export const createTerrainLayout = (
  transactions: readonly MempoolTransaction[],
  width: number,
  height: number,
  mode: TerrainMode,
): TerrainLayout => ({
  ...createBucketTerrainLayout(
    terrainGroups(transactions),
    width,
    height,
    mode,
  ),
  totals: classificationTotals(transactions),
});

export const hitTestTerrain = (
  layout: TerrainLayout,
  x: number,
  y: number,
): TerrainHit => hitTestBucketTerrain(layout, x, y);

const regionColor = (region: TerrainRegion): string => {
  if (region.key === "compatible") {
    return "#53d9d4";
  }
  if (region.key === "indeterminate") {
    return "#e1aa4b";
  }
  if (region.key === "unclassified") {
    return "#697988";
  }
  return region.signature?.foundationRule === null ||
    region.signature?.foundationRule === undefined
    ? "#e06b72"
    : terrainRule(region.signature.foundationRule).color;
};

const regionMatchesSelection = (
  region: TerrainRegion,
  selection: TerrainSelection,
): boolean => {
  if (selection.kind === "region") {
    return region.key === selection.regionKey;
  }
  return region.signature?.violatedRules.includes(selection.rule) ?? false;
};

export const paintTerrain = (
  context: CanvasRenderingContext2D,
  layout: TerrainLayout,
  selection: TerrainSelection,
  selectedTxid: string | null = null,
): void =>
  paintBucketTerrain(
    context,
    layout,
    {
      color: regionColor,
      selected: (region) => regionMatchesSelection(region, selection),
      partial: (region) => region.signature?.completeness === "partial",
      glyphOpacity: (region, selected) =>
        region.signature === null
          ? 0.82
          : selected
            ? 1
            : selection.kind === "rule"
              ? 0.46
              : 0.7,
    },
    selectedTxid,
  );

export const renderTerrain = (
  canvas: HTMLCanvasElement,
  transactions: readonly MempoolTransaction[],
  mode: TerrainMode,
  selection: TerrainSelection,
  previousLayout: TerrainLayout | null = null,
  selectedTxid: string | null = null,
): TerrainLayout => {
  const { context, width, height } = prepareCanvasBacking(canvas, "subpixel");
  const layout =
    previousLayout !== null &&
    previousLayout.width === width &&
    previousLayout.height === height &&
    previousLayout.mode === mode
      ? previousLayout
      : createTerrainLayout(transactions, width, height, mode);
  paintTerrain(context, layout, selection, selectedTxid);
  return layout;
};
