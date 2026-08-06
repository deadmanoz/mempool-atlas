import { classifierDescriptor } from "./classification-view";
import {
  KNOTS_BIP110_CLASSIFIER_ID,
  type ClassifierBucketKey,
} from "./classifier-terrain";
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
import { countFormat, formatVsize } from "./format";
import { PANEL_GROUP_LIMIT } from "./panel-groups";
import {
  SnapshotDistributionCache,
  buildSnapshotDistributionModel,
} from "./snapshot-distributions";
import type { TerrainMode } from "./terrain";
import type { MempoolSnapshot } from "./types";

const EMPTY_SNAPSHOT_MESSAGE = "This snapshot contains an empty mempool.";
const DATA_GROUP_LIMIT = 5;

export interface SnapshotDistributionSelection {
  classifierId: string;
  bucketKey: ClassifierBucketKey | null;
  metric: TerrainMode;
}

export interface SnapshotDistributionsView {
  render(
    snapshot: MempoolSnapshot,
    selection: SnapshotDistributionSelection,
  ): void;
  reset(message: string): void;
  setSelection(
    classifierId: string,
    bucketKey: ClassifierBucketKey | null,
  ): void;
}

export interface SnapshotDistributionsViewOptions {
  onSelectBucket: (selection: {
    classifierId: string;
    bucketKey: ClassifierBucketKey;
  }) => void;
}

const requiredRoot = (): HTMLElement => {
  const root = document.getElementById("snapshot-distributions");
  if (!(root instanceof HTMLElement)) {
    throw new Error("Missing required element #snapshot-distributions");
  }
  return root;
};

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

