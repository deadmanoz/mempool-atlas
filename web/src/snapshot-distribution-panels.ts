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
  prepareJointChartCanvas,
  renderCompositionBars,
  renderJointChart,
  renderMosaicChart,
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
import type { ClassifierDescriptor, MempoolSnapshot } from "./types";

export const EMPTY_SNAPSHOT_MESSAGE =
  "This snapshot contains an empty mempool.";

export interface SnapshotDistributionPanelSelection {
  classifierId: string;
  bucketKey: ClassifierBucketKey | null;
  metric: TerrainMode;
}

export interface SnapshotDistributionPanelElements {
  jointNote: HTMLElement;
  composition: HTMLElement;
  compositionNote: HTMLElement;
  spectrumChart: HTMLElement;
  spectrumNote: HTMLElement;
  packageChart: HTMLElement;
  packageNote: HTMLElement;
  mosaicChart: HTMLElement;
  mosaicNote: HTMLElement;
  dataChart: HTMLElement;
  dataNote: HTMLElement;
  jointChart: HTMLElement;
  jointCanvas: HTMLCanvasElement;
  jointYAxis: HTMLElement;
  complexityChart: HTMLElement;
  complexityCanvas: HTMLCanvasElement;
  complexityYAxis: HTMLElement;
  complexityNote: HTMLElement;
  entanglement: HTMLElement;
  entanglementNote: HTMLElement;
  valueChart: HTMLElement;
  valueNote: HTMLElement;
}

interface SnapshotDistributionPanelCommit {
  elements: SnapshotDistributionPanelElements;
  model: SnapshotDistributionModel;
  snapshot: MempoolSnapshot;
  selection: SnapshotDistributionPanelSelection;
  descriptor: ClassifierDescriptor | null;
  onSelectBucket: (selection: {
    classifierId: string;
    bucketKey: ClassifierBucketKey;
  }) => void;
}

const requiredDescendant = <T extends HTMLElement>(
  root: HTMLElement,
  id: string,
): T => {
  const element = root.querySelector(`#${id}`);
  if (!(element instanceof HTMLElement)) {
    throw new Error(`Missing required element #snapshot-distributions #${id}`);
  }
  return element as T;
};

export const createSnapshotDistributionPanelElements = (
  root: HTMLElement,
  scheduleJointRender: () => void,
): SnapshotDistributionPanelElements => {
  const elements = {
    jointNote: requiredDescendant<HTMLElement>(root, "joint-note"),
    composition: requiredDescendant<HTMLElement>(root, "composition-bars"),
    compositionNote: requiredDescendant<HTMLElement>(root, "composition-note"),
    spectrumChart: requiredDescendant<HTMLElement>(root, "spectrum-chart"),
    spectrumNote: requiredDescendant<HTMLElement>(root, "spectrum-note"),
    packageChart: requiredDescendant<HTMLElement>(root, "package-chart"),
    packageNote: requiredDescendant<HTMLElement>(root, "package-note"),
    mosaicChart: requiredDescendant<HTMLElement>(root, "mosaic-chart"),
    mosaicNote: requiredDescendant<HTMLElement>(root, "mosaic-note"),
    dataChart: requiredDescendant<HTMLElement>(root, "data-chart"),
    dataNote: requiredDescendant<HTMLElement>(root, "data-note"),
    jointChart: requiredDescendant<HTMLElement>(root, "joint-chart"),
    jointCanvas: requiredDescendant<HTMLCanvasElement>(root, "joint-canvas"),
    jointYAxis: requiredDescendant<HTMLElement>(root, "joint-y-axis"),
    complexityChart: requiredDescendant<HTMLElement>(root, "complexity-chart"),
    complexityCanvas: requiredDescendant<HTMLCanvasElement>(
      root,
      "complexity-canvas",
    ),
    complexityYAxis: requiredDescendant<HTMLElement>(root, "complexity-y-axis"),
    complexityNote: requiredDescendant<HTMLElement>(root, "complexity-note"),
    entanglement: requiredDescendant<HTMLElement>(root, "entanglement-bars"),
    entanglementNote: requiredDescendant<HTMLElement>(
      root,
      "entanglement-note",
    ),
    valueChart: requiredDescendant<HTMLElement>(root, "value-chart"),
    valueNote: requiredDescendant<HTMLElement>(root, "value-note"),
  };

  new ResizeObserver(scheduleJointRender).observe(elements.jointChart);
  new ResizeObserver(scheduleJointRender).observe(elements.complexityChart);
  elements.jointChart.append(feeRateAxisRow());
  elements.complexityChart.append(panelAxisRow(IO_COUNT_TICKS));
  return elements;
};

