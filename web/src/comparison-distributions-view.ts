import { TRANSACTION_PROPERTIES_CLASSIFIER_ID } from "./classifier-terrain";
import {
  commitComparisonDistributionSide,
  createComparisonDistributionPanels,
  resetComparisonDistributionSide,
} from "./comparison-distribution-panels";
import {
  comparisonDistributionScopeSuffix,
  comparisonDistributionTransactions,
  type ComparisonDistributionScope,
} from "./comparison-distribution-population";
import type { ComparisonSide, CurrentComparison } from "./comparison-model";
import { DEFAULT_JOINT_COLOR, renderJointChart } from "./detail-panels";
import type { JointDensity } from "./fee-distribution";
import {
  SnapshotDistributionCache,
  buildSnapshotDistributionModelCooperatively,
  type SnapshotDistributionModel,
} from "./snapshot-distributions";

export {
  comparisonDistributionScopeSuffix,
  comparisonDistributionTransactions,
  type ComparisonDistributionScope,
} from "./comparison-distribution-population";

export interface ComparisonDistributionsView {
  prepare(
    current: CurrentComparison,
    signal?: AbortSignal,
  ): Promise<PreparedComparisonDistributions>;
  canCommit(
    prepared: PreparedComparisonDistributions,
    current: CurrentComparison,
  ): boolean;
  commit(
    prepared: PreparedComparisonDistributions,
    current: CurrentComparison,
  ): boolean;
  render(current: CurrentComparison): Promise<void>;
  reset(): void;
}

interface ComparisonSnapshotIdentity {
  sourceId: string;
  observedAtMs: number;
  classificationRevision: number;
}

const PREPARED_COMPARISON_VIEW = Symbol("prepared-comparison-view");

export interface PreparedComparisonDistributions {
  readonly current: CurrentComparison;
  readonly scope: ComparisonDistributionScope;
  readonly models: Readonly<Record<ComparisonSide, SnapshotDistributionModel>>;
  readonly variants: Readonly<Record<ComparisonSide, string>>;
  readonly leftIdentity: ComparisonSnapshotIdentity;
  readonly rightIdentity: ComparisonSnapshotIdentity;
  readonly [PREPARED_COMPARISON_VIEW]: object;
}

const COMPARISON_SIDES = ["left", "right"] as const;
const DISTRIBUTION_SCOPES: readonly ComparisonDistributionScope[] = [
  "all",
  "common",
  "left_only",
  "right_only",
];
const COMPARISON_PANEL_GROUP_LIMIT = 4;

const comparisonSnapshotIdentity = (
  current: CurrentComparison,
  side: ComparisonSide,
): ComparisonSnapshotIdentity => ({
  sourceId: current[side].snapshot.source_id,
  observedAtMs: current[side].snapshot.observed_at_ms,
  classificationRevision: current[side].snapshot.classification_revision,
});

const sameComparisonSnapshotIdentity = (
  current: CurrentComparison,
  side: ComparisonSide,
  identity: ComparisonSnapshotIdentity,
): boolean => {
  const { snapshot } = current[side];
  return (
    snapshot.source_id === identity.sourceId &&
    snapshot.observed_at_ms === identity.observedAtMs &&
    snapshot.classification_revision === identity.classificationRevision
  );
};

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

