import { TRANSACTION_PROPERTIES_CLASSIFIER_ID } from "./classifier-terrain";
import type {
  ComparedTransaction,
  ComparisonSide,
  CurrentComparison,
} from "./comparison-model";
import {
  DEFAULT_JOINT_COLOR,
  feeRateAxisRow,
  panelAxisRow,
  renderCompositionBars,
  renderJointChart,
  renderMosaicChart,
  renderSpectrumChart,
} from "./detail-panels";
import {
  DATA_BYTES_TICKS,
  FEE_RATE_TICKS,
  IO_COUNT_TICKS,
  OUTPUT_VALUE_TICKS,
  type JointDensity,
} from "./fee-distribution";
import { formatVsize } from "./format";
import {
  SnapshotDistributionCache,
  buildSnapshotDistributionModel,
} from "./snapshot-distributions";
import type { MempoolTransaction } from "./types";

export type ComparisonDistributionScope =
  "all" | "common" | "left_only" | "right_only";

export interface ComparisonDistributionsView {
  render(current: CurrentComparison): void;
  reset(): void;
}

const COMPARISON_SIDES = ["left", "right"] as const;
const DISTRIBUTION_SCOPES: readonly ComparisonDistributionScope[] = [
  "all",
  "common",
  "left_only",
  "right_only",
];
const COMPARISON_PANEL_GROUP_LIMIT = 4;
const COMPARISON_EMPTY_MESSAGE =
  "This population has no members on this source.";

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

const sidePair = <T extends HTMLElement>(
  root: HTMLElement,
  leftId: string,
  rightId: string,
): Record<ComparisonSide, T> => ({
  left: requiredDescendant<T>(root, leftId),
  right: requiredDescendant<T>(root, rightId),
});

const presentTransactions = (
  entries: readonly ComparedTransaction[],
  side: ComparisonSide,
): MempoolTransaction[] =>
  entries
    .map((entry) => (side === "left" ? entry.left : entry.right))
    .filter(
      (transaction): transaction is MempoolTransaction => transaction !== null,
    );

export const comparisonDistributionTransactions = (
  current: CurrentComparison,
  side: ComparisonSide,
  scope: ComparisonDistributionScope,
): MempoolTransaction[] => {
  if (scope === "all") {
    return current[side].snapshot.transactions;
  }
  if (scope === "common") {
    return presentTransactions(current.common, side);
  }
  if (scope === "left_only") {
    return side === "left" ? presentTransactions(current.left_only, side) : [];
  }
  return side === "right" ? presentTransactions(current.right_only, side) : [];
};

export const comparisonDistributionScopeSuffix = (
  scope: ComparisonDistributionScope,
): string =>
  scope === "all"
    ? ""
    : scope === "common"
      ? " · present in both"
      : scope === "left_only"
        ? " · only in Source A"
        : " · only in Source B";