export const renderSnapshotDistributionJointPanels = (
  elements: SnapshotDistributionPanelElements,
  jointDensity: JointDensity | null,
  complexityDensity: JointDensity | null,
  metric: TerrainMode,
): void => {
  const metricLabel = metric === "count" ? "Transaction count" : "Virtual size";
  const metricFormat =
    metric === "count"
      ? (value: number): string => `${countFormat.format(value)} tx`
      : formatVsize;
  if (jointDensity !== null) {
    renderJointChart(elements.jointChart, elements.jointCanvas, jointDensity, {
      color: DEFAULT_JOINT_COLOR,
      emptyMessage: EMPTY_SNAPSHOT_MESSAGE,
      keyPrefix: "snapshot:fee-size",
      xDomain: FEE_RATE_DOMAIN,
      yDomain: VSIZE_DOMAIN,
      xAxisLabel: "Fee rate",
      yAxisLabel: "Virtual size",
      metricLabel,
      formatXValue: formatFeeRateAxisValue,
      formatYValue: formatVirtualSizeAxisValue,
      formatMetric: metricFormat,
      yAxis: elements.jointYAxis,
      yTicks: VSIZE_TICKS,
    });
  }
  if (complexityDensity !== null) {
    renderJointChart(
      elements.complexityChart,
      elements.complexityCanvas,
      complexityDensity,
      {
        color: DEFAULT_JOINT_COLOR,
        emptyMessage: "No structure facts are available yet.",
        keyPrefix: "snapshot:input-output",
        xDomain: IO_COUNT_DOMAIN,
        yDomain: IO_COUNT_DOMAIN,
        xAxisLabel: "Inputs",
        yAxisLabel: "Outputs",
        metricLabel,
        shareDenominatorLabel: "transactions with structure facts",
        formatXValue: formatCountAxisValue,
        formatYValue: formatCountAxisValue,
        formatMetric: metricFormat,
        yAxis: elements.complexityYAxis,
        yTicks: IO_COUNT_TICKS,
      },
    );
  }
};

export const prepareSnapshotDistributionJointCanvases = (
  elements: SnapshotDistributionPanelElements,
  jointDensity: JointDensity | null,
  complexityDensity: JointDensity | null,
): void => {
  if (jointDensity !== null) {
    prepareJointChartCanvas(elements.jointCanvas, jointDensity);
  }
  if (complexityDensity !== null) {
    prepareJointChartCanvas(elements.complexityCanvas, complexityDensity);
  }
};

export const invalidateSnapshotDistributionJointPanels = (
  elements: SnapshotDistributionPanelElements,
): void => {
  invalidateJointChart(
    elements.jointChart,
    elements.jointCanvas,
    elements.jointYAxis,
  );
  invalidateJointChart(
    elements.complexityChart,
    elements.complexityCanvas,
    elements.complexityYAxis,
  );
};

export const syncSnapshotDistributionSelection = (
  composition: HTMLElement,
  mosaic: HTMLElement,
  selection: SnapshotDistributionPanelSelection | null,
): void => {
  for (const button of composition.querySelectorAll<HTMLButtonElement>(
    "button.composition-segment",
  )) {
    const selected =
      selection?.bucketKey !== null &&
      button.dataset.classifier === selection?.classifierId &&
      button.dataset.segment === selection?.bucketKey;
    button.setAttribute("aria-pressed", String(selected));
  }
  for (const button of mosaic.querySelectorAll<HTMLButtonElement>(
    "button.mosaic-column",
  )) {
    button.setAttribute(
      "aria-pressed",
      String(button.dataset.column === selection?.bucketKey),
    );
  }
};

