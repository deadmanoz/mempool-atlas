import {
  comparisonRegionEntries,
  type ComparedTransaction,
  type ComparisonRegionKey,
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
  glyphs: ComparisonGlyph[];
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

const contains = (rect: ComparisonRect, x: number, y: number): boolean =>
  x >= rect.x &&
  y >= rect.y &&
  x <= rect.x + rect.width &&
  y <= rect.y + rect.height;

const packRegion = (
  entries: readonly ComparedTransaction[],
  region: ComparisonRegionLayout,
): ComparisonGlyph[] => {
  if (
    entries.length === 0 ||
    region.contentRect.width <= 0 ||
    region.contentRect.height <= 0
  ) {
    return [];
  }
  const aspect = region.contentRect.width / region.contentRect.height;
  const columns = Math.max(1, Math.ceil(Math.sqrt(entries.length * aspect)));
  const rows = Math.max(1, Math.ceil(entries.length / columns));
  const cellWidth = region.contentRect.width / columns;
  const cellHeight = region.contentRect.height / rows;
  const gap = Math.min(0.65, cellWidth * 0.08, cellHeight * 0.08);

  return entries.map((entry, index) => {
    const column = index % columns;
    const row = Math.floor(index / columns);
    return {
      txid: entry.txid,
      regionKey: region.key,
      rect: {
        x: region.contentRect.x + column * cellWidth + gap,
        y: region.contentRect.y + row * cellHeight + gap,
        width: Math.max(0.2, cellWidth - gap * 2),
        height: Math.max(0.2, cellHeight - gap * 2),
      },
      differingWitnessVariant: entry.same_wtxid === false,
    };
  });
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
    regions.push({
      key,
      rect,
      contentRect: {
        x: rect.x + 4,
        y: rect.y + labelHeight,
        width: Math.max(0, rect.width - 8),
        height: Math.max(0, rect.height - labelHeight - 4),
      },
      transactionCount: comparisonRegionEntries(comparison, key).length,
    });
    x += regionWidth + REGION_GAP;
  }

  return {
    width: safeWidth,
    height: safeHeight,
    regions,
    glyphs: regions.flatMap((region) =>
      packRegion(comparisonRegionEntries(comparison, region.key), region),
    ),
  };
};

export const hitTestComparison = (
  layout: ComparisonLayout,
  x: number,
  y: number,
): ComparisonHit => {
  const glyph = layout.glyphs.find(({ rect }) => contains(rect, x, y));
  if (glyph !== undefined) {
    return { kind: "transaction", glyph };
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

export const paintComparison = (
  context: CanvasRenderingContext2D,
  layout: ComparisonLayout,
  comparison: CurrentComparison,
  selectedRegion: ComparisonRegionKey,
  activeTransactionId: string | null = null,
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

  for (const region of layout.regions) {
    context.fillStyle = REGION_COLOR[region.key];
    context.globalAlpha = region.key === selectedRegion ? 0.9 : 0.52;
    for (const glyph of layout.glyphs) {
      if (glyph.regionKey === region.key) {
        context.fillRect(
          glyph.rect.x,
          glyph.rect.y,
          glyph.rect.width,
          glyph.rect.height,
        );
      }
    }
  }
  context.globalAlpha = 1;
  context.strokeStyle = "#e1aa4b";
  context.lineWidth = 1;
  for (const glyph of layout.glyphs) {
    if (glyph.differingWitnessVariant) {
      context.strokeRect(
        glyph.rect.x,
        glyph.rect.y,
        glyph.rect.width,
        glyph.rect.height,
      );
    }
  }
  if (activeTransactionId !== null) {
    const activeGlyph = layout.glyphs.find(
      ({ txid }) => txid === activeTransactionId,
    );
    if (activeGlyph !== undefined) {
      context.strokeStyle = "#f5fbff";
      context.lineWidth = 2;
      context.strokeRect(
        activeGlyph.rect.x,
        activeGlyph.rect.y,
        activeGlyph.rect.width,
        activeGlyph.rect.height,
      );
    }
  }
};

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
    activeTransactionId,
  );
  return result;
};
