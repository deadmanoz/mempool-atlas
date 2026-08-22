import {
  logDomainBinBounds,
  type FeeSpectrum,
  type JointDensity,
  type LogDomain,
  type LogDomainBinBounds,
} from "./fee-distribution";
import { percentageFormat } from "./format";

/** Geometry shared with the bounded joint-density canvas renderer. */
export const JOINT_CHART_MARGIN_CSS_PX = 26;
export const JOINT_CHART_GAP_CSS_PX = 3;
export const JOINT_CHART_CELL_ASPECT_RATIO = 0.82;

export type InspectionValueFormatter = (value: number) => string;

export interface DistributionInspectionMetric {
  total: number;
  totalLabel: string;
  share: number;
  shareLabel: string;
}

export interface SpectrumSeriesInspection {
  key: string;
  label: string;
  color: string;
  metric: DistributionInspectionMetric;
  populationShare: number;
  populationShareLabel: string;
}

export interface SpectrumBinInspection {
  kind: "spectrum-bin";
  key: string;
  bin: number;
  bounds: LogDomainBinBounds;
  rangeLabel: string;
  title: string;
  detail: string;
  metric: DistributionInspectionMetric;
  series: readonly SpectrumSeriesInspection[];
}

export interface JointDensityCellInspection {
  kind: "joint-cell";
  key: string;
  column: number;
  row: number;
  xBounds: LogDomainBinBounds;
  yBounds: LogDomainBinBounds;
  xRangeLabel: string;
  yRangeLabel: string;
  title: string;
  detail: string;
  metric: DistributionInspectionMetric;
}

export interface SpectrumBinInspectionInput {
  spectrum: Readonly<FeeSpectrum>;
  domain: Readonly<LogDomain>;
  bin: number;
  keyPrefix: string;
  axisLabel: string;
  metricLabel: string;
  shareDenominatorLabel?: string;
  formatRangeValue: InspectionValueFormatter;
  formatMetric: InspectionValueFormatter;
  formatShare?: InspectionValueFormatter;
}

export interface JointDensityCellInspectionInput {
  density: Readonly<JointDensity>;
  xDomain: Readonly<LogDomain>;
  yDomain: Readonly<LogDomain>;
  column: number;
  row: number;
  keyPrefix: string;
  xAxisLabel: string;
  yAxisLabel: string;
  metricLabel: string;
  shareDenominatorLabel?: string;
  formatXValue: InspectionValueFormatter;
  formatYValue: InspectionValueFormatter;
  formatMetric: InspectionValueFormatter;
  formatShare?: InspectionValueFormatter;
}

export interface SpectrumXHitTestInput {
  x: number;
  width: number;
  binCount: number;
}

export type JointChartHit =
  | { kind: "grid"; column: number; row: number }
  | { kind: "x-marginal"; column: number }
  | { kind: "y-marginal"; row: number };

export interface JointChartCssHitTestInput {
  x: number;
  y: number;
  width: number;
  columns: number;
  rows: number;
  margin?: number;
  gap?: number;
  cellAspectRatio?: number;
}

const boundedShare = (value: number, total: number): number =>
  value > 0 && total > 0 ? Math.min(1, value / total) : 0;

const metricInspection = (
  total: number,
  populationTotal: number,
  formatMetric: InspectionValueFormatter,
  formatShare: InspectionValueFormatter,
): DistributionInspectionMetric => {
  const share = boundedShare(total, populationTotal);
  return {
    total,
    totalLabel: formatMetric(total),
    share,
    shareLabel: formatShare(share),
  };
};

const requiredKeyPrefix = (keyPrefix: string): string => {
  if (keyPrefix.length === 0) {
    throw new RangeError(
      "Distribution inspection key prefix must not be empty",
    );
  }
  return keyPrefix;
};

/**
 * Describe exact logarithmic-bin membership, including the clamped open edge
 * bins. Interior bins include their lower bound and exclude their upper bound.
 */
export const formatLogBinRange = (
  bounds: Readonly<LogDomainBinBounds>,
  formatValue: InspectionValueFormatter,
): string => {
  const { lowerInclusive, upperExclusive } = bounds;
  if (lowerInclusive === null) {
    return upperExclusive === null
      ? "all values"
      : `less than ${formatValue(upperExclusive)}`;
  }
  if (upperExclusive === null) {
    return `${formatValue(lowerInclusive)} or more`;
  }
  return `${formatValue(lowerInclusive)} to less than ${formatValue(upperExclusive)}`;
};

