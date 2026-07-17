// Pure presentation logic for the workbench: colour system, bin labels
// derived from the server-emitted bin catalog, treemap layout, and chart
// geometry. No DOM access so everything here is unit-testable.

import type {
  BinCatalog,
  ClassificationKey,
  FeeRateEcdf,
  JointFeeSize,
  MempoolSummary,
  ScriptTypeKey,
  SummaryDimension,
} from "./types";

export const CLASSIFICATION_META: Record<
  ClassificationKey,
  { label: string; color: string }
> = {
  payment: { label: "Payment", color: "#56c7d9" },
  consolidation: { label: "Consolidation", color: "#5b9dd9" },
  batch: { label: "Batch payout", color: "#7fce6b" },
  coinjoin: { label: "CoinJoin", color: "#8f7de0" },
  data: { label: "Data / inscription", color: "#e0a35a" },
  lightning: { label: "Lightning", color: "#d9759e" },
  unknown: { label: "Unknown", color: "#6b7a8d" },
};

export const SCRIPT_META: Record<
  ScriptTypeKey,
  { label: string; color: string }
> = {
  p2tr: { label: "P2TR", color: "#56c7d9" },
  p2wpkh: { label: "P2WPKH", color: "#5ad1c9" },
  p2wsh: { label: "P2WSH", color: "#5b9dd9" },
  p2sh: { label: "P2SH", color: "#8f7de0" },
  p2pkh: { label: "P2PKH", color: "#e0a35a" },
  op_return: { label: "OP_RETURN", color: "#d9759e" },
  other: { label: "Other", color: "#6b7a8d" },
};

// Age colours run newest (bright) to oldest (grey-blue), matching ascending
// age-bin order.
const AGE_COLORS = [
  "#c3f5f7",
  "#69cedc",
  "#37a8c0",
  "#418eaa",
  "#557c98",
  "#637083",
];

const RAMP_STOPS: [number, number, number][] = [
  [16, 27, 39],
  [22, 57, 74],
  [47, 125, 146],
  [86, 199, 217],
  [195, 245, 247],
];

/** Interpolates the shared sequential colour ramp at `t` in [0, 1]. */
export const ramp = (t: number): string => {
  const clamped = Math.max(0, Math.min(1, t));
  const position = clamped * (RAMP_STOPS.length - 1);
  const index = Math.min(Math.floor(position), RAMP_STOPS.length - 2);
  const fraction = position - index;
  const from = RAMP_STOPS[index]!;
  const to = RAMP_STOPS[index + 1]!;
  const channel = (start: number, end: number): number =>
    Math.round(start + (end - start) * fraction);
  return `rgb(${channel(from[0], to[0])},${channel(from[1], to[1])},${channel(from[2], to[2])})`;
};

const integerFormat = new Intl.NumberFormat();

export const formatCount = (value: number): string =>
  integerFormat.format(Math.round(value));

export const formatCompact = (value: number): string => {
  if (value >= 1_000_000) {
    return `${(value / 1_000_000).toFixed(2)}M`;
  }
  if (value >= 1_000) {
    return `${(value / 1_000).toFixed(1)}k`;
  }
  return String(Math.round(value));
};

const formatEdge = (value: number): string =>
  value >= 1 || value === 0
    ? String(Math.round(value * 100) / 100)
    : String(value);

/** `<1, 1–2, …, 128+` style labels from interior numeric edges. */
export const rangeBinLabels = (edges: number[]): string[] => {
  if (edges.length === 0) {
    return [];
  }
  const labels = [`<${formatEdge(edges[0]!)}`];
  for (let index = 1; index < edges.length; index += 1) {
    labels.push(
      `${formatEdge(edges[index - 1]!)}–${formatEdge(edges[index]!)}`,
    );
  }
  labels.push(`${formatEdge(edges[edges.length - 1]!)}+`);
  return labels;
};

const formatDurationMs = (ms: number): string => {
  if (ms < 3_600_000) {
    return `${Math.round(ms / 60_000)}m`;
  }
  if (ms < 86_400_000) {
    return `${Math.round(ms / 3_600_000)}h`;
  }
  return `${Math.round(ms / 86_400_000)}d`;
};

export const ageBinLabels = (edgesMs: number[]): string[] => {
  if (edgesMs.length === 0) {
    return [];
  }
  const labels = [`<${formatDurationMs(edgesMs[0]!)}`];
  for (let index = 1; index < edgesMs.length; index += 1) {
    labels.push(
      `${formatDurationMs(edgesMs[index - 1]!)}–${formatDurationMs(edgesMs[index]!)}`,
    );
  }
  labels.push(`${formatDurationMs(edgesMs[edgesMs.length - 1]!)}+`);
  return labels;
};

const SATS_PER_BTC = 100_000_000;

const formatBtc = (sats: number): string => {
  const btc = sats / SATS_PER_BTC;
  return btc >= 1
    ? String(btc)
    : btc.toFixed(Math.min(8, -Math.floor(Math.log10(btc))));
};

