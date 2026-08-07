import type { CompositionBar, CompositionSegment } from "./composition";
import type { AxisTick, FeeSpectrum, JointDensity } from "./fee-distribution";
import { FEE_RATE_TICKS } from "./fee-distribution";
import { countFormat, formatVsize, percentageFormat } from "./format";
import type { AgeMosaic } from "./mosaic";
import { AGE_BANDS } from "./mosaic";

const SVG_NS = "http://www.w3.org/2000/svg";

const svgElement = <K extends keyof SVGElementTagNameMap>(
  tag: K,
  attributes: Record<string, string>,
): SVGElementTagNameMap[K] => {
  const element = document.createElementNS(SVG_NS, tag);
  for (const [name, value] of Object.entries(attributes)) {
    element.setAttribute(name, value);
  }
  return element;
};

const axisTickRow = (ticks: readonly AxisTick[]): HTMLElement => {
  const row = document.createElement("div");
  row.className = "panel-axis";
  row.setAttribute("aria-hidden", "true");
  for (const tick of ticks) {
    const label = document.createElement("span");
    label.textContent = tick.label;
    label.style.left = `${(tick.position * 100).toFixed(2)}%`;
    if (tick.position === 0) {
      label.dataset.edge = "start";
    } else if (tick.position === 1) {
      label.dataset.edge = "end";
    }
    row.append(label);
  }
  return row;
};

const emptyPanelState = (message: string): HTMLElement => {
  const empty = document.createElement("p");
  empty.className = "empty-state";
  empty.textContent = message;
  return empty;
};

const seriesLegend = (
  entries: readonly { label: string; color: string; detail: string }[],
): HTMLElement => {
  const legend = document.createElement("ul");
  legend.className = "panel-legend";
  for (const entry of entries) {
    const item = document.createElement("li");
    const dot = document.createElement("i");
    dot.style.background = entry.color;
    const label = document.createElement("span");
    label.textContent = entry.label;
    const detail = document.createElement("span");
    detail.className = "panel-legend-count";
    detail.textContent = entry.detail;
    item.append(dot, label, detail);
    legend.append(item);
  }
  return legend;
};

export const feeRateAxisRow = (): HTMLElement => axisTickRow(FEE_RATE_TICKS);

export const panelAxisRow = (ticks: readonly AxisTick[]): HTMLElement =>
  axisTickRow(ticks);

const JOINT_MARGIN = 26;
const JOINT_GAP = 3;
const JOINT_OPACITY_LEVELS = 32;

export interface JointChartOptions {
  color: { r: number; g: number; b: number };
  emptyMessage: string;
}

export const DEFAULT_JOINT_COLOR = { r: 102, g: 227, b: 232 } as const;

interface JointChartGeometry {
  width: number;
  height: number;
  gridWidth: number;
  gridHeight: number;
  cellSpanX: number;
  cellSpanY: number;
  ratio: number;
}

export const prepareJointChartCanvas = (
  canvas: HTMLCanvasElement,
  density: JointDensity,
): JointChartGeometry | null => {
  if (density.totalWeight <= 0) return null;
  const width = canvas.clientWidth;
  if (width === 0) return null;
  const gridWidth = width - JOINT_MARGIN - JOINT_GAP;
  const cellSpanX = gridWidth / density.columns;
  const cellSpanY = cellSpanX * 0.82;
  const gridHeight = cellSpanY * density.rows;
  const height = Math.round(gridHeight + JOINT_MARGIN + JOINT_GAP);
  const ratio = window.devicePixelRatio || 1;
  const backingWidth = Math.round(width * ratio);
  const backingHeight = Math.round(height * ratio);
  if (canvas.width !== backingWidth) canvas.width = backingWidth;
  if (canvas.height !== backingHeight) canvas.height = backingHeight;
  canvas.style.height = `${height}px`;
  return {
    width,
    height,
    gridWidth,
    gridHeight,
    cellSpanX,
    cellSpanY,
    ratio,
  };
};

