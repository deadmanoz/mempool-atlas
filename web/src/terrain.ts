import type { Bip110Assessment, MempoolTransaction, RuleId } from "./types";
import { RULE_IDS } from "./types";

export type TerrainMode = "count" | "vsize";
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

export interface TerrainRect {
  x: number;
  y: number;
  width: number;
  height: number;
}

export interface TerrainSection {
  key: TerrainSectionKey;
  rect: TerrainRect;
  contentRect: TerrainRect;
  transactionCount: number;
  totalVsize: number;
  weight: number;
  labelHeight: number;
}

export interface TerrainRegion {
  key: TerrainRegionKey;
  sectionKey: TerrainSectionKey;
  signature: ViolationSignature | null;
  rect: TerrainRect;
  contentRect: TerrainRect;
  transactionCount: number;
  totalVsize: number;
  weight: number;
  labelHeight: number;
}

export interface TerrainGlyph {
  txid: string;
  regionKey: TerrainRegionKey;
  sectionKey: TerrainSectionKey;
  rect: TerrainRect;
  vsize: number;
}

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

export interface TerrainLayout {
  width: number;
  height: number;
  mode: TerrainMode;
  sections: TerrainSection[];
  regions: TerrainRegion[];
  glyphs: TerrainGlyph[];
  totals: ClassificationTotals;
}

export type TerrainHit =
  | { kind: "transaction"; glyph: TerrainGlyph }
  | { kind: "region"; region: TerrainRegion }
  | null;

const SECTION_GAP = 4;
const SECTION_LABEL_HEIGHT = 42;
const REGION_GAP = 3;
const REGION_LABEL_HEIGHT = 36;
const GLYPH_GAP = 0.75;
const MIN_CONTENT_EXTENT = 0.5;
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
): MempoolTransaction[] =>
  [...transactions].sort(
    (left, right) =>
      right.vsize - left.vsize || left.txid.localeCompare(right.txid),
  );

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