export const createComparisonDistributionsView = (
  root: HTMLElement,
): ComparisonDistributionsView => {
  const panels = createComparisonDistributionPanels(root);
  const scopeButtons: Record<ComparisonDistributionScope, HTMLButtonElement> = {
    all: requiredDescendant<HTMLButtonElement>(root, "dist-scope-all"),
    common: requiredDescendant<HTMLButtonElement>(root, "dist-scope-common"),
    left_only: requiredDescendant<HTMLButtonElement>(root, "dist-scope-left"),
    right_only: requiredDescendant<HTMLButtonElement>(root, "dist-scope-right"),
  };

  const cache = new SnapshotDistributionCache();
  const viewToken = {};
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
  let renderController: AbortController | null = null;
  let renderRevision = 0;
  let committedRevision: number | null = null;
  let densitiesVisible = typeof IntersectionObserver === "undefined";

  const requestDensityFrame = (revision: number, taskIndex: number): void => {
    const callback = function comparisonDensityFrame() {
      renderDensityFrame(revision, taskIndex);
    };
    Object.assign(callback, { __atlasPerfLabel: "comparison-density" });
    pendingDensityFrame = window.requestAnimationFrame(callback);
  };

  const renderDensityFrame = (revision: number, taskIndex: number): void => {
    pendingDensityFrame = null;
    if (committedRevision !== revision) return;
    if (taskIndex < 0) {
      requestDensityFrame(revision, 0);
      return;
    }
    const sideIndex = Math.floor(taskIndex / 2);
    const side = COMPARISON_SIDES[sideIndex];
    if (side === undefined) return;
    const complexity = taskIndex % 2 === 1;
    const density = complexity
      ? complexityDensities[side]
      : jointDensities[side];
    if (density !== null) {
      renderJointChart(
        complexity
          ? panels[side].complexity.container
          : panels[side].joint.container,
        complexity ? panels[side].complexity.canvas : panels[side].joint.canvas,
        density,
        {
          color: DEFAULT_JOINT_COLOR,
          emptyMessage: complexity
            ? "No structure facts are available yet."
            : "This sampled mempool is empty.",
        },
      );
    }
    if (taskIndex + 1 < COMPARISON_SIDES.length * 2) {
      requestDensityFrame(revision, taskIndex + 1);
    }
  };

  const scheduleDensityRender = (): void => {
    const revision = committedRevision;
    if (
      revision === null ||
      !densitiesVisible ||
      pendingDensityFrame !== null ||
      (jointDensities.left === null &&
        jointDensities.right === null &&
        complexityDensities.left === null &&
        complexityDensities.right === null)
    ) {
      return;
    }
    requestDensityFrame(revision, -1);
  };

  if (typeof IntersectionObserver !== "undefined") {
    new IntersectionObserver(
      ([entry]) => {
        densitiesVisible = entry?.isIntersecting ?? false;
        if (densitiesVisible) scheduleDensityRender();
      },
      { rootMargin: "400px" },
    ).observe(root);
  }

  const cancelDensityRender = (): void => {
    if (pendingDensityFrame !== null) {
      window.cancelAnimationFrame(pendingDensityFrame);
      pendingDensityFrame = null;
    }
  };

  const syncScopeButtons = (): void => {
    for (const candidate of DISTRIBUTION_SCOPES) {
      scopeButtons[candidate].setAttribute(
        "aria-pressed",
        String(candidate === scope),
      );
    }
  };

  const variantFor = (
    side: ComparisonSide,
    renderScope: ComparisonDistributionScope,
  ): string =>
    `side=${side};scope=${renderScope};metric=vsize;groups=${COMPARISON_PANEL_GROUP_LIMIT};dataGroups=${COMPARISON_PANEL_GROUP_LIMIT}`;

  const buildSideModel = (
    current: CurrentComparison,
    side: ComparisonSide,
    renderScope: ComparisonDistributionScope,
    signal: AbortSignal,
  ): Promise<SnapshotDistributionModel> => {
    const { snapshot } = current[side];
    const propertyDescriptor = snapshot.classifier_catalog.find(
      ({ id }) => id === TRANSACTION_PROPERTIES_CLASSIFIER_ID,
    );
    return buildSnapshotDistributionModelCooperatively(
      {
        transactions: comparisonDistributionTransactions(
          current,
          side,
          renderScope,
        ),
        classifierCatalog: snapshot.classifier_catalog,
        selectedClassifier: propertyDescriptor ?? null,
        observedAtMs: snapshot.observed_at_ms,
        metric: "vsize",
        groupLimit: COMPARISON_PANEL_GROUP_LIMIT,
        dataGroupLimit: COMPARISON_PANEL_GROUP_LIMIT,
      },
      { signal },
    );
  };

  const preparedComparison = (
    current: CurrentComparison,
    renderScope: ComparisonDistributionScope,
    models: Record<ComparisonSide, SnapshotDistributionModel>,
  ): PreparedComparisonDistributions => ({
    current,
    scope: renderScope,
    models,
    variants: {
      left: variantFor("left", renderScope),
      right: variantFor("right", renderScope),
    },
    leftIdentity: comparisonSnapshotIdentity(current, "left"),
    rightIdentity: comparisonSnapshotIdentity(current, "right"),
    [PREPARED_COMPARISON_VIEW]: viewToken,
  });

  const prepare = async (
    current: CurrentComparison,
    signal?: AbortSignal,
  ): Promise<PreparedComparisonDistributions> => {
    signal?.throwIfAborted();
    const renderScope = scope;
    const renderSignal = signal ?? new AbortController().signal;
    const models = {} as Record<ComparisonSide, SnapshotDistributionModel>;
    for (const side of COMPARISON_SIDES) {
      models[side] = await buildSideModel(
        current,
        side,
        renderScope,
        renderSignal,
      );
    }
    signal?.throwIfAborted();
    return preparedComparison(current, renderScope, models);
  };

  const commitPrepared = (
    prepared: PreparedComparisonDistributions,
    current: CurrentComparison,
    revision: number,
  ): boolean => {
    if (!canCommit(prepared, current)) {
      return false;
    }

    cancelDensityRender();
    committedRevision = null;
    for (const side of COMPARISON_SIDES) {
      const { snapshot } = current[side];
      const model = prepared.models[side];
      cache.adopt(current, prepared.variants[side], model);
      commitComparisonDistributionSide(
        panels[side],
        model,
        snapshot.source_label,
        comparisonDistributionScopeSuffix(prepared.scope),
      );
      jointDensities[side] = model.jointDensity;
      complexityDensities[side] = model.complexityDensity;
    }
    currentComparison = current;
    root.hidden = false;
    committedRevision = revision;
    scheduleDensityRender();
    return true;
  };

  const canCommit = (
    prepared: PreparedComparisonDistributions,
    current: CurrentComparison,
  ): boolean =>
    !(
      prepared[PREPARED_COMPARISON_VIEW] !== viewToken ||
      prepared.current !== current ||
      prepared.scope !== scope ||
      !sameComparisonSnapshotIdentity(current, "left", prepared.leftIdentity) ||
      !sameComparisonSnapshotIdentity(current, "right", prepared.rightIdentity)
    );

  const commit = (
    prepared: PreparedComparisonDistributions,
    current: CurrentComparison,
  ): boolean => {
    if (!canCommit(prepared, current)) return false;
    renderController?.abort();
    renderController = null;
    const revision = ++renderRevision;
    const committed = commitPrepared(prepared, current, revision);
    if (committed) root.setAttribute("aria-busy", "false");
    return committed;
  };

  const render = async (current: CurrentComparison): Promise<void> => {
    renderController?.abort();
    const controller = new AbortController();
    renderController = controller;
    const revision = ++renderRevision;
    const renderScope = scope;
    currentComparison = current;
    cache.replaceOwner(current);
    cancelDensityRender();
    committedRevision = null;
    jointDensities.left = null;
    jointDensities.right = null;
    complexityDensities.left = null;
    complexityDensities.right = null;
    root.setAttribute("aria-busy", "true");

    const isCurrentRender = (): boolean =>
      renderController === controller &&
      renderRevision === revision &&
      currentComparison === current &&
      scope === renderScope &&
      !controller.signal.aborted;

    try {
      const models = {} as Record<ComparisonSide, SnapshotDistributionModel>;
      for (const side of COMPARISON_SIDES) {
        const variant = variantFor(side, renderScope);
        models[side] = await cache.getAsync(
          current,
          variant,
          (signal) => buildSideModel(current, side, renderScope, signal),
          controller.signal,
        );
      }
      if (!isCurrentRender()) return;
      commitPrepared(
        preparedComparison(current, renderScope, models),
        current,
        revision,
      );
    } catch (error) {
      if (error instanceof DOMException && error.name === "AbortError") return;
      throw error;
    } finally {
      if (renderController === controller && renderRevision === revision) {
        root.setAttribute("aria-busy", "false");
        renderController = null;
      }
    }
  };

  const setScope = (nextScope: ComparisonDistributionScope): void => {
    scope = nextScope;
    syncScopeButtons();
    if (currentComparison !== null) {
      void render(currentComparison);
    }
  };

  const reset = (): void => {
    renderController?.abort();
    renderController = null;
    renderRevision += 1;
    committedRevision = null;
    currentComparison = null;
    cache.reset();
    root.hidden = true;
    root.setAttribute("aria-busy", "false");
    scope = "all";
    syncScopeButtons();
    cancelDensityRender();
    for (const side of COMPARISON_SIDES) {
      jointDensities[side] = null;
      complexityDensities[side] = null;
      resetComparisonDistributionSide(panels[side]);
    }
  };

  for (const candidate of DISTRIBUTION_SCOPES) {
    scopeButtons[candidate].addEventListener("click", () => {
      setScope(candidate);
    });
  }
  const densityResizeObserver = new ResizeObserver(scheduleDensityRender);
  densityResizeObserver.observe(panels.left.joint.container);
  densityResizeObserver.observe(panels.right.joint.container);
  densityResizeObserver.observe(panels.left.complexity.container);
  densityResizeObserver.observe(panels.right.complexity.container);

  return { prepare, canCommit, commit, render, reset };
};
