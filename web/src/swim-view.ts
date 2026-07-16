import type { MempoolEntry } from "./types";

const MINUTE_MS = 60_000;
const HOUR_MS = 60 * MINUTE_MS;
const DAY_MS = 24 * HOUR_MS;

const MIN_GLYPH_SIZE = 1.25;
const MAX_GLYPH_SIZE = 14;
const AWAITING_GLYPH_SIZE = 2;
const TARGET_GLYPH_COVERAGE = 0.36;
const AWAITING_COLOR = "#8996a6";

export interface FeeRateLane {
  label: string;
  minimum: number;
  maximum: number;
}

export const FEE_RATE_LANES: readonly FeeRateLane[] = [
  { label: "128+", minimum: 128, maximum: Number.POSITIVE_INFINITY },
  { label: "64–128", minimum: 64, maximum: 128 },
  { label: "32–64", minimum: 32, maximum: 64 },
  { label: "16–32", minimum: 16, maximum: 32 },
  { label: "8–16", minimum: 8, maximum: 16 },
  { label: "4–8", minimum: 4, maximum: 8 },
  { label: "2–4", minimum: 2, maximum: 4 },
  { label: "1–2", minimum: 1, maximum: 2 },
  { label: "<1", minimum: 0, maximum: 1 },
];

export interface AgeColumn {
  label: string;
  minimumMs: number;
  maximumMs: number;
  color: string;
}

export const AGE_COLUMNS: readonly AgeColumn[] = [
  {
    label: "3d+",
    minimumMs: 3 * DAY_MS,
    maximumMs: Number.POSITIVE_INFINITY,
    color: "#637083",
  },
  {
    label: "1–3d",
    minimumMs: DAY_MS,
    maximumMs: 3 * DAY_MS,
    color: "#557c98",
  },
  {
    label: "6–24h",
    minimumMs: 6 * HOUR_MS,
    maximumMs: DAY_MS,
    color: "#418eaa",
  },
  {
    label: "1–6h",
    minimumMs: HOUR_MS,
    maximumMs: 6 * HOUR_MS,
    color: "#37a8c0",
  },
  {
    label: "10–60m",
    minimumMs: 10 * MINUTE_MS,
    maximumMs: HOUR_MS,
    color: "#69cedc",
  },
  {
    label: "<10m",
    minimumMs: 0,
    maximumMs: 10 * MINUTE_MS,
    color: "#c3f5f7",
  },
];

export interface SwimGeometry {
  width: number;
  height: number;
  plotLeft: number;
  plotTop: number;
  plotWidth: number;
  plotHeight: number;
  laneHeight: number;
  columnWidth: number;
  awaitingTop: number;
  awaitingHeight: number;
}

export interface SwimGlyph {
  x: number;
  y: number;
  size: number;
  feeLaneIndex: number | null;
  ageColumnIndex: number | null;
}

export interface SwimGlyphBatch {
  color: string;
  glyphs: SwimGlyph[];
}

export interface SwimSummary {
  availableCount: number;
  awaitingCount: number;
  totalVsize: number;
  feeLaneCounts: number[];
  ageColumnCounts: number[];
}

export interface SwimLayout {
  geometry: SwimGeometry;
  batches: SwimGlyphBatch[];
  awaitingBatch: SwimGlyphBatch;
  summary: SwimSummary;
}

type GlyphPaintContext = Pick<
  CanvasRenderingContext2D,
  "fillRect" | "fillStyle"
>;

export const feeRateLaneIndex = (feeRate: number): number => {
  const index = FEE_RATE_LANES.findIndex(
    (lane) => feeRate >= lane.minimum && feeRate < lane.maximum,
  );
  return index === -1 ? FEE_RATE_LANES.length - 1 : index;
};

export const ageColumnIndex = (ageMs: number): number => {
  const nonNegativeAgeMs = Math.max(0, ageMs);
  const index = AGE_COLUMNS.findIndex(
    (column) =>
      nonNegativeAgeMs >= column.minimumMs &&
      nonNegativeAgeMs < column.maximumMs,
  );
  return index === -1 ? AGE_COLUMNS.length - 1 : index;
};

const unitHash = (value: string, seed: number): number => {
  let hash = (2_166_136_261 ^ seed) >>> 0;
  for (let index = 0; index < value.length; index += 1) {
    hash ^= value.charCodeAt(index);
    hash = Math.imul(hash, 16_777_619) >>> 0;
  }
  return hash / 4_294_967_296;
};

