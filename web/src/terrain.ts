import type { MempoolTransaction, RuleId } from "./types";
import { RULE_IDS } from "./types";

export type TerrainMode = "count" | "vsize";
export type TerrainRegionKey =
  | "compatible"
  | "indeterminate"
  | "violating_unresolved"
  | "unclassified"
  | RuleId;

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

export interface TerrainRect {
  x: number;
  y: number;
  width: number;
  height: number;
}

export interface TerrainRegion {
  key: TerrainRegionKey;
  rect: TerrainRect;
  contentRect: TerrainRect;
  transactionCount: number;
  totalVsize: number;
  weight: number;
}

export interface TerrainGlyph {
  txid: string;
  regionKey: TerrainRegionKey;
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
  regions: TerrainRegion[];
  glyphs: TerrainGlyph[];
  totals: ClassificationTotals;
}

export type TerrainHit =
  | { kind: "transaction"; glyph: TerrainGlyph }
  | { kind: "region"; region: TerrainRegion }
  | null;

const REGION_GAP = 4;
const REGION_LABEL_HEIGHT = 48;
const GLYPH_GAP = 0.75;

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

export const selectedRulePopulation = (
  transactions: readonly MempoolTransaction[],
  rule: RuleId,
): RulePopulation => {
  const selected = transactions
    .filter((transaction) => transaction.bip110?.primary_rule === rule)
    .sort(
      (left, right) =>
        right.vsize - left.vsize || left.txid.localeCompare(right.txid),
    );
  const vsize = selected.reduce(
    (total, transaction) => total + transaction.vsize,
    0,
  );
  return {
    rule,
    transactions: selected,
    count: selected.length,
    vsize,
    totalShare:
      transactions.length === 0 ? 0 : selected.length / transactions.length,
  };
};

export const unresolvedPrimaryPopulation = (
  transactions: readonly MempoolTransaction[],
): StatusPopulation => {
  const selected = transactions
    .filter(
      (transaction) =>
        transaction.bip110?.status === "violating" &&
        transaction.bip110.primary_rule === null,
    )
    .sort(
      (left, right) =>
        right.vsize - left.vsize || left.txid.localeCompare(right.txid),
    );
  const vsize = selected.reduce(
    (total, transaction) => total + transaction.vsize,
    0,
  );
  return {
    transactions: selected,
    count: selected.length,
    vsize,
    totalShare:
      transactions.length === 0 ? 0 : selected.length / transactions.length,
  };
};

const regionKeyFor = (transaction: MempoolTransaction): TerrainRegionKey => {
  const assessment = transaction.bip110;
  if (assessment === null) {
    return "unclassified";
  }
  if (assessment.status === "compatible") {
    return "compatible";
  }
  if (assessment.status === "indeterminate") {
    return "indeterminate";
  }
  return assessment.primary_rule ?? "violating_unresolved";
};

const insetRect = (rect: TerrainRect, inset: number): TerrainRect => ({
  x: rect.x + inset,
  y: rect.y + inset,
  width: Math.max(0, rect.width - inset * 2),
  height: Math.max(0, rect.height - inset * 2),
});

interface WeightedRegion {
  key: TerrainRegionKey;
  weight: number;
}

const splitWeightedRegions = (
  items: readonly WeightedRegion[],
  rect: TerrainRect,
): Map<TerrainRegionKey, TerrainRect> => {
  const result = new Map<TerrainRegionKey, TerrainRect>();
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
      ? {
          ...current.rect,
          width: current.rect.width * ratio,
        }
      : {
          ...current.rect,
          height: current.rect.height * ratio,
        };
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
): number =>
  mode === "count"
    ? transactions.length
    : transactions.reduce((total, transaction) => total + transaction.vsize, 0);