export const valueBinLabels = (edgesSats: number[]): string[] => {
  if (edgesSats.length === 0) {
    return [];
  }
  const labels = [`<${formatBtc(edgesSats[0]!)}`];
  for (let index = 1; index < edgesSats.length; index += 1) {
    labels.push(
      `${formatBtc(edgesSats[index - 1]!)}–${formatBtc(edgesSats[index]!)}`,
    );
  }
  labels.push(`${formatBtc(edgesSats[edgesSats.length - 1]!)}+`);
  return labels;
};

/** `1, 2–5, 6–20, …, 100+` labels from inclusive band uppers. */
export const countBandLabels = (uppers: number[]): string[] => {
  const labels: string[] = [];
  let lower = 1;
  for (const upper of uppers) {
    labels.push(upper === lower ? String(upper) : `${lower}–${upper}`);
    lower = upper + 1;
  }
  if (uppers.length > 0) {
    labels.push(`${uppers[uppers.length - 1]!}+`);
  }
  return labels;
};

export interface DimensionBinMeta {
  key: string;
  label: string;
  color: string;
}

/** Ordered per-bin labels and colours for one dimension, derived from the
 * server-emitted bin catalog so the client never hardcodes edges. */
export const dimensionBinMeta = (
  dimension: SummaryDimension,
  bins: BinCatalog,
): DimensionBinMeta[] => {
  switch (dimension) {
    case "classification":
      return bins.classification_keys.map((key) => ({
        key,
        ...CLASSIFICATION_META[key],
      }));
    case "script":
      return bins.script_keys.map((key) => ({ key, ...SCRIPT_META[key] }));
    case "value":
      return valueBinLabels(bins.value_sats_edges).map((label, index, all) => ({
        key: String(index),
        label,
        color: ramp(0.2 + (0.8 * index) / Math.max(1, all.length - 1)),
      }));
    case "inputs":
      return countBandLabels(bins.input_count_uppers).map(
        (label, index, all) => ({
          key: String(index),
          label,
          color: ramp(0.2 + (0.8 * index) / Math.max(1, all.length - 1)),
        }),
      );
    case "outputs":
      return countBandLabels(bins.output_count_uppers).map(
        (label, index, all) => ({
          key: String(index),
          label,
          color: ramp(0.2 + (0.8 * index) / Math.max(1, all.length - 1)),
        }),
      );
    case "age":
      return ageBinLabels(bins.age_ms_edges).map((label, index) => ({
        key: String(index),
        label,
        color: AGE_COLORS[Math.min(index, AGE_COLORS.length - 1)]!,
      }));
    case "feerate":
      return rangeBinLabels(bins.feerate_sat_per_vb_edges).map(
        (label, index, all) => ({
          key: String(index),
          label,
          color: ramp(0.15 + (0.85 * index) / Math.max(1, all.length - 1)),
        }),
      );
  }
};

export interface CompositionSegment extends DimensionBinMeta {
  fraction: number;
  count: number;
  vsize: number;
}

export type CompositionRow =
  | {
      dimension: SummaryDimension;
      label: string;
      status: "unavailable";
      reason: string;
    }
  | {
      dimension: SummaryDimension;
      label: string;
      status: "available";
      segments: CompositionSegment[];
    };

const DIMENSION_LABELS: Record<SummaryDimension, string> = {
  classification: "Classification",
  script: "Script type",
  value: "Output value (BTC)",
  inputs: "Inputs",
  outputs: "Outputs",
  age: "Age",
  feerate: "Fee-rate (sat/vB)",
};

export const COMPOSITION_ORDER: readonly SummaryDimension[] = [
  "classification",
  "script",
  "value",
  "inputs",
  "outputs",
  "age",
  "feerate",
];

export const compositionRows = (
  summary: MempoolSummary,
  metric: "count" | "vsize",
): CompositionRow[] =>
  COMPOSITION_ORDER.map((dimension) => {
    const histogram = summary.histograms[dimension];
    const label = DIMENSION_LABELS[dimension];
    if (histogram.status === "unavailable") {
      return {
        dimension,
        label,
        status: "unavailable",
        reason: histogram.reason,
      };
    }
    const meta = dimensionBinMeta(dimension, summary.bins);
    const binTotal = histogram.bins.reduce((sum, bin) => sum + bin[metric], 0);
    const total = binTotal + histogram.underived[metric];
    const segments = histogram.bins.flatMap((bin, index) => {
      if (bin[metric] === 0 || index >= meta.length) {
        return [];
      }
      return [
        {
          ...meta[index]!,
          fraction: total === 0 ? 0 : bin[metric] / total,
          count: bin.count,
          vsize: bin.vsize,
        },
      ];
    });
    if (histogram.underived[metric] > 0) {
      segments.push({
        key: "underived",
        label: "Underived",
        color: "#38434f",
        fraction: total === 0 ? 0 : histogram.underived[metric] / total,
        count: histogram.underived.count,
        vsize: histogram.underived.vsize,
      });
    }
    return { dimension, label, status: "available", segments };
  });

export interface TreemapTile {
  key: string;
  label: string;
  color: string;
  count: number;
  vsize: number;
  x: number;
  y: number;
  width: number;
  height: number;
}