const createGeometry = (width: number, height: number): SwimGeometry => {
  const safeWidth = Math.max(1, width);
  const safeHeight = Math.max(1, height);
  const plotLeft = Math.min(82, Math.max(48, safeWidth * 0.25));
  const plotTop = 36;
  const rightGutter = 12;
  const bottomGutter = 12;
  const awaitingGap = 18;
  const awaitingHeight = 34;
  const plotWidth = Math.max(1, safeWidth - plotLeft - rightGutter);
  const plotHeight = Math.max(
    1,
    safeHeight - plotTop - awaitingGap - awaitingHeight - bottomGutter,
  );

  return {
    width: safeWidth,
    height: safeHeight,
    plotLeft,
    plotTop,
    plotWidth,
    plotHeight,
    laneHeight: plotHeight / FEE_RATE_LANES.length,
    columnWidth: plotWidth / AGE_COLUMNS.length,
    awaitingTop: plotTop + plotHeight + awaitingGap,
    awaitingHeight,
  };
};

const glyphPosition = (
  txid: string,
  left: number,
  top: number,
  width: number,
  height: number,
  size: number,
): Pick<SwimGlyph, "x" | "y"> => ({
  x: left + unitHash(txid, 0x9e3779b9) * Math.max(0, width - size),
  y: top + unitHash(txid, 0x85ebca6b) * Math.max(0, height - size),
});

export const createSwimLayout = (
  memberships: readonly MempoolEntry[],
  width: number,
  height: number,
  nowMs: number,
): SwimLayout => {
  const geometry = createGeometry(width, height);
  const batches: SwimGlyphBatch[] = AGE_COLUMNS.map((column) => ({
    color: column.color,
    glyphs: [],
  }));
  const awaitingBatch: SwimGlyphBatch = {
    color: AWAITING_COLOR,
    glyphs: [],
  };
  const feeLaneCounts = Array<number>(FEE_RATE_LANES.length).fill(0);
  const ageColumnCounts = Array<number>(AGE_COLUMNS.length).fill(0);
  const totalVsize = memberships.reduce(
    (total, membership) =>
      membership.facts.status === "available"
        ? total + membership.facts.vsize
        : total,
    0,
  );
  const maximumVsize = memberships.reduce(
    (maximum, membership) =>
      membership.facts.status === "available"
        ? Math.max(maximum, membership.facts.vsize)
        : maximum,
    0,
  );
  const coveragePixelsPerVbyte =
    totalVsize === 0
      ? 0
      : (geometry.plotWidth * geometry.plotHeight * TARGET_GLYPH_COVERAGE) /
        totalVsize;
  const maximumPixelsPerVbyte =
    maximumVsize === 0 ? 0 : (MAX_GLYPH_SIZE * MAX_GLYPH_SIZE) / maximumVsize;
  const pixelsPerVbyte = Math.min(
    coveragePixelsPerVbyte,
    maximumPixelsPerVbyte,
  );
  let availableCount = 0;
  let awaitingCount = 0;

  for (const membership of memberships) {
    if (membership.facts.status === "awaiting_rpc") {
      const size = Math.min(
        AWAITING_GLYPH_SIZE,
        geometry.plotWidth,
        geometry.awaitingHeight,
      );
      const position = glyphPosition(
        membership.txid,
        geometry.plotLeft,
        geometry.awaitingTop,
        geometry.plotWidth,
        geometry.awaitingHeight,
        size,
      );
      awaitingBatch.glyphs.push({
        ...position,
        size,
        feeLaneIndex: null,
        ageColumnIndex: null,
      });
      awaitingCount += 1;
      continue;
    }

    const feeRate = membership.facts.fee_sats / membership.facts.vsize;
    const feeLane = feeRateLaneIndex(feeRate);
    const ageColumn = ageColumnIndex(nowMs - membership.facts.entered_at_ms);
    const calculatedSize = Math.sqrt(membership.facts.vsize * pixelsPerVbyte);
    const size = Math.min(
      MAX_GLYPH_SIZE,
      geometry.columnWidth,
      geometry.laneHeight,
      Math.max(MIN_GLYPH_SIZE, calculatedSize),
    );
    const position = glyphPosition(
      membership.txid,
      geometry.plotLeft + ageColumn * geometry.columnWidth,
      geometry.plotTop + feeLane * geometry.laneHeight,
      geometry.columnWidth,
      geometry.laneHeight,
      size,
    );
    const batch = batches[ageColumn];
    if (batch === undefined) {
      throw new Error(`Missing glyph batch for age column ${ageColumn}`);
    }
    batch.glyphs.push({
      ...position,
      size,
      feeLaneIndex: feeLane,
      ageColumnIndex: ageColumn,
    });
    feeLaneCounts[feeLane] = (feeLaneCounts[feeLane] ?? 0) + 1;
    ageColumnCounts[ageColumn] = (ageColumnCounts[ageColumn] ?? 0) + 1;
    availableCount += 1;
  }

  return {
    geometry,
    batches,
    awaitingBatch,
    summary: {
      availableCount,
      awaitingCount,
      totalVsize,
      feeLaneCounts,
      ageColumnCounts,
    },
  };
};