export const createComparisonDistributionsView = (
  root: HTMLElement,
): ComparisonDistributionsView => {
  const mosaicTitles = sidePair<HTMLElement>(
    root,
    "dist-mosaic-left-title",
    "dist-mosaic-right-title",
  );
  const mosaicContainers = sidePair<HTMLElement>(
    root,
    "mosaic-left",
    "mosaic-right",
  );
  const dataTitles = sidePair<HTMLElement>(
    root,
    "dist-data-left-title",
    "dist-data-right-title",
  );
  const dataContainers = sidePair<HTMLElement>(root, "data-left", "data-right");
  const complexityTitles = sidePair<HTMLElement>(
    root,
    "dist-complexity-left-title",
    "dist-complexity-right-title",
  );
  const complexityContainers = sidePair<HTMLElement>(
    root,
    "complexity-left",
    "complexity-right",
  );
  const complexityCanvases = sidePair<HTMLCanvasElement>(
    root,
    "complexity-left-canvas",
    "complexity-right-canvas",
  );
  const entanglementTitles = sidePair<HTMLElement>(
    root,
    "dist-entanglement-left-title",
    "dist-entanglement-right-title",
  );
  const entanglementContainers = sidePair<HTMLElement>(
    root,
    "entanglement-left",
    "entanglement-right",
  );
  const valueTitles = sidePair<HTMLElement>(
    root,
    "dist-value-left-title",
    "dist-value-right-title",
  );
  const valueContainers = sidePair<HTMLElement>(
    root,
    "value-left",
    "value-right",
  );
  const jointTitles = sidePair<HTMLElement>(
    root,
    "dist-joint-left-title",
    "dist-joint-right-title",
  );
  const compositionTitles = sidePair<HTMLElement>(
    root,
    "dist-comp-left-title",
    "dist-comp-right-title",
  );
  const packageTitles = sidePair<HTMLElement>(
    root,
    "dist-package-left-title",
    "dist-package-right-title",
  );
  const packageContainers = sidePair<HTMLElement>(
    root,
    "package-left",
    "package-right",
  );
  const spectrumTitles = sidePair<HTMLElement>(
    root,
    "dist-spectrum-left-title",
    "dist-spectrum-right-title",
  );
  const spectrumContainers = sidePair<HTMLElement>(
    root,
    "spectrum-left",
    "spectrum-right",
  );
  const jointContainers = sidePair<HTMLElement>(
    root,
    "joint-left",
    "joint-right",
  );
  const jointCanvases = sidePair<HTMLCanvasElement>(
    root,
    "joint-left-canvas",
    "joint-right-canvas",
  );
  const compositionContainers = sidePair<HTMLElement>(
    root,
    "composition-left",
    "composition-right",
  );
  const scopeButtons: Record<ComparisonDistributionScope, HTMLButtonElement> = {
    all: requiredDescendant<HTMLButtonElement>(root, "dist-scope-all"),
    common: requiredDescendant<HTMLButtonElement>(root, "dist-scope-common"),
    left_only: requiredDescendant<HTMLButtonElement>(root, "dist-scope-left"),
    right_only: requiredDescendant<HTMLButtonElement>(root, "dist-scope-right"),
  };

  const cache = new SnapshotDistributionCache();
  const jointDensities: Record<ComparisonSide, JointDensity | null> = {
    left: null,
    right: null,
  };
  const complexityDensities: Record<ComparisonSide, JointDensity | null> = {
    left: null,
    right: null,
  };
  let currentComparison: CurrentComparison | null = null;
  let scope: ComparisonDistributionScope = "all";
  let pendingDensityFrame: number | null = null;

  const renderDensityFrame = (): void => {
    pendingDensityFrame = null;
    for (const side of COMPARISON_SIDES) {
      const density = jointDensities[side];
      if (density !== null) {
        renderJointChart(jointContainers[side], jointCanvases[side], density, {
          color: DEFAULT_JOINT_COLOR,
          emptyMessage: "This sampled mempool is empty.",
        });
      }
      const complexity = complexityDensities[side];
      if (complexity !== null) {
        renderJointChart(
          complexityContainers[side],
          complexityCanvases[side],
          complexity,
          {
            color: DEFAULT_JOINT_COLOR,
            emptyMessage: "No structure facts are available yet.",
          },
        );
      }
    }
  };

  const scheduleDensityRender = (): void => {
    if (
      pendingDensityFrame !== null ||
      (jointDensities.left === null &&
        jointDensities.right === null &&
        complexityDensities.left === null &&
        complexityDensities.right === null)
    ) {
      return;
    }
    pendingDensityFrame = window.requestAnimationFrame(renderDensityFrame);
  };

  const syncScopeButtons = (): void => {
    for (const candidate of DISTRIBUTION_SCOPES) {
      scopeButtons[candidate].setAttribute(
        "aria-pressed",
        String(candidate === scope),
      );
    }
  };

  const render = (current: CurrentComparison): void => {
    currentComparison = current;
    root.hidden = false;
    const scopeSuffix = comparisonDistributionScopeSuffix(scope);
    for (const side of COMPARISON_SIDES) {
      const loaded = current[side];
      const { snapshot } = loaded;
      const label = `${snapshot.source_label}${scopeSuffix}`;
      compositionTitles[side].textContent = `Composition by lens · ${label}`;
      dataTitles[side].textContent = `Data carriage · ${label}`;
      complexityTitles[side].textContent = `Inputs × outputs · ${label}`;
      entanglementTitles[side].textContent = `Entanglement · ${label}`;
      valueTitles[side].textContent = `Total output value · ${label}`;
      spectrumTitles[side].textContent = `Fee structure · ${label}`;
      packageTitles[side].textContent = `Ancestor fee rate · ${label}`;
      jointTitles[side].textContent = `Fee rate × size · ${label}`;
      mosaicTitles[side].textContent = `Bucket × age · ${label}`;
      const propertyDescriptor = snapshot.classifier_catalog.find(
        ({ id }) => id === TRANSACTION_PROPERTIES_CLASSIFIER_ID,
      );
      const variant = `side=${side};scope=${scope};metric=vsize;groups=${COMPARISON_PANEL_GROUP_LIMIT};dataGroups=${COMPARISON_PANEL_GROUP_LIMIT}`;
      const model = cache.get(current, variant, () =>
        buildSnapshotDistributionModel({
          transactions: comparisonDistributionTransactions(
            current,
            side,
            scope,
          ),
          classifierCatalog: snapshot.classifier_catalog,
          selectedClassifier: propertyDescriptor ?? null,
          observedAtMs: snapshot.observed_at_ms,
          metric: "vsize",
          groupLimit: COMPARISON_PANEL_GROUP_LIMIT,
          dataGroupLimit: COMPARISON_PANEL_GROUP_LIMIT,
        }),
      );
      renderCompositionBars(compositionContainers[side], model.composition, {
        metricLabel: "virtual size",
        emptyMessage: COMPARISON_EMPTY_MESSAGE,
      });
      renderSpectrumChart(
        spectrumContainers[side],
        model.feeSpectrum,
        FEE_RATE_TICKS,
        (value) => formatVsize(value),
        COMPARISON_EMPTY_MESSAGE,
      );
      jointDensities[side] = model.jointDensity;
      complexityDensities[side] = model.complexityDensity;
      renderSpectrumChart(
        packageContainers[side],
        model.ancestorFeeSpectrum,
        FEE_RATE_TICKS,
        (value) => formatVsize(value),
        COMPARISON_EMPTY_MESSAGE,
      );
      renderSpectrumChart(
        dataContainers[side],
        model.dataSpectrum,
        DATA_BYTES_TICKS,
        (value) => formatVsize(value),
        "No observed transaction carries OP_RETURN data.",
      );
      renderCompositionBars(entanglementContainers[side], model.entanglement, {
        metricLabel: "virtual size",
        emptyMessage: COMPARISON_EMPTY_MESSAGE,
      });
      renderSpectrumChart(
        valueContainers[side],
        model.valueSpectrum,
        OUTPUT_VALUE_TICKS,
        (value) => formatVsize(value),
        "No structure facts are available yet.",
      );
      renderMosaicChart(mosaicContainers[side], model.ageMosaic, {
        metricFormat: (value) => formatVsize(value),
        emptyMessage: COMPARISON_EMPTY_MESSAGE,
      });
    }
    scheduleDensityRender();
  };

  const setScope = (nextScope: ComparisonDistributionScope): void => {
    scope = nextScope;
    syncScopeButtons();
    if (currentComparison !== null) {
      render(currentComparison);
    }
  };

  const reset = (): void => {
    currentComparison = null;
    cache.reset();
    root.hidden = true;
    scope = "all";
    syncScopeButtons();
    if (pendingDensityFrame !== null) {
      window.cancelAnimationFrame(pendingDensityFrame);
      pendingDensityFrame = null;
    }
    for (const side of COMPARISON_SIDES) {
      jointDensities[side] = null;
      complexityDensities[side] = null;
      compositionContainers[side].replaceChildren();
      spectrumContainers[side].replaceChildren();
      packageContainers[side].replaceChildren();
      mosaicContainers[side].replaceChildren();
      dataContainers[side].replaceChildren();
      entanglementContainers[side].replaceChildren();
      valueContainers[side].replaceChildren();
    }
  };

  for (const candidate of DISTRIBUTION_SCOPES) {
    scopeButtons[candidate].addEventListener("click", () => {
      setScope(candidate);
    });
  }
  const densityResizeObserver = new ResizeObserver(scheduleDensityRender);
  densityResizeObserver.observe(jointContainers.left);
  densityResizeObserver.observe(jointContainers.right);
  densityResizeObserver.observe(complexityContainers.left);
  densityResizeObserver.observe(complexityContainers.right);
  jointContainers.left.append(feeRateAxisRow());
  jointContainers.right.append(feeRateAxisRow());
  complexityContainers.left.append(panelAxisRow(IO_COUNT_TICKS));
  complexityContainers.right.append(panelAxisRow(IO_COUNT_TICKS));

  return { render, reset };
};