const compareSignatures = (
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
    transactions.filter((transaction) =>
      transaction.bip110?.violated_rules.includes(rule),
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
    { signature: ViolationSignature; transactions: MempoolTransaction[] }
  >();
  for (const transaction of transactions) {
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
      groups.set(signature.key, { signature, transactions: [transaction] });
    } else {
      existing.transactions.push(transaction);
    }
  }
  return [...groups.values()]
    .sort((left, right) => compareSignatures(left.signature, right.signature))
    .map(({ signature, transactions: selected }) => {
      const sorted = sortTransactions(selected);
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
    transactions.filter((transaction) =>
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
    transactions.filter(
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

const insetRect = (rect: TerrainRect, inset: number): TerrainRect => ({
  x: rect.x + inset,
  y: rect.y + inset,
  width: Math.max(0, rect.width - inset * 2),
  height: Math.max(0, rect.height - inset * 2),
});

const adaptiveInsetRect = (
  rect: TerrainRect,
  preferredInset: number,
): TerrainRect => {
  const maximumInset = Math.max(
    0,
    Math.min(rect.width, rect.height) / 2 - MIN_CONTENT_EXTENT / 2,
  );
  return insetRect(rect, Math.min(preferredInset, maximumInset));
};

const adaptiveLabelHeight = (
  availableHeight: number,
  preferredHeight: number,
  proportionalHeight: number,
): number =>
  Math.max(
    0,
    Math.min(
      preferredHeight,
      availableHeight * proportionalHeight,
      availableHeight - MIN_CONTENT_EXTENT,
    ),
  );

interface WeightedItem<Key extends string> {
  key: Key;
  weight: number;
}

const splitWeightedRegions = <Key extends string>(
  items: readonly WeightedItem<Key>[],
  rect: TerrainRect,
): Map<Key, TerrainRect> => {
  const result = new Map<Key, TerrainRect>();
  const stack: Array<{
    start: number;
    end: number;
    rect: TerrainRect;
  }> = [{ start: 0, end: items.length, rect }];
  const prefix = [0];
  for (const item of items) {
    prefix.push((prefix.at(-1) ?? 0) + item.weight);
  }

  while (stack.length > 0) {
    const current = stack.pop();
    if (current === undefined || current.start >= current.end) {
      continue;
    }
    if (current.end - current.start === 1) {
      const item = items[current.start];
      if (item !== undefined) {
        result.set(item.key, current.rect);
      }
      continue;
    }

    const total = (prefix[current.end] ?? 0) - (prefix[current.start] ?? 0);
    const halfway = (prefix[current.start] ?? 0) + total / 2;
    let split = current.start + 1;
    while (
      split < current.end - 1 &&
      (prefix[split] ?? Number.POSITIVE_INFINITY) < halfway
    ) {
      split += 1;
    }
    const firstWeight = (prefix[split] ?? 0) - (prefix[current.start] ?? 0);
    const ratio = total === 0 ? 0.5 : firstWeight / total;
    const splitVertically = current.rect.width >= current.rect.height;
    const firstRect: TerrainRect = splitVertically
      ? { ...current.rect, width: current.rect.width * ratio }
      : { ...current.rect, height: current.rect.height * ratio };
    const secondRect: TerrainRect = splitVertically
      ? {
          x: current.rect.x + firstRect.width,
          y: current.rect.y,
          width: current.rect.width - firstRect.width,
          height: current.rect.height,
        }
      : {
          x: current.rect.x,
          y: current.rect.y + firstRect.height,
          width: current.rect.width,
          height: current.rect.height - firstRect.height,
        };
    stack.push({ start: split, end: current.end, rect: secondRect });
    stack.push({ start: current.start, end: split, rect: firstRect });
  }
  return result;
};

const packTransactions = (
  transactions: readonly MempoolTransaction[],
  rect: TerrainRect,
  regionKey: TerrainRegionKey,
  sectionKey: TerrainSectionKey,
  mode: TerrainMode,
): TerrainGlyph[] => {
  if (transactions.length === 0 || rect.width <= 0 || rect.height <= 0) {
    return [];
  }
  const weights = transactions.map((transaction) =>
    mode === "count" ? 1 : transaction.vsize,
  );
  const prefix = [0];
  for (const weight of weights) {
    prefix.push((prefix.at(-1) ?? 0) + weight);
  }
  const glyphs: TerrainGlyph[] = [];
  const stack: Array<{
    start: number;
    end: number;
    rect: TerrainRect;
  }> = [{ start: 0, end: transactions.length, rect }];

  while (stack.length > 0) {
    const current = stack.pop();
    if (current === undefined || current.start >= current.end) {
      continue;
    }
    if (current.end - current.start === 1) {
      const transaction = transactions[current.start];
      if (transaction !== undefined) {
        const adaptiveGap = Math.min(
          GLYPH_GAP,
          current.rect.width * 0.08,
          current.rect.height * 0.08,
        );
        glyphs.push({
          txid: transaction.txid,
          regionKey,
          sectionKey,
          rect: insetRect(current.rect, adaptiveGap),
          vsize: transaction.vsize,
        });
      }
      continue;
    }

    const total = (prefix[current.end] ?? 0) - (prefix[current.start] ?? 0);
    const halfway = (prefix[current.start] ?? 0) + total / 2;
    let low = current.start + 1;
    let high = current.end - 1;
    while (low < high) {
      const middle = Math.floor((low + high) / 2);
      if ((prefix[middle] ?? 0) < halfway) {
        low = middle + 1;
      } else {
        high = middle;
      }
    }
    const split = Math.min(current.end - 1, Math.max(current.start + 1, low));
    const firstWeight = (prefix[split] ?? 0) - (prefix[current.start] ?? 0);
    const ratio = total === 0 ? 0.5 : firstWeight / total;
    const splitVertically = current.rect.width >= current.rect.height;
    const firstRect: TerrainRect = splitVertically
      ? { ...current.rect, width: current.rect.width * ratio }
      : { ...current.rect, height: current.rect.height * ratio };
    const secondRect: TerrainRect = splitVertically
      ? {
          x: current.rect.x + firstRect.width,
          y: current.rect.y,
          width: current.rect.width - firstRect.width,
          height: current.rect.height,
        }
      : {
          x: current.rect.x,
          y: current.rect.y + firstRect.height,
          width: current.rect.width,
          height: current.rect.height - firstRect.height,
        };
    stack.push({ start: split, end: current.end, rect: secondRect });
    stack.push({ start: current.start, end: split, rect: firstRect });
  }
  return glyphs;
};

const metric = (
  transactions: readonly MempoolTransaction[],
  mode: TerrainMode,
): number => (mode === "count" ? transactions.length : sumVsize(transactions));

interface TerrainGroup {
  key: TerrainRegionKey;
  sectionKey: TerrainSectionKey;
  signature: ViolationSignature | null;
  transactions: MempoolTransaction[];
}

interface SectionGroup {
  key: TerrainSectionKey;
  transactions: MempoolTransaction[];
  regions: TerrainGroup[];
}

const terrainGroups = (
  transactions: readonly MempoolTransaction[],
): SectionGroup[] => {
  const compatible: MempoolTransaction[] = [];
  const indeterminate: MempoolTransaction[] = [];
  const unclassified: MempoolTransaction[] = [];
  const signatures = new Map<ViolationSignatureKey, TerrainGroup>();

  for (const transaction of transactions) {
    const assessment = transaction.bip110;
    if (assessment === null) {
      unclassified.push(transaction);
      continue;
    }
    if (assessment.status === "compatible") {
      compatible.push(transaction);
      continue;
    }
    if (assessment.status === "indeterminate") {
      indeterminate.push(transaction);
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
        transactions: [transaction],
      });
    } else {
      existing.transactions.push(transaction);
    }
  }

  const signatureGroups = [...signatures.values()].sort((left, right) =>
    compareSignatures(
      left.signature as ViolationSignature,
      right.signature as ViolationSignature,
    ),
  );
  const exact = signatureGroups.filter(
    ({ sectionKey }) => sectionKey === "violating_exact",
  );
  const incomplete = signatureGroups.filter(
    ({ sectionKey }) => sectionKey === "violating_incomplete",
  );
  const groups: SectionGroup[] = [];
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
    });
  };

  pushStatus("compatible", compatible);
  pushStatus("indeterminate", indeterminate);
  if (exact.length > 0) {
    groups.push({
      key: "violating_exact",
      transactions: exact.flatMap(({ transactions: entries }) => entries),
      regions: exact,
    });
  }
  if (incomplete.length > 0) {
    groups.push({
      key: "violating_incomplete",
      transactions: incomplete.flatMap(({ transactions: entries }) => entries),
      regions: incomplete,
    });
  }
  pushStatus("unclassified", unclassified);
  return groups;
};

