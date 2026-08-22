import { prepareCanvasBacking } from "./canvas-backing";
import {
  findBucketTerrainGlyph,
  packBucketTerrainGlyphs,
  packBucketTerrainGlyphsCooperatively,
  type BucketTerrainGlyphCollection,
} from "./bucket-terrain-glyphs";
import {
  yieldCooperatively,
  type CooperativeWorkOptions,
} from "./cooperative-work";
import {
  transactionViewIdentityAt,
  transactionViewVsizeAt,
} from "./transaction-view";
import type { MempoolTransaction } from "./types";

export type BucketTerrainMode = "count" | "vsize";

export interface BucketTerrainRect {
  x: number;
  y: number;
  width: number;
  height: number;
}

export interface BucketTerrainRegionGroup<
  SectionKey extends string,
  RegionKey extends string,
  Signature,
> {
  key: RegionKey;
  sectionKey: SectionKey;
  signature: Signature;
  transactions: MempoolTransaction[];
}

export interface BucketTerrainSectionGroup<
  SectionKey extends string,
  RegionKey extends string,
  Signature,
> {
  key: SectionKey;
  transactions: MempoolTransaction[];
  regions: BucketTerrainRegionGroup<SectionKey, RegionKey, Signature>[];
  nested: boolean;
}

export interface BucketTerrainSection<SectionKey extends string> {
  key: SectionKey;
  rect: BucketTerrainRect;
  contentRect: BucketTerrainRect;
  transactionCount: number;
  totalVsize: number;
  weight: number;
  labelHeight: number;
}

export interface BucketTerrainRegion<
  SectionKey extends string,
  RegionKey extends string,
  Signature,
> {
  key: RegionKey;
  sectionKey: SectionKey;
  signature: Signature;
  rect: BucketTerrainRect;
  contentRect: BucketTerrainRect;
  transactionCount: number;
  totalVsize: number;
  weight: number;
  labelHeight: number;
}

export interface BucketTerrainGlyph<
  SectionKey extends string,
  RegionKey extends string,
> {
  txid: string;
  sourceRow: number;
  regionKey: RegionKey;
  sectionKey: SectionKey;
  rect: BucketTerrainRect;
  vsize: number;
}

export interface BucketTerrainLayout<
  SectionKey extends string,
  RegionKey extends string,
  Signature,
> {
  width: number;
  height: number;
  mode: BucketTerrainMode;
  sections: BucketTerrainSection<SectionKey>[];
  regions: BucketTerrainRegion<SectionKey, RegionKey, Signature>[];
  glyphs: BucketTerrainGlyphCollection<SectionKey, RegionKey>;
}

export type BucketTerrainHit<
  SectionKey extends string,
  RegionKey extends string,
  Signature,
> =
  | {
      kind: "transaction";
      glyph: BucketTerrainGlyph<SectionKey, RegionKey>;
    }
  | {
      kind: "region";
      region: BucketTerrainRegion<SectionKey, RegionKey, Signature>;
    }
  | null;

export interface BucketTerrainPaint<
  SectionKey extends string,
  RegionKey extends string,
  Signature,
> {
  color: (
    region: BucketTerrainRegion<SectionKey, RegionKey, Signature>,
  ) => string;
  selected: (
    region: BucketTerrainRegion<SectionKey, RegionKey, Signature>,
  ) => boolean;
  partial: (
    region: BucketTerrainRegion<SectionKey, RegionKey, Signature>,
  ) => boolean;
  glyphOpacity: (
    region: BucketTerrainRegion<SectionKey, RegionKey, Signature>,
    selected: boolean,
    glyph: BucketTerrainGlyph<SectionKey, RegionKey>,
  ) => number;
  glyphOpacityByRegion?: (
    region: BucketTerrainRegion<SectionKey, RegionKey, Signature>,
    selected: boolean,
  ) => number;
  rasterStyleKey?: string;
}

export const bucketTerrainRegionCanShowLabel = (region: {
  rect: BucketTerrainRect;
  labelHeight: number;
}): boolean =>
  region.rect.width >= 132 &&
  region.labelHeight >= 26 &&
  region.rect.width * region.rect.height >= 9_000;

const SECTION_GAP = 4;
const SECTION_LABEL_HEIGHT = 42;
const REGION_GAP = 3;
const REGION_LABEL_HEIGHT = 36;
const GLYPH_GAP = 0.32;
const MIN_CONTENT_EXTENT = 0.5;
const MAX_RASTER_PIXELS = 4_194_304;

interface BucketTerrainRasterCache {
  styleKey: string;
  pixelWidth: number;
  pixelHeight: number;
  dim: HTMLCanvasElement;
  bright: HTMLCanvasElement;
}

const rasterCacheByLayout = new WeakMap<object, BucketTerrainRasterCache>();
const stagingCanvasByCanvas = new WeakMap<
  HTMLCanvasElement,
  HTMLCanvasElement
>();

type BucketTerrainRasterPaths = Map<string, Map<number, Path2D>>;

const glyphDensity = <SectionKey extends string, RegionKey extends string>(
  glyph: BucketTerrainGlyph<SectionKey, RegionKey>,
): number => {
  const shortestSide = Math.min(glyph.rect.width, glyph.rect.height);
  return shortestSide < 0.9
    ? 0.38
    : shortestSide < 1.6
      ? 0.48
      : shortestSide < 3
        ? 0.62
        : 0.78;
};

const insetRect = (
  rect: BucketTerrainRect,
  inset: number,
): BucketTerrainRect => ({
  x: rect.x + inset,
  y: rect.y + inset,
  width: Math.max(0, rect.width - inset * 2),
  height: Math.max(0, rect.height - inset * 2),
});