export const createTerrainLayout = (
  transactions: readonly MempoolTransaction[],
  width: number,
  height: number,
  mode: TerrainMode,
): TerrainLayout => {
  const safeWidth = Math.max(1, width);
  const safeHeight = Math.max(1, height);
  const groups = new Map<TerrainRegionKey, MempoolTransaction[]>();
  const keys: TerrainRegionKey[] = [
    "compatible",
    "indeterminate",
    "violating_unresolved",
    "unclassified",
    ...RULE_IDS,
  ];
  for (const key of keys) {
    groups.set(key, []);
  }
  for (const transaction of transactions) {
    groups.get(regionKeyFor(transaction))?.push(transaction);
  }

  const rawWeights = keys.map((key) => metric(groups.get(key) ?? [], mode));
  // Territory frames use a square-root scale so a dominant compatible
  // population cannot make the seven rule groups unreadable. Exact counts and
  // vsize remain visible in labels; the selected mode controls transaction
  // tile area within each territory.
  const layoutWeights = rawWeights.map(Math.sqrt);
  const totalWeight = layoutWeights.reduce(
    (total, weight) => total + weight,
    0,
  );
  const minimumDisplayWeight =
    totalWeight === 0 ? 1 : Math.max(totalWeight * 0.003, 0.1);
  const weightedRegions = keys.map((key, index) => ({
    key,
    weight: Math.max(layoutWeights[index] ?? 0, minimumDisplayWeight),
  }));
  const regionRects = splitWeightedRegions(weightedRegions, {
    x: 0,
    y: 0,
    width: safeWidth,
    height: safeHeight,
  });

  const regions: TerrainRegion[] = [];
  const glyphs: TerrainGlyph[] = [];
  for (const [index, key] of keys.entries()) {
    const entries = groups.get(key) ?? [];
    const rawRect = regionRects.get(key) ?? {
      x: 0,
      y: 0,
      width: 0,
      height: 0,
    };
    const rect = insetRect(rawRect, REGION_GAP / 2);
    const labelHeight = Math.min(
      REGION_LABEL_HEIGHT,
      Math.max(18, rect.height * 0.28),
    );
    const contentRect = insetRect(
      {
        x: rect.x,
        y: rect.y + labelHeight,
        width: rect.width,
        height: Math.max(0, rect.height - labelHeight),
      },
      3,
    );
    const totalVsize = entries.reduce(
      (total, transaction) => total + transaction.vsize,
      0,
    );
    const region: TerrainRegion = {
      key,
      rect,
      contentRect,
      transactionCount: entries.length,
      totalVsize,
      weight: rawWeights[index] ?? 0,
    };
    regions.push(region);
    for (const glyph of packTransactions(entries, contentRect, key, mode)) {
      glyphs.push(glyph);
    }
  }

  return {
    width: safeWidth,
    height: safeHeight,
    mode,
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

const regionColor = (key: TerrainRegionKey): string => {
  if (key === "compatible") {
    return "#53d9d4";
  }
  if (key === "indeterminate") {
    return "#e1aa4b";
  }
  if (key === "violating_unresolved") {
    return "#e06b72";
  }
  if (key === "unclassified") {
    return "#697988";
  }
  return terrainRule(key).color;
};

export const paintTerrain = (
  context: CanvasRenderingContext2D,
  layout: TerrainLayout,
  selectedRegion: TerrainRegionKey,
): void => {
  context.clearRect(0, 0, layout.width, layout.height);
  context.fillStyle = "#071018";
  context.fillRect(0, 0, layout.width, layout.height);

  for (const region of layout.regions) {
    context.fillStyle = "#0d1922";
    context.fillRect(
      region.rect.x,
      region.rect.y,
      region.rect.width,
      region.rect.height,
    );
    context.strokeStyle = region.key === selectedRegion ? "#6ef2f0" : "#2a3b48";
    context.lineWidth = region.key === selectedRegion ? 2.5 : 1;
    context.strokeRect(
      region.rect.x,
      region.rect.y,
      region.rect.width,
      region.rect.height,
    );
  }

  for (const glyph of layout.glyphs) {
    context.fillStyle = regionColor(glyph.regionKey);
    context.globalAlpha = glyph.regionKey === selectedRegion ? 1 : 0.82;
    context.fillRect(
      glyph.rect.x,
      glyph.rect.y,
      glyph.rect.width,
      glyph.rect.height,
    );
  }
  context.globalAlpha = 1;
};

export const renderTerrain = (
  canvas: HTMLCanvasElement,
  transactions: readonly MempoolTransaction[],
  mode: TerrainMode,
  selectedRegion: TerrainRegionKey,
): TerrainLayout => {
  const bounds = canvas.getBoundingClientRect();
  const width = Math.max(1, bounds.width);
  const height = Math.max(1, bounds.height);
  const scale = Math.max(1, window.devicePixelRatio || 1);
  canvas.width = Math.round(width * scale);
  canvas.height = Math.round(height * scale);
  const context = canvas.getContext("2d");
  if (context === null) {
    throw new Error("Canvas rendering is not available");
  }
  context.setTransform(scale, 0, 0, scale, 0, 0);
  const layout = createTerrainLayout(transactions, width, height, mode);
  paintTerrain(context, layout, selectedRegion);
  return layout;
};
