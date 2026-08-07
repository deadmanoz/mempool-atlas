import { classifierDescriptor } from "./classification-view";
import type { ClassifierBucketKey } from "./classifier-terrain";
import type { JointDensity } from "./fee-distribution";
import { createFrameSequenceRenderer } from "./frame-sequence-renderer";
import { PANEL_GROUP_LIMIT } from "./panel-groups";
import {
  commitSnapshotDistributionPanels,
  createSnapshotDistributionPanelElements,
  EMPTY_SNAPSHOT_MESSAGE,
  prepareSnapshotDistributionJointCanvases,
  renderSnapshotDistributionJointPanels,
  syncSnapshotDistributionSelection,
} from "./snapshot-distribution-panels";
import {
  SnapshotDistributionCache,
  buildSnapshotDistributionModelCooperatively,
  type SnapshotDistributionModel,
} from "./snapshot-distributions";
import type { TerrainMode } from "./terrain";
import type { ClassifierDescriptor, MempoolSnapshot } from "./types";

const DATA_GROUP_LIMIT = 5;

export interface SnapshotDistributionSelection {
  classifierId: string;
  bucketKey: ClassifierBucketKey | null;
  metric: TerrainMode;
}

export interface SnapshotDistributionsView {
  prepare(
    snapshot: MempoolSnapshot,
    selection: SnapshotDistributionSelection,
    signal?: AbortSignal,
  ): Promise<PreparedSnapshotDistributions>;
  canCommit(
    prepared: PreparedSnapshotDistributions,
    snapshot: MempoolSnapshot,
    selection: SnapshotDistributionSelection,
  ): boolean;
  commit(
    prepared: PreparedSnapshotDistributions,
    snapshot: MempoolSnapshot,
    selection: SnapshotDistributionSelection,
  ): boolean;
  render(
    snapshot: MempoolSnapshot,
    selection: SnapshotDistributionSelection,
  ): Promise<void>;
  reset(message: string): void;
  setSelection(
    classifierId: string,
    bucketKey: ClassifierBucketKey | null,
  ): void;
}

interface SnapshotDistributionIdentity {
  sourceId: string;
  observedAtMs: number;
  classificationRevision: number;
}

const PREPARED_SNAPSHOT_VIEW = Symbol("prepared-snapshot-view");

