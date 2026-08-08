import {
  buildComparisonDistributionSideModel,
  comparisonDistributionVariant,
  comparisonSnapshotIdentity,
  PREPARED_COMPARISON_VIEW,
  sameComparisonSnapshotIdentity,
  type PreparedComparisonDistributions,
} from "./comparison-distribution-model";
import {
  commitComparisonDistributionSide,
  createComparisonDistributionPanels,
  invalidateComparisonDistributionDensities,
  renderComparisonDistributionDensity,
  resetComparisonDistributionSide,
} from "./comparison-distribution-panels";
import {
  comparisonDistributionScopeSuffix,
  type ComparisonDistributionScope,
} from "./comparison-distribution-population";
import type { ComparisonSide, CurrentComparison } from "./comparison-model";
import {
  COMPARISON_DISTRIBUTION_SCOPES,
  createComparisonDistributionControls,
  sameComparisonDistributionSelection,
  type ComparisonDistributionSelection,
} from "./comparison-distribution-controls";
import { prepareJointChartCanvas } from "./detail-panels";
import type { JointDensity } from "./fee-distribution";
import { createSectionDistributionInspector } from "./distribution-interaction";
import {
  SnapshotDistributionCache,
  type SnapshotDistributionModel,
} from "./snapshot-distributions";

export {
  comparisonDistributionScopeSuffix,
  comparisonDistributionTransactions,
  type ComparisonDistributionScope,
} from "./comparison-distribution-population";
export type { PreparedComparisonDistributions } from "./comparison-distribution-model";

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

