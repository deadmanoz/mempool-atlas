import type { ComparisonDistributionScope } from "./comparison-distribution-population";
import type { CurrentComparison } from "./comparison-model";
import {
  TRANSACTION_PROPERTIES_CLASSIFIER_ID,
  type ClassifierBucketKey,
} from "./classifier-terrain";
import type { TerrainMode } from "./terrain";
import type { ClassifierDescriptor } from "./types";

export interface ComparisonDistributionSelection {
  classifierId: string | null;
  bucketKey: ClassifierBucketKey | null;
  metric: TerrainMode;
}

export interface ComparisonDistributionControlSnapshot {
  scope: ComparisonDistributionScope;
  selection: ComparisonDistributionSelection;
}

export interface ComparisonDistributionControls {
  snapshot(current: CurrentComparison): ComparisonDistributionControlSnapshot;
  commit(
    current: CurrentComparison,
    snapshot: ComparisonDistributionControlSnapshot,
  ): void;
  selectBucket(selection: {
    classifierId: string;
    bucketKey: ClassifierBucketKey;
  }): void;
  metric(): TerrainMode;
  reset(): void;
}

export const COMPARISON_DISTRIBUTION_SCOPES: readonly ComparisonDistributionScope[] =
  ["all", "common", "left_only", "right_only"];
const DISTRIBUTION_METRICS: readonly TerrainMode[] = ["count", "vsize"];

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

const sharedClassifierCatalog = (
  current: CurrentComparison,
): readonly ClassifierDescriptor[] => {
  const rightIds = new Set(
    current.right.snapshot.classifier_catalog.map(({ id }) => id),
  );
  return current.left.snapshot.classifier_catalog.filter(({ id }) =>
    rightIds.has(id),
  );
};

const resolvedClassifierId = (
  current: CurrentComparison,
  requested: string | null,
): string | null => {
  const catalog = sharedClassifierCatalog(current);
  if (catalog.some(({ id }) => id === requested)) return requested;
  return (
    catalog.find(({ id }) => id === TRANSACTION_PROPERTIES_CLASSIFIER_ID)?.id ??
    catalog[0]?.id ??
    null
  );
};

export const sameComparisonDistributionSelection = (
  left: ComparisonDistributionSelection,
  right: ComparisonDistributionSelection,
): boolean =>
  left.classifierId === right.classifierId &&
  left.bucketKey === right.bucketKey &&
  left.metric === right.metric;

