import {
  lookupComparisonTransaction,
  policySideForRegion,
  type ComparedTransaction,
  type ComparisonPolicyFilter,
  type ComparisonRegionKey,
  type ComparisonSide,
  type CurrentComparison,
} from "./comparison-model";
import type { ComparisonViewState } from "./view-state";
import type { ComparisonCanvasRenderStatus } from "./comparison-canvas-view";

export type ComparisonViewUpdateKind = "none" | "transaction" | "population";

export interface ResolvedComparisonViewTransition {
  region: ComparisonRegionKey;
  side: ComparisonSide;
  filter: ComparisonPolicyFilter;
  txid: string | null;
  selectedEntry: ComparedTransaction | null;
  transactionIndex: number;
  updateKind: ComparisonViewUpdateKind;
}

export interface ComparisonViewTransitionEffects {
  applyResolvedView: (resolved: ResolvedComparisonViewTransition) => void;
  renderPopulation: () => void;
  renderTransactionNavigator: () => void;
  updateQuery: () => void;
  scheduleCanvasRender: () => Promise<ComparisonCanvasRenderStatus>;
  handleCanvasRenderFailure: (error: unknown) => void;
  loadTransactionDetail: (entry: ComparedTransaction) => void;
}

export const comparisonPolicyFiltersMatch = (
  left: ComparisonPolicyFilter,
  right: ComparisonPolicyFilter,
): boolean => {
  if (left.kind !== right.kind) {
    return false;
  }
  if (left.kind === "all") {
    return true;
  }
  if (left.kind === "rule") {
    return (
      left.rule ===
      (right as Extract<ComparisonPolicyFilter, { kind: "rule" }>).rule
    );
  }
  if (left.kind === "status") {
    return (
      left.status ===
      (right as Extract<ComparisonPolicyFilter, { kind: "status" }>).status
    );
  }
  return (
    left.signature ===
    (right as Extract<ComparisonPolicyFilter, { kind: "signature" }>).signature
  );
};

/**
 * Resolve txid-driven region changes before deciding which comparison UI is
 * stale. Population identity excludes txid and includes only the effective
 * per-node region, side, and policy filter.
 */
export const resolveComparisonViewTransition = (
  comparison: CurrentComparison,
  previous: ComparisonViewState,
  requested: ComparisonViewState,
): ResolvedComparisonViewTransition => {
  const lookup =
    requested.txid === null
      ? null
      : lookupComparisonTransaction(comparison, requested.txid);
  const region = lookup?.region ?? requested.region ?? "common";
  const side = policySideForRegion(region, requested.side ?? "left");
  const filter = requested.filter;
  const previousRegion = previous.region ?? "common";
  const previousSide = policySideForRegion(
    previousRegion,
    previous.side ?? "left",
  );
  const populationChanged =
    previousRegion !== region ||
    previousSide !== side ||
    !comparisonPolicyFiltersMatch(previous.filter, filter);
  const transactionChanged = previous.txid !== requested.txid;

  return {
    region,
    side,
    filter,
    txid: requested.txid,
    selectedEntry: lookup?.entry ?? null,
    transactionIndex: lookup?.index ?? 0,
    updateKind: populationChanged
      ? "population"
      : transactionChanged
        ? "transaction"
        : "none",
  };
};

/**
 * Apply an interactive comparison transition only when its resolved state
 * changes. Snapshot refreshes deliberately bypass this router because their
 * new comparison identity requires detail and rendered data to be refreshed.
 */
export const executeComparisonViewTransition = (
  comparison: CurrentComparison,
  previous: ComparisonViewState,
  requested: ComparisonViewState,
  effects: ComparisonViewTransitionEffects,
): ResolvedComparisonViewTransition => {
  const resolved = resolveComparisonViewTransition(
    comparison,
    previous,
    requested,
  );
  if (resolved.updateKind === "none") {
    return resolved;
  }

  effects.applyResolvedView(resolved);
  if (resolved.updateKind === "population") {
    effects.renderPopulation();
  }
  effects.renderTransactionNavigator();
  effects.updateQuery();
  void effects
    .scheduleCanvasRender()
    .catch((error: unknown) => effects.handleCanvasRenderFailure(error));
  if (resolved.selectedEntry !== null) {
    effects.loadTransactionDetail(resolved.selectedEntry);
  }

  return resolved;
};