export const renderJointChart = (
  container: HTMLElement,
  canvas: HTMLCanvasElement,
  density: JointDensity,
  options: JointChartOptions,
): void => {
  const hasWeight = density.totalWeight > 0;
  const existingEmpty = container.querySelector(".empty-state");
  existingEmpty?.remove();
  canvas.hidden = !hasWeight;
  const axes = container.querySelectorAll<HTMLElement>(".panel-axis");
  for (const axis of axes) {
    axis.hidden = !hasWeight;
  }
  if (!hasWeight) {
    container.append(emptyPanelState(options.emptyMessage));
    return;
  }
  const geometry = prepareJointChartCanvas(canvas, density);
  if (geometry === null) return;
  const { width, height, gridWidth, gridHeight, cellSpanX, cellSpanY, ratio } =
    geometry;
  const context = canvas.getContext("2d");
  if (context === null) {
    return;
  }
  context.setTransform(ratio, 0, 0, ratio, 0, 0);
  context.clearRect(0, 0, width, height);

  const { r, g, b } = options.color;
  const densityColor = `rgb(${r}, ${g}, ${b})`;
  const gridTop = JOINT_MARGIN + JOINT_GAP;
  context.fillStyle = "#0b121a";
  context.fillRect(0, gridTop, gridWidth, gridHeight);
  context.fillStyle = densityColor;
  const opacityPaths = Array.from(
    { length: JOINT_OPACITY_LEVELS },
    () => [] as Array<{ x: number; y: number }>,
  );
  for (let row = 0; row < density.rows; row += 1) {
    for (let column = 0; column < density.columns; column += 1) {
      const weight = density.cells[row * density.columns + column] ?? 0;
      if (weight <= 0) {
        continue;
      }
      const intensity = Math.sqrt(weight / density.maxCell);
      const opacityIndex = Math.min(
        JOINT_OPACITY_LEVELS - 1,
        Math.round(intensity * (JOINT_OPACITY_LEVELS - 1)),
      );
      opacityPaths[opacityIndex]?.push({
        x: column * cellSpanX + 0.5,
        y: gridTop + gridHeight - (row + 1) * cellSpanY + 0.5,
      });
    }
  }
  for (let index = 0; index < opacityPaths.length; index += 1) {
    const cells = opacityPaths[index];
    if (cells === undefined || cells.length === 0) continue;
    context.globalAlpha = 0.08 + (index / (JOINT_OPACITY_LEVELS - 1)) * 0.92;
    context.beginPath();
    for (const { x, y } of cells) {
      context.rect(x, y, cellSpanX - 1, cellSpanY - 1);
    }
    context.fill();
  }

  const maxColumn = Math.max(...density.columnTotals, 1);
  context.globalAlpha = 0.55;
  for (let column = 0; column < density.columns; column += 1) {
    const share = (density.columnTotals[column] ?? 0) / maxColumn;
    if (share <= 0) {
      continue;
    }
    const barHeight = Math.max(1, share * JOINT_MARGIN);
    context.fillRect(
      column * cellSpanX + 0.5,
      JOINT_MARGIN - barHeight,
      cellSpanX - 1,
      barHeight,
    );
  }
  const maxRow = Math.max(...density.rowTotals, 1);
  for (let row = 0; row < density.rows; row += 1) {
    const share = (density.rowTotals[row] ?? 0) / maxRow;
    if (share <= 0) {
      continue;
    }
    const barWidth = Math.max(1, share * JOINT_MARGIN);
    context.fillRect(
      gridWidth + JOINT_GAP,
      gridTop + gridHeight - (row + 1) * cellSpanY + 0.5,
      barWidth,
      cellSpanY - 1,
    );
  }
  context.globalAlpha = 1;
};

export interface CompositionRenderOptions {
  metricLabel: string;
  emptyMessage: string;
  onSegmentSelect?: (bar: CompositionBar, segment: CompositionSegment) => void;
  isSegmentInteractive?: (bar: CompositionBar) => boolean;
  isSegmentSelected?: (
    bar: CompositionBar,
    segment: CompositionSegment,
  ) => boolean;
}

