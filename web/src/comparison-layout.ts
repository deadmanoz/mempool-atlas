import {
  comparisonPolicyFilterMatches,
  comparisonRegionEntries,
  policySideForRegion,
  sourceEntry,
  type ComparedTransaction,
  type ComparisonPolicyFilter,
  type ComparisonRegionKey,
  type ComparisonSide,
  type CurrentComparison,
} from "./comparison-model";
import { prepareCanvasBacking } from "./canvas-backing";

export interface ComparisonRect {
  x: number;
  y: number;
  width: number;
  height: number;
}

export interface ComparisonRegionLayout {
  key: ComparisonRegionKey;
  rect: ComparisonRect;
  contentRect: ComparisonRect;
  transactionCount: number;
  entries: readonly ComparedTransaction[];
  differingWitnessBits: Uint8Array | null;
  columns: number;
  rows: number;
  cellWidth: number;
  cellHeight: number;
  gap: number;
}

export interface ComparisonGlyph {
  txid: string;
  regionKey: ComparisonRegionKey;
  rect: ComparisonRect;
  differingWitnessVariant: boolean;
}

export interface ComparisonLayout {
  width: number;
  height: number;
  regions: ComparisonRegionLayout[];
}

export interface ComparisonGeometry {
  comparison: CurrentComparison;
  width: number;
  height: number;
  pixelRatio: number;
  layout: ComparisonLayout;
}

export interface ComparisonCanvasRenderResult {
  geometry: ComparisonGeometry;
  reusedGeometry: boolean;
}

export type ComparisonPaintBatch =
  | {
      kind: "population";
      region: ComparisonRegionLayout;
      start: number;
      end: number;
    }
  | {
      kind: "witness";
      region: ComparisonRegionLayout;
      start: number;
      end: number;
    };

export type ComparisonHit =
  | { kind: "transaction"; glyph: ComparisonGlyph }
  | { kind: "region"; region: ComparisonRegionLayout }
  | null;

const REGION_ORDER: readonly ComparisonRegionKey[] = [
  "left_only",
  "common",
  "right_only",
];
const OUTER_INSET = 10;
const REGION_GAP = 5;
const LABEL_HEIGHT = 48;
export const COMPARISON_PAINT_BATCH_SIZE = 1_500;

const contains = (rect: ComparisonRect, x: number, y: number): boolean =>
  x >= rect.x &&
  y >= rect.y &&
  x <= rect.x + rect.width &&
  y <= rect.y + rect.height;

const configureRegionGrid = (
  entries: readonly ComparedTransaction[],
  contentRect: ComparisonRect,
): Pick<
  ComparisonRegionLayout,
  "entries" | "columns" | "rows" | "cellWidth" | "cellHeight" | "gap"
> => {
  if (
    entries.length === 0 ||
    contentRect.width <= 0 ||
    contentRect.height <= 0
  ) {
    return {
      entries,
      columns: 0,
      rows: 0,
      cellWidth: 0,
      cellHeight: 0,
      gap: 0,
    };
  }
  const aspect = contentRect.width / contentRect.height;
  const columns = Math.max(1, Math.ceil(Math.sqrt(entries.length * aspect)));
  const rows = Math.max(1, Math.ceil(entries.length / columns));
  const cellWidth = contentRect.width / columns;
  const cellHeight = contentRect.height / rows;
  const gap = Math.min(0.65, cellWidth * 0.08, cellHeight * 0.08);
  return { entries, columns, rows, cellWidth, cellHeight, gap };
};

const glyphRect = (
  region: ComparisonRegionLayout,
  index: number,
): ComparisonRect => {
  const column = index % region.columns;
  const row = Math.floor(index / region.columns);
  return {
    x: region.contentRect.x + column * region.cellWidth + region.gap,
    y: region.contentRect.y + row * region.cellHeight + region.gap,
    width: Math.max(0.2, region.cellWidth - region.gap * 2),
    height: Math.max(0.2, region.cellHeight - region.gap * 2),
  };
};

const glyphAtPoint = (
  region: ComparisonRegionLayout,
  x: number,
  y: number,
): ComparisonGlyph | null => {
  if (region.columns === 0 || !contains(region.contentRect, x, y)) return null;
  const column = Math.floor((x - region.contentRect.x) / region.cellWidth);
  const row = Math.floor((y - region.contentRect.y) / region.cellHeight);
  const index = row * region.columns + column;
  const entry = region.entries[index];
  if (entry === undefined) return null;
  const rect = glyphRect(region, index);
  return contains(rect, x, y)
    ? {
        txid: entry.txid,
        regionKey: region.key,
        rect,
        differingWitnessVariant: region.differingWitnessBits?.[index] === 1,
      }
    : null;
};