/** Squarified treemap layout over a unit-free width × height canvas. */
export const squarify = (
  nodes: {
    key: string;
    label: string;
    color: string;
    count: number;
    vsize: number;
  }[],
  width: number,
  height: number,
): TreemapTile[] => {
  const placed: TreemapTile[] = [];
  const sorted = nodes
    .filter((node) => node.vsize > 0)
    .sort((left, right) => right.vsize - left.vsize);
  const total = sorted.reduce((sum, node) => sum + node.vsize, 0);
  if (total <= 0) {
    return placed;
  }
  const scale = (width * height) / total;
  const areas = sorted.map((node) => ({ node, area: node.vsize * scale }));

  let x = 0;
  let y = 0;
  let remainingWidth = width;
  let remainingHeight = height;
  let row: { node: (typeof sorted)[number]; area: number }[] = [];

  const worst = (candidate: typeof row, side: number): number => {
    const sum = candidate.reduce((acc, entry) => acc + entry.area, 0);
    let smallest = Number.POSITIVE_INFINITY;
    let largest = 0;
    for (const entry of candidate) {
      smallest = Math.min(smallest, entry.area);
      largest = Math.max(largest, entry.area);
    }
    return Math.max(
      (side * side * largest) / (sum * sum),
      (sum * sum) / (side * side * smallest),
    );
  };

  const layRow = (finished: typeof row): void => {
    const sum = finished.reduce((acc, entry) => acc + entry.area, 0);
    if (remainingWidth >= remainingHeight) {
      const columnWidth = sum / remainingHeight;
      let cursor = y;
      for (const entry of finished) {
        const tileHeight = entry.area / columnWidth;
        placed.push({
          ...entry.node,
          x,
          y: cursor,
          width: columnWidth,
          height: tileHeight,
        });
        cursor += tileHeight;
      }
      x += columnWidth;
      remainingWidth -= columnWidth;
    } else {
      const rowHeight = sum / remainingWidth;
      let cursor = x;
      for (const entry of finished) {
        const tileWidth = entry.area / rowHeight;
        placed.push({
          ...entry.node,
          x: cursor,
          y,
          width: tileWidth,
          height: rowHeight,
        });
        cursor += tileWidth;
      }
      y += rowHeight;
      remainingHeight -= rowHeight;
    }
  };

  for (const entry of areas) {
    const side = Math.min(remainingWidth, remainingHeight);
    if (row.length === 0) {
      row.push(entry);
      continue;
    }
    if (worst([...row, entry], side) <= worst(row, side)) {
      row.push(entry);
    } else {
      layRow(row);
      row = [entry];
    }
  }
  if (row.length > 0) {
    layRow(row);
  }
  return placed;
};

export interface EcdfPath {
  key: ClassificationKey;
  color: string;
  d: string;
}

/** SVG path per classification: x spans the fine fee bins, y is cumulative
 * vsize share of that class (1 at its own total). */
export const ecdfPaths = (
  ecdf: FeeRateEcdf,
  plot: { left: number; top: number; width: number; height: number },
): EcdfPath[] =>
  ecdf.series.flatMap((series) => {
    const total = series.cum_vsize[series.cum_vsize.length - 1];
    if (total === undefined || total === 0 || series.cum_vsize.length < 2) {
      return [];
    }
    const steps = series.cum_vsize.length - 1;
    const d = series.cum_vsize
      .map((cumulative, index) => {
        const x = plot.left + (index / steps) * plot.width;
        const y = plot.top + (1 - cumulative / total) * plot.height;
        return `${index === 0 ? "M" : "L"}${x.toFixed(1)} ${y.toFixed(1)}`;
      })
      .join("");
    return [
      { key: series.key, color: CLASSIFICATION_META[series.key].color, d },
    ];
  });

export interface JointViewModel {
  /** Rows top-to-bottom are largest-to-smallest size bins; values in [0, 1]. */
  rows: number[][];
  /** Per-fee-bin marginal in [0, 1]. */
  top: number[];
  /** Per-size-bin marginal in [0, 1], top-to-bottom like `rows`. */
  right: number[];
  feeBinCount: number;
  sizeBinCount: number;
}

export const jointViewModel = (joint: JointFeeSize): JointViewModel => {
  const feeBinCount = Math.max(0, joint.fee_edges.length - 1);
  const sizeBinCount = Math.max(0, joint.size_edges.length - 1);
  let peak = 0;
  const top = Array.from({ length: feeBinCount }, () => 0);
  const right: number[] = [];
  for (const row of joint.grid) {
    let rowSum = 0;
    row.forEach((value, index) => {
      peak = Math.max(peak, value);
      rowSum += value;
      top[index] = (top[index] ?? 0) + value;
    });
    right.push(rowSum);
  }
  const topPeak = Math.max(...top, 1);
  const rightPeak = Math.max(...right, 1);
  const rows = [...joint.grid]
    .reverse()
    .map((row) =>
      row.map((value) => (peak === 0 ? 0 : Math.sqrt(value / peak))),
    );
  return {
    rows,
    top: top.map((value) => value / topPeak),
    right: right.reverse().map((value) => value / rightPeak),
    feeBinCount,
    sizeBinCount,
  };
};