export const buildSpectrumBinInspection = ({
  spectrum,
  domain,
  bin,
  keyPrefix,
  axisLabel,
  metricLabel,
  shareDenominatorLabel = "population",
  formatRangeValue,
  formatMetric,
  formatShare = (share) => percentageFormat.format(share),
}: SpectrumBinInspectionInput): SpectrumBinInspection => {
  const bounds = logDomainBinBounds(domain, bin, spectrum.bins.length);
  const spectrumBin = spectrum.bins[bin];
  if (spectrumBin === undefined) {
    throw new RangeError("Spectrum inspection bin is outside the spectrum");
  }

  const prefix = requiredKeyPrefix(keyPrefix);
  const key = `${prefix}:bin:${bin}`;
  const rangeLabel = formatLogBinRange(bounds, formatRangeValue);
  const metric = metricInspection(
    spectrumBin.total,
    spectrum.totalWeight,
    formatMetric,
    formatShare,
  );
  const series = spectrumBin.segments.map((segment) => ({
    key: `${key}:series:${encodeURIComponent(segment.key)}`,
    label: segment.label,
    color: segment.color,
    metric: metricInspection(
      segment.weight,
      spectrumBin.total,
      formatMetric,
      formatShare,
    ),
    populationShare: boundedShare(segment.weight, spectrum.totalWeight),
    populationShareLabel: formatShare(
      boundedShare(segment.weight, spectrum.totalWeight),
    ),
  }));
  const seriesDetail = series
    .map(
      (entry) =>
        `${entry.label}: ${entry.metric.totalLabel} (${entry.metric.shareLabel} of bin)`,
    )
    .join(" · ");
  const detail = `${metricLabel}: ${metric.totalLabel} (${metric.shareLabel} of ${shareDenominatorLabel})`;

  return {
    kind: "spectrum-bin",
    key,
    bin,
    bounds,
    rangeLabel,
    title: `${axisLabel}: ${rangeLabel}`,
    detail: seriesDetail.length === 0 ? detail : `${detail} · ${seriesDetail}`,
    metric,
    series,
  };
};

export const buildJointDensityCellInspection = ({
  density,
  xDomain,
  yDomain,
  column,
  row,
  keyPrefix,
  xAxisLabel,
  yAxisLabel,
  metricLabel,
  shareDenominatorLabel = "population",
  formatXValue,
  formatYValue,
  formatMetric,
  formatShare = (share) => percentageFormat.format(share),
}: JointDensityCellInspectionInput): JointDensityCellInspection => {
  const xBounds = logDomainBinBounds(xDomain, column, density.columns);
  const yBounds = logDomainBinBounds(yDomain, row, density.rows);
  const cellTotal = density.cells[row * density.columns + column];
  if (cellTotal === undefined) {
    throw new RangeError(
      "Joint-density inspection cell is outside the density",
    );
  }

  const key = `${requiredKeyPrefix(keyPrefix)}:cell:${column}:${row}`;
  const xRangeLabel = formatLogBinRange(xBounds, formatXValue);
  const yRangeLabel = formatLogBinRange(yBounds, formatYValue);
  const metric = metricInspection(
    cellTotal,
    density.totalWeight,
    formatMetric,
    formatShare,
  );

  return {
    kind: "joint-cell",
    key,
    column,
    row,
    xBounds,
    yBounds,
    xRangeLabel,
    yRangeLabel,
    title: `${xAxisLabel}: ${xRangeLabel} · ${yAxisLabel}: ${yRangeLabel}`,
    detail: `${metricLabel}: ${metric.totalLabel} (${metric.shareLabel} of ${shareDenominatorLabel})`,
    metric,
  };
};

/** Map any in-bounds CSS x coordinate to its logarithmic spectrum bin. */
export const hitTestSpectrumX = ({
  x,
  width,
  binCount,
}: SpectrumXHitTestInput): number | null => {
  if (
    !Number.isFinite(x) ||
    !Number.isFinite(width) ||
    width <= 0 ||
    !Number.isInteger(binCount) ||
    binCount <= 0 ||
    x < 0 ||
    x >= width
  ) {
    return null;
  }
  return Math.min(binCount - 1, Math.floor((x / width) * binCount));
};

/**
 * Hit-test the joint canvas in CSS pixels. Model rows increase from bottom to
 * top, matching `JointDensity`, while pointer y coordinates increase downward.
 */
export const hitTestJointChartCss = ({
  x,
  y,
  width,
  columns,
  rows,
  margin = JOINT_CHART_MARGIN_CSS_PX,
  gap = JOINT_CHART_GAP_CSS_PX,
  cellAspectRatio = JOINT_CHART_CELL_ASPECT_RATIO,
}: JointChartCssHitTestInput): JointChartHit | null => {
  if (
    !Number.isFinite(x) ||
    !Number.isFinite(y) ||
    !Number.isFinite(width) ||
    !Number.isFinite(margin) ||
    !Number.isFinite(gap) ||
    !Number.isFinite(cellAspectRatio) ||
    width <= margin + gap ||
    margin < 0 ||
    gap < 0 ||
    cellAspectRatio <= 0 ||
    !Number.isInteger(columns) ||
    columns <= 0 ||
    !Number.isInteger(rows) ||
    rows <= 0 ||
    x < 0 ||
    x >= width ||
    y < 0
  ) {
    return null;
  }

  const gridWidth = width - margin - gap;
  const cellSpanX = gridWidth / columns;
  const cellSpanY = cellSpanX * cellAspectRatio;
  const gridTop = margin + gap;
  const gridHeight = cellSpanY * rows;
  const gridBottom = gridTop + gridHeight;

  if (x < gridWidth && y < margin) {
    return {
      kind: "x-marginal",
      column: Math.min(columns - 1, Math.floor(x / cellSpanX)),
    };
  }
  if (y < gridTop || y >= gridBottom) return null;

  const visualRow = Math.min(rows - 1, Math.floor((y - gridTop) / cellSpanY));
  const row = rows - 1 - visualRow;
  if (x < gridWidth) {
    return {
      kind: "grid",
      column: Math.min(columns - 1, Math.floor(x / cellSpanX)),
      row,
    };
  }
  if (x >= gridWidth + gap) {
    return { kind: "y-marginal", row };
  }
  return null;
};
