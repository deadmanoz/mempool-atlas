// @vitest-environment happy-dom

import { beforeEach, describe, expect, it, vi } from "vitest";

import { createClassificationOverviewView } from "./classification-overview-view";
import type { MempoolSnapshot } from "./types";

const snapshot = (): MempoolSnapshot => ({
  source_id: "core",
  source_label: "Bitcoin Core",
  collection_started_at_ms: 1_700_000_000_000,
  collection_completed_at_ms: 1_700_000_001_000,
  collection_duration_ms: 1_000,
  observed_at_ms: 1_700_000_001_000,
  classification_revision: 3,
  chain_tip: { height: 917_432, hash: "ab".repeat(32) },
  transaction_count: 3,
  total_vsize: 600,
  classifier_catalog: [
    {
      id: "transaction_shape",
      version: "1",
      title: "Transaction shape",
      methodology: "heuristic",
      semantics: "multi_label",
      required_facts: ["raw_transaction"],
      labels: [
        { key: "first", label: "First", description: "First shape." },
        { key: "second", label: "Second", description: "Second shape." },
      ],
    },
  ],
  classification_summaries: [
    {
      classifier_id: "transaction_shape",
      complete_count: 2,
      partial_count: 0,
      unclassified_count: 1,
      label_counts: { first: 1, second: 2 },
    },
  ],
  bip110_summary: {
    evaluator_id: "rdts-rules",
    evaluator_version: "1.0.0",
    scope: "knots_mempool_policy",
    compatible_count: 2,
    violating_count: 0,
    indeterminate_count: 0,
    unclassified_count: 1,
  },
  transactions: [],
});

describe("classification overview view", () => {
  beforeEach(() => {
    document.body.innerHTML = `
      <select id="lens"></select>
      <div id="method"></div>
      <div id="empty"></div>
      <div id="labels"></div>
      <div id="summary"></div>`;
  });

  it("normalizes selection and keeps label controls stable across renders", () => {
    const onSelectLabel = vi.fn();
    const lensSelect = document.querySelector<HTMLSelectElement>("#lens");
    const method = document.querySelector<HTMLElement>("#method");
    const empty = document.querySelector<HTMLElement>("#empty");
    const labels = document.querySelector<HTMLElement>("#labels");
    const summary = document.querySelector<HTMLElement>("#summary");
    if (
      lensSelect === null ||
      method === null ||
      empty === null ||
      labels === null ||
      summary === null
    ) {
      throw new Error("missing classification overview fixture");
    }
    const view = createClassificationOverviewView(
      { lensSelect, method, empty, labels, summary },
      onSelectLabel,
    );

    view.render(null, { classifierId: "missing", label: null });
    expect(method.textContent).toContain("Waiting for classifier catalog");

    const selection = view.render(snapshot(), {
      classifierId: "missing",
      label: "missing",
    });
    expect(selection).toEqual({
      classifierId: "transaction_shape",
      label: "second",
    });
    expect(lensSelect.value).toBe("transaction_shape");
    expect(method.textContent).toContain("Heuristic signals");
    expect(summary.textContent).toContain("2 complete");
    const second = labels.querySelector<HTMLButtonElement>(
      'button[data-label="second"]',
    );
    expect(second?.getAttribute("aria-pressed")).toBe("true");
    second?.click();
    expect(onSelectLabel).toHaveBeenCalledWith("second");

    view.render(snapshot(), selection);
    expect(
      labels.querySelector<HTMLButtonElement>('button[data-label="second"]'),
    ).toBe(second);
  });
});
