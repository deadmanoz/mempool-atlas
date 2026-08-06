import { prepareCanvasBacking } from "./canvas-backing";
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
  glyphs: BucketTerrainGlyph<SectionKey, RegionKey>[];
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

const sumVsize = (transactions: readonly MempoolTransaction[]): number =>
  transactions.reduce((total, transaction) => total + transaction.vsize, 0);

const metric = (
  transactions: readonly MempoolTransaction[],
  mode: BucketTerrainMode,
): number => (mode === "count" ? transactions.length : sumVsize(transactions));

const transactionGlyph = <SectionKey extends string, RegionKey extends string>(
  transaction: MempoolTransaction,
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
    txid: transaction.txid,
    regionKey,
    sectionKey,
    rect: insetRect(rect, adaptiveGap),
    vsize: transaction.vsize,
  };
};

const packCountTransactions = <
  SectionKey extends string,
  RegionKey extends string,
>(
  transactions: readonly MempoolTransaction[],
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
  return transactions.map((transaction, index) => {
    const column = index % columns;
    const row = Math.floor(index / columns);
    const finalRow = row === rows - 1;
    const rowColumns = finalRow ? finalRowCount : columns;
    const cellWidth = rect.width / rowColumns;
    const cellHeight = finalRow ? finalRowHeight : completeRowHeight;
    return transactionGlyph(
      transaction,
      {
        x: rect.x + column * cellWidth,
        y: rect.y + row * completeRowHeight,
        width: cellWidth,
        height: cellHeight,
      },
      regionKey,
      sectionKey,
    );
  });
};

const packTransactions = <SectionKey extends string, RegionKey extends string>(
  transactions: readonly MempoolTransaction[],
  rect: BucketTerrainRect,
  regionKey: RegionKey,
  sectionKey: SectionKey,
  mode: BucketTerrainMode,
): BucketTerrainGlyph<SectionKey, RegionKey>[] => {
  if (transactions.length === 0 || rect.width <= 0 || rect.height <= 0) {
    return [];
  }
  if (mode === "count") {
    return packCountTransactions(transactions, rect, regionKey, sectionKey);
  }
  const weights = transactions.map((transaction) => transaction.vsize);
  const prefix = [0];
  for (const weight of weights) {
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
      const transaction = transactions[current.start];
      if (transaction !== undefined) {
        glyphs.push(
          transactionGlyph(transaction, current.rect, regionKey, sectionKey),
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

const sectionLayoutWeights = <
  SectionKey extends string,
  RegionKey extends string,
  Signature,
>(
  groups: readonly BucketTerrainSectionGroup<
    SectionKey,
    RegionKey,
    Signature
  >[],
  mode: BucketTerrainMode,
): number[] => {
  return groups.map((group) => metric(group.transactions, mode));
};

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
  const layoutWeights = sectionLayoutWeights(groups, mode);
  const sectionRects = splitWeightedRegions(
    groups.map((group, index) => ({
      key: group.key,
      weight: layoutWeights[index] ?? 1,
    })),
    { x: 0, y: 0, width: safeWidth, height: safeHeight },
  );

  const sections: BucketTerrainSection<SectionKey>[] = [];
  const regions: BucketTerrainRegion<SectionKey, RegionKey, Signature>[] = [];
  const glyphs: BucketTerrainGlyph<SectionKey, RegionKey>[] = [];

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
    sections.push({
      key: group.key,
      rect,
      contentRect,
      transactionCount: group.transactions.length,
      totalVsize: sumVsize(group.transactions),
      weight: metric(group.transactions, mode),
      labelHeight: sectionLabelHeight,
    });

    const regionWeights = group.regions.map((region) => ({
      key: region.key,
      weight: metric(region.transactions, mode),
    }));
    const regionRects = group.nested
      ? splitWeightedRegions(regionWeights, contentRect)
      : new Map<RegionKey, BucketTerrainRect>([
          [group.regions[0]?.key as RegionKey, contentRect],
        ]);

    for (const regionGroup of group.regions) {
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
        transactionCount: regionGroup.transactions.length,
        totalVsize: sumVsize(regionGroup.transactions),
        weight: metric(regionGroup.transactions, mode),
        labelHeight: regionLabelHeight,
      });
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
  for (const glyph of layout.glyphs) {
    if (glyph.regionKey === region.key && containsPoint(glyph.rect, x, y)) {
      return { kind: "transaction", glyph };
    }
  }
  return { kind: "region", region };
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
  let paintedRegionKey: RegionKey | null = null;
  let paintedRegionSelected = false;
  for (const glyph of layout.glyphs) {
    const region = regionsByKey.get(glyph.regionKey);
    if (region === undefined) {
      continue;
    }
    if (paintedRegionKey !== glyph.regionKey) {
      paintedRegionSelected = presentation.selected(region);
      context.fillStyle = presentation.color(region);
      paintedRegionKey = glyph.regionKey;
    }
    const glyphOpacity = presentation.glyphOpacity(
      region,
      paintedRegionSelected,
      glyph,
    );
    const shortestSide = Math.min(glyph.rect.width, glyph.rect.height);
    const density =
      shortestSide < 0.9
        ? 0.38
        : shortestSide < 1.6
          ? 0.48
          : shortestSide < 3
            ? 0.62
            : 0.78;
    context.globalAlpha = Math.min(
      paintedRegionSelected ? 0.86 : 0.68,
      glyphOpacity * density,
    );
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