const adaptiveInsetRect = (
  rect: BucketTerrainRect,
  preferredInset: number,
): BucketTerrainRect => {
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
  rect: BucketTerrainRect,
): Map<Key, BucketTerrainRect> => {
  const result = new Map<Key, BucketTerrainRect>();
  const stack: Array<{
    start: number;
    end: number;
    rect: BucketTerrainRect;
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
    const firstRect: BucketTerrainRect = splitVertically
      ? { ...current.rect, width: current.rect.width * ratio }
      : { ...current.rect, height: current.rect.height * ratio };
    const secondRect: BucketTerrainRect = splitVertically
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

const paintBucketTerrainRasterLayer = <
  SectionKey extends string,
  RegionKey extends string,
  Signature,
>(
  layer: CanvasRenderingContext2D,
  layout: BucketTerrainLayout<SectionKey, RegionKey, Signature>,
  presentation: BucketTerrainPaint<SectionKey, RegionKey, Signature>,
  pathsByRegion: BucketTerrainRasterPaths,
  selected: boolean,
): void => {
  layer.clearRect(0, 0, layout.width, layout.height);
  layer.fillStyle = "#071018";
  layer.fillRect(0, 0, layout.width, layout.height);
  for (const section of layout.sections) {
    layer.fillStyle = "#0a161e";
    layer.fillRect(
      section.rect.x,
      section.rect.y,
      section.rect.width,
      section.rect.height,
    );
    layer.strokeStyle = "#2a3b48";
    layer.lineWidth = 1;
    layer.strokeRect(
      section.rect.x,
      section.rect.y,
      section.rect.width,
      section.rect.height,
    );
  }
  for (const region of layout.regions) {
    const color = presentation.color(region);
    layer.fillStyle = color;
    layer.globalAlpha = selected ? 0.12 : 0.025;
    layer.fillRect(
      region.contentRect.x,
      region.contentRect.y,
      region.contentRect.width,
      region.contentRect.height,
    );
    layer.globalAlpha = 1;
    if (!selected) {
      layer.strokeStyle = presentation.partial(region) ? "#9a7735" : "#243744";
      layer.lineWidth = 1;
      layer.strokeRect(
        region.rect.x,
        region.rect.y,
        region.rect.width,
        region.rect.height,
      );
    }
    const pathsByDensity = pathsByRegion.get(region.key);
    if (pathsByDensity === undefined) continue;
    const opacity = presentation.glyphOpacityByRegion!(region, selected);
    layer.fillStyle = color;
    for (const [density, path] of pathsByDensity) {
      layer.globalAlpha = Math.min(selected ? 0.86 : 0.68, opacity * density);
      layer.fill(path);
    }
  }
  layer.globalAlpha = 1;
};

interface PopulationMeasurement {
  count: number;
  totalVsize: number;
  vsizes: Float64Array;
}

const measurePopulation = (
  transactions: readonly MempoolTransaction[],
): PopulationMeasurement => {
  const vsizes = new Float64Array(transactions.length);
  let totalVsize = 0;
  for (let index = 0; index < transactions.length; index += 1) {
    const vsize = transactionViewVsizeAt(transactions, index) ?? 0;
    vsizes[index] = vsize;
    totalVsize += vsize;
  }
  return { count: transactions.length, totalVsize, vsizes };
};

const measuredWeight = (
  measurement: PopulationMeasurement,
  mode: BucketTerrainMode,
): number => (mode === "count" ? measurement.count : measurement.totalVsize);

const transactionGlyph = <SectionKey extends string, RegionKey extends string>(
  identity: { sourceRow: number; txid: string },
  vsize: number,
  rect: BucketTerrainRect,
  regionKey: RegionKey,
  sectionKey: SectionKey,
): BucketTerrainGlyph<SectionKey, RegionKey> => {
  const adaptiveGap = Math.min(
    GLYPH_GAP,
    rect.width * 0.035,
    rect.height * 0.035,
  );
  return {
    txid: identity.txid,
    sourceRow: identity.sourceRow,
    regionKey,
    sectionKey,
    rect: insetRect(rect, adaptiveGap),
    vsize,
  };
};

const packCountTransactions = <
  SectionKey extends string,
  RegionKey extends string,
>(
  transactions: readonly MempoolTransaction[],
  vsizes: Float64Array,
  rect: BucketTerrainRect,
  regionKey: RegionKey,
  sectionKey: SectionKey,
): BucketTerrainGlyph<SectionKey, RegionKey>[] => {
  const aspectRatio = rect.width / Math.max(rect.height, 1);
  const columns = Math.min(
    transactions.length,
    Math.max(1, Math.ceil(Math.sqrt(transactions.length * aspectRatio))),
  );
  const rows = Math.ceil(transactions.length / columns);
  const completeRowHeight = (rect.height * columns) / transactions.length;
  const finalRowCount = transactions.length - columns * (rows - 1);
  const finalRowHeight = (rect.height * finalRowCount) / transactions.length;
  const glyphs: BucketTerrainGlyph<SectionKey, RegionKey>[] = [];
  for (let index = 0; index < transactions.length; index += 1) {
    const identity = transactionViewIdentityAt(transactions, index);
    if (identity === null) continue;
    const column = index % columns;
    const row = Math.floor(index / columns);
    const finalRow = row === rows - 1;
    const rowColumns = finalRow ? finalRowCount : columns;
    const cellWidth = rect.width / rowColumns;
    const cellHeight = finalRow ? finalRowHeight : completeRowHeight;
    glyphs.push(
      transactionGlyph(
        identity,
        vsizes[index] ?? 0,
        {
          x: rect.x + column * cellWidth,
          y: rect.y + row * completeRowHeight,
          width: cellWidth,
          height: cellHeight,
        },
        regionKey,
        sectionKey,
      ),
    );
  }
  return glyphs;
};

const packTransactions = <SectionKey extends string, RegionKey extends string>(
  transactions: readonly MempoolTransaction[],
  vsizes: Float64Array,
  rect: BucketTerrainRect,
  regionKey: RegionKey,
  sectionKey: SectionKey,
  mode: BucketTerrainMode,
): BucketTerrainGlyph<SectionKey, RegionKey>[] => {
  if (transactions.length === 0 || rect.width <= 0 || rect.height <= 0) {
    return [];
  }
  if (mode === "count") {
    return packCountTransactions(
      transactions,
      vsizes,
      rect,
      regionKey,
      sectionKey,
    );
  }
  const prefix = [0];
  for (const weight of vsizes) {
    prefix.push((prefix.at(-1) ?? 0) + weight);
  }
  const glyphs: BucketTerrainGlyph<SectionKey, RegionKey>[] = [];
  const stack: Array<{
    start: number;
    end: number;
    rect: BucketTerrainRect;
  }> = [{ start: 0, end: transactions.length, rect }];

  while (stack.length > 0) {
    const current = stack.pop();
    if (current === undefined || current.start >= current.end) {
      continue;
    }
    if (current.end - current.start === 1) {
      const identity = transactionViewIdentityAt(transactions, current.start);
      if (identity !== null) {
        glyphs.push(
          transactionGlyph(
            identity,
            vsizes[current.start] ?? 0,
            current.rect,
            regionKey,
            sectionKey,
          ),
        );
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
    const firstRect: BucketTerrainRect = splitVertically
      ? { ...current.rect, width: current.rect.width * ratio }
      : { ...current.rect, height: current.rect.height * ratio };
    const secondRect: BucketTerrainRect = splitVertically
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

interface MeasuredRegion<
  SectionKey extends string,
  RegionKey extends string,
  Signature,
> {
  group: BucketTerrainRegionGroup<SectionKey, RegionKey, Signature>;
  measurement: PopulationMeasurement;
}

interface MeasuredSection<
  SectionKey extends string,
  RegionKey extends string,
  Signature,
> {
  group: BucketTerrainSectionGroup<SectionKey, RegionKey, Signature>;
  regions: MeasuredRegion<SectionKey, RegionKey, Signature>[];
  count: number;
  totalVsize: number;
}

export const createBucketTerrainLayout = <
  SectionKey extends string,
  RegionKey extends string,
  Signature,
>(
  groups: readonly BucketTerrainSectionGroup<
    SectionKey,
    RegionKey,
    Signature
  >[],
  width: number,
  height: number,
  mode: BucketTerrainMode,
): BucketTerrainLayout<SectionKey, RegionKey, Signature> => {
  const safeWidth = Math.max(1, width);
  const safeHeight = Math.max(1, height);
  const measuredSections: MeasuredSection<SectionKey, RegionKey, Signature>[] =
    groups.map((group) => {
      const regions = group.regions.map((region) => ({
        group: region,
        measurement: measurePopulation(region.transactions),
      }));
      return {
        group,
        regions,
        count: regions.reduce(
          (total, region) => total + region.measurement.count,
          0,
        ),
        totalVsize: regions.reduce(
          (total, region) => total + region.measurement.totalVsize,
          0,
        ),
      };
    });
  const sectionRects = splitWeightedRegions(
    measuredSections.map(({ group, count, totalVsize }) => ({
      key: group.key,
      weight: mode === "count" ? count : totalVsize,
    })),
    { x: 0, y: 0, width: safeWidth, height: safeHeight },
  );

  const sections: BucketTerrainSection<SectionKey>[] = [];
  const regions: BucketTerrainRegion<SectionKey, RegionKey, Signature>[] = [];
  const glyphs: BucketTerrainGlyph<SectionKey, RegionKey>[] = [];

  for (const measuredSection of measuredSections) {
    const { group } = measuredSection;
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
    sections.push({
      key: group.key,
      rect,
      contentRect,
      transactionCount: measuredSection.count,
      totalVsize: measuredSection.totalVsize,
      weight:
        mode === "count" ? measuredSection.count : measuredSection.totalVsize,
      labelHeight: sectionLabelHeight,
    });

    const regionWeights = measuredSection.regions.map(
      ({ group, measurement }) => ({
        key: group.key,
        weight: measuredWeight(measurement, mode),
      }),
    );
    const regionRects = group.nested
      ? splitWeightedRegions(regionWeights, contentRect)
      : new Map<RegionKey, BucketTerrainRect>([
          [group.regions[0]?.key as RegionKey, contentRect],
        ]);

    for (const measuredRegion of measuredSection.regions) {
      const { group: regionGroup, measurement } = measuredRegion;
      const rawRegionRect = regionRects.get(regionGroup.key) ?? contentRect;
      const regionRect = group.nested
        ? adaptiveInsetRect(rawRegionRect, REGION_GAP / 2)
        : rawRegionRect;
      const regionLabelHeight = group.nested
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
      regions.push({
        key: regionGroup.key,
        sectionKey: group.key,
        signature: regionGroup.signature,
        rect: regionRect,
        contentRect: regionContentRect,
        transactionCount: measurement.count,
        totalVsize: measurement.totalVsize,
        weight: measuredWeight(measurement, mode),
        labelHeight: regionLabelHeight,
      });
      for (const glyph of packTransactions(
        regionGroup.transactions,
        measurement.vsizes,
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
    glyphs: packBucketTerrainGlyphs(glyphs),
  };
};

const cooperativePopulationMeasurement = async (
  transactions: readonly MempoolTransaction[],
  options: CooperativeWorkOptions,
): Promise<PopulationMeasurement> => {
  const batchSize = cooperativeBatchSize(options);
  const vsizes = new Float64Array(transactions.length);
  let totalVsize = 0;
  for (let start = 0; start < transactions.length; start += batchSize) {
    const end = Math.min(transactions.length, start + batchSize);
    for (let index = start; index < end; index += 1) {
      const vsize = transactionViewVsizeAt(transactions, index) ?? 0;
      vsizes[index] = vsize;
      totalVsize += vsize;
    }
    if (end < transactions.length) await yieldCooperatively(options);
  }
  return { count: transactions.length, totalVsize, vsizes };
};

const packCountTransactionsCooperatively = async <
  SectionKey extends string,
  RegionKey extends string,
>(
  transactions: readonly MempoolTransaction[],
  vsizes: Float64Array,
  rect: BucketTerrainRect,
  regionKey: RegionKey,
  sectionKey: SectionKey,
  options: CooperativeWorkOptions,
): Promise<BucketTerrainGlyph<SectionKey, RegionKey>[]> => {
  const batchSize = cooperativeBatchSize(options);
  const aspectRatio = rect.width / Math.max(rect.height, 1);
  const columns = Math.min(
    transactions.length,
    Math.max(1, Math.ceil(Math.sqrt(transactions.length * aspectRatio))),
  );
  const rows = Math.ceil(transactions.length / columns);
  const completeRowHeight = (rect.height * columns) / transactions.length;
  const finalRowCount = transactions.length - columns * (rows - 1);
  const finalRowHeight = (rect.height * finalRowCount) / transactions.length;
  const glyphs: BucketTerrainGlyph<SectionKey, RegionKey>[] = [];
  for (let start = 0; start < transactions.length; start += batchSize) {
    const end = Math.min(transactions.length, start + batchSize);
    for (let index = start; index < end; index += 1) {
      const identity = transactionViewIdentityAt(transactions, index);
      if (identity === null) continue;
      const column = index % columns;
      const row = Math.floor(index / columns);
      const finalRow = row === rows - 1;
      const rowColumns = finalRow ? finalRowCount : columns;
      const cellWidth = rect.width / rowColumns;
      const cellHeight = finalRow ? finalRowHeight : completeRowHeight;
      glyphs.push(
        transactionGlyph(
          identity,
          vsizes[index] ?? 0,
          {
            x: rect.x + column * cellWidth,
            y: rect.y + row * completeRowHeight,
            width: cellWidth,
            height: cellHeight,
          },
          regionKey,
          sectionKey,
        ),
      );
    }
    if (end < transactions.length) await yieldCooperatively(options);
  }
  return glyphs;
};

const packTransactionsCooperatively = async <
  SectionKey extends string,
  RegionKey extends string,
>(
  transactions: readonly MempoolTransaction[],
  vsizes: Float64Array,
  rect: BucketTerrainRect,
  regionKey: RegionKey,
  sectionKey: SectionKey,
  mode: BucketTerrainMode,
  options: CooperativeWorkOptions,
): Promise<BucketTerrainGlyph<SectionKey, RegionKey>[]> => {
  if (transactions.length === 0 || rect.width <= 0 || rect.height <= 0) {
    return [];
  }
  if (mode === "count") {
    return packCountTransactionsCooperatively(
      transactions,
      vsizes,
      rect,
      regionKey,
      sectionKey,
      options,
    );
  }
  const batchSize = cooperativeBatchSize(options);
  const prefix = new Float64Array(vsizes.length + 1);
  for (let start = 0; start < vsizes.length; start += batchSize) {
    const end = Math.min(vsizes.length, start + batchSize);
    for (let index = start; index < end; index += 1) {
      prefix[index + 1] = (prefix[index] ?? 0) + (vsizes[index] ?? 0);
    }
    if (end < vsizes.length) await yieldCooperatively(options);
  }
  const glyphs: BucketTerrainGlyph<SectionKey, RegionKey>[] = [];
  const stack: Array<{
    start: number;
    end: number;
    rect: BucketTerrainRect;
  }> = [{ start: 0, end: transactions.length, rect }];
  let operations = 0;
  while (stack.length > 0) {
    const current = stack.pop();
    if (current === undefined || current.start >= current.end) continue;
    if (current.end - current.start === 1) {
      const identity = transactionViewIdentityAt(transactions, current.start);
      if (identity !== null) {
        glyphs.push(
          transactionGlyph(
            identity,
            vsizes[current.start] ?? 0,
            current.rect,
            regionKey,
            sectionKey,
          ),
        );
      }
    } else {
      const total = (prefix[current.end] ?? 0) - (prefix[current.start] ?? 0);
      const halfway = (prefix[current.start] ?? 0) + total / 2;
      let low = current.start + 1;
      let high = current.end - 1;
      while (low < high) {
        const middle = Math.floor((low + high) / 2);
        if ((prefix[middle] ?? 0) < halfway) low = middle + 1;
        else high = middle;
      }
      const split = Math.min(current.end - 1, Math.max(current.start + 1, low));
      const firstWeight = (prefix[split] ?? 0) - (prefix[current.start] ?? 0);
      const ratio = total === 0 ? 0.5 : firstWeight / total;
      const splitVertically = current.rect.width >= current.rect.height;
      const firstRect: BucketTerrainRect = splitVertically
        ? { ...current.rect, width: current.rect.width * ratio }
        : { ...current.rect, height: current.rect.height * ratio };
      const secondRect: BucketTerrainRect = splitVertically
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
    operations += 1;
    if (operations >= batchSize && stack.length > 0) {
      operations = 0;
      await yieldCooperatively(options);
    }
  }
  return glyphs;
};

export const createBucketTerrainLayoutCooperatively = async <
  SectionKey extends string,
  RegionKey extends string,
  Signature,
>(
  groups: readonly BucketTerrainSectionGroup<
    SectionKey,
    RegionKey,
    Signature
  >[],
  width: number,
  height: number,
  mode: BucketTerrainMode,
  options: CooperativeWorkOptions = {},
): Promise<BucketTerrainLayout<SectionKey, RegionKey, Signature>> => {
  options.signal?.throwIfAborted();
  const safeWidth = Math.max(1, width);
  const safeHeight = Math.max(1, height);
  const measuredSections: MeasuredSection<SectionKey, RegionKey, Signature>[] =
    [];
  for (const group of groups) {
    const regions: MeasuredRegion<SectionKey, RegionKey, Signature>[] = [];
    for (const region of group.regions) {
      regions.push({
        group: region,
        measurement: await cooperativePopulationMeasurement(
          region.transactions,
          options,
        ),
      });
    }
    measuredSections.push({
      group,
      regions,
      count: regions.reduce(
        (total, region) => total + region.measurement.count,
        0,
      ),
      totalVsize: regions.reduce(
        (total, region) => total + region.measurement.totalVsize,
        0,
      ),
    });
  }
  const sectionRects = splitWeightedRegions(
    measuredSections.map(({ group, count, totalVsize }) => ({
      key: group.key,
      weight: mode === "count" ? count : totalVsize,
    })),
    { x: 0, y: 0, width: safeWidth, height: safeHeight },
  );
  const sections: BucketTerrainSection<SectionKey>[] = [];
  const regions: BucketTerrainRegion<SectionKey, RegionKey, Signature>[] = [];
  const glyphs: BucketTerrainGlyph<SectionKey, RegionKey>[] = [];

  for (const measuredSection of measuredSections) {
    const { group } = measuredSection;
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
    sections.push({
      key: group.key,
      rect,
      contentRect,
      transactionCount: measuredSection.count,
      totalVsize: measuredSection.totalVsize,
      weight:
        mode === "count" ? measuredSection.count : measuredSection.totalVsize,
      labelHeight: sectionLabelHeight,
    });
    const regionWeights = measuredSection.regions.map(
      ({ group: region, measurement }) => ({
        key: region.key,
        weight: measuredWeight(measurement, mode),
      }),
    );
    const regionRects = group.nested
      ? splitWeightedRegions(regionWeights, contentRect)
      : new Map<RegionKey, BucketTerrainRect>([
          [group.regions[0]?.key as RegionKey, contentRect],
        ]);
    for (const measuredRegion of measuredSection.regions) {
      const { group: regionGroup, measurement } = measuredRegion;
      const rawRegionRect = regionRects.get(regionGroup.key) ?? contentRect;
      const regionRect = group.nested
        ? adaptiveInsetRect(rawRegionRect, REGION_GAP / 2)
        : rawRegionRect;
      const regionLabelHeight = group.nested
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
      regions.push({
        key: regionGroup.key,
        sectionKey: group.key,
        signature: regionGroup.signature,
        rect: regionRect,
        contentRect: regionContentRect,
        transactionCount: measurement.count,
        totalVsize: measurement.totalVsize,
        weight: measuredWeight(measurement, mode),
        labelHeight: regionLabelHeight,
      });
      const packedGlyphs = await packTransactionsCooperatively(
        regionGroup.transactions,
        measurement.vsizes,
        regionContentRect,
        regionGroup.key,
        group.key,
        mode,
        options,
      );
      for (const glyph of packedGlyphs) glyphs.push(glyph);
    }
  }
  options.signal?.throwIfAborted();
  return {
    width: safeWidth,
    height: safeHeight,
    mode,
    sections,
    regions,
    glyphs: await packBucketTerrainGlyphsCooperatively(glyphs, options),
  };
};

const containsPoint = (
  rect: BucketTerrainRect,
  x: number,
  y: number,
): boolean =>
  x >= rect.x &&
  x <= rect.x + rect.width &&
  y >= rect.y &&
  y <= rect.y + rect.height;

export const hitTestBucketTerrain = <
  SectionKey extends string,
  RegionKey extends string,
  Signature,
>(
  layout: BucketTerrainLayout<SectionKey, RegionKey, Signature>,
  x: number,
  y: number,
): BucketTerrainHit<SectionKey, RegionKey, Signature> => {
  const region = layout.regions.find(({ rect }) => containsPoint(rect, x, y));
  if (region === undefined) {
    return null;
  }
  const scratch = layout.glyphs.read(0);
  for (let index = 0; index < layout.glyphs.length; index += 1) {
    const glyph =
      scratch === undefined ? undefined : layout.glyphs.read(index, scratch);
    if (glyph === undefined) continue;
    if (glyph.regionKey === region.key && containsPoint(glyph.rect, x, y)) {
      return { kind: "transaction", glyph: layout.glyphs.at(index)! };
    }
  }
  return { kind: "region", region };
};

const createBucketTerrainRasterCache = <
  SectionKey extends string,
  RegionKey extends string,
  Signature,
>(
  context: CanvasRenderingContext2D,
  layout: BucketTerrainLayout<SectionKey, RegionKey, Signature>,
  presentation: BucketTerrainPaint<SectionKey, RegionKey, Signature>,
): BucketTerrainRasterCache | null => {
  const styleKey = presentation.rasterStyleKey;
  if (
    styleKey === undefined ||
    presentation.glyphOpacityByRegion === undefined ||
    typeof document === "undefined" ||
    typeof Path2D !== "function" ||
    context.canvas === undefined
  ) {
    return null;
  }
  const pixelWidth = Math.max(1, context.canvas.width);
  const pixelHeight = Math.max(1, context.canvas.height);
  // A smaller retained bitmap would visibly resample dense glyphs and region
  // outlines. The direct painter keeps native backing resolution without
  // allocating another over-cap canvas.
  if (pixelWidth * pixelHeight > MAX_RASTER_PIXELS) {
    return null;
  }
  const cached = rasterCacheByLayout.get(layout);
  if (
    cached !== undefined &&
    cached.styleKey === styleKey &&
    cached.pixelWidth === pixelWidth &&
    cached.pixelHeight === pixelHeight
  ) {
    return cached;
  }
  const createLayer = (): {
    canvas: HTMLCanvasElement;
    context: CanvasRenderingContext2D;
  } | null => {
    const canvas = document.createElement("canvas");
    canvas.width = pixelWidth;
    canvas.height = pixelHeight;
    const layerContext = canvas.getContext("2d");
    if (layerContext === null) return null;
    layerContext.setTransform(
      pixelWidth / layout.width,
      0,
      0,
      pixelHeight / layout.height,
      0,
      0,
    );
    return { canvas, context: layerContext };
  };
  const dim = createLayer();
  const bright = createLayer();
  if (dim === null || bright === null) return null;

  const regionsByKey = new Map(
    layout.regions.map((region) => [region.key, region]),
  );
  const pathsByRegion: BucketTerrainRasterPaths = new Map();
  const scratch = layout.glyphs.read(0);
  for (let index = 0; index < layout.glyphs.length; index += 1) {
    const glyph =
      scratch === undefined ? undefined : layout.glyphs.read(index, scratch);
    if (glyph === undefined) continue;
    if (!regionsByKey.has(glyph.regionKey)) continue;
    let pathsByDensity = pathsByRegion.get(glyph.regionKey);
    if (pathsByDensity === undefined) {
      pathsByDensity = new Map();
      pathsByRegion.set(glyph.regionKey, pathsByDensity);
    }
    const density = glyphDensity(glyph);
    let path = pathsByDensity.get(density);
    if (path === undefined) {
      path = new Path2D();
      pathsByDensity.set(density, path);
    }
    path.rect(glyph.rect.x, glyph.rect.y, glyph.rect.width, glyph.rect.height);
  }

  paintBucketTerrainRasterLayer(
    dim.context,
    layout,
    presentation,
    pathsByRegion,
    false,
  );
  paintBucketTerrainRasterLayer(
    bright.context,
    layout,
    presentation,
    pathsByRegion,
    true,
  );
  const created = {
    styleKey,
    pixelWidth,
    pixelHeight,
    dim: dim.canvas,
    bright: bright.canvas,
  };
  rasterCacheByLayout.set(layout, created);
  return created;
};

const createBucketTerrainRasterCacheCooperatively = async <
  SectionKey extends string,
  RegionKey extends string,
  Signature,
>(
  context: CanvasRenderingContext2D,
  layout: BucketTerrainLayout<SectionKey, RegionKey, Signature>,
  presentation: BucketTerrainPaint<SectionKey, RegionKey, Signature>,
  options: CooperativeWorkOptions,
): Promise<BucketTerrainRasterCache | null> => {
  const styleKey = presentation.rasterStyleKey;
  if (
    styleKey === undefined ||
    presentation.glyphOpacityByRegion === undefined ||
    typeof document === "undefined" ||
    typeof Path2D !== "function" ||
    context.canvas === undefined
  ) {
    return null;
  }
  const pixelWidth = Math.max(1, context.canvas.width);
  const pixelHeight = Math.max(1, context.canvas.height);
  if (pixelWidth * pixelHeight > MAX_RASTER_PIXELS) return null;
  const cached = rasterCacheByLayout.get(layout);
  if (
    cached !== undefined &&
    cached.styleKey === styleKey &&
    cached.pixelWidth === pixelWidth &&
    cached.pixelHeight === pixelHeight
  ) {
    return cached;
  }

  const createLayer = (): {
    canvas: HTMLCanvasElement;
    context: CanvasRenderingContext2D;
  } | null => {
    const canvas = document.createElement("canvas");
    canvas.width = pixelWidth;
    canvas.height = pixelHeight;
    const layerContext = canvas.getContext("2d");
    if (layerContext === null) return null;
    layerContext.setTransform(
      pixelWidth / layout.width,
      0,
      0,
      pixelHeight / layout.height,
      0,
      0,
    );
    return { canvas, context: layerContext };
  };
  const dim = createLayer();
  const bright = createLayer();
  if (dim === null || bright === null) return null;

  const batchSize = cooperativeBatchSize(options);
  const regionsByKey = new Set(layout.regions.map(({ key }) => key));
  const pathsByRegion: BucketTerrainRasterPaths = new Map();
  const scratch = layout.glyphs.read(0);
  for (let start = 0; start < layout.glyphs.length; start += batchSize) {
    const end = Math.min(layout.glyphs.length, start + batchSize);
    for (let index = start; index < end; index += 1) {
      const glyph =
        scratch === undefined ? undefined : layout.glyphs.read(index, scratch);
      if (glyph === undefined || !regionsByKey.has(glyph.regionKey)) continue;
      let pathsByDensity = pathsByRegion.get(glyph.regionKey);
      if (pathsByDensity === undefined) {
        pathsByDensity = new Map();
        pathsByRegion.set(glyph.regionKey, pathsByDensity);
      }
      const density = glyphDensity(glyph);
      let path = pathsByDensity.get(density);
      if (path === undefined) {
        path = new Path2D();
        pathsByDensity.set(density, path);
      }
      path.rect(
        glyph.rect.x,
        glyph.rect.y,
        glyph.rect.width,
        glyph.rect.height,
      );
    }
    if (end < layout.glyphs.length) await yieldCooperatively(options);
  }
  paintBucketTerrainRasterLayer(
    dim.context,
    layout,
    presentation,
    pathsByRegion,
    false,
  );
  await yieldCooperatively(options);
  paintBucketTerrainRasterLayer(
    bright.context,
    layout,
    presentation,
    pathsByRegion,
    true,
  );
  options.signal?.throwIfAborted();
  const created = {
    styleKey,
    pixelWidth,
    pixelHeight,
    dim: dim.canvas,
    bright: bright.canvas,
  };
  rasterCacheByLayout.set(layout, created);
  return created;
};

const paintSelectedTerrainGlyph = <
  SectionKey extends string,
  RegionKey extends string,
>(
  context: CanvasRenderingContext2D,
  glyph: BucketTerrainGlyph<SectionKey, RegionKey>,
): void => {
  const { x, y, width, height } = glyph.rect;
  const inset = Math.min(1.5, width / 6, height / 6);
  context.save();
  context.globalAlpha = 1;
  context.fillStyle = "#f7ff6a";
  context.shadowColor = "#f7ff6a";
  context.shadowBlur = 12;
  context.globalAlpha = 0.28;
  context.fillRect(x, y, width, height);
  context.globalAlpha = 1;
  context.strokeStyle = "#f7ff6a";
  context.lineWidth = 2.5;
  context.strokeRect(x, y, width, height);
  if (width > inset * 4 && height > inset * 4) {
    context.shadowBlur = 0;
    context.strokeStyle = "#ffffff";
    context.lineWidth = 1;
    context.strokeRect(
      x + inset,
      y + inset,
      width - inset * 2,
      height - inset * 2,
    );
  }
  context.restore();
};

const paintBucketTerrainRaster = <
  SectionKey extends string,
  RegionKey extends string,
  Signature,
>(
  context: CanvasRenderingContext2D,
  layout: BucketTerrainLayout<SectionKey, RegionKey, Signature>,
  presentation: BucketTerrainPaint<SectionKey, RegionKey, Signature>,
  raster: BucketTerrainRasterCache,
  selectedTxid: string | null,
): boolean => {
  if (
    typeof context.drawImage !== "function" ||
    typeof context.save !== "function" ||
    typeof context.restore !== "function" ||
    typeof context.beginPath !== "function" ||
    typeof context.rect !== "function" ||
    typeof context.clip !== "function"
  ) {
    return false;
  }
  context.globalAlpha = 1;
  context.drawImage(
    raster.dim,
    0,
    0,
    raster.pixelWidth,
    raster.pixelHeight,
    0,
    0,
    layout.width,
    layout.height,
  );
  const selectedRegions = layout.regions.filter(presentation.selected);
  if (selectedRegions.length > 0) {
    context.save();
    context.beginPath();
    for (const region of selectedRegions) {
      context.rect(
        region.rect.x,
        region.rect.y,
        region.rect.width,
        region.rect.height,
      );
    }
    context.clip();
    context.drawImage(
      raster.bright,
      0,
      0,
      raster.pixelWidth,
      raster.pixelHeight,
      0,
      0,
      layout.width,
      layout.height,
    );
    context.restore();
    context.strokeStyle = "#6ef2f0";
    context.lineWidth = 1.75;
    for (const region of selectedRegions) {
      context.strokeRect(
        region.rect.x,
        region.rect.y,
        region.rect.width,
        region.rect.height,
      );
    }
  }
  paintBucketTerrainSelection(context, layout, selectedTxid);
  return true;
};

export const paintBucketTerrainSelection = <
  SectionKey extends string,
  RegionKey extends string,
  Signature,
>(
  context: CanvasRenderingContext2D,
  layout: BucketTerrainLayout<SectionKey, RegionKey, Signature>,
  selectedTxid: string | null,
): boolean => {
  if (selectedTxid === null) return false;
  const selectedGlyph = findBucketTerrainGlyph(layout.glyphs, selectedTxid);
  if (selectedGlyph === undefined) return false;
  paintSelectedTerrainGlyph(context, selectedGlyph);
  return true;
};

export const paintBucketTerrainCanvasSelection = (
  canvas: HTMLCanvasElement,
  layout: BucketTerrainLayout<string, string, unknown>,
  selectedTxid: string | null,
): boolean => {
  const context = canvas.getContext("2d");
  if (context === null) return false;
  context.setTransform(
    canvas.width / layout.width,
    0,
    0,
    canvas.height / layout.height,
    0,
    0,
  );
  return paintBucketTerrainSelection(context, layout, selectedTxid);
};

export const paintBucketTerrain = <
  SectionKey extends string,
  RegionKey extends string,
  Signature,
>(
  context: CanvasRenderingContext2D,
  layout: BucketTerrainLayout<SectionKey, RegionKey, Signature>,
  presentation: BucketTerrainPaint<SectionKey, RegionKey, Signature>,
  selectedTxid: string | null = null,
): void => {
  const raster = createBucketTerrainRasterCache(context, layout, presentation);
  if (
    raster !== null &&
    paintBucketTerrainRaster(
      context,
      layout,
      presentation,
      raster,
      selectedTxid,
    )
  ) {
    return;
  }
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
    const selected = presentation.selected(region);
    context.fillStyle = presentation.color(region);
    context.globalAlpha = selected ? 0.12 : 0.025;
    context.fillRect(
      region.contentRect.x,
      region.contentRect.y,
      region.contentRect.width,
      region.contentRect.height,
    );
    context.globalAlpha = 1;
    context.strokeStyle = selected
      ? "#6ef2f0"
      : presentation.partial(region)
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
  let selectedGlyph: BucketTerrainGlyph<SectionKey, RegionKey> | null = null;
  const glyphAlpha = (
    region: BucketTerrainRegion<SectionKey, RegionKey, Signature>,
    selected: boolean,
    glyph: BucketTerrainGlyph<SectionKey, RegionKey>,
  ): number => {
    const glyphOpacity = presentation.glyphOpacity(region, selected, glyph);
    return Math.min(selected ? 0.86 : 0.68, glyphOpacity * glyphDensity(glyph));
  };
  const canBatchGlyphs =
    layout.glyphs.length >= 512 &&
    typeof Path2D === "function" &&
    typeof context.fill === "function";
  if (canBatchGlyphs) {
    const pathsByRegion = new Map<
      RegionKey,
      { color: string; pathsByAlpha: Map<number, Path2D> }
    >();
    const scratch = layout.glyphs.read(0);
    for (let index = 0; index < layout.glyphs.length; index += 1) {
      const glyph =
        scratch === undefined ? undefined : layout.glyphs.read(index, scratch);
      if (glyph === undefined) continue;
      const region = regionsByKey.get(glyph.regionKey);
      if (region === undefined) continue;
      let paths = pathsByRegion.get(glyph.regionKey);
      if (paths === undefined) {
        paths = {
          color: presentation.color(region),
          pathsByAlpha: new Map(),
        };
        pathsByRegion.set(glyph.regionKey, paths);
      }
      const alpha = glyphAlpha(region, presentation.selected(region), glyph);
      let path = paths.pathsByAlpha.get(alpha);
      if (path === undefined) {
        path = new Path2D();
        paths.pathsByAlpha.set(alpha, path);
      }
      path.rect(
        glyph.rect.x,
        glyph.rect.y,
        glyph.rect.width,
        glyph.rect.height,
      );
      if (glyph.txid === selectedTxid) {
        selectedGlyph = layout.glyphs.at(index) ?? null;
      }
    }
    for (const { color, pathsByAlpha } of pathsByRegion.values()) {
      context.fillStyle = color;
      for (const [alpha, path] of pathsByAlpha) {
        context.globalAlpha = alpha;
        context.fill(path);
      }
    }
  } else {
    let paintedRegionKey: RegionKey | null = null;
    let paintedRegionSelected = false;
    const scratch = layout.glyphs.read(0);
    for (let index = 0; index < layout.glyphs.length; index += 1) {
      const glyph =
        scratch === undefined ? undefined : layout.glyphs.read(index, scratch);
      if (glyph === undefined) continue;
      const region = regionsByKey.get(glyph.regionKey);
      if (region === undefined) continue;
      if (paintedRegionKey !== glyph.regionKey) {
        paintedRegionSelected = presentation.selected(region);
        context.fillStyle = presentation.color(region);
        paintedRegionKey = glyph.regionKey;
      }
      context.globalAlpha = glyphAlpha(region, paintedRegionSelected, glyph);
      context.fillRect(
        glyph.rect.x,
        glyph.rect.y,
        glyph.rect.width,
        glyph.rect.height,
      );
      if (glyph.txid === selectedTxid) {
        selectedGlyph = layout.glyphs.at(index) ?? null;
      }
    }
  }
  context.globalAlpha = 1;
  if (selectedGlyph !== null) paintSelectedTerrainGlyph(context, selectedGlyph);
};

const cooperativeBatchSize = (options: CooperativeWorkOptions): number => {
  const batchSize = options.batchSize ?? 750;
  if (!Number.isSafeInteger(batchSize) || batchSize <= 0) {
    throw new RangeError("Terrain paint batch size must be a positive integer");
  }
  return batchSize;
};

const stagingCanvas = (canvas: HTMLCanvasElement): HTMLCanvasElement => {
  let staging = stagingCanvasByCanvas.get(canvas);
  if (staging === undefined) {
    staging = document.createElement("canvas");
    stagingCanvasByCanvas.set(canvas, staging);
  }
  if (staging.width !== canvas.width || staging.height !== canvas.height) {
    staging.width = canvas.width;
    staging.height = canvas.height;
  }
  return staging;
};

/**
 * Paint a complete terrain into a retained offscreen canvas in bounded slices,
 * then commit it to the visible canvas in one draw. Users never see a partly
 * rastered population, and common 70k-row interactions do not monopolise an
 * animation-frame callback.
 */
export const paintBucketTerrainCooperatively = async <
  SectionKey extends string,
  RegionKey extends string,
  Signature,
>(
  canvas: HTMLCanvasElement,
  context: CanvasRenderingContext2D,
  layout: BucketTerrainLayout<SectionKey, RegionKey, Signature>,
  presentation: BucketTerrainPaint<SectionKey, RegionKey, Signature>,
  selectedTxid: string | null = null,
  options: CooperativeWorkOptions = {},
): Promise<void> => {
  const batchSize = cooperativeBatchSize(options);
  options.signal?.throwIfAborted();
  const raster = await createBucketTerrainRasterCacheCooperatively(
    context,
    layout,
    presentation,
    options,
  );
  options.signal?.throwIfAborted();
  if (raster !== null) {
    context.setTransform(
      canvas.width / layout.width,
      0,
      0,
      canvas.height / layout.height,
      0,
      0,
    );
    if (
      paintBucketTerrainRaster(
        context,
        layout,
        presentation,
        raster,
        selectedTxid,
      )
    ) {
      return;
    }
  }
  const staging = stagingCanvas(canvas);
  const layer = staging.getContext("2d");
  if (layer === null) throw new Error("Canvas 2D rendering is unavailable");
  layer.setTransform(
    staging.width / layout.width,
    0,
    0,
    staging.height / layout.height,
    0,
    0,
  );
  layer.clearRect(0, 0, layout.width, layout.height);
  layer.fillStyle = "#071018";
  layer.fillRect(0, 0, layout.width, layout.height);
  for (const section of layout.sections) {
    layer.fillStyle = "#0a161e";
    layer.fillRect(
      section.rect.x,
      section.rect.y,
      section.rect.width,
      section.rect.height,
    );
    layer.strokeStyle = "#2a3b48";
    layer.lineWidth = 1;
    layer.strokeRect(
      section.rect.x,
      section.rect.y,
      section.rect.width,
      section.rect.height,
    );
  }

  const regionsByKey = new Map(
    layout.regions.map((region) => [region.key, region]),
  );
  for (const region of layout.regions) {
    const selected = presentation.selected(region);
    layer.fillStyle = presentation.color(region);
    layer.globalAlpha = selected ? 0.12 : 0.025;
    layer.fillRect(
      region.contentRect.x,
      region.contentRect.y,
      region.contentRect.width,
      region.contentRect.height,
    );
    layer.globalAlpha = 1;
    layer.strokeStyle = selected
      ? "#6ef2f0"
      : presentation.partial(region)
        ? "#9a7735"
        : "#243744";
    layer.lineWidth = selected ? 1.75 : 1;
    layer.strokeRect(
      region.rect.x,
      region.rect.y,
      region.rect.width,
      region.rect.height,
    );
  }

  let selectedGlyph: BucketTerrainGlyph<SectionKey, RegionKey> | null = null;
  let paintedRegionKey: RegionKey | null = null;
  let paintedRegionSelected = false;
  const scratch = layout.glyphs.read(0);
  for (let start = 0; start < layout.glyphs.length; start += batchSize) {
    const end = Math.min(layout.glyphs.length, start + batchSize);
    for (let index = start; index < end; index += 1) {
      const glyph =
        scratch === undefined ? undefined : layout.glyphs.read(index, scratch);
      if (glyph === undefined) continue;
      const region = regionsByKey.get(glyph.regionKey);
      if (region === undefined) continue;
      if (paintedRegionKey !== glyph.regionKey) {
        paintedRegionSelected = presentation.selected(region);
        layer.fillStyle = presentation.color(region);
        paintedRegionKey = glyph.regionKey;
      }
      layer.globalAlpha = Math.min(
        paintedRegionSelected ? 0.86 : 0.68,
        presentation.glyphOpacity(region, paintedRegionSelected, glyph) *
          glyphDensity(glyph),
      );
      layer.fillRect(
        glyph.rect.x,
        glyph.rect.y,
        glyph.rect.width,
        glyph.rect.height,
      );
      if (glyph.txid === selectedTxid) {
        selectedGlyph = layout.glyphs.at(index) ?? null;
      }
    }
    if (end < layout.glyphs.length) await yieldCooperatively(options);
  }
  layer.globalAlpha = 1;
  if (selectedGlyph !== null) paintSelectedTerrainGlyph(layer, selectedGlyph);
  options.signal?.throwIfAborted();

  context.save();
  context.setTransform(1, 0, 0, 1, 0, 0);
  context.clearRect(0, 0, canvas.width, canvas.height);
  context.drawImage(staging, 0, 0);
  context.restore();
};

export const renderBucketTerrain = <
  SectionKey extends string,
  RegionKey extends string,
  Signature,
>(
  canvas: HTMLCanvasElement,
  groups: readonly BucketTerrainSectionGroup<
    SectionKey,
    RegionKey,
    Signature
  >[],
  mode: BucketTerrainMode,
  presentation: BucketTerrainPaint<SectionKey, RegionKey, Signature>,
  previousLayout: BucketTerrainLayout<SectionKey, RegionKey, Signature> | null,
  selectedTxid: string | null = null,
): BucketTerrainLayout<SectionKey, RegionKey, Signature> => {
  const { context, width, height } = prepareCanvasBacking(canvas, "subpixel");
  const layout =
    previousLayout !== null &&
    previousLayout.width === width &&
    previousLayout.height === height &&
    previousLayout.mode === mode
      ? previousLayout
      : createBucketTerrainLayout(groups, width, height, mode);
  paintBucketTerrain(context, layout, presentation, selectedTxid);
  return layout;
};

export const renderBucketTerrainCooperatively = async <
  SectionKey extends string,
  RegionKey extends string,
  Signature,
>(
  canvas: HTMLCanvasElement,
  groups: readonly BucketTerrainSectionGroup<
    SectionKey,
    RegionKey,
    Signature
  >[],
  mode: BucketTerrainMode,
  presentation: BucketTerrainPaint<SectionKey, RegionKey, Signature>,
  previousLayout: BucketTerrainLayout<SectionKey, RegionKey, Signature> | null,
  selectedTxid: string | null = null,
  options: CooperativeWorkOptions = {},
): Promise<BucketTerrainLayout<SectionKey, RegionKey, Signature>> => {
  const { context, width, height } = prepareCanvasBacking(canvas, "subpixel");
  // Leave the scheduling animation frame before layout construction. The
  // retained visible bitmap remains intact until the new terrain is complete.
  await yieldCooperatively(options);
  const layout =
    previousLayout !== null &&
    previousLayout.width === width &&
    previousLayout.height === height &&
    previousLayout.mode === mode
      ? previousLayout
      : await createBucketTerrainLayoutCooperatively(
          groups,
          width,
          height,
          mode,
          options,
        );
  options.signal?.throwIfAborted();
  await paintBucketTerrainCooperatively(
    canvas,
    context,
    layout,
    presentation,
    selectedTxid,
    options,
  );
  return layout;
};
