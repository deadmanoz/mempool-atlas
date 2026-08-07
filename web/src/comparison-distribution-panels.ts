import type { ComparisonSide } from "./comparison-model";
import {
  feeRateAxisRow,
  panelAxisRow,
  renderCompositionBars,
  renderMosaicChart,
  renderSpectrumChart,
} from "./detail-panels";
import {
  DATA_BYTES_TICKS,
  FEE_RATE_TICKS,
  IO_COUNT_TICKS,
  OUTPUT_VALUE_TICKS,
} from "./fee-distribution";
import { formatVsize } from "./format";
import type { SnapshotDistributionModel } from "./snapshot-distributions";

const COMPARISON_EMPTY_MESSAGE =
  "This population has no members on this source.";

interface TitledPanel {
  title: HTMLElement;
  container: HTMLElement;
}

interface CanvasPanel extends TitledPanel {
  canvas: HTMLCanvasElement;
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
  container: requiredDescendant(root, `${containerName}-${side}`),
});

const canvasPanel = (
  root: HTMLElement,
  side: ComparisonSide,
  name: string,
): CanvasPanel => ({
  ...titledPanel(root, side, name, name),
  canvas: requiredDescendant(root, `${name}-${side}-canvas`),
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
  panels.left.complexity.container.append(panelAxisRow(IO_COUNT_TICKS));
  panels.right.complexity.container.append(panelAxisRow(IO_COUNT_TICKS));

  return panels;
};

export const commitComparisonDistributionSide = (
  panels: ComparisonDistributionSidePanels,
  model: SnapshotDistributionModel,
  sourceLabel: string,
  scopeSuffix: string,
): void => {
  const label = `${sourceLabel}${scopeSuffix}`;
  panels.composition.title.textContent = `Composition by lens · ${label}`;
  panels.data.title.textContent = `Data carriage · ${label}`;
  panels.complexity.title.textContent = `Inputs × outputs · ${label}`;
  panels.entanglement.title.textContent = `Entanglement · ${label}`;
  panels.value.title.textContent = `Total output value · ${label}`;
  panels.spectrum.title.textContent = `Fee structure · ${label}`;
  panels.package.title.textContent = `Ancestor fee rate · ${label}`;
  panels.joint.title.textContent = `Fee rate × size · ${label}`;
  panels.mosaic.title.textContent = `Bucket × age · ${label}`;

  renderCompositionBars(panels.composition.container, model.composition, {
    metricLabel: "virtual size",
    emptyMessage: COMPARISON_EMPTY_MESSAGE,
  });
  renderSpectrumChart(
    panels.spectrum.container,
    model.feeSpectrum,
    FEE_RATE_TICKS,
    formatVsize,
    COMPARISON_EMPTY_MESSAGE,
  );
  renderSpectrumChart(
    panels.package.container,
    model.ancestorFeeSpectrum,
    FEE_RATE_TICKS,
    formatVsize,
    COMPARISON_EMPTY_MESSAGE,
  );
  renderSpectrumChart(
    panels.data.container,
    model.dataSpectrum,
    DATA_BYTES_TICKS,
    formatVsize,
    "No observed transaction carries OP_RETURN data.",
  );
  renderCompositionBars(panels.entanglement.container, model.entanglement, {
    metricLabel: "virtual size",
    emptyMessage: COMPARISON_EMPTY_MESSAGE,
  });
  renderSpectrumChart(
    panels.value.container,
    model.valueSpectrum,
    OUTPUT_VALUE_TICKS,
    formatVsize,
    "No structure facts are available yet.",
  );
  renderMosaicChart(panels.mosaic.container, model.ageMosaic, {
    metricFormat: formatVsize,
    emptyMessage: COMPARISON_EMPTY_MESSAGE,
  });
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
