import type { ComparisonSide } from "./comparison-model";
import {
  KNOTS_BIP110_CLASSIFIER_ID,
  type ClassifierBucketKey,
} from "./classifier-terrain";
import {
  DEFAULT_JOINT_COLOR,
  feeRateAxisRow,
  formatByteAxisValue,
  formatCountAxisValue,
  formatFeeRateAxisValue,
  formatOutputValueAxisValue,
  formatVirtualSizeAxisValue,
  invalidateJointChart,
  panelAxisRow,
  renderCompositionBars,
  renderMosaicChart,
  renderJointChart,
  renderSpectrumChart,
} from "./detail-panels";
import {
  DATA_BYTES_TICKS,
  DATA_BYTES_DOMAIN,
  FEE_RATE_DOMAIN,
  FEE_RATE_TICKS,
  IO_COUNT_DOMAIN,
  IO_COUNT_TICKS,
  OUTPUT_VALUE_DOMAIN,
  OUTPUT_VALUE_TICKS,
  VSIZE_DOMAIN,
  VSIZE_TICKS,
  type JointDensity,
} from "./fee-distribution";
import { countFormat, formatVsize } from "./format";
import type { SnapshotDistributionModel } from "./snapshot-distributions";
import type { TerrainMode } from "./terrain";
import type { ClassifierDescriptor } from "./types";

const COMPARISON_EMPTY_MESSAGE =
  "This population has no members on this source.";

interface TitledPanel {
  title: HTMLElement;
  note: HTMLElement;
  container: HTMLElement;
}

interface CanvasPanel extends TitledPanel {
  canvas: HTMLCanvasElement;
  yAxis: HTMLElement;
}

export interface ComparisonDistributionSidePanels {
  composition: TitledPanel;
  data: TitledPanel;
  complexity: CanvasPanel;
  entanglement: TitledPanel;
  value: TitledPanel;
  joint: CanvasPanel;
  package: TitledPanel;
  spectrum: TitledPanel;
  mosaic: TitledPanel;
}

export type ComparisonDistributionPanels = Record<
  ComparisonSide,
  ComparisonDistributionSidePanels
>;

const requiredDescendant = <T extends HTMLElement>(
  root: HTMLElement,
  id: string,
): T => {
  const element = root.querySelector(`#${id}`);
  if (!(element instanceof HTMLElement)) {
    throw new Error(`Missing required comparison distribution element #${id}`);
  }
  return element as T;
};

const titledPanel = (
  root: HTMLElement,
  side: ComparisonSide,
  titleName: string,
  containerName: string,
): TitledPanel => ({
  title: requiredDescendant(root, `dist-${titleName}-${side}-title`),
  note: requiredDescendant(root, `dist-${titleName}-${side}-note`),
  container: requiredDescendant(root, `${containerName}-${side}`),
});

const canvasPanel = (
  root: HTMLElement,
  side: ComparisonSide,
  name: string,
): CanvasPanel => ({
  ...titledPanel(root, side, name, name),
  canvas: requiredDescendant(root, `${name}-${side}-canvas`),
  yAxis: requiredDescendant(root, `${name}-${side}-y-axis`),
});

const createSidePanels = (
  root: HTMLElement,
  side: ComparisonSide,
): ComparisonDistributionSidePanels => ({
  composition: titledPanel(root, side, "comp", "composition"),
  data: titledPanel(root, side, "data", "data"),
  complexity: canvasPanel(root, side, "complexity"),
  entanglement: titledPanel(root, side, "entanglement", "entanglement"),
  value: titledPanel(root, side, "value", "value"),
  joint: canvasPanel(root, side, "joint"),
  package: titledPanel(root, side, "package", "package"),
  spectrum: titledPanel(root, side, "spectrum", "spectrum"),
  mosaic: titledPanel(root, side, "mosaic", "mosaic"),
});

export const createComparisonDistributionPanels = (
  root: HTMLElement,
): ComparisonDistributionPanels => {
  const panels = {
    left: createSidePanels(root, "left"),
    right: createSidePanels(root, "right"),
  };

  panels.left.joint.container.append(feeRateAxisRow());
  panels.right.joint.container.append(feeRateAxisRow());
  panels.left.complexity.container.append(
    panelAxisRow(IO_COUNT_TICKS, "Inputs"),
  );
  panels.right.complexity.container.append(
    panelAxisRow(IO_COUNT_TICKS, "Inputs"),
  );

  return panels;
};

interface ComparisonDistributionPanelSelection {
  classifierId: string | null;
  bucketKey: ClassifierBucketKey | null;
  metric: TerrainMode;
}