export const createComparisonDistributionControls = (
  root: HTMLElement,
  onChange: () => void,
): ComparisonDistributionControls => {
  const scopeButtons: Record<ComparisonDistributionScope, HTMLButtonElement> = {
    all: requiredDescendant<HTMLButtonElement>(root, "dist-scope-all"),
    common: requiredDescendant<HTMLButtonElement>(root, "dist-scope-common"),
    left_only: requiredDescendant<HTMLButtonElement>(root, "dist-scope-left"),
    right_only: requiredDescendant<HTMLButtonElement>(root, "dist-scope-right"),
  };
  const classifierSelect = requiredDescendant<HTMLSelectElement>(
    root,
    "dist-classifier",
  );
  const metricButtons: Record<TerrainMode, HTMLButtonElement> = {
    count: requiredDescendant<HTMLButtonElement>(root, "dist-metric-count"),
    vsize: requiredDescendant<HTMLButtonElement>(root, "dist-metric-vsize"),
  };
  const summary = requiredDescendant<HTMLElement>(
    root,
    "comparison-distribution-summary",
  );

  let currentComparison: CurrentComparison | null = null;
  let scope: ComparisonDistributionScope = "all";
  let classifierId: string | null = TRANSACTION_PROPERTIES_CLASSIFIER_ID;
  let bucketKey: ClassifierBucketKey | null = null;
  let selectedMetric: TerrainMode = "vsize";

  const selectionFor = (
    current: CurrentComparison,
  ): ComparisonDistributionSelection => {
    const resolved = resolvedClassifierId(current, classifierId);
    return {
      classifierId: resolved,
      bucketKey: resolved === classifierId ? bucketKey : null,
      metric: selectedMetric,
    };
  };

  const syncScope = (): void => {
    for (const candidate of COMPARISON_DISTRIBUTION_SCOPES) {
      scopeButtons[candidate].setAttribute(
        "aria-pressed",
        String(candidate === scope),
      );
    }
  };

  const syncSelection = (
    current: CurrentComparison,
    selection: ComparisonDistributionSelection,
  ): void => {
    const catalog = sharedClassifierCatalog(current);
    const options = catalog.map((descriptor) => {
      const option = document.createElement("option");
      option.value = descriptor.id;
      option.textContent = descriptor.title;
      return option;
    });
    if (options.length === 0) {
      const option = document.createElement("option");
      option.value = "";
      option.textContent = "No shared classifier lens";
      options.push(option);
    }
    classifierSelect.replaceChildren(...options);
    classifierSelect.value = selection.classifierId ?? "";
    classifierSelect.disabled = catalog.length < 2;
    for (const candidate of DISTRIBUTION_METRICS) {
      metricButtons[candidate].setAttribute(
        "aria-pressed",
        String(candidate === selection.metric),
      );
    }
    const descriptor = catalog.find(({ id }) => id === selection.classifierId);
    const metricName =
      selection.metric === "count" ? "transaction count" : "virtual size";
    summary.textContent = `Shared axes use ${descriptor?.title ?? "the available classifier"} and are weighted by ${metricName}. Absence from one node is not evidence of rejection.`;
  };

  const snapshot = (
    current: CurrentComparison,
  ): ComparisonDistributionControlSnapshot => ({
    scope,
    selection: selectionFor(current),
  });

  const commit = (
    current: CurrentComparison,
    next: ComparisonDistributionControlSnapshot,
  ): void => {
    currentComparison = current;
    scope = next.scope;
    classifierId = next.selection.classifierId;
    bucketKey = next.selection.bucketKey;
    selectedMetric = next.selection.metric;
    syncScope();
    syncSelection(current, next.selection);
  };

  const selectBucket = (selection: {
    classifierId: string;
    bucketKey: ClassifierBucketKey;
  }): void => {
    classifierId = selection.classifierId;
    bucketKey = selection.bucketKey;
    if (currentComparison !== null) {
      syncSelection(currentComparison, selectionFor(currentComparison));
      onChange();
    }
  };

  for (const candidate of COMPARISON_DISTRIBUTION_SCOPES) {
    scopeButtons[candidate].addEventListener("click", () => {
      if (scope === candidate) return;
      scope = candidate;
      syncScope();
      onChange();
    });
  }
  classifierSelect.addEventListener("change", () => {
    const current = currentComparison;
    if (
      current === null ||
      !sharedClassifierCatalog(current).some(
        ({ id }) => id === classifierSelect.value,
      )
    ) {
      return;
    }
    classifierId = classifierSelect.value;
    bucketKey = null;
    syncSelection(current, selectionFor(current));
    onChange();
  });
  for (const candidate of DISTRIBUTION_METRICS) {
    metricButtons[candidate].addEventListener("click", () => {
      if (selectedMetric === candidate) return;
      selectedMetric = candidate;
      if (currentComparison !== null) {
        syncSelection(currentComparison, selectionFor(currentComparison));
      }
      onChange();
    });
  }

  const reset = (): void => {
    currentComparison = null;
    scope = "all";
    classifierId = TRANSACTION_PROPERTIES_CLASSIFIER_ID;
    bucketKey = null;
    selectedMetric = "vsize";
    syncScope();
    classifierSelect.replaceChildren();
    classifierSelect.disabled = true;
    for (const candidate of DISTRIBUTION_METRICS) {
      metricButtons[candidate].setAttribute(
        "aria-pressed",
        String(candidate === selectedMetric),
      );
    }
    summary.textContent =
      "Shared axes use the selected lens and snapshot metric. Absence from one node is not evidence of rejection.";
  };

  return {
    snapshot,
    commit,
    selectBucket,
    metric: () => selectedMetric,
    reset,
  };
};