const segmentTitle = (
  bar: CompositionBar,
  segment: CompositionSegment,
): string =>
  `${bar.title} · ${segment.label} — ${countFormat.format(segment.count)} tx · ` +
  `${formatVsize(segment.vsize)} · ${percentageFormat.format(segment.share)}`;

export const renderCompositionBars = (
  container: HTMLElement,
  bars: readonly CompositionBar[],
  options: CompositionRenderOptions,
): void => {
  const populated = bars.filter(({ segments }) => segments.length > 0);
  if (populated.length === 0) {
    container.replaceChildren(emptyPanelState(options.emptyMessage));
    return;
  }
  const list = document.createElement("div");
  list.className = "composition-list";
  for (const bar of populated) {
    const block = document.createElement("div");
    block.className = "composition-bar";
    const heading = document.createElement("div");
    heading.className = "composition-bar-title";
    const title = document.createElement("span");
    title.textContent = bar.title;
    heading.append(title);
    const track = document.createElement("div");
    track.className = "composition-track";
    track.setAttribute("role", "group");
    track.setAttribute(
      "aria-label",
      `${bar.title} composition by ${options.metricLabel}`,
    );
    for (const segment of bar.segments) {
      const interactive =
        options.onSegmentSelect !== undefined &&
        segment.bucketCount === 1 &&
        (options.isSegmentInteractive?.(bar) ?? true);
      const cell = document.createElement(interactive ? "button" : "div");
      cell.className = "composition-segment";
      cell.dataset.classifier = bar.classifierId;
      cell.dataset.segment = segment.key;
      cell.style.flexGrow = String(Math.max(segment.share, 0.002));
      cell.style.background = segment.color;
      cell.title = segmentTitle(bar, segment);
      if (interactive) {
        (cell as HTMLButtonElement).type = "button";
        cell.setAttribute("aria-label", segmentTitle(bar, segment));
        if (options.isSegmentSelected?.(bar, segment) === true) {
          cell.setAttribute("aria-pressed", "true");
        }
        cell.addEventListener("click", () => {
          options.onSegmentSelect?.(bar, segment);
        });
      } else {
        cell.setAttribute("aria-hidden", "true");
      }
      track.append(cell);
    }
    block.append(
      heading,
      track,
      seriesLegend(
        bar.segments.map((segment) => ({
          label: segment.label,
          color: segment.color,
          detail: percentageFormat.format(segment.share),
        })),
      ),
    );
    list.append(block);
  }
  container.replaceChildren(list);
};

const SPECTRUM_WIDTH = 480;
const SPECTRUM_HEIGHT = 160;

