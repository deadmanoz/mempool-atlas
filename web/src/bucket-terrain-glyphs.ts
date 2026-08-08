import {
  DEFAULT_COOPERATIVE_BATCH_SIZE,
  yieldCooperatively,
  type CooperativeWorkOptions,
} from "./cooperative-work";
import type { BucketTerrainGlyph } from "./bucket-terrain";

export interface BucketTerrainGlyphCollection<
  SectionKey extends string,
  RegionKey extends string,
> extends Iterable<BucketTerrainGlyph<SectionKey, RegionKey>> {
  readonly length: number;
  at(index: number): BucketTerrainGlyph<SectionKey, RegionKey> | undefined;
  read(
    index: number,
    target?: BucketTerrainGlyph<SectionKey, RegionKey>,
  ): BucketTerrainGlyph<SectionKey, RegionKey> | undefined;
}

type RegionCodeColumn = Uint8Array | Uint16Array | Uint32Array;

interface PackedGlyphColumns<
  SectionKey extends string,
  RegionKey extends string,
> {
  txids: string[];
  sourceRows: Uint32Array;
  vsizes: Float64Array;
  rects: Float64Array;
  regionCodes: RegionCodeColumn;
  regions: Array<{ regionKey: RegionKey; sectionKey: SectionKey }>;
}

class PackedBucketTerrainGlyphs<
  SectionKey extends string,
  RegionKey extends string,
> implements BucketTerrainGlyphCollection<SectionKey, RegionKey> {
  readonly length: number;
  private readonly txids: string[];
  private readonly sourceRows: Uint32Array;
  private readonly vsizes: Float64Array;
  private readonly rects: Float64Array;
  private readonly regionCodes: RegionCodeColumn;
  private readonly regions: Array<{
    regionKey: RegionKey;
    sectionKey: SectionKey;
  }>;

  constructor(columns: PackedGlyphColumns<SectionKey, RegionKey>) {
    this.length = columns.txids.length;
    this.txids = columns.txids;
    this.sourceRows = columns.sourceRows;
    this.vsizes = columns.vsizes;
    this.rects = columns.rects;
    this.regionCodes = columns.regionCodes;
    this.regions = columns.regions;
  }

  at(index: number): BucketTerrainGlyph<SectionKey, RegionKey> | undefined {
    const normalized = index < 0 ? this.length + index : index;
    return this.read(normalized);
  }

  read(
    index: number,
    target?: BucketTerrainGlyph<SectionKey, RegionKey>,
  ): BucketTerrainGlyph<SectionKey, RegionKey> | undefined {
    if (!Number.isSafeInteger(index) || index < 0 || index >= this.length) {
      return undefined;
    }
    const region = this.regions[this.regionCodes[index] ?? 0];
    const txid = this.txids[index];
    if (region === undefined || txid === undefined) return undefined;
    const rectOffset = index * 4;
    const glyph =
      target ??
      ({
        txid,
        sourceRow: 0,
        regionKey: region.regionKey,
        sectionKey: region.sectionKey,
        rect: { x: 0, y: 0, width: 0, height: 0 },
        vsize: 0,
      } satisfies BucketTerrainGlyph<SectionKey, RegionKey>);
    glyph.txid = txid;
    glyph.sourceRow = this.sourceRows[index] ?? 0;
    glyph.vsize = this.vsizes[index] ?? 0;
    glyph.regionKey = region.regionKey;
    glyph.sectionKey = region.sectionKey;
    glyph.rect.x = this.rects[rectOffset] ?? 0;
    glyph.rect.y = this.rects[rectOffset + 1] ?? 0;
    glyph.rect.width = this.rects[rectOffset + 2] ?? 0;
    glyph.rect.height = this.rects[rectOffset + 3] ?? 0;
    return glyph;
  }

  *[Symbol.iterator](): IterableIterator<
    BucketTerrainGlyph<SectionKey, RegionKey>
  > {
    for (let index = 0; index < this.length; index += 1) {
      const glyph = this.read(index);
      if (glyph !== undefined) yield glyph;
    }
  }
}

const batchSize = (options: CooperativeWorkOptions): number => {
  const size = options.batchSize ?? DEFAULT_COOPERATIVE_BATCH_SIZE;
  if (!Number.isSafeInteger(size) || size <= 0) {
    throw new RangeError("Terrain glyph batch size must be a positive integer");
  }
  return size;
};

