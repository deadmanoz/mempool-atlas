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
      <div id="summary"></div>
      <div id="selected"></div>
      <button id="any"></button>
      <button id="all"></button>
      <button id="clear"></button>`;
  });

  it("normalizes a multi-label query and keeps label controls stable", () => {
    const onToggleLabel = vi.fn();
    const onSetMatchMode = vi.fn();
    const onClear = vi.fn();
    const lensSelect = document.querySelector<HTMLSelectElement>("#lens");
    const method = document.querySelector<HTMLElement>("#method");
    const empty = document.querySelector<HTMLElement>("#empty");
    const labels = document.querySelector<HTMLElement>("#labels");
    const summary = document.querySelector<HTMLElement>("#summary");
    const selectedSummary = document.querySelector<HTMLElement>("#selected");
    const matchAny = document.querySelector<HTMLButtonElement>("#any");
    const matchAll = document.querySelector<HTMLButtonElement>("#all");
    const clear = document.querySelector<HTMLButtonElement>("#clear");
    if (
      lensSelect === null ||
      method === null ||
      empty === null ||
      labels === null ||
      summary === null ||
      selectedSummary === null ||
      matchAny === null ||
      matchAll === null ||
      clear === null
    ) {
      throw new Error("missing classification overview fixture");
    }
    const view = createClassificationOverviewView(
      {
        lensSelect,
        method,
        empty,
        labels,
        summary,
        selectedSummary,
        matchAny,
        matchAll,
        clear,
      },
      onToggleLabel,
      onSetMatchMode,
      onClear,
    );

    view.render(null, {
      classifierId: "missing",
      labels: [],
      matchMode: "any",
    });
    expect(method.textContent).toContain("Waiting for classifier catalog");

    const selection = view.render(snapshot(), {
      classifierId: "missing",
      labels: ["second", "missing", "first", "second"],
      matchMode: "all",
    });
    expect(selection).toEqual({
      classifierId: "transaction_shape",
      labels: ["first", "second"],
      matchMode: "all",
    });
    expect(lensSelect.value).toBe("transaction_shape");
    expect(method.textContent).toBe("Labels can overlap within this lens.");
    expect(method.textContent).not.toContain("version");
    expect(summary.textContent).toContain("2 complete");
    expect(selectedSummary.textContent).toContain("2 labels selected");
    const second = labels.querySelector<HTMLButtonElement>(
      'button[data-label="second"]',
    );
    expect(second?.getAttribute("aria-pressed")).toBe("true");
    second?.click();
    expect(onToggleLabel).toHaveBeenCalledWith("second");
    expect(matchAny.disabled).toBe(false);
    expect(matchAll.disabled).toBe(false);
    expect(matchAll.getAttribute("aria-pressed")).toBe("true");
    matchAny.click();
    clear.click();
    expect(onSetMatchMode).toHaveBeenCalledWith("any");
    expect(onClear).toHaveBeenCalledOnce();

    view.render(snapshot(), selection);
    expect(
      labels.querySelector<HTMLButtonElement>('button[data-label="second"]'),
    ).toBe(second);
  });

  it("keeps an empty selection instead of choosing the largest label", () => {
    const lensSelect = document.querySelector<HTMLSelectElement>("#lens")!;
    const method = document.querySelector<HTMLElement>("#method")!;
    const empty = document.querySelector<HTMLElement>("#empty")!;
    const labels = document.querySelector<HTMLElement>("#labels")!;
    const summary = document.querySelector<HTMLElement>("#summary")!;
    const selectedSummary = document.querySelector<HTMLElement>("#selected")!;
    const matchAny = document.querySelector<HTMLButtonElement>("#any")!;
    const matchAll = document.querySelector<HTMLButtonElement>("#all")!;
    const clear = document.querySelector<HTMLButtonElement>("#clear")!;
    const view = createClassificationOverviewView(
      {
        lensSelect,
        method,
        empty,
        labels,
        summary,
        selectedSummary,
        matchAny,
        matchAll,
        clear,
      },
      vi.fn(),
      vi.fn(),
      vi.fn(),
    );

    expect(
      view.render(snapshot(), {
        classifierId: "transaction_shape",
        labels: [],
        matchMode: "all",
      }),
    ).toEqual({
      classifierId: "transaction_shape",
      labels: [],
      matchMode: "all",
    });
    expect(labels.querySelectorAll('[aria-pressed="true"]')).toHaveLength(0);
    expect(clear.disabled).toBe(true);
    expect(matchAll.disabled).toBe(true);
  });
});