export const paintMembershipGlyphs = (
  context: GlyphPaintContext,
  layout: SwimLayout,
): void => {
  for (const batch of [...layout.batches, layout.awaitingBatch]) {
    context.fillStyle = batch.color;
    for (const glyph of batch.glyphs) {
      context.fillRect(glyph.x, glyph.y, glyph.size, glyph.size);
    }
  }
};

const paintGrid = (
  context: CanvasRenderingContext2D,
  layout: SwimLayout,
): void => {
  const { geometry } = layout;
  context.clearRect(0, 0, geometry.width, geometry.height);
  context.fillStyle = "#0b121a";
  context.fillRect(0, 0, geometry.width, geometry.height);

  for (let feeIndex = 0; feeIndex < FEE_RATE_LANES.length; feeIndex += 1) {
    for (let ageIndex = 0; ageIndex < AGE_COLUMNS.length; ageIndex += 1) {
      context.fillStyle =
        (feeIndex + ageIndex) % 2 === 0 ? "#101b27" : "#0e1822";
      context.fillRect(
        geometry.plotLeft + ageIndex * geometry.columnWidth,
        geometry.plotTop + feeIndex * geometry.laneHeight,
        geometry.columnWidth,
        geometry.laneHeight,
      );
      context.strokeStyle = "#223243";
      context.lineWidth = 1;
      context.strokeRect(
        geometry.plotLeft + ageIndex * geometry.columnWidth,
        geometry.plotTop + feeIndex * geometry.laneHeight,
        geometry.columnWidth,
        geometry.laneHeight,
      );
    }
  }

  context.fillStyle = "#111c27";
  context.fillRect(
    geometry.plotLeft,
    geometry.awaitingTop,
    geometry.plotWidth,
    geometry.awaitingHeight,
  );
  context.strokeStyle = "#344455";
  context.strokeRect(
    geometry.plotLeft,
    geometry.awaitingTop,
    geometry.plotWidth,
    geometry.awaitingHeight,
  );

  context.fillStyle = "#9aacbf";
  context.font =
    '11px Inter, ui-sans-serif, system-ui, -apple-system, "Segoe UI", sans-serif';
  context.textBaseline = "middle";
  context.textAlign = "right";
  for (let index = 0; index < FEE_RATE_LANES.length; index += 1) {
    context.fillText(
      FEE_RATE_LANES[index]?.label ?? "",
      geometry.plotLeft - 8,
      geometry.plotTop + (index + 0.5) * geometry.laneHeight,
    );
  }
  context.fillText(
    "RPC pending",
    geometry.plotLeft - 8,
    geometry.awaitingTop + geometry.awaitingHeight / 2,
  );

  context.textAlign = "center";
  for (let index = 0; index < AGE_COLUMNS.length; index += 1) {
    context.fillText(
      AGE_COLUMNS[index]?.label ?? "",
      geometry.plotLeft + (index + 0.5) * geometry.columnWidth,
      geometry.plotTop - 17,
    );
  }
};

export const renderSwimView = (
  canvas: HTMLCanvasElement,
  memberships: readonly MempoolEntry[],
  nowMs: number,
): SwimSummary => {
  const bounds = canvas.getBoundingClientRect();
  const width = Math.max(1, Math.round(bounds.width));
  const height = Math.max(1, Math.round(bounds.height));
  const pixelRatio = Math.min(window.devicePixelRatio, 2);
  const backingWidth = Math.round(width * pixelRatio);
  const backingHeight = Math.round(height * pixelRatio);
  if (canvas.width !== backingWidth || canvas.height !== backingHeight) {
    canvas.width = backingWidth;
    canvas.height = backingHeight;
  }

  const context = canvas.getContext("2d");
  if (context === null) {
    throw new Error("Canvas 2D rendering is unavailable");
  }
  context.setTransform(pixelRatio, 0, 0, pixelRatio, 0, 0);
  const layout = createSwimLayout(memberships, width, height, nowMs);
  paintGrid(context, layout);
  paintMembershipGlyphs(context, layout);
  return layout.summary;
};