interface ComparisonDistributionPanelCommit {
  panels: ComparisonDistributionSidePanels;
  model: SnapshotDistributionModel;
  side: ComparisonSide;
  sourceLabel: string;
  scopeSuffix: string;
  selection: ComparisonDistributionPanelSelection;
  descriptor: ClassifierDescriptor | null;
  onSelectBucket: (selection: {
    classifierId: string;
    bucketKey: ClassifierBucketKey;
  }) => void;
}

export const commitComparisonDistributionSide = ({
  panels,
  model,
  side,
  sourceLabel,
  scopeSuffix,
  selection,
  descriptor,
  onSelectBucket,
}: ComparisonDistributionPanelCommit): void => {
  const label = `${sourceLabel}${scopeSuffix}`;
  const metricNoun =
    selection.metric === "count" ? "transaction count" : "virtual size";
  const metricLabel =
    selection.metric === "count" ? "Transaction count" : "Virtual size";
  const metricFormat =
    selection.metric === "count"
      ? (value: number): string => `${countFormat.format(value)} tx`
      : formatVsize;
  const lensName = descriptor?.title ?? "the selected classifier";
  const interactive =
    selection.classifierId !== null &&
    selection.classifierId !== KNOTS_BIP110_CLASSIFIER_ID;
  const coverage = `structure facts cover ${countFormat.format(model.totals.structured.count)} of ${countFormat.format(model.totals.population.count)} transactions`;
  panels.composition.title.textContent = `Composition by lens · ${label}`;
  panels.data.title.textContent = `Data carriage · ${label}`;
  panels.complexity.title.textContent = `Inputs × outputs · ${label}`;
  panels.entanglement.title.textContent = `Entanglement · ${label}`;
  panels.value.title.textContent = `Total output value · ${label}`;
  panels.spectrum.title.textContent = `Fee structure · ${label}`;
  panels.package.title.textContent = `Ancestor fee rate · ${label}`;
  panels.joint.title.textContent = `Fee rate × size · ${label}`;
  panels.mosaic.title.textContent = `Bucket × age · ${label}`;

  panels.composition.note.textContent = `Share of ${metricNoun} per independent classifier lens. Select a segment to use that lens for the other charts.`;
  renderCompositionBars(panels.composition.container, model.composition, {
    metricLabel: metricNoun,
    emptyMessage: COMPARISON_EMPTY_MESSAGE,
    isSegmentInteractive: (bar) =>
      bar.classifierId !== KNOTS_BIP110_CLASSIFIER_ID,
    isSegmentSelected: (bar, segment) =>
      bar.classifierId === selection.classifierId &&
      segment.key === selection.bucketKey,
    onSegmentSelect: (bar, segment) => {
      onSelectBucket({
        classifierId: bar.classifierId,
        bucketKey: segment.key as ClassifierBucketKey,
      });
    },
  });
  panels.spectrum.note.textContent = `Stacked ${metricNoun} per log fee-rate bin by ${lensName} bucket.`;
  renderSpectrumChart(panels.spectrum.container, model.feeSpectrum, {
    domain: FEE_RATE_DOMAIN,
    ticks: FEE_RATE_TICKS,
    axisLabel: "Fee rate",
    metricLabel,
    keyPrefix: `comparison:${side}:fee-rate`,
    formatRangeValue: formatFeeRateAxisValue,
    metricFormat,
    emptyMessage: COMPARISON_EMPTY_MESSAGE,
  });
  panels.package.note.textContent = `Ancestor fee rate by ${lensName} bucket: delta-adjusted ancestor fees over ancestor virtual size.`;
  renderSpectrumChart(panels.package.container, model.ancestorFeeSpectrum, {
    domain: FEE_RATE_DOMAIN,
    ticks: FEE_RATE_TICKS,
    axisLabel: "Ancestor fee rate",
    metricLabel,
    keyPrefix: `comparison:${side}:ancestor-fee-rate`,
    formatRangeValue: formatFeeRateAxisValue,
    metricFormat,
    emptyMessage: COMPARISON_EMPTY_MESSAGE,
  });
  panels.data.note.textContent = `Stacked ${metricNoun} per log carried-byte bin by Data protocols bucket · ${countFormat.format(model.totals.carrier.count)} transactions carry OP_RETURN bytes. References describe conventional pushed-payload forms; Atlas plots carried bytes summed across OP_RETURN outputs, not serialized script size.`;
  renderSpectrumChart(panels.data.container, model.dataSpectrum, {
    domain: DATA_BYTES_DOMAIN,
    ticks: DATA_BYTES_TICKS,
    axisLabel: "Carried bytes",
    metricLabel,
    shareDenominatorLabel: "OP_RETURN carriers",
    keyPrefix: `comparison:${side}:data-carriage`,
    formatRangeValue: formatByteAxisValue,
    metricFormat,
    emptyMessage: "No observed transaction carries OP_RETURN data.",
  });
  panels.entanglement.note.textContent = `Unconfirmed mempool relatives reported by this source · ${countFormat.format(model.totals.replaceable.count)} transactions are reported replaceable.`;
  renderCompositionBars(panels.entanglement.container, model.entanglement, {
    metricLabel: metricNoun,
    emptyMessage: COMPARISON_EMPTY_MESSAGE,
  });
  panels.value.note.textContent = `Total output value per transaction by ${lensName} bucket · ${coverage}.`;
  renderSpectrumChart(panels.value.container, model.valueSpectrum, {
    domain: OUTPUT_VALUE_DOMAIN,
    ticks: OUTPUT_VALUE_TICKS,
    axisLabel: "Total output value",
    metricLabel,
    shareDenominatorLabel: "transactions with structure facts",
    keyPrefix: `comparison:${side}:output-value`,
    formatRangeValue: formatOutputValueAxisValue,
    metricFormat,
    emptyMessage: "No structure facts are available yet.",
  });
  panels.mosaic.note.textContent = `Column width is each ${lensName} bucket's ${metricNoun} share; cells split the bucket by age at observation.`;
  renderMosaicChart(panels.mosaic.container, model.ageMosaic, {
    metricFormat,
    emptyMessage: COMPARISON_EMPTY_MESSAGE,
    isColumnInteractive: (column) =>
      interactive && column.key !== "all" && column.key !== "overflow",
    isColumnSelected: (column) => column.key === selection.bucketKey,
    onColumnSelect: (column) => {
      if (selection.classifierId === null) return;
      onSelectBucket({
        classifierId: selection.classifierId,
        bucketKey: column.key as ClassifierBucketKey,
      });
    },
  });
  panels.joint.note.textContent = `Density of ${metricNoun} across fee rate and size, with marginals, normalized within this source.`;
  panels.complexity.note.textContent = `Density of ${metricNoun} across input and output counts, with marginals · ${coverage}.`;
};