export const createSnapshotDistributionsView = ({
  onSelectBucket,
}: SnapshotDistributionsViewOptions): SnapshotDistributionsView => {
  const root = requiredRoot();
  const empty = requiredDescendant<HTMLElement>(root, "distribution-empty");
  const grid = requiredDescendant<HTMLElement>(root, "distribution-grid");
  const jointNote = requiredDescendant<HTMLElement>(root, "joint-note");
  const jointChart = requiredDescendant<HTMLElement>(root, "joint-chart");
  const jointCanvas = requiredDescendant<HTMLCanvasElement>(
    root,
    "joint-canvas",
  );
  const compositionNote = requiredDescendant<HTMLElement>(
    root,
    "composition-note",
  );
  const composition = requiredDescendant<HTMLElement>(root, "composition-bars");
  const spectrumNote = requiredDescendant<HTMLElement>(root, "spectrum-note");
  const spectrumChart = requiredDescendant<HTMLElement>(root, "spectrum-chart");
  const packageNote = requiredDescendant<HTMLElement>(root, "package-note");
  const packageChart = requiredDescendant<HTMLElement>(root, "package-chart");
  const mosaicNote = requiredDescendant<HTMLElement>(root, "mosaic-note");
  const mosaicChart = requiredDescendant<HTMLElement>(root, "mosaic-chart");
  const dataNote = requiredDescendant<HTMLElement>(root, "data-note");
  const dataChart = requiredDescendant<HTMLElement>(root, "data-chart");
  const complexityNote = requiredDescendant<HTMLElement>(
    root,
    "complexity-note",
  );
  const complexityChart = requiredDescendant<HTMLElement>(
    root,
    "complexity-chart",
  );
  const complexityCanvas = requiredDescendant<HTMLCanvasElement>(
    root,
    "complexity-canvas",
  );
  const entanglementNote = requiredDescendant<HTMLElement>(
    root,
    "entanglement-note",
  );
  const entanglement = requiredDescendant<HTMLElement>(
    root,
    "entanglement-bars",
  );
  const valueNote = requiredDescendant<HTMLElement>(root, "value-note");
  const valueChart = requiredDescendant<HTMLElement>(root, "value-chart");

  const cache = new SnapshotDistributionCache();
  let currentSelection: SnapshotDistributionSelection | null = null;
  let jointDensity: JointDensity | null = null;
  let complexityDensity: JointDensity | null = null;
  let pendingFrame: number | null = null;

  const renderJointFrame = (): void => {
    pendingFrame = null;
    if (jointDensity !== null) {
      renderJointChart(jointChart, jointCanvas, jointDensity, {
        color: DEFAULT_JOINT_COLOR,
        emptyMessage: EMPTY_SNAPSHOT_MESSAGE,
      });
    }
    if (complexityDensity !== null) {
      renderJointChart(complexityChart, complexityCanvas, complexityDensity, {
        color: DEFAULT_JOINT_COLOR,
        emptyMessage: "No structure facts are available yet.",
      });
    }
  };

  const scheduleJointRender = (): void => {
    if (
      pendingFrame !== null ||
      (jointDensity === null && complexityDensity === null)
    ) {
      return;
    }
    pendingFrame = window.requestAnimationFrame(renderJointFrame);
  };

  const cancelJointRender = (): void => {
    if (pendingFrame !== null) {
      window.cancelAnimationFrame(pendingFrame);
      pendingFrame = null;
    }
  };

  const syncSelection = (): void => {
    for (const button of composition.querySelectorAll<HTMLButtonElement>(
      "button.composition-segment",
    )) {
      const selected =
        currentSelection?.bucketKey !== null &&
        button.dataset.classifier === currentSelection?.classifierId &&
        button.dataset.segment === currentSelection?.bucketKey;
      if (selected) {
        button.setAttribute("aria-pressed", "true");
      } else {
        button.removeAttribute("aria-pressed");
      }
    }
  };

  const reset = (message: string): void => {
    cache.reset();
    currentSelection = null;
    jointDensity = null;
    complexityDensity = null;
    cancelJointRender();
    grid.hidden = true;
    empty.hidden = false;
    empty.textContent = message;
  };

  const setSelection = (
    classifierId: string,
    bucketKey: ClassifierBucketKey | null,
  ): void => {
    if (currentSelection !== null) {
      currentSelection = {
        ...currentSelection,
        classifierId,
        bucketKey,
      };
    }
    syncSelection();
  };

  const render = (
    snapshot: MempoolSnapshot,
    selection: SnapshotDistributionSelection,
  ): void => {
    cache.replaceOwner(snapshot);
    currentSelection = selection;
    if (snapshot.transaction_count === 0) {
      jointDensity = null;
      complexityDensity = null;
      cancelJointRender();
      grid.hidden = true;
      empty.hidden = false;
      empty.textContent = EMPTY_SNAPSHOT_MESSAGE;
      return;
    }

    grid.hidden = false;
    empty.hidden = true;
    const metricNoun =
      selection.metric === "count" ? "transaction count" : "virtual size";
    const metricFormat = (value: number): string =>
      selection.metric === "count"
        ? `${countFormat.format(value)} tx`
        : formatVsize(value);
    const descriptor = classifierDescriptor(snapshot, selection.classifierId);
    const variant = `classifier=${selection.classifierId};metric=${selection.metric};groups=${PANEL_GROUP_LIMIT};dataGroups=${DATA_GROUP_LIMIT}`;
    const model = cache.get(snapshot, variant, () =>
      buildSnapshotDistributionModel({
        transactions: snapshot.transactions,
        classifierCatalog: snapshot.classifier_catalog,
        selectedClassifier: descriptor,
        observedAtMs: snapshot.observed_at_ms,
        metric: selection.metric,
        groupLimit: PANEL_GROUP_LIMIT,
        dataGroupLimit: DATA_GROUP_LIMIT,
      }),
    );
    const lensName = descriptor?.title ?? "the selected classifier";
    const interactive = selection.classifierId !== KNOTS_BIP110_CLASSIFIER_ID;

    renderSpectrumChart(
      spectrumChart,
      model.feeSpectrum,
      FEE_RATE_TICKS,
      metricFormat,
      EMPTY_SNAPSHOT_MESSAGE,
    );
    spectrumNote.textContent = `Stacked ${metricNoun} per log fee-rate bin by ${lensName} bucket.`;

    renderSpectrumChart(
      packageChart,
      model.ancestorFeeSpectrum,
      FEE_RATE_TICKS,
      metricFormat,
      EMPTY_SNAPSHOT_MESSAGE,
    );
    packageNote.textContent = `Ancestor fee rate by ${lensName} bucket: delta-adjusted ancestor fees over ancestor virtual size. This is not the cluster mempool mining score.`;

    jointDensity = model.jointDensity;
    jointNote.textContent = `Density of ${metricNoun} across fee rate and size, with marginals.`;
    scheduleJointRender();

    renderMosaicChart(mosaicChart, model.ageMosaic, {
      metricFormat,
      emptyMessage: EMPTY_SNAPSHOT_MESSAGE,
      isColumnInteractive: interactive,
      onColumnSelect: (column) => {
        if (column.key !== "all" && column.key !== "overflow") {
          onSelectBucket({
            classifierId: selection.classifierId,
            bucketKey: column.key as ClassifierBucketKey,
          });
        }
      },
    });
    mosaicNote.textContent = `Column width is each ${lensName} bucket's ${metricNoun} share; cells split the bucket by age at observation.`;

    const coverage = `structure facts cover ${countFormat.format(model.totals.structured.count)} of ${countFormat.format(snapshot.transaction_count)} transactions`;
    renderSpectrumChart(
      dataChart,
      model.dataSpectrum,
      DATA_BYTES_TICKS,
      metricFormat,
      "No observed transaction carries OP_RETURN data.",
    );
    dataNote.textContent = `Stacked ${metricNoun} per log carried-byte bin by Data protocols bucket · ${countFormat.format(model.totals.carrier.count)} transactions carry OP_RETURN bytes.`;

    complexityDensity = model.complexityDensity;
    complexityNote.textContent = `Density of ${metricNoun} across input and output counts, with marginals · ${coverage}.`;
    scheduleJointRender();

    renderCompositionBars(entanglement, model.entanglement, {
      metricLabel: metricNoun,
      emptyMessage: EMPTY_SNAPSHOT_MESSAGE,
    });
    entanglementNote.textContent = `Unconfirmed mempool relatives reported by the source node · ${countFormat.format(model.totals.replaceable.count)} transactions are reported replaceable.`;

    renderSpectrumChart(
      valueChart,
      model.valueSpectrum,
      OUTPUT_VALUE_TICKS,
      metricFormat,
      "No structure facts are available yet.",
    );
    valueNote.textContent = `Total output value per transaction by ${lensName} bucket · ${coverage}.`;

    compositionNote.textContent = `Share of ${metricNoun} per independent classifier lens. Lenses are not combined.`;
    renderCompositionBars(composition, model.composition, {
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

  new ResizeObserver(scheduleJointRender).observe(jointChart);
  new ResizeObserver(scheduleJointRender).observe(complexityChart);
  jointChart.append(feeRateAxisRow());
  complexityChart.append(panelAxisRow(IO_COUNT_TICKS));

  return { render, reset, setSelection };
};
