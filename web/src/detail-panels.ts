import type { CompositionBar, CompositionSegment } from "./composition";
import {
  buildJointDensityCellInspection,
  buildSpectrumBinInspection,
  formatLogBinRange,
  hitTestJointChartCss,
  hitTestSpectrumX,
} from "./distribution-chart-inspection";
import {
  annotateDistributionInspectionTarget,
  notifyDistributionInspectionTargetChanged,
  type DistributionInspectionMetadata,
} from "./distribution-interaction";
import type {
  AxisTick,
  FeeSpectrum,
  JointDensity,
  LogDomain,
} from "./fee-distribution";
import { FEE_RATE_TICKS, logDomainBinBounds } from "./fee-distribution";
import {
  countFormat,
  decimalFormat,
  formatVsize,
  percentageFormat,
} from "./format";
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

const axisTickRow = (
  ticks: readonly AxisTick[],
  axisLabel?: string,
): HTMLElement => {
  const row = document.createElement("div");
  row.className = "panel-axis";
  const hasReferences = ticks.some(
    ({ description }) => description !== undefined,
  );
  if (!hasReferences) {
    row.setAttribute("aria-hidden", "true");
  } else {
    row.dataset.hasReferences = "true";
  }
  let referenceOrder = 0;
  for (const tick of ticks) {
    const label = document.createElement("span");
    label.textContent = tick.label;
    label.style.left = `${(tick.position * 100).toFixed(2)}%`;
    label.dataset.priority = String(tick.priority);
    if (tick.position === 0) {
      label.dataset.edge = "start";
    } else if (tick.position === 1) {
      label.dataset.edge = "end";
    }
    if (tick.description !== undefined) {
      label.tabIndex = 0;
      label.setAttribute("role", "note");
      label.dataset.reference = "true";
      label.dataset.referenceOrder = String(referenceOrder);
      referenceOrder += 1;
      annotateDistributionInspectionTarget(label, {
        key: `axis:${tick.value}`,
        title: tick.label,
        detail: tick.description,
        accessibleLabel: `${tick.label}. ${tick.description}`,
        pinOnClick: false,
      });
    }
    row.append(label);
  }
  if (axisLabel !== undefined) {
    row.dataset.hasTitle = "true";
    const title = document.createElement("strong");
    title.className = "panel-axis-title";
    title.textContent = axisLabel;
    row.append(title);
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

export const feeRateAxisRow = (): HTMLElement =>
  axisTickRow(FEE_RATE_TICKS, "Fee rate");

export const panelAxisRow = (
  ticks: readonly AxisTick[],
  axisLabel?: string,
): HTMLElement => axisTickRow(ticks, axisLabel);

const JOINT_MARGIN = 26;
const JOINT_GAP = 3;
const JOINT_OPACITY_LEVELS = 32;

export interface JointChartOptions {
  color: { r: number; g: number; b: number };
  emptyMessage: string;
  keyPrefix: string;
  xDomain: Readonly<LogDomain>;
  yDomain: Readonly<LogDomain>;
  xAxisLabel: string;
  yAxisLabel: string;
  metricLabel: string;
  shareDenominatorLabel?: string;
  formatXValue: (value: number) => string;
  formatYValue: (value: number) => string;
  formatMetric: (value: number) => string;
  yAxis?: HTMLElement;
  yTicks?: readonly AxisTick[];
}

export const DEFAULT_JOINT_COLOR = { r: 102, g: 227, b: 232 } as const;

export interface JointChartGeometry {
  width: number;
  height: number;
  gridWidth: number;
  gridHeight: number;
  gridTop: number;
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
    gridTop: JOINT_MARGIN + JOINT_GAP,
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
): JointChartGeometry | null => {
  const hasWeight = density.totalWeight > 0;
  const existingEmpty = container.querySelector(".empty-state");
  existingEmpty?.remove();
  canvas.hidden = !hasWeight;
  const axes = container.querySelectorAll<HTMLElement>(".panel-axis");
  for (const axis of axes) {
    axis.hidden = !hasWeight;
  }
  if (!hasWeight) {
    jointInteractionCleanup.get(canvas)?.();
    container.append(emptyPanelState(options.emptyMessage));
    options.yAxis?.replaceChildren();
    return null;
  }
  const geometry = prepareJointChartCanvas(canvas, density);
  if (geometry === null) return null;
  const {
    width,
    height,
    gridWidth,
    gridHeight,
    gridTop,
    cellSpanX,
    cellSpanY,
    ratio,
  } = geometry;
  const context = canvas.getContext("2d");
  if (context === null) {
    return null;
  }
  context.setTransform(ratio, 0, 0, ratio, 0, 0);
  context.clearRect(0, 0, width, height);

  const { r, g, b } = options.color;
  const densityColor = `rgb(${r}, ${g}, ${b})`;
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
  if (options.yAxis !== undefined && options.yTicks !== undefined) {
    const labels = options.yTicks.map((tick) => {
      const label = document.createElement("span");
      label.textContent = tick.label;
      label.dataset.priority = String(tick.priority);
      label.style.top = `${gridTop + (1 - tick.position) * gridHeight}px`;
      if (tick.position === 0) {
        label.dataset.edge = "bottom";
      } else if (tick.position === 1) {
        label.dataset.edge = "top";
      }
      return label;
    });
    const title = document.createElement("strong");
    title.className = "joint-y-axis-title";
    title.textContent = options.yAxisLabel;
    options.yAxis.replaceChildren(title, ...labels);
  }
  installJointChartInteraction(container, canvas, density, geometry, options);
  return geometry;
};

const jointInteractionCleanup = new WeakMap<HTMLCanvasElement, () => void>();

/**
 * Synchronously retire a deferred density paint before its surrounding model
 * is replaced. The canvas keeps its CSS footprint, but exposes no stale pixels,
 * metadata, or interaction until the next complete paint is installed.
 */
export const invalidateJointChart = (
  container: HTMLElement,
  canvas: HTMLCanvasElement,
  yAxis?: HTMLElement,
): void => {
  jointInteractionCleanup.get(canvas)?.();
  jointInteractionCleanup.delete(canvas);
  container.querySelector(".empty-state")?.remove();
  canvas.width = canvas.width;
  canvas.removeAttribute("role");
  canvas.removeAttribute("tabindex");
  canvas.setAttribute("aria-hidden", "true");
  for (const attribute of [
    "aria-label",
    "aria-pressed",
    "data-distribution-inspection-key",
    "data-distribution-inspection-title",
    "data-distribution-inspection-detail",
    "data-distribution-inspection-pin",
    "data-distribution-inspection-pinned",
  ]) {
    canvas.removeAttribute(attribute);
  }
  for (const axis of container.querySelectorAll<HTMLElement>(".panel-axis")) {
    axis.hidden = true;
  }
  yAxis?.replaceChildren();
};

const jointMarginalInspection = (
  density: JointDensity,
  options: JointChartOptions,
  hit:
    | { kind: "x-marginal"; column: number }
    | { kind: "y-marginal"; row: number },
): DistributionInspectionMetadata => {
  const isX = hit.kind === "x-marginal";
  const index = isX ? hit.column : hit.row;
  const binCount = isX ? density.columns : density.rows;
  const domain = isX ? options.xDomain : options.yDomain;
  const formatValue = isX ? options.formatXValue : options.formatYValue;
  const axisLabel = isX ? options.xAxisLabel : options.yAxisLabel;
  const total = isX
    ? (density.columnTotals[hit.column] ?? 0)
    : (density.rowTotals[hit.row] ?? 0);
  const range = formatLogBinRange(
    logDomainBinBounds(domain, index, binCount),
    formatValue,
  );
  const share = density.totalWeight > 0 ? total / density.totalWeight : 0;
  const shareDenominatorLabel = options.shareDenominatorLabel ?? "population";
  return {
    key: `${options.keyPrefix}:${hit.kind}:${index}`,
    title: `${axisLabel}: ${range}`,
    detail: `${options.metricLabel}: ${options.formatMetric(total)} (${percentageFormat.format(share)} of ${shareDenominatorLabel})`,
    accessibleLabel: `${axisLabel}: ${range}. ${options.metricLabel}: ${options.formatMetric(total)}, ${percentageFormat.format(share)} of ${shareDenominatorLabel}.`,
    pinOnClick: false,
  };
};

const installJointChartInteraction = (
  container: HTMLElement,
  canvas: HTMLCanvasElement,
  density: JointDensity,
  geometry: JointChartGeometry,
  options: JointChartOptions,
): void => {
  const pinnedInspectionKey = canvas.hasAttribute(
    "data-distribution-inspection-pinned",
  )
    ? canvas.dataset.distributionInspectionKey
    : undefined;
  jointInteractionCleanup.get(canvas)?.();

  const highlight = document.createElement("span");
  highlight.className = "joint-inspection-highlight";
  highlight.hidden = true;
  highlight.setAttribute("aria-hidden", "true");
  container.append(highlight);

  let column = 0;
  let row = 0;
  let interacting = false;
  let maxWeight = -1;
  for (let candidateRow = 0; candidateRow < density.rows; candidateRow += 1) {
    for (
      let candidateColumn = 0;
      candidateColumn < density.columns;
      candidateColumn += 1
    ) {
      const weight =
        density.cells[candidateRow * density.columns + candidateColumn] ?? 0;
      if (weight > maxWeight) {
        maxWeight = weight;
        column = candidateColumn;
        row = candidateRow;
      }
    }
  }

  const showGridCell = (nextColumn: number, nextRow: number): void => {
    column = Math.min(density.columns - 1, Math.max(0, nextColumn));
    row = Math.min(density.rows - 1, Math.max(0, nextRow));
    const inspection = buildJointDensityCellInspection({
      density,
      xDomain: options.xDomain,
      yDomain: options.yDomain,
      column,
      row,
      keyPrefix: options.keyPrefix,
      xAxisLabel: options.xAxisLabel,
      yAxisLabel: options.yAxisLabel,
      metricLabel: options.metricLabel,
      ...(options.shareDenominatorLabel === undefined
        ? {}
        : { shareDenominatorLabel: options.shareDenominatorLabel }),
      formatXValue: options.formatXValue,
      formatYValue: options.formatYValue,
      formatMetric: options.formatMetric,
    });
    annotateDistributionInspectionTarget(canvas, {
      key: inspection.key,
      title: inspection.title,
      detail: inspection.detail,
      accessibleLabel: `${inspection.title}. ${inspection.detail}. Use the arrow keys to inspect neighbouring regions; press Enter to pin or unpin.`,
    });
    highlight.style.left = `${column * geometry.cellSpanX}px`;
    highlight.style.top = `${geometry.gridTop + geometry.gridHeight - (row + 1) * geometry.cellSpanY}px`;
    highlight.style.width = `${geometry.cellSpanX}px`;
    highlight.style.height = `${geometry.cellSpanY}px`;
    highlight.hidden = false;
  };

  const showMarginal = (
    hit:
      | { kind: "x-marginal"; column: number }
      | { kind: "y-marginal"; row: number },
  ): void => {
    const metadata = jointMarginalInspection(density, options, hit);
    annotateDistributionInspectionTarget(canvas, metadata);
    if (hit.kind === "x-marginal") {
      column = hit.column;
      highlight.style.left = `${column * geometry.cellSpanX}px`;
      highlight.style.top = "0";
      highlight.style.width = `${geometry.cellSpanX}px`;
      highlight.style.height = `${JOINT_MARGIN}px`;
    } else {
      row = hit.row;
      highlight.style.left = `${geometry.gridWidth + JOINT_GAP}px`;
      highlight.style.top = `${geometry.gridTop + geometry.gridHeight - (row + 1) * geometry.cellSpanY}px`;
      highlight.style.width = `${JOINT_MARGIN}px`;
      highlight.style.height = `${geometry.cellSpanY}px`;
    }
    highlight.hidden = false;
  };

  const showDefault = (): void => {
    showGridCell(column, row);
  };
  canvas.tabIndex = 0;
  canvas.setAttribute("role", "button");
  canvas.removeAttribute("aria-hidden");
  const pinnedCell = pinnedInspectionKey?.match(/:cell:(\d+):(\d+)$/);
  if (pinnedCell === undefined || pinnedCell === null) {
    showDefault();
    highlight.hidden = true;
  } else {
    showGridCell(Number(pinnedCell[1]), Number(pinnedCell[2]));
  }

  const pointerMove = (event: PointerEvent): void => {
    if (canvas.hasAttribute("data-distribution-inspection-pinned")) return;
    interacting = true;
    const bounds = canvas.getBoundingClientRect();
    const hit = hitTestJointChartCss({
      x: event.clientX - bounds.left,
      y: event.clientY - bounds.top,
      width: bounds.width,
      columns: density.columns,
      rows: density.rows,
    });
    if (hit === null) {
      highlight.hidden = true;
      annotateDistributionInspectionTarget(canvas, {
        key: `${options.keyPrefix}:gap`,
        title: "Between plotted regions",
        detail:
          "Move into the density grid or either marginal distribution to inspect its exact range.",
        accessibleLabel:
          "Between plotted regions. Move into the density grid or either marginal distribution to inspect its exact range.",
        pinOnClick: false,
      });
      return;
    }
    if (hit.kind === "grid") {
      showGridCell(hit.column, hit.row);
    } else {
      showMarginal(hit);
    }
  };
  const pointerLeave = (): void => {
    interacting = false;
    if (
      !canvas.hasAttribute("data-distribution-inspection-pinned") &&
      document.activeElement !== canvas
    ) {
      highlight.hidden = true;
    }
  };
  const focus = (): void => {
    if (!interacting) showDefault();
  };
  const blur = (): void => {
    if (!canvas.hasAttribute("data-distribution-inspection-pinned")) {
      highlight.hidden = true;
    }
  };
  const keyDown = (event: KeyboardEvent): void => {
    const pinned = canvas.hasAttribute("data-distribution-inspection-pinned");
    if (pinned && (event.key === "Enter" || event.key === " ")) {
      event.preventDefault();
      canvas.dispatchEvent(new MouseEvent("click", { bubbles: true }));
      return;
    }
    if (pinned) {
      if (
        [
          "ArrowLeft",
          "ArrowRight",
          "ArrowDown",
          "ArrowUp",
          "Home",
          "End",
        ].includes(event.key)
      ) {
        event.preventDefault();
      }
      return;
    }
    let handled = true;
    switch (event.key) {
      case "ArrowLeft":
        showGridCell(column - 1, row);
        break;
      case "ArrowRight":
        showGridCell(column + 1, row);
        break;
      case "ArrowDown":
        showGridCell(column, row - 1);
        break;
      case "ArrowUp":
        showGridCell(column, row + 1);
        break;
      case "Home":
        showGridCell(0, 0);
        break;
      case "End":
        showGridCell(density.columns - 1, density.rows - 1);
        break;
      case "Enter":
      case " ":
        canvas.dispatchEvent(new MouseEvent("click", { bubbles: true }));
        break;
      default:
        handled = false;
    }
    if (!handled) return;
    event.preventDefault();
    notifyDistributionInspectionTargetChanged(canvas);
  };

  canvas.addEventListener("pointermove", pointerMove);
  canvas.addEventListener("pointerdown", pointerMove);
  canvas.addEventListener("pointerleave", pointerLeave);
  canvas.addEventListener("focus", focus);
  canvas.addEventListener("blur", blur);
  canvas.addEventListener("keydown", keyDown);
  if (pinnedCell !== undefined && pinnedCell !== null) {
    notifyDistributionInspectionTargetChanged(canvas);
  }
  jointInteractionCleanup.set(canvas, () => {
    canvas.removeEventListener("pointermove", pointerMove);
    canvas.removeEventListener("pointerdown", pointerMove);
    canvas.removeEventListener("pointerleave", pointerLeave);
    canvas.removeEventListener("focus", focus);
    canvas.removeEventListener("blur", blur);
    canvas.removeEventListener("keydown", keyDown);
    highlight.remove();
  });
};

interface LinearInspectionItem {
  element: HTMLElement;
  metadata: DistributionInspectionMetadata;
  weight: number;
}

const installLinearInspectionKeyboard = (
  target: HTMLElement,
  items: readonly LinearInspectionItem[],
  accessibleLabel: string,
): void => {
  if (items.length === 0) return;
  let index = items.reduce(
    (largest, item, candidate) =>
      item.weight > (items[largest]?.weight ?? -1) ? candidate : largest,
    0,
  );
  const show = (nextIndex: number, active: boolean): void => {
    for (const item of items) {
      delete item.element.dataset.keyboardInspectionActive;
    }
    index = Math.min(items.length - 1, Math.max(0, nextIndex));
    const item = items[index];
    if (item === undefined) return;
    annotateDistributionInspectionTarget(target, {
      ...item.metadata,
      accessibleLabel: `${item.metadata.title}. ${item.metadata.detail}. ${accessibleLabel}`,
      pinOnClick: false,
    });
    if (active) item.element.dataset.keyboardInspectionActive = "true";
  };

  target.tabIndex = 0;
  target.setAttribute("role", "group");
  show(index, false);
  target.addEventListener("focus", () => show(index, true));
  target.addEventListener("blur", () => show(index, false));
  target.addEventListener("keydown", (event) => {
    if (event.target !== target) return;
    let nextIndex = index;
    switch (event.key) {
      case "ArrowLeft":
      case "ArrowUp":
        nextIndex -= 1;
        break;
      case "ArrowRight":
      case "ArrowDown":
        nextIndex += 1;
        break;
      case "Home":
        nextIndex = 0;
        break;
      case "End":
        nextIndex = items.length - 1;
        break;
      default:
        return;
    }
    event.preventDefault();
    show(nextIndex, true);
    notifyDistributionInspectionTargetChanged(target);
  });
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
    const passiveSegments: LinearInspectionItem[] = [];
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
      const summary = segmentTitle(bar, segment);
      const actionDetail = interactive ? " Click to open this bucket." : "";
      annotateDistributionInspectionTarget(cell, {
        key: `composition:${bar.classifierId}:${segment.key}`,
        title: `${bar.title} · ${segment.label}`,
        detail: `${summary}.${actionDetail}`,
        accessibleLabel: `${summary}.${actionDetail}`,
        pinOnClick: false,
      });
      if (interactive) {
        (cell as HTMLButtonElement).type = "button";
        cell.setAttribute(
          "aria-pressed",
          String(options.isSegmentSelected?.(bar, segment) === true),
        );
        cell.addEventListener("click", () => {
          options.onSegmentSelect?.(bar, segment);
        });
      } else {
        passiveSegments.push({
          element: cell,
          metadata: {
            key: `composition:${bar.classifierId}:${segment.key}`,
            title: `${bar.title} · ${segment.label}`,
            detail: summary,
            pinOnClick: false,
          },
          weight: segment.share,
        });
      }
      track.append(cell);
    }
    installLinearInspectionKeyboard(
      track,
      passiveSegments,
      "Use the arrow keys to inspect neighbouring segments.",
    );
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

export interface SpectrumChartOptions {
  domain: Readonly<LogDomain>;
  ticks: readonly AxisTick[];
  axisLabel: string;
  metricLabel: string;
  shareDenominatorLabel?: string;
  keyPrefix: string;
  formatRangeValue: (value: number) => string;
  metricFormat: (value: number) => string;
  emptyMessage: string;
}

export const formatFeeRateAxisValue = (value: number): string =>
  `${decimalFormat.format(value)} sat/vB`;

export const formatByteAxisValue = (value: number): string => {
  if (value >= 1_024) return `${decimalFormat.format(value / 1_024)} KiB`;
  return `${decimalFormat.format(value)} B`;
};

export const formatVirtualSizeAxisValue = (value: number): string => {
  if (value >= 1_024) return `${decimalFormat.format(value / 1_024)} KivB`;
  return `${decimalFormat.format(value)} vB`;
};

export const formatCountAxisValue = (value: number): string =>
  decimalFormat.format(value);

export const formatOutputValueAxisValue = (value: number): string =>
  value >= 100_000_000
    ? `${decimalFormat.format(value / 100_000_000)} BTC`
    : `${decimalFormat.format(value)} sats`;

const spectrumYAxis = (
  maxValue: number,
  formatValue: (value: number) => string,
  axisLabel: string,
): HTMLElement => {
  const axis = document.createElement("div");
  axis.className = "spectrum-y-axis";
  axis.setAttribute("aria-hidden", "true");
  const title = document.createElement("strong");
  title.className = "spectrum-y-axis-title";
  title.textContent = axisLabel;
  axis.append(title);

  const seen = new Set<string>();
  for (const { value, position, priority } of [
    { value: maxValue, position: 0, priority: 3 },
    { value: maxValue / 2, position: 0.5, priority: 2 },
    { value: 0, position: 1, priority: 3 },
  ]) {
    const label = formatValue(value);
    if (seen.has(label)) continue;
    seen.add(label);
    const tick = document.createElement("span");
    tick.textContent = label;
    tick.dataset.priority = String(priority);
    tick.style.top = `${position * 100}%`;
    if (position === 0) tick.dataset.edge = "top";
    if (position === 1) tick.dataset.edge = "bottom";
    axis.append(tick);
  }
  return axis;
};

export const renderSpectrumChart = (
  container: HTMLElement,
  spectrum: FeeSpectrum,
  options: SpectrumChartOptions,
): void => {
  if (spectrum.totalWeight === 0 || spectrum.maxBin === 0) {
    container.replaceChildren(emptyPanelState(options.emptyMessage));
    return;
  }
  const svg = svgElement("svg", {
    viewBox: `0 0 ${SPECTRUM_WIDTH} ${SPECTRUM_HEIGHT}`,
    class: "spectrum-chart",
    role: "button",
    tabindex: "0",
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
  for (const position of [0, 0.5]) {
    svg.append(
      svgElement("line", {
        x1: "0",
        y1: (position * SPECTRUM_HEIGHT + 0.5).toFixed(2),
        x2: String(SPECTRUM_WIDTH),
        y2: (position * SPECTRUM_HEIGHT + 0.5).toFixed(2),
        class: "chart-gridline chart-gridline-horizontal",
      }),
    );
  }
  const binCount = spectrum.bins.length;
  const step = SPECTRUM_WIDTH / binCount;
  const barWidth = Math.max(1, step - 2);
  for (const tick of options.ticks) {
    const line = svgElement("line", {
      x1: (tick.position * SPECTRUM_WIDTH).toFixed(2),
      y1: "0",
      x2: (tick.position * SPECTRUM_WIDTH).toFixed(2),
      y2: String(SPECTRUM_HEIGHT),
      class: "chart-gridline",
    });
    if (tick.description !== undefined) line.dataset.reference = "true";
    svg.append(line);
  }
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
      svg.append(rect);
    }
  }
  const highlight = svgElement("rect", {
    class: "spectrum-inspection-highlight",
    y: "0",
    height: String(SPECTRUM_HEIGHT),
    width: step.toFixed(2),
  });
  highlight.setAttribute("visibility", "hidden");
  svg.append(highlight);

  let bin = spectrum.bins.reduce(
    (largest, candidate, index) =>
      candidate.total > (spectrum.bins[largest]?.total ?? -1) ? index : largest,
    0,
  );
  let interacting = false;
  const showBin = (nextBin: number): void => {
    bin = Math.min(binCount - 1, Math.max(0, nextBin));
    const inspection = buildSpectrumBinInspection({
      spectrum,
      domain: options.domain,
      bin,
      keyPrefix: options.keyPrefix,
      axisLabel: options.axisLabel,
      metricLabel: options.metricLabel,
      ...(options.shareDenominatorLabel === undefined
        ? {}
        : { shareDenominatorLabel: options.shareDenominatorLabel }),
      formatRangeValue: options.formatRangeValue,
      formatMetric: options.metricFormat,
    });
    annotateDistributionInspectionTarget(svg, {
      key: inspection.key,
      title: inspection.title,
      detail: inspection.detail,
      accessibleLabel: `${inspection.title}. ${inspection.detail}. Use the Left and Right Arrow keys to inspect neighbouring bins; press Enter to pin or unpin.`,
    });
    highlight.setAttribute("x", (bin * step).toFixed(2));
    highlight.setAttribute("visibility", "visible");
  };
  showBin(bin);
  highlight.setAttribute("visibility", "hidden");

  const pointerMove = (event: PointerEvent): void => {
    if (svg.hasAttribute("data-distribution-inspection-pinned")) return;
    const bounds = svg.getBoundingClientRect();
    const hit = hitTestSpectrumX({
      x: event.clientX - bounds.left,
      width: bounds.width,
      binCount,
    });
    if (hit === null) return;
    interacting = true;
    showBin(hit);
  };
  const pointerLeave = (): void => {
    interacting = false;
    if (
      !svg.hasAttribute("data-distribution-inspection-pinned") &&
      document.activeElement !== svg
    ) {
      highlight.setAttribute("visibility", "hidden");
    }
  };
  const focus = (): void => {
    if (!interacting) showBin(bin);
  };
  const blur = (): void => {
    if (!svg.hasAttribute("data-distribution-inspection-pinned")) {
      highlight.setAttribute("visibility", "hidden");
    }
  };
  const keyDown = (event: KeyboardEvent): void => {
    const pinned = svg.hasAttribute("data-distribution-inspection-pinned");
    if (pinned && (event.key === "Enter" || event.key === " ")) {
      event.preventDefault();
      svg.dispatchEvent(new MouseEvent("click", { bubbles: true }));
      return;
    }
    if (pinned) {
      if (["ArrowLeft", "ArrowRight", "Home", "End"].includes(event.key)) {
        event.preventDefault();
      }
      return;
    }
    let handled = true;
    switch (event.key) {
      case "ArrowLeft":
        showBin(bin - 1);
        break;
      case "ArrowRight":
        showBin(bin + 1);
        break;
      case "Home":
        showBin(0);
        break;
      case "End":
        showBin(binCount - 1);
        break;
      case "Enter":
      case " ":
        svg.dispatchEvent(new MouseEvent("click", { bubbles: true }));
        break;
      default:
        handled = false;
    }
    if (!handled) return;
    event.preventDefault();
    notifyDistributionInspectionTargetChanged(svg);
  };
  svg.addEventListener("pointermove", pointerMove);
  svg.addEventListener("pointerdown", pointerMove);
  svg.addEventListener("pointerleave", pointerLeave);
  svg.addEventListener("focus", focus);
  svg.addEventListener("blur", blur);
  svg.addEventListener("keydown", keyDown);
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
  const plot = document.createElement("div");
  plot.className = "spectrum-plot-layout";
  plot.append(
    spectrumYAxis(spectrum.maxBin, options.metricFormat, options.metricLabel),
    svg,
    axisTickRow(options.ticks, options.axisLabel),
  );
  container.replaceChildren(plot, legend);
};

export interface MosaicRenderOptions {
  metricFormat: (value: number) => string;
  emptyMessage: string;
  onColumnSelect?: (column: { key: string; label: string }) => void;
  isColumnInteractive?:
    boolean | ((column: AgeMosaic["columns"][number]) => boolean);
  isColumnSelected?: (column: AgeMosaic["columns"][number]) => boolean;
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
  const passiveColumns: LinearInspectionItem[] = [];
  for (const column of mosaic.columns) {
    const interactive =
      options.onColumnSelect !== undefined &&
      (typeof options.isColumnInteractive === "function"
        ? options.isColumnInteractive(column)
        : options.isColumnInteractive === true);
    const columnElement = document.createElement(
      interactive ? "button" : "div",
    );
    columnElement.className = "mosaic-column";
    columnElement.dataset.column = column.key;
    columnElement.style.flexGrow = String(Math.max(column.share, 0.004));
    const summary = `${column.label} — ${countFormat.format(column.count)} tx · ${options.metricFormat(column.weight)} · ${percentageFormat.format(column.share)}`;
    const ageBreakdown = column.cells
      .map(
        (cell) =>
          `${cell.bandLabel}: ${countFormat.format(cell.count)} tx · ${options.metricFormat(cell.weight)} · ${percentageFormat.format(cell.share)} of bucket`,
      )
      .join("; ");
    const accessibleSummary = `${summary}. Age breakdown: ${ageBreakdown}`;
    annotateDistributionInspectionTarget(columnElement, {
      key: `mosaic:${column.key}`,
      title: column.label,
      detail: `${accessibleSummary}.${interactive ? " Click to open this bucket." : ""}`,
      accessibleLabel: `${accessibleSummary}.${interactive ? " Click to open this bucket." : ""}`,
      pinOnClick: false,
    });
    if (interactive) {
      (columnElement as HTMLButtonElement).type = "button";
      columnElement.setAttribute(
        "aria-pressed",
        String(options.isColumnSelected?.(column) === true),
      );
      columnElement.addEventListener("click", () => {
        options.onColumnSelect?.({ key: column.key, label: column.label });
      });
    } else {
      passiveColumns.push({
        element: columnElement,
        metadata: {
          key: `mosaic:${column.key}`,
          title: column.label,
          detail: accessibleSummary,
          pinOnClick: false,
        },
        weight: column.weight,
      });
    }
    for (const cell of column.cells) {
      const cellElement = document.createElement("span");
      cellElement.className = "mosaic-cell";
      cellElement.style.flexGrow = String(Math.max(cell.share, 0.01));
      cellElement.style.background = cell.bandColor;
      const cellSummary = `${countFormat.format(cell.count)} tx · ${options.metricFormat(cell.weight)} · ${percentageFormat.format(cell.share)} of bucket`;
      annotateDistributionInspectionTarget(cellElement, {
        key: `mosaic:${column.key}:${cell.bandKey}`,
        title: `${column.label} · ${cell.bandLabel}`,
        detail: cellSummary,
        pinOnClick: false,
      });
      columnElement.append(cellElement);
    }
    const marker = document.createElement("i");
    marker.className = "mosaic-column-marker";
    marker.style.background = column.color;
    columnElement.append(marker);
    board.append(columnElement);
  }
  installLinearInspectionKeyboard(
    board,
    passiveColumns,
    "Use the arrow keys to inspect neighbouring columns.",
  );
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
