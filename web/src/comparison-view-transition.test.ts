import { describe, expect, it } from "vitest";

import { compareCurrentSnapshots } from "./comparison-model";
import { loadedSource } from "./comparison-test-fixtures";
import { mempoolTransaction, txid } from "./test-fixtures";
import {
  executeComparisonViewTransition,
  resolveComparisonViewTransition,
  type ComparisonViewTransitionEffects,
} from "./comparison-view-transition";
import type { ComparisonViewState } from "./view-state";
import type { MempoolTransaction } from "./types";

const transaction = (value: number): MempoolTransaction =>
  mempoolTransaction(value, {
    vsize: 100 + value,
    fee_sats: 200 + value,
    entered_at_ms: 1_700_000_000_000 + value,
    bip110: {
      status: "compatible",
      primary_rule: null,
      violated_rules: [],
      unknown_rules: [],
    },
  });

const comparison = compareCurrentSnapshots(
  loadedSource("core", [transaction(1), transaction(2), transaction(3)]),
  loadedSource("knots", [transaction(1), transaction(2), transaction(4)]),
);

const state = (
  overrides: Partial<ComparisonViewState> = {},
): ComparisonViewState => ({
  left: "core",
  right: "knots",
  region: "common",
  side: "left",
  filter: { kind: "all" },
  txid: null,
  ...overrides,
});

describe("resolveComparisonViewTransition", () => {
  it.each([
    {
      name: "selects another txid",
      previous: state({ txid: txid(1) }),
      requested: state({ txid: txid(2) }),
      selectedTxid: txid(2),
      transactionIndex: 1,
    },
    {
      name: "clears a txid",
      previous: state({ txid: txid(1) }),
      requested: state({ txid: null }),
      selectedTxid: null,
      transactionIndex: 0,
    },
  ])(
    "marks a same-population change that $name as transaction-only",
    ({ previous, requested, selectedTxid, transactionIndex }) => {
      const resolved = resolveComparisonViewTransition(
        comparison,
        previous,
        requested,
      );

      expect(resolved).toMatchObject({
        region: "common",
        side: "left",
        filter: { kind: "all" },
        txid: selectedTxid,
        transactionIndex,
        updateKind: "transaction",
      });
      expect(resolved.selectedEntry?.txid ?? null).toBe(selectedTxid);
    },
  );

  it("requires a population update when selecting a txid clears a filter", () => {
    const resolved = resolveComparisonViewTransition(
      comparison,
      state({
        filter: { kind: "status", status: "compatible" },
        txid: null,
      }),
      state({
        filter: { kind: "status", status: "compatible" },
        txid: txid(1),
      }),
    );

    expect(resolved.filter).toEqual({ kind: "all" });
    expect(resolved.updateKind).toBe("population");
  });

  it("requires a population update when the source-local common side changes", () => {
    const resolved = resolveComparisonViewTransition(
      comparison,
      state({ side: "left", txid: txid(1) }),
      state({ side: "right", txid: txid(1) }),
    );

    expect(resolved.side).toBe("right");
    expect(resolved.updateKind).toBe("population");
  });

  it("requires a population update when the txid resolves into another region", () => {
    const resolved = resolveComparisonViewTransition(
      comparison,
      state({ txid: txid(1) }),
      state({ txid: txid(3) }),
    );

    expect(resolved).toMatchObject({
      region: "left_only",
      side: "left",
      txid: txid(3),
      transactionIndex: 0,
      updateKind: "population",
    });
  });

  it("marks an identical resolved view as requiring no population work", () => {
    const current = state({ txid: txid(2) });
    const resolved = resolveComparisonViewTransition(
      comparison,
      current,
      current,
    );

    expect(resolved.updateKind).toBe("none");
  });
});

describe("executeComparisonViewTransition", () => {
  const effectCounters = (): {
    counters: Record<keyof ComparisonViewTransitionEffects, number>;
    effects: ComparisonViewTransitionEffects;
  } => {
    const counters: Record<keyof ComparisonViewTransitionEffects, number> = {
      applyResolvedView: 0,
      renderPopulation: 0,
      syncTransactionSelection: 0,
      renderTransactionNavigator: 0,
      updateQuery: 0,
      scheduleCanvasRender: 0,
      loadTransactionDetail: 0,
    };
    const count =
      (effect: keyof ComparisonViewTransitionEffects) => (): void => {
        counters[effect] += 1;
      };

    return {
      counters,
      effects: {
        applyResolvedView: count("applyResolvedView"),
        renderPopulation: count("renderPopulation"),
        syncTransactionSelection: count("syncTransactionSelection"),
        renderTransactionNavigator: count("renderTransactionNavigator"),
        updateQuery: count("updateQuery"),
        scheduleCanvasRender: count("scheduleCanvasRender"),
        loadTransactionDetail: count("loadTransactionDetail"),
      },
    };
  };

  it("performs no mutation, rendering, or detail work for an identical view", () => {
    const current = state({ txid: txid(2) });
    const { counters, effects } = effectCounters();

    const resolved = executeComparisonViewTransition(
      comparison,
      current,
      current,
      effects,
    );

    expect(resolved.updateKind).toBe("none");
    expect(counters).toEqual({
      applyResolvedView: 0,
      renderPopulation: 0,
      syncTransactionSelection: 0,
      renderTransactionNavigator: 0,
      updateQuery: 0,
      scheduleCanvasRender: 0,
      loadTransactionDetail: 0,
    });
  });

  it("routes a transaction-only change through the bounded effect path", () => {
    const { counters, effects } = effectCounters();

    const resolved = executeComparisonViewTransition(
      comparison,
      state({ txid: txid(1) }),
      state({ txid: txid(2) }),
      effects,
    );

    expect(resolved.updateKind).toBe("transaction");
    expect(counters).toEqual({
      applyResolvedView: 1,
      renderPopulation: 0,
      syncTransactionSelection: 1,
      renderTransactionNavigator: 1,
      updateQuery: 1,
      scheduleCanvasRender: 1,
      loadTransactionDetail: 1,
    });
  });
});