export const renderComparisonDistributionDensity = (
  panels: ComparisonDistributionSidePanels,
  side: ComparisonSide,
  density: JointDensity,
  complexity: boolean,
  metric: TerrainMode,
): void => {
  const panel = complexity ? panels.complexity : panels.joint;
  const metricLabel = metric === "count" ? "Transaction count" : "Virtual size";
  const metricFormat =
    metric === "count"
      ? (value: number): string => `${countFormat.format(value)} tx`
      : formatVsize;
  renderJointChart(panel.container, panel.canvas, density, {
    color: DEFAULT_JOINT_COLOR,
    emptyMessage: complexity
      ? "No structure facts are available yet."
      : "This sampled mempool is empty.",
    keyPrefix: `comparison:${side}:${complexity ? "input-output" : "fee-size"}`,
    xDomain: complexity ? IO_COUNT_DOMAIN : FEE_RATE_DOMAIN,
    yDomain: complexity ? IO_COUNT_DOMAIN : VSIZE_DOMAIN,
    xAxisLabel: complexity ? "Inputs" : "Fee rate",
    yAxisLabel: complexity ? "Outputs" : "Virtual size",
    metricLabel,
    ...(complexity
      ? { shareDenominatorLabel: "transactions with structure facts" }
      : {}),
    formatXValue: complexity ? formatCountAxisValue : formatFeeRateAxisValue,
    formatYValue: complexity
      ? formatCountAxisValue
      : formatVirtualSizeAxisValue,
    formatMetric: metricFormat,
    yAxis: panel.yAxis,
    yTicks: complexity ? IO_COUNT_TICKS : VSIZE_TICKS,
  });
};

export const invalidateComparisonDistributionDensities = (
  panels: ComparisonDistributionPanels,
): void => {
  for (const side of ["left", "right"] as const) {
    invalidateJointChart(
      panels[side].joint.container,
      panels[side].joint.canvas,
      panels[side].joint.yAxis,
    );
    invalidateJointChart(
      panels[side].complexity.container,
      panels[side].complexity.canvas,
      panels[side].complexity.yAxis,
    );
  }
};

export const resetComparisonDistributionSide = (
  panels: ComparisonDistributionSidePanels,
): void => {
  panels.composition.container.replaceChildren();
  panels.spectrum.container.replaceChildren();
  panels.package.container.replaceChildren();
  panels.mosaic.container.replaceChildren();
  panels.data.container.replaceChildren();
  panels.entanglement.container.replaceChildren();
  panels.value.container.replaceChildren();
};