const allocateColumns = <SectionKey extends string, RegionKey extends string>(
  length: number,
  regions: Array<{ regionKey: RegionKey; sectionKey: SectionKey }>,
): PackedGlyphColumns<SectionKey, RegionKey> => ({
  txids: new Array<string>(length),
  sourceRows: new Uint32Array(length),
  vsizes: new Float64Array(length),
  rects: new Float64Array(length * 4),
  regionCodes:
    regions.length <= 0x100
      ? new Uint8Array(length)
      : regions.length <= 0x1_0000
        ? new Uint16Array(length)
        : new Uint32Array(length),
  regions,
});

const writeGlyph = <SectionKey extends string, RegionKey extends string>(
  columns: PackedGlyphColumns<SectionKey, RegionKey>,
  regionIndexes: ReadonlyMap<RegionKey, number>,
  glyph: BucketTerrainGlyph<SectionKey, RegionKey>,
  index: number,
): void => {
  columns.txids[index] = glyph.txid;
  columns.sourceRows[index] = glyph.sourceRow;
  columns.vsizes[index] = glyph.vsize;
  columns.regionCodes[index] = regionIndexes.get(glyph.regionKey) ?? 0;
  const rectOffset = index * 4;
  columns.rects[rectOffset] = glyph.rect.x;
  columns.rects[rectOffset + 1] = glyph.rect.y;
  columns.rects[rectOffset + 2] = glyph.rect.width;
  columns.rects[rectOffset + 3] = glyph.rect.height;
};

const registerRegion = <SectionKey extends string, RegionKey extends string>(
  glyph: BucketTerrainGlyph<SectionKey, RegionKey>,
  indexes: Map<RegionKey, number>,
  regions: Array<{ regionKey: RegionKey; sectionKey: SectionKey }>,
): void => {
  if (indexes.has(glyph.regionKey)) return;
  indexes.set(glyph.regionKey, regions.length);
  regions.push({ regionKey: glyph.regionKey, sectionKey: glyph.sectionKey });
};

export const packBucketTerrainGlyphs = <
  SectionKey extends string,
  RegionKey extends string,
>(
  glyphs: readonly BucketTerrainGlyph<SectionKey, RegionKey>[],
): BucketTerrainGlyphCollection<SectionKey, RegionKey> => {
  const indexes = new Map<RegionKey, number>();
  const regions: Array<{ regionKey: RegionKey; sectionKey: SectionKey }> = [];
  for (const glyph of glyphs) registerRegion(glyph, indexes, regions);
  const columns = allocateColumns(glyphs.length, regions);
  for (let index = 0; index < glyphs.length; index += 1) {
    const glyph = glyphs[index];
    if (glyph !== undefined) writeGlyph(columns, indexes, glyph, index);
  }
  return new PackedBucketTerrainGlyphs(columns);
};

export const packBucketTerrainGlyphsCooperatively = async <
  SectionKey extends string,
  RegionKey extends string,
>(
  glyphs: readonly BucketTerrainGlyph<SectionKey, RegionKey>[],
  options: CooperativeWorkOptions,
): Promise<BucketTerrainGlyphCollection<SectionKey, RegionKey>> => {
  const size = batchSize(options);
  const indexes = new Map<RegionKey, number>();
  const regions: Array<{ regionKey: RegionKey; sectionKey: SectionKey }> = [];
  for (let start = 0; start < glyphs.length; start += size) {
    const end = Math.min(glyphs.length, start + size);
    for (let index = start; index < end; index += 1) {
      const glyph = glyphs[index];
      if (glyph !== undefined) registerRegion(glyph, indexes, regions);
    }
    if (end < glyphs.length) await yieldCooperatively(options);
  }
  const columns = allocateColumns(glyphs.length, regions);
  for (let start = 0; start < glyphs.length; start += size) {
    const end = Math.min(glyphs.length, start + size);
    for (let index = start; index < end; index += 1) {
      const glyph = glyphs[index];
      if (glyph !== undefined) writeGlyph(columns, indexes, glyph, index);
    }
    if (end < glyphs.length) await yieldCooperatively(options);
  }
  options.signal?.throwIfAborted();
  return new PackedBucketTerrainGlyphs(columns);
};

export const findBucketTerrainGlyph = <
  SectionKey extends string,
  RegionKey extends string,
>(
  glyphs: BucketTerrainGlyphCollection<SectionKey, RegionKey>,
  txid: string,
): BucketTerrainGlyph<SectionKey, RegionKey> | undefined => {
  if (Array.isArray(glyphs)) {
    return (
      glyphs as unknown as BucketTerrainGlyph<SectionKey, RegionKey>[]
    ).find(({ txid: candidate }) => candidate === txid);
  }
  const scratch = glyphs.read(0);
  if (scratch === undefined) return undefined;
  for (let index = 0; index < glyphs.length; index += 1) {
    const glyph = glyphs.read(index, scratch);
    if (glyph?.txid === txid) return glyphs.at(index);
  }
  return undefined;
};