export interface PreparedSnapshotDistributions {
  readonly snapshot: MempoolSnapshot;
  readonly selection: SnapshotDistributionSelection;
  readonly model: SnapshotDistributionModel | null;
  readonly [PREPARED_SNAPSHOT_VIEW]: object;
  readonly identity: SnapshotDistributionIdentity;
  readonly variant: string;
  readonly descriptor: ClassifierDescriptor | null;
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

const snapshotIdentity = (
  snapshot: MempoolSnapshot,
): SnapshotDistributionIdentity => ({
  sourceId: snapshot.source_id,
  observedAtMs: snapshot.observed_at_ms,
  classificationRevision: snapshot.classification_revision,
});

const sameSnapshotIdentity = (
  snapshot: MempoolSnapshot,
  identity: SnapshotDistributionIdentity,
): boolean =>
  snapshot.source_id === identity.sourceId &&
  snapshot.observed_at_ms === identity.observedAtMs &&
  snapshot.classification_revision === identity.classificationRevision;

const sameSelection = (
  left: SnapshotDistributionSelection,
  right: SnapshotDistributionSelection,
): boolean =>
  left.classifierId === right.classifierId &&
  left.bucketKey === right.bucketKey &&
  left.metric === right.metric;

const snapshotVariant = (selection: SnapshotDistributionSelection): string =>
  `classifier=${selection.classifierId};metric=${selection.metric};groups=${PANEL_GROUP_LIMIT};dataGroups=${DATA_GROUP_LIMIT}`;

export const createSnapshotDistributionsView = ({
  onSelectBucket,
}: SnapshotDistributionsViewOptions): SnapshotDistributionsView => {
  const root = requiredRoot();
  const empty = requiredDescendant<HTMLElement>(root, "distribution-empty");
  const grid = requiredDescendant<HTMLElement>(root, "distribution-grid");
  const cache = new SnapshotDistributionCache();
  const viewToken = {};
  let currentSelection: SnapshotDistributionSelection | null = null;
  let jointDensity: JointDensity | null = null;
  let complexityDensity: JointDensity | null = null;
  let renderController: AbortController | null = null;
  let renderEpoch = 0;
  let panels: ReturnType<typeof createSnapshotDistributionPanelElements>;

  const densityRenderer = createFrameSequenceRenderer([
    () => renderSnapshotDistributionJointPanels(panels, jointDensity, null),
    () =>
      renderSnapshotDistributionJointPanels(panels, null, complexityDensity),
  ]);
  const scheduleJointRender = (): void => {
    if (jointDensity !== null || complexityDensity !== null) {
      densityRenderer.schedule();
    }
  };
  const cancelJointRender = densityRenderer.cancel;

  panels = createSnapshotDistributionPanelElements(root, scheduleJointRender);

  const syncSelection = (): void => {
    syncSnapshotDistributionSelection(panels.composition, currentSelection);
  };

  const reset = (message: string): void => {
    renderController?.abort();
    renderController = null;
    renderEpoch += 1;
    cache.reset();
    currentSelection = null;
    jointDensity = null;
    complexityDensity = null;
    cancelJointRender();
    root.setAttribute("aria-busy", "false");
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

  const preparedSnapshot = (
    snapshot: MempoolSnapshot,
    selection: SnapshotDistributionSelection,
    descriptor: ClassifierDescriptor | null,
    variant: string,
    model: SnapshotDistributionModel | null,
  ): PreparedSnapshotDistributions => ({
    snapshot,
    selection: { ...selection },
    descriptor,
    variant,
    model,
    identity: snapshotIdentity(snapshot),
    [PREPARED_SNAPSHOT_VIEW]: viewToken,
  });

  const prepare = async (
    snapshot: MempoolSnapshot,
    selection: SnapshotDistributionSelection,
    signal?: AbortSignal,
  ): Promise<PreparedSnapshotDistributions> => {
    signal?.throwIfAborted();
    const descriptor = classifierDescriptor(snapshot, selection.classifierId);
    const variant = snapshotVariant(selection);
    if (snapshot.transaction_count === 0) {
      return preparedSnapshot(snapshot, selection, descriptor, variant, null);
    }
    const model = await buildSnapshotDistributionModelCooperatively(
      {
        transactions: snapshot.transactions,
        classifierCatalog: snapshot.classifier_catalog,
        selectedClassifier: descriptor,
        observedAtMs: snapshot.observed_at_ms,
        metric: selection.metric,
        groupLimit: PANEL_GROUP_LIMIT,
        dataGroupLimit: DATA_GROUP_LIMIT,
      },
      signal === undefined ? {} : { signal },
    );
    signal?.throwIfAborted();
    return preparedSnapshot(snapshot, selection, descriptor, variant, model);
  };

  const commitPrepared = (
    prepared: PreparedSnapshotDistributions,
    snapshot: MempoolSnapshot,
    selection: SnapshotDistributionSelection,
  ): boolean => {
    if (!canCommit(prepared, snapshot, selection)) {
      return false;
    }

    currentSelection = { ...selection };
    cancelJointRender();
    jointDensity = null;
    complexityDensity = null;
    if (prepared.model === null) {
      cache.replaceOwner(snapshot);
      grid.hidden = true;
      empty.hidden = false;
      empty.textContent = EMPTY_SNAPSHOT_MESSAGE;
    } else {
      cache.adopt(snapshot, prepared.variant, prepared.model);
      grid.hidden = false;
      empty.hidden = true;
      jointDensity = prepared.model.jointDensity;
      complexityDensity = prepared.model.complexityDensity;
      commitSnapshotDistributionPanels({
        elements: panels,
        model: prepared.model,
        snapshot,
        selection,
        descriptor: prepared.descriptor,
        onSelectBucket,
      });
      prepareSnapshotDistributionJointCanvases(
        panels,
        jointDensity,
        complexityDensity,
      );
      scheduleJointRender();
    }
    return true;
  };

  const canCommit = (
    prepared: PreparedSnapshotDistributions,
    snapshot: MempoolSnapshot,
    selection: SnapshotDistributionSelection,
  ): boolean =>
    !(
      prepared[PREPARED_SNAPSHOT_VIEW] !== viewToken ||
      prepared.snapshot !== snapshot ||
      !sameSnapshotIdentity(snapshot, prepared.identity) ||
      !sameSelection(prepared.selection, selection)
    );

  const commit = (
    prepared: PreparedSnapshotDistributions,
    snapshot: MempoolSnapshot,
    selection: SnapshotDistributionSelection,
  ): boolean => {
    if (!canCommit(prepared, snapshot, selection)) return false;
    renderController?.abort();
    renderController = null;
    renderEpoch += 1;
    const committed = commitPrepared(prepared, snapshot, selection);
    if (committed) root.setAttribute("aria-busy", "false");
    return committed;
  };

  const render = async (
    snapshot: MempoolSnapshot,
    selection: SnapshotDistributionSelection,
  ): Promise<void> => {
    renderController?.abort();
    const controller = new AbortController();
    renderController = controller;
    const epoch = ++renderEpoch;
    const identity = snapshotIdentity(snapshot);
    const isCurrentRender = (): boolean =>
      renderController === controller &&
      renderEpoch === epoch &&
      !controller.signal.aborted &&
      sameSnapshotIdentity(snapshot, identity);

    cache.replaceOwner(snapshot);
    currentSelection = selection;
    cancelJointRender();
    jointDensity = null;
    complexityDensity = null;
    root.setAttribute("aria-busy", "true");

    try {
      const descriptor = classifierDescriptor(snapshot, selection.classifierId);
      const variant = snapshotVariant(selection);
      const model =
        snapshot.transaction_count === 0
          ? null
          : await cache.getAsync(
              snapshot,
              variant,
              (signal) =>
                buildSnapshotDistributionModelCooperatively(
                  {
                    transactions: snapshot.transactions,
                    classifierCatalog: snapshot.classifier_catalog,
                    selectedClassifier: descriptor,
                    observedAtMs: snapshot.observed_at_ms,
                    metric: selection.metric,
                    groupLimit: PANEL_GROUP_LIMIT,
                    dataGroupLimit: DATA_GROUP_LIMIT,
                  },
                  { signal },
                ),
              controller.signal,
            );
      if (!isCurrentRender()) return;
      commitPrepared(
        preparedSnapshot(snapshot, selection, descriptor, variant, model),
        snapshot,
        selection,
      );
    } catch (error) {
      if (error instanceof DOMException && error.name === "AbortError") {
        return;
      }
      throw error;
    } finally {
      if (renderController === controller && renderEpoch === epoch) {
        root.setAttribute("aria-busy", "false");
        renderController = null;
      }
    }
  };

  return { prepare, canCommit, commit, render, reset, setSelection };
};