const entryIndex = (
  entries: readonly ComparedTransaction[],
  txid: string,
): number | null => {
  let low = 0;
  let high = entries.length;
  while (low < high) {
    const middle = low + Math.floor((high - low) / 2);
    const candidate = entries[middle];
    if (candidate === undefined) return null;
    if (candidate.txid < txid) low = middle + 1;
    else high = middle;
  }
  return entries[low]?.txid === txid ? low : null;
};

export const createComparisonLayout = (
  comparison: CurrentComparison,
  width: number,
  height: number,
): ComparisonLayout => {
  const safeWidth = Math.max(1, width);
  const safeHeight = Math.max(1, height);
  const innerWidth = Math.max(
    1,
    safeWidth - OUTER_INSET * 2 - REGION_GAP * (REGION_ORDER.length - 1),
  );
  const innerHeight = Math.max(1, safeHeight - OUTER_INSET * 2);
  const minimumWidth = Math.min(96, innerWidth / (REGION_ORDER.length * 2));
  const distributedWidth = Math.max(
    0,
    innerWidth - minimumWidth * REGION_ORDER.length,
  );
  const weights = REGION_ORDER.map((key) =>
    Math.cbrt(Math.max(1, comparisonRegionEntries(comparison, key).length)),
  );
  const totalWeight = weights.reduce((total, weight) => total + weight, 0);
  const regions: ComparisonRegionLayout[] = [];
  let x = OUTER_INSET;

  for (const [index, key] of REGION_ORDER.entries()) {
    const regionWidth =
      index === REGION_ORDER.length - 1
        ? safeWidth - OUTER_INSET - x
        : minimumWidth +
          distributedWidth * ((weights[index] ?? 0) / totalWeight);
    const rect = {
      x,
      y: OUTER_INSET,
      width: Math.max(1, regionWidth),
      height: innerHeight,
    };
    const labelHeight = Math.min(LABEL_HEIGHT, Math.max(0, rect.height - 1));
    const entries = comparisonRegionEntries(comparison, key);
    const contentRect = {
      x: rect.x + 4,
      y: rect.y + labelHeight,
      width: Math.max(0, rect.width - 8),
      height: Math.max(0, rect.height - labelHeight - 4),
    };
    regions.push({
      key,
      rect,
      contentRect,
      transactionCount: entries.length,
      differingWitnessBits:
        key === "common" ? comparison.common_differing_wtxids : null,
      ...configureRegionGrid(entries, contentRect),
    });
    x += regionWidth + REGION_GAP;
  }

  return {
    width: safeWidth,
    height: safeHeight,
    regions,
  };
};

export const hitTestComparison = (
  layout: ComparisonLayout,
  x: number,
  y: number,
): ComparisonHit => {
  for (const region of layout.regions) {
    const glyph = glyphAtPoint(region, x, y);
    if (glyph !== null) return { kind: "transaction", glyph };
  }
  const region = layout.regions.find(({ rect }) => contains(rect, x, y));
  return region === undefined ? null : { kind: "region", region };
};

const REGION_COLOR: Record<ComparisonRegionKey, string> = {
  left_only: "#7f9be2",
  common: "#53d9d4",
  right_only: "#d97ab8",
};

const regionTitle = (
  comparison: CurrentComparison,
  key: ComparisonRegionKey,
): string => {
  if (key === "common") {
    return "Present in both";
  }
  return key === "left_only"
    ? `Observed only in ${comparison.left.snapshot.source_label} snapshot`
    : `Observed only in ${comparison.right.snapshot.source_label} snapshot`;
};

const paintComparisonHeaders = (
  context: CanvasRenderingContext2D,
  layout: ComparisonLayout,
  comparison: CurrentComparison,
  selectedRegion: ComparisonRegionKey,
): void => {
  context.clearRect(0, 0, layout.width, layout.height);
  context.textBaseline = "middle";
  for (const region of layout.regions) {
    const color = REGION_COLOR[region.key];
    context.fillStyle = "#09151d";
    context.fillRect(
      region.rect.x,
      region.rect.y,
      region.rect.width,
      region.rect.height,
    );
    context.strokeStyle = region.key === selectedRegion ? color : "#314854";
    context.lineWidth = region.key === selectedRegion ? 2 : 1;
    context.strokeRect(
      region.rect.x + 0.5,
      region.rect.y + 0.5,
      Math.max(0, region.rect.width - 1),
      Math.max(0, region.rect.height - 1),
    );
    context.fillStyle = color;
    context.font =
      '700 11px "SFMono-Regular", Consolas, "Liberation Mono", monospace';
    context.fillText(
      regionTitle(comparison, region.key),
      region.rect.x + 8,
      region.rect.y + 17,
      Math.max(0, region.rect.width - 16),
    );
    context.fillStyle = "#91a4ae";
    context.font =
      '600 10px "SFMono-Regular", Consolas, "Liberation Mono", monospace';
    context.fillText(
      `${region.transactionCount.toLocaleString()} txids`,
      region.rect.x + 8,
      region.rect.y + 34,
      Math.max(0, region.rect.width - 16),
    );
  }
};