export const commitSnapshotDistributionPanels = ({
  elements,
  model,
  snapshot,
  selection,
  descriptor,
  onSelectBucket,
}: SnapshotDistributionPanelCommit): void => {
  const metricNoun =
    selection.metric === "count" ? "transaction count" : "virtual size";
  const metricFormat = (value: number): string =>
    selection.metric === "count"
      ? `${countFormat.format(value)} tx`
      : formatVsize(value);
  const lensName = descriptor?.title ?? "the selected classifier";
  const interactive = selection.classifierId !== KNOTS_BIP110_CLASSIFIER_ID;

  renderSpectrumChart(elements.spectrumChart, model.feeSpectrum, {
    domain: FEE_RATE_DOMAIN,
    ticks: FEE_RATE_TICKS,
    axisLabel: "Fee rate",
    metricLabel: metricNoun,
    keyPrefix: "snapshot:fee-rate",
    formatRangeValue: formatFeeRateAxisValue,
    metricFormat,
    emptyMessage: EMPTY_SNAPSHOT_MESSAGE,
  });
  elements.spectrumNote.textContent = `Stacked ${metricNoun} per log fee-rate bin by ${lensName} bucket.`;

  renderSpectrumChart(elements.packageChart, model.ancestorFeeSpectrum, {
    domain: FEE_RATE_DOMAIN,
    ticks: FEE_RATE_TICKS,
    axisLabel: "Ancestor fee rate",
    metricLabel: metricNoun,
    keyPrefix: "snapshot:ancestor-fee-rate",
    formatRangeValue: formatFeeRateAxisValue,
    metricFormat,
    emptyMessage: EMPTY_SNAPSHOT_MESSAGE,
  });
  elements.packageNote.textContent = `Ancestor fee rate by ${lensName} bucket: delta-adjusted ancestor fees over ancestor virtual size. This is not the cluster mempool mining score.`;
  elements.jointNote.textContent = `Density of ${metricNoun} across fee rate and size, with marginals.`;

  renderMosaicChart(elements.mosaicChart, model.ageMosaic, {
    metricFormat,
    emptyMessage: EMPTY_SNAPSHOT_MESSAGE,
    isColumnInteractive: (column) =>
      interactive && column.key !== "all" && column.key !== "overflow",
    isColumnSelected: (column) => column.key === selection.bucketKey,
    onColumnSelect: (column) => {
      onSelectBucket({
        classifierId: selection.classifierId,
        bucketKey: column.key as ClassifierBucketKey,
      });
    },
  });
  elements.mosaicNote.textContent = `Column width is each ${lensName} bucket's ${metricNoun} share; cells split the bucket by age at observation.`;

  const coverage = `structure facts cover ${countFormat.format(model.totals.structured.count)} of ${countFormat.format(snapshot.transaction_count)} transactions`;
  renderSpectrumChart(elements.dataChart, model.dataSpectrum, {
    domain: DATA_BYTES_DOMAIN,
    ticks: DATA_BYTES_TICKS,
    axisLabel: "Carried bytes",
    metricLabel: metricNoun,
    shareDenominatorLabel: "OP_RETURN carriers",
    keyPrefix: "snapshot:data-carriage",
    formatRangeValue: formatByteAxisValue,
    metricFormat,
    emptyMessage: "No observed transaction carries OP_RETURN data.",
  });
  elements.dataNote.textContent = `Stacked ${metricNoun} per log carried-byte bin by Data protocols bucket · ${countFormat.format(model.totals.carrier.count)} transactions carry OP_RETURN bytes. References describe conventional pushed-payload forms; Atlas plots carried bytes summed across OP_RETURN outputs, not serialized script size.`;
  elements.complexityNote.textContent = `Density of ${metricNoun} across input and output counts, with marginals · ${coverage}.`;

  renderCompositionBars(elements.entanglement, model.entanglement, {
    metricLabel: metricNoun,
    emptyMessage: EMPTY_SNAPSHOT_MESSAGE,
  });
  elements.entanglementNote.textContent = `Unconfirmed mempool relatives reported by the source node · ${countFormat.format(model.totals.replaceable.count)} transactions are reported replaceable.`;

  renderSpectrumChart(elements.valueChart, model.valueSpectrum, {
    domain: OUTPUT_VALUE_DOMAIN,
    ticks: OUTPUT_VALUE_TICKS,
    axisLabel: "Total output value",
    metricLabel: metricNoun,
    shareDenominatorLabel: "transactions with structure facts",
    keyPrefix: "snapshot:output-value",
    formatRangeValue: formatOutputValueAxisValue,
    metricFormat,
    emptyMessage: "No structure facts are available yet.",
  });
  elements.valueNote.textContent = `Total output value per transaction by ${lensName} bucket · ${coverage}.`;

  elements.compositionNote.textContent = `Share of ${metricNoun} per independent classifier lens. Lenses are not combined.`;
  renderCompositionBars(elements.composition, model.composition, {
    metricLabel: metricNoun,
    emptyMessage: EMPTY_SNAPSHOT_MESSAGE,
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
};