export const renderSpectrumChart = (
  container: HTMLElement,
  spectrum: FeeSpectrum,
  ticks: readonly AxisTick[],
  metricFormat: (value: number) => string,
  emptyMessage: string,
): void => {
  if (spectrum.totalWeight === 0 || spectrum.maxBin === 0) {
    container.replaceChildren(emptyPanelState(emptyMessage));
    return;
  }
  const svg = svgElement("svg", {
    viewBox: `0 0 ${SPECTRUM_WIDTH} ${SPECTRUM_HEIGHT}`,
    class: "spectrum-chart",
    role: "img",
  });
  svg.append(
    svgElement("line", {
      x1: "0",
      y1: String(SPECTRUM_HEIGHT - 0.5),
      x2: String(SPECTRUM_WIDTH),
      y2: String(SPECTRUM_HEIGHT - 0.5),
      class: "chart-baseline",
    }),
  );
  const binCount = spectrum.bins.length;
  const step = SPECTRUM_WIDTH / binCount;
  const barWidth = Math.max(1, step - 2);
  for (const [index, bin] of spectrum.bins.entries()) {
    if (bin.total <= 0) {
      continue;
    }
    let stackTop = SPECTRUM_HEIGHT;
    for (const segment of bin.segments) {
      const height = (segment.weight / spectrum.maxBin) * (SPECTRUM_HEIGHT - 6);
      stackTop -= height;
      const rect = svgElement("rect", {
        x: (index * step + 1).toFixed(2),
        y: stackTop.toFixed(2),
        width: barWidth.toFixed(2),
        height: Math.max(height, 0.75).toFixed(2),
        fill: segment.color,
      });
      const title = document.createElementNS(SVG_NS, "title");
      title.textContent = `${segment.label} · ${metricFormat(segment.weight)}`;
      rect.append(title);
      svg.append(rect);
    }
  }
  const groupWeights = new Map<
    string,
    { label: string; color: string; weight: number }
  >();
  for (const bin of spectrum.bins) {
    for (const segment of bin.segments) {
      const existing = groupWeights.get(segment.key);
      if (existing === undefined) {
        groupWeights.set(segment.key, {
          label: segment.label,
          color: segment.color,
          weight: segment.weight,
        });
      } else {
        existing.weight += segment.weight;
      }
    }
  }
  const legend = seriesLegend(
    [...groupWeights.values()].map((group) => ({
      label: group.label,
      color: group.color,
      detail: percentageFormat.format(group.weight / spectrum.totalWeight),
    })),
  );
  container.replaceChildren(svg, axisTickRow(ticks), legend);
};

export interface MosaicRenderOptions {
  metricFormat: (value: number) => string;
  emptyMessage: string;
  onColumnSelect?: (column: { key: string; label: string }) => void;
  isColumnInteractive?: boolean;
}

export const renderMosaicChart = (
  container: HTMLElement,
  mosaic: AgeMosaic,
  options: MosaicRenderOptions,
): void => {
  if (mosaic.columns.length === 0 || mosaic.totalWeight === 0) {
    container.replaceChildren(emptyPanelState(options.emptyMessage));
    return;
  }
  const board = document.createElement("div");
  board.className = "mosaic-board";
  for (const column of mosaic.columns) {
    const interactive =
      options.isColumnInteractive === true &&
      options.onColumnSelect !== undefined;
    const columnElement = document.createElement(
      interactive ? "button" : "div",
    );
    columnElement.className = "mosaic-column";
    columnElement.style.flexGrow = String(Math.max(column.share, 0.004));
    const summary = `${column.label} — ${countFormat.format(column.count)} tx · ${options.metricFormat(column.weight)} · ${percentageFormat.format(column.share)}`;
    columnElement.title = summary;
    if (interactive) {
      (columnElement as HTMLButtonElement).type = "button";
      columnElement.setAttribute("aria-label", summary);
      columnElement.addEventListener("click", () => {
        options.onColumnSelect?.({ key: column.key, label: column.label });
      });
    } else {
      columnElement.setAttribute("aria-hidden", "true");
    }
    for (const cell of column.cells) {
      const cellElement = document.createElement("span");
      cellElement.className = "mosaic-cell";
      cellElement.style.flexGrow = String(Math.max(cell.share, 0.01));
      cellElement.style.background = cell.bandColor;
      cellElement.title = `${column.label} · ${cell.bandLabel} — ${countFormat.format(cell.count)} tx · ${options.metricFormat(cell.weight)} · ${percentageFormat.format(cell.share)} of bucket`;
      columnElement.append(cellElement);
    }
    const marker = document.createElement("i");
    marker.className = "mosaic-column-marker";
    marker.style.background = column.color;
    columnElement.append(marker);
    board.append(columnElement);
  }
  const bucketLegend = seriesLegend(
    mosaic.columns.map((column) => ({
      label: column.label,
      color: column.color,
      detail: percentageFormat.format(column.share),
    })),
  );
  const ageLegend = seriesLegend(
    AGE_BANDS.map((band) => ({
      label: band.label,
      color: band.color,
      detail: "",
    })),
  );
  ageLegend.classList.add("panel-legend-secondary");
  container.replaceChildren(board, bucketLegend, ageLegend);
};