export const comparisonPaintBatches = (
  layout: ComparisonLayout,
  batchSize = COMPARISON_PAINT_BATCH_SIZE,
): ComparisonPaintBatch[] => {
  if (!Number.isSafeInteger(batchSize) || batchSize <= 0) {
    throw new RangeError("Comparison paint batch size must be positive");
  }
  const batches: ComparisonPaintBatch[] = [];
  for (const region of layout.regions) {
    for (let start = 0; start < region.transactionCount; start += batchSize) {
      batches.push({
        kind: "population",
        region,
        start,
        end: Math.min(start + batchSize, region.transactionCount),
      });
    }
  }
  const common = layout.regions.find(({ key }) => key === "common");
  if (common !== undefined) {
    for (let start = 0; start < common.transactionCount; start += batchSize) {
      batches.push({
        kind: "witness",
        region: common,
        start,
        end: Math.min(start + batchSize, common.transactionCount),
      });
    }
  }
  return batches;
};

const paintComparisonBatch = (
  context: CanvasRenderingContext2D,
  batch: ComparisonPaintBatch,
  selectedRegion: ComparisonRegionKey,
  policySide: ComparisonSide,
  policyFilter: ComparisonPolicyFilter,
): void => {
  const { region } = batch;
  if (batch.kind === "population") {
    context.fillStyle = REGION_COLOR[region.key];
    if (region.key !== selectedRegion || policyFilter.kind === "all") {
      context.beginPath();
      context.globalAlpha = region.key === selectedRegion ? 0.9 : 0.52;
      for (let index = batch.start; index < batch.end; index += 1) {
        const rect = glyphRect(region, index);
        context.rect(rect.x, rect.y, rect.width, rect.height);
      }
      context.fill();
      return;
    }

    context.beginPath();
    context.globalAlpha = 0.11;
    for (let index = batch.start; index < batch.end; index += 1) {
      const rect = glyphRect(region, index);
      context.rect(rect.x, rect.y, rect.width, rect.height);
    }
    context.fill();

    context.beginPath();
    context.globalAlpha = 0.96;
    const effectiveSide = policySideForRegion(region.key, policySide);
    for (let index = batch.start; index < batch.end; index += 1) {
      const entry = region.entries[index];
      const transaction =
        entry === undefined ? null : sourceEntry(entry, effectiveSide);
      if (
        transaction !== null &&
        comparisonPolicyFilterMatches(transaction, policyFilter)
      ) {
        const rect = glyphRect(region, index);
        context.rect(rect.x, rect.y, rect.width, rect.height);
      }
    }
    context.fill();
    return;
  }
  context.beginPath();
  context.globalAlpha = 1;
  context.strokeStyle = "#e1aa4b";
  context.lineWidth = 1;
  const effectiveSide = policySideForRegion(region.key, policySide);
  for (let index = batch.start; index < batch.end; index += 1) {
    if (region.differingWitnessBits?.[index] !== 1) {
      continue;
    }
    const entry = region.entries[index];
    const transaction =
      entry === undefined ? null : sourceEntry(entry, effectiveSide);
    if (
      region.key === selectedRegion &&
      policyFilter.kind !== "all" &&
      (transaction === null ||
        !comparisonPolicyFilterMatches(transaction, policyFilter))
    ) {
      continue;
    }
    const rect = glyphRect(region, index);
    context.rect(rect.x, rect.y, rect.width, rect.height);
  }
  context.stroke();
};

export const paintActiveComparisonTransaction = (
  context: CanvasRenderingContext2D,
  layout: ComparisonLayout,
  activeTransactionId: string | null,
): void => {
  context.globalAlpha = 1;
  if (activeTransactionId !== null) {
    for (const region of layout.regions) {
      const index = entryIndex(region.entries, activeTransactionId);
      if (index === null) continue;
      const rect = glyphRect(region, index);
      context.strokeStyle = "#f5fbff";
      context.lineWidth = 2;
      context.strokeRect(rect.x, rect.y, rect.width, rect.height);
      break;
    }
  }
};