const sectionLayoutWeights = (
  groups: readonly SectionGroup[],
  mode: TerrainMode,
): number[] => {
  const weights = groups.map((group) =>
    Math.cbrt(metric(group.transactions, mode)),
  );
  const violatingIndexes = groups.flatMap((group, index) =>
    group.key === "violating_exact" || group.key === "violating_incomplete"
      ? [index]
      : [],
  );
  const violatingWeight = violatingIndexes.reduce(
    (total, index) => total + (weights[index] ?? 0),
    0,
  );
  const otherWeight = weights.reduce((total, weight, index) => {
    return violatingIndexes.includes(index) ? total : total + weight;
  }, 0);
  if (violatingWeight > 0 && otherWeight > 0) {
    const requiredForVisibility = (otherWeight * 0.32) / 0.68;
    if (violatingWeight < requiredForVisibility) {
      const scale = requiredForVisibility / violatingWeight;
      for (const index of violatingIndexes) {
        weights[index] = (weights[index] ?? 0) * scale;
      }
    }
  }
  return weights;
};

export const createTerrainLayout = (
  transactions: readonly MempoolTransaction[],
  width: number,
  height: number,
  mode: TerrainMode,
): TerrainLayout => {
  const safeWidth = Math.max(1, width);
  const safeHeight = Math.max(1, height);
  const groups = terrainGroups(transactions);
  const layoutWeights = sectionLayoutWeights(groups, mode);
  const sectionRects = splitWeightedRegions(
    groups.map((group, index) => ({
      key: group.key,
      weight: layoutWeights[index] ?? 1,
    })),
    { x: 0, y: 0, width: safeWidth, height: safeHeight },
  );

  const sections: TerrainSection[] = [];
  const regions: TerrainRegion[] = [];
  const glyphs: TerrainGlyph[] = [];

  for (const group of groups) {
    const rawRect = sectionRects.get(group.key) ?? {
      x: 0,
      y: 0,
      width: 0,
      height: 0,
    };
    const rect = adaptiveInsetRect(rawRect, SECTION_GAP / 2);
    const sectionLabelHeight = adaptiveLabelHeight(
      rect.height,
      SECTION_LABEL_HEIGHT,
      0.16,
    );
    const contentRect = adaptiveInsetRect(
      {
        x: rect.x,
        y: rect.y + sectionLabelHeight,
        width: rect.width,
        height: Math.max(0, rect.height - sectionLabelHeight),
      },
      2,
    );
    const section: TerrainSection = {
      key: group.key,
      rect,
      contentRect,
      transactionCount: group.transactions.length,
      totalVsize: sumVsize(group.transactions),
      weight: metric(group.transactions, mode),
      labelHeight: sectionLabelHeight,
    };
    sections.push(section);

    const nested =
      group.key === "violating_exact" || group.key === "violating_incomplete";
    const regionWeights = group.regions.map((region) => ({
      key: region.key,
      weight: nested
        ? Math.sqrt(metric(region.transactions, mode))
        : metric(region.transactions, mode),
    }));
    const regionRects = nested
      ? splitWeightedRegions(regionWeights, contentRect)
      : new Map<TerrainRegionKey, TerrainRect>([
          [group.regions[0]?.key ?? "unclassified", contentRect],
        ]);

    for (const regionGroup of group.regions) {
      const rawRegionRect = regionRects.get(regionGroup.key) ?? contentRect;
      const regionRect = nested
        ? adaptiveInsetRect(rawRegionRect, REGION_GAP / 2)
        : rawRegionRect;
      const regionLabelHeight = nested
        ? adaptiveLabelHeight(regionRect.height, REGION_LABEL_HEIGHT, 0.2)
        : 0;
      const regionContentRect = adaptiveInsetRect(
        {
          x: regionRect.x,
          y: regionRect.y + regionLabelHeight,
          width: regionRect.width,
          height: Math.max(0, regionRect.height - regionLabelHeight),
        },
        3,
      );
      const region: TerrainRegion = {
        key: regionGroup.key,
        sectionKey: group.key,
        signature: regionGroup.signature,
        rect: regionRect,
        contentRect: regionContentRect,
        transactionCount: regionGroup.transactions.length,
        totalVsize: sumVsize(regionGroup.transactions),
        weight: metric(regionGroup.transactions, mode),
        labelHeight: regionLabelHeight,
      };
      regions.push(region);
      for (const glyph of packTransactions(
        regionGroup.transactions,
        regionContentRect,
        regionGroup.key,
        group.key,
        mode,
      )) {
        glyphs.push(glyph);
      }
    }
  }

  return {
    width: safeWidth,
    height: safeHeight,
    mode,
    sections,
    regions,
    glyphs,
    totals: classificationTotals(transactions),
  };
};