const COMPARISON_SIDES = ["left", "right"] as const;
export const createComparisonDistributionsView = (
  root: HTMLElement,
): ComparisonDistributionsView => {
  const panels = createComparisonDistributionPanels(root);
  const inspector = createSectionDistributionInspector(
    root,
    ":scope > .comparison-distributions-heading",
  );
  let currentComparison: CurrentComparison | null = null;
  let renderFromControls = (): void => {};
  const controls = createComparisonDistributionControls(root, () => {
    renderFromControls();
  });

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
      renderComparisonDistributionDensity(
        panels[side],
        side,
        density,
        complexity,
        controls.metric(),
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

  const preparedComparison = (
    current: CurrentComparison,
    renderScope: ComparisonDistributionScope,
    renderSelection: ComparisonDistributionSelection,
    models: Record<ComparisonSide, SnapshotDistributionModel>,
    prefetchedModels: Partial<
      Record<
        ComparisonDistributionScope,
        Record<ComparisonSide, SnapshotDistributionModel>
      >
    > = {},
  ): PreparedComparisonDistributions => ({
    current,
    scope: renderScope,
    selection: { ...renderSelection },
    models,
    prefetchedModels,
    variants: {
      left: comparisonDistributionVariant(
        "left",
        renderScope,
        renderSelection.classifierId,
        renderSelection.metric,
      ),
      right: comparisonDistributionVariant(
        "right",
        renderScope,
        renderSelection.classifierId,
        renderSelection.metric,
      ),
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
    const controlSnapshot = controls.snapshot(current);
    const renderScope = controlSnapshot.scope;
    const renderSelection = controlSnapshot.selection;
    const renderSignal = signal ?? new AbortController().signal;
    const buildScopeModels = async (
      targetScope: ComparisonDistributionScope,
    ): Promise<Record<ComparisonSide, SnapshotDistributionModel>> => {
      const models = {} as Record<ComparisonSide, SnapshotDistributionModel>;
      for (const side of COMPARISON_SIDES) {
        models[side] = await buildComparisonDistributionSideModel(
          current,
          side,
          targetScope,
          renderSelection.classifierId,
          renderSelection.metric,
          renderSignal,
        );
      }
      return models;
    };
    const models = await buildScopeModels(renderScope);
    const prefetchedModels: Partial<
      Record<
        ComparisonDistributionScope,
        Record<ComparisonSide, SnapshotDistributionModel>
      >
    > = {};
    if (renderScope === "all" && current.totals.common_count > 0) {
      prefetchedModels.common = await buildScopeModels("common");
    }
    signal?.throwIfAborted();
    return preparedComparison(
      current,
      renderScope,
      renderSelection,
      models,
      prefetchedModels,
    );
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
    inspector.reset();
    invalidateComparisonDistributionDensities(panels);
    committedRevision = null;
    controls.commit(current, {
      scope: prepared.scope,
      selection: prepared.selection,
    });
    for (const side of COMPARISON_SIDES) {
      const { snapshot } = current[side];
      const model = prepared.models[side];
      const descriptor = snapshot.classifier_catalog.find(
        ({ id }) => id === prepared.selection.classifierId,
      );
      cache.adopt(current, prepared.variants[side], model);
      commitComparisonDistributionSide({
        panels: panels[side],
        model,
        side,
        sourceLabel: snapshot.source_label,
        scopeSuffix: comparisonDistributionScopeSuffix(prepared.scope),
        selection: prepared.selection,
        descriptor: descriptor ?? null,
        onSelectBucket: controls.selectBucket,
      });
      jointDensities[side] = model.jointDensity;
      complexityDensities[side] = model.complexityDensity;
    }
    for (const prefetchedScope of COMPARISON_DISTRIBUTION_SCOPES) {
      const models = prepared.prefetchedModels[prefetchedScope];
      if (models === undefined || prefetchedScope === prepared.scope) continue;
      for (const side of COMPARISON_SIDES) {
        cache.adopt(
          current,
          comparisonDistributionVariant(
            side,
            prefetchedScope,
            prepared.selection.classifierId,
            prepared.selection.metric,
          ),
          models[side],
        );
      }
    }
    currentComparison = current;
    root.hidden = false;
    for (const side of COMPARISON_SIDES) {
      const jointDensity = jointDensities[side];
      if (jointDensity !== null) {
        prepareJointChartCanvas(panels[side].joint.canvas, jointDensity);
      }
      const complexityDensity = complexityDensities[side];
      if (complexityDensity !== null) {
        prepareJointChartCanvas(
          panels[side].complexity.canvas,
          complexityDensity,
        );
      }
    }
    committedRevision = revision;
    scheduleDensityRender();
    return true;
  };

  const canCommit = (
    prepared: PreparedComparisonDistributions,
    current: CurrentComparison,
  ): boolean => {
    const controlSnapshot = controls.snapshot(current);
    return !(
      prepared[PREPARED_COMPARISON_VIEW] !== viewToken ||
      prepared.current !== current ||
      prepared.scope !== controlSnapshot.scope ||
      !sameComparisonDistributionSelection(
        prepared.selection,
        controlSnapshot.selection,
      ) ||
      !sameComparisonSnapshotIdentity(current, "left", prepared.leftIdentity) ||
      !sameComparisonSnapshotIdentity(current, "right", prepared.rightIdentity)
    );
  };

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
    const controlSnapshot = controls.snapshot(current);
    const renderScope = controlSnapshot.scope;
    const renderSelection = controlSnapshot.selection;
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
      renderScope === controls.snapshot(current).scope &&
      sameComparisonDistributionSelection(
        renderSelection,
        controls.snapshot(current).selection,
      ) &&
      !controller.signal.aborted;

    try {
      const models = {} as Record<ComparisonSide, SnapshotDistributionModel>;
      for (const side of COMPARISON_SIDES) {
        const variant = comparisonDistributionVariant(
          side,
          renderScope,
          renderSelection.classifierId,
          renderSelection.metric,
        );
        models[side] = await cache.getAsync(
          current,
          variant,
          (signal) =>
            buildComparisonDistributionSideModel(
              current,
              side,
              renderScope,
              renderSelection.classifierId,
              renderSelection.metric,
              signal,
            ),
          controller.signal,
        );
      }
      if (!isCurrentRender()) return;
      commitPrepared(
        preparedComparison(current, renderScope, renderSelection, models),
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

  renderFromControls = () => {
    if (currentComparison !== null) void render(currentComparison);
  };

  const reset = (): void => {
    renderController?.abort();
    renderController = null;
    renderRevision += 1;
    committedRevision = null;
    currentComparison = null;
    cache.reset();
    inspector.reset();
    root.hidden = true;
    root.setAttribute("aria-busy", "false");
    controls.reset();
    cancelDensityRender();
    for (const side of COMPARISON_SIDES) {
      jointDensities[side] = null;
      complexityDensities[side] = null;
      resetComparisonDistributionSide(panels[side]);
    }
  };

  const densityResizeObserver = new ResizeObserver(scheduleDensityRender);
  densityResizeObserver.observe(panels.left.joint.container);
  densityResizeObserver.observe(panels.right.joint.container);
  densityResizeObserver.observe(panels.left.complexity.container);
  densityResizeObserver.observe(panels.right.complexity.container);

  return { prepare, canCommit, commit, render, reset };
};