export const paintComparison = (
  context: CanvasRenderingContext2D,
  layout: ComparisonLayout,
  comparison: CurrentComparison,
  selectedRegion: ComparisonRegionKey,
  policySide: ComparisonSide,
  policyFilter: ComparisonPolicyFilter,
  activeTransactionId: string | null = null,
): void => {
  paintComparisonHeaders(context, layout, comparison, selectedRegion);
  for (const batch of comparisonPaintBatches(layout, Number.MAX_SAFE_INTEGER)) {
    paintComparisonBatch(
      context,
      batch,
      selectedRegion,
      policySide,
      policyFilter,
    );
  }
  paintActiveComparisonTransaction(context, layout, activeTransactionId);
};

const runInAnimationFrame = (
  work: () => void,
  signal: AbortSignal,
): Promise<void> =>
  new Promise((resolve, reject) => {
    signal.throwIfAborted();
    const abort = (): void => {
      window.cancelAnimationFrame(frame);
      reject(signal.reason);
    };
    const callback = function comparisonPaintFrame() {
      signal.removeEventListener("abort", abort);
      try {
        signal.throwIfAborted();
        work();
        resolve();
      } catch (error) {
        reject(error);
      }
    };
    Object.assign(callback, { __atlasPerfLabel: "comparison-paint" });
    const frame = window.requestAnimationFrame(callback);
    signal.addEventListener("abort", abort, { once: true });
  });

export const resolveComparisonGeometry = (
  comparison: CurrentComparison,
  width: number,
  height: number,
  pixelRatio: number,
  cached: ComparisonGeometry | null,
): ComparisonCanvasRenderResult => {
  const safeWidth = Math.max(1, Math.round(width));
  const safeHeight = Math.max(1, Math.round(height));
  const safePixelRatio = Math.max(1, pixelRatio);
  if (
    cached !== null &&
    cached.comparison === comparison &&
    cached.width === safeWidth &&
    cached.height === safeHeight &&
    cached.pixelRatio === safePixelRatio
  ) {
    return { geometry: cached, reusedGeometry: true };
  }
  return {
    geometry: {
      comparison,
      width: safeWidth,
      height: safeHeight,
      pixelRatio: safePixelRatio,
      layout: createComparisonLayout(comparison, safeWidth, safeHeight),
    },
    reusedGeometry: false,
  };
};

export const renderComparisonCanvas = (
  canvas: HTMLCanvasElement,
  comparison: CurrentComparison,
  selectedRegion: ComparisonRegionKey,
  policySide: ComparisonSide,
  policyFilter: ComparisonPolicyFilter,
  activeTransactionId: string | null,
  cached: ComparisonGeometry | null,
): ComparisonCanvasRenderResult => {
  const { context, width, height, pixelRatio } = prepareCanvasBacking(
    canvas,
    "whole-pixel",
  );
  const result = resolveComparisonGeometry(
    comparison,
    width,
    height,
    pixelRatio,
    cached,
  );
  paintComparison(
    context,
    result.geometry.layout,
    comparison,
    selectedRegion,
    policySide,
    policyFilter,
    activeTransactionId,
  );
  return result;
};

export const renderComparisonCanvasProgressively = async (
  canvas: HTMLCanvasElement,
  comparison: CurrentComparison,
  selectedRegion: ComparisonRegionKey,
  policySide: ComparisonSide,
  policyFilter: ComparisonPolicyFilter,
  activeTransactionId: string | null,
  cached: ComparisonGeometry | null,
  signal: AbortSignal,
): Promise<ComparisonCanvasRenderResult> => {
  signal.throwIfAborted();
  const { context, width, height, pixelRatio } = prepareCanvasBacking(
    canvas,
    "whole-pixel",
  );
  const result = resolveComparisonGeometry(
    comparison,
    width,
    height,
    pixelRatio,
    cached,
  );
  await runInAnimationFrame(
    () =>
      paintComparisonHeaders(
        context,
        result.geometry.layout,
        comparison,
        selectedRegion,
      ),
    signal,
  );
  for (const batch of comparisonPaintBatches(result.geometry.layout)) {
    await runInAnimationFrame(
      () =>
        paintComparisonBatch(
          context,
          batch,
          selectedRegion,
          policySide,
          policyFilter,
        ),
      signal,
    );
  }
  await runInAnimationFrame(
    () =>
      paintActiveComparisonTransaction(
        context,
        result.geometry.layout,
        activeTransactionId,
      ),
    signal,
  );
  return result;
};