const containsPoint = (rect: TerrainRect, x: number, y: number): boolean =>
  x >= rect.x &&
  x <= rect.x + rect.width &&
  y >= rect.y &&
  y <= rect.y + rect.height;

export const hitTestTerrain = (
  layout: TerrainLayout,
  x: number,
  y: number,
): TerrainHit => {
  const region = layout.regions.find(({ rect }) => containsPoint(rect, x, y));
  if (region === undefined) {
    return null;
  }
  for (const glyph of layout.glyphs) {
    if (glyph.regionKey === region.key && containsPoint(glyph.rect, x, y)) {
      return { kind: "transaction", glyph };
    }
  }
  return { kind: "region", region };
};

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
): void => {
  context.clearRect(0, 0, layout.width, layout.height);
  context.fillStyle = "#071018";
  context.fillRect(0, 0, layout.width, layout.height);

  for (const section of layout.sections) {
    context.fillStyle = "#0a161e";
    context.fillRect(
      section.rect.x,
      section.rect.y,
      section.rect.width,
      section.rect.height,
    );
    context.strokeStyle = "#2a3b48";
    context.lineWidth = 1;
    context.strokeRect(
      section.rect.x,
      section.rect.y,
      section.rect.width,
      section.rect.height,
    );
  }

  for (const region of layout.regions) {
    const selected = regionMatchesSelection(region, selection);
    context.fillStyle = "#0d1922";
    context.fillRect(
      region.rect.x,
      region.rect.y,
      region.rect.width,
      region.rect.height,
    );
    context.strokeStyle = selected
      ? "#6ef2f0"
      : region.signature?.completeness === "partial"
        ? "#9a7735"
        : "#243744";
    context.lineWidth = selected ? 1.75 : 1;
    context.strokeRect(
      region.rect.x,
      region.rect.y,
      region.rect.width,
      region.rect.height,
    );
  }

  const regionsByKey = new Map(
    layout.regions.map((region) => [region.key, region]),
  );
  let selectedGlyph: TerrainGlyph | null = null;
  for (const glyph of layout.glyphs) {
    const region = regionsByKey.get(glyph.regionKey);
    if (region === undefined) {
      continue;
    }
    context.fillStyle = regionColor(region);
    if (region.signature === null) {
      context.globalAlpha = 0.82;
    } else if (regionMatchesSelection(region, selection)) {
      context.globalAlpha = 1;
    } else {
      context.globalAlpha = selection.kind === "rule" ? 0.46 : 0.7;
    }
    context.fillRect(
      glyph.rect.x,
      glyph.rect.y,
      glyph.rect.width,
      glyph.rect.height,
    );
    if (glyph.txid === selectedTxid) {
      selectedGlyph = glyph;
    }
  }
  context.globalAlpha = 1;
  if (selectedGlyph !== null) {
    context.strokeStyle = "#f7ff6a";
    context.lineWidth = 2.5;
    context.strokeRect(
      selectedGlyph.rect.x,
      selectedGlyph.rect.y,
      selectedGlyph.rect.width,
      selectedGlyph.rect.height,
    );
  }
};

export const renderTerrain = (
  canvas: HTMLCanvasElement,
  transactions: readonly MempoolTransaction[],
  mode: TerrainMode,
  selection: TerrainSelection,
  previousLayout: TerrainLayout | null = null,
  selectedTxid: string | null = null,
): TerrainLayout => {
  const bounds = canvas.getBoundingClientRect();
  const width = Math.max(1, bounds.width);
  const height = Math.max(1, bounds.height);
  const scale = Math.max(1, window.devicePixelRatio || 1);
  const pixelWidth = Math.round(width * scale);
  const pixelHeight = Math.round(height * scale);
  if (canvas.width !== pixelWidth || canvas.height !== pixelHeight) {
    canvas.width = pixelWidth;
    canvas.height = pixelHeight;
  }
  const context = canvas.getContext("2d");
  if (context === null) {
    throw new Error("Canvas rendering is not available");
  }
  context.setTransform(scale, 0, 0, scale, 0, 0);
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
