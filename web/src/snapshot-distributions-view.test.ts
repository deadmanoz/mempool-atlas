// @vitest-environment happy-dom

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { ClassifierBucketKey } from "./classifier-terrain";
import { loadedSource } from "./comparison-test-fixtures";
import {
  installDistributionViewTestHarness,
  type DistributionViewTestHarness,
} from "./distribution-view-test-harness";
import {
  createSnapshotDistributionsView,
  type SnapshotDistributionSelection,
} from "./snapshot-distributions-view";
import { mempoolTransaction } from "./test-fixtures";
import type {
  ClassificationResult,
  ClassifierDescriptor,
  MempoolSnapshot,
  MempoolTransaction,
} from "./types";

const descriptor: ClassifierDescriptor = {
  id: "transaction_shapes",
  version: "1",
  title: "Transaction shapes",
  methodology: "heuristic",
  semantics: "multi_label",
  required_facts: ["raw_transaction"],
  labels: [
    { key: "alpha", label: "Alpha", description: "Alpha shape." },
    { key: "beta", label: "Beta", description: "Beta shape." },
  ],
};

const result = (label: "alpha" | "beta"): ClassificationResult => ({
  classifier_id: descriptor.id,
  state: "complete",
  primary_label: label,
  labels: [label],
  missing_facts: [],
  evidence: null,
});

const transaction = (
  value: number,
  label: "alpha" | "beta",
): MempoolTransaction =>
  mempoolTransaction(value, {
    vsize: 100 + value,
    weight: (100 + value) * 4,
    fee_sats: 400 + value,
    entered_at_ms: 1_700_000_000_000 + value,
    ancestor_vsize: 100 + value,
    ancestor_fee_sats: 400 + value,
    descendant_vsize: 100 + value,
    replaceable: value % 2 === 0,
    structure: {
      input_count: value,
      output_count: value + 1,
      op_return_bytes: value === 1 ? 24 : 0,
      output_sats: value * 10_000,
      witness_bytes: value * 10,
    },
    classifications: [result(label)],
  });

const snapshot = (
  transactions: MempoolTransaction[],
  observedAtMs: number = 1_700_000_010_000,
): MempoolSnapshot => ({
  ...loadedSource("core", transactions, observedAtMs).snapshot,
  classifier_catalog: [descriptor],
});

const selection = (
  bucketKey: ClassifierBucketKey | null = "complete:0",
): SnapshotDistributionSelection => ({
  classifierId: descriptor.id,
  bucketKey,
  metric: "count",
});

const snapshotDistributionMarkup = (): string => `
  <section id="snapshot-distributions">
    <p id="distribution-empty"></p>
    <div id="distribution-grid">
      <p id="joint-note"></p>
      <div id="joint-chart"><canvas id="joint-canvas"></canvas></div>
      <p id="composition-note"></p>
      <div id="composition-bars"></div>
      <p id="spectrum-note"></p>
      <div id="spectrum-chart"></div>
      <p id="package-note"></p>
      <div id="package-chart"></div>
      <p id="mosaic-note"></p>
      <div id="mosaic-chart"></div>
      <p id="data-note"></p>
      <div id="data-chart"></div>
      <p id="complexity-note"></p>
      <div id="complexity-chart"><canvas id="complexity-canvas"></canvas></div>
      <p id="entanglement-note"></p>
      <div id="entanglement-bars"></div>
      <p id="value-note"></p>
      <div id="value-chart"></div>
    </div>
  </section>
`;

describe("createSnapshotDistributionsView", () => {
  let harness: DistributionViewTestHarness;

  beforeEach(() => {
    harness = installDistributionViewTestHarness();
    document.body.innerHTML = `
      <div id="composition-bars">outside sentinel</div>
      ${snapshotDistributionMarkup()}
    `;
  });

  afterEach(() => {
    harness.cleanup();
  });

  it("owns root-scoped rendering, axes, scheduling, and bucket events", () => {
    const onSelectBucket = vi.fn();
    const view = createSnapshotDistributionsView({ onSelectBucket });
    const root = document.querySelector<HTMLElement>(
      "section#snapshot-distributions",
    )!;

    expect(harness.resizeObservers).toHaveLength(2);
    expect(harness.resizeObservers[0]?.observed).toHaveLength(1);
    expect(harness.resizeObservers[1]?.observed).toHaveLength(1);
    expect(root.querySelectorAll("#joint-chart .panel-axis")).toHaveLength(1);
    expect(root.querySelectorAll("#complexity-chart .panel-axis")).toHaveLength(
      1,
    );

    view.render(
      snapshot([transaction(1, "alpha"), transaction(2, "beta")]),
      selection(),
    );

    expect(root.querySelector<HTMLElement>("#distribution-grid")?.hidden).toBe(
      false,
    );
    expect(root.querySelector("#spectrum-chart svg")).not.toBeNull();
    expect(document.body.firstElementChild?.textContent).toBe(
      "outside sentinel",
    );
    const buttons = root.querySelectorAll<HTMLButtonElement>(
      `#composition-bars button[data-classifier="${descriptor.id}"]`,
    );
    expect(buttons).toHaveLength(2);
    buttons[1]?.click();
    expect(onSelectBucket).toHaveBeenCalledWith({
      classifierId: descriptor.id,
      bucketKey: "complete:1",
    });
    expect(harness.pendingAnimationFrames()).toBe(1);
    harness.resizeObservers[0]?.trigger();
    expect(harness.pendingAnimationFrames()).toBe(1);
    harness.flushAnimationFrames();
    expect(harness.canvasContext.setTransform).toHaveBeenCalled();
  });

  it("updates selection aria state without rebuilding aggregate panels", () => {
    const view = createSnapshotDistributionsView({
      onSelectBucket: vi.fn(),
    });
    const root = document.querySelector<HTMLElement>(
      "section#snapshot-distributions",
    )!;
    view.render(
      snapshot([transaction(1, "alpha"), transaction(2, "beta")]),
      selection(),
    );
    const composition = root.querySelector("#composition-bars")!;
    const spectrum = root.querySelector("#spectrum-chart svg")!;
    const first = composition.querySelector<HTMLButtonElement>(
      'button[data-segment="complete:0"]',
    )!;
    const second = composition.querySelector<HTMLButtonElement>(
      'button[data-segment="complete:1"]',
    )!;
    const originalComposition = composition.firstElementChild;

    view.setSelection(descriptor.id, "complete:1");

    expect(composition.firstElementChild).toBe(originalComposition);
    expect(root.querySelector("#spectrum-chart svg")).toBe(spectrum);
    expect(first.hasAttribute("aria-pressed")).toBe(false);
    expect(second.getAttribute("aria-pressed")).toBe("true");
  });

  it("replaces empty and nonempty owners and cancels reset lifecycle work", () => {
    const view = createSnapshotDistributionsView({
      onSelectBucket: vi.fn(),
    });
    const root = document.querySelector<HTMLElement>(
      "section#snapshot-distributions",
    )!;
    view.render(snapshot([transaction(1, "alpha")]), selection());
    expect(harness.pendingAnimationFrames()).toBe(1);

    view.render(snapshot([]), selection());

    expect(root.querySelector<HTMLElement>("#distribution-grid")?.hidden).toBe(
      true,
    );
    expect(root.querySelector<HTMLElement>("#distribution-empty")?.hidden).toBe(
      false,
    );
    expect(root.querySelector("#distribution-empty")?.textContent).toBe(
      "This snapshot contains an empty mempool.",
    );
    expect(harness.pendingAnimationFrames()).toBe(0);

    view.render(snapshot([transaction(2, "beta")]), selection("complete:1"));
    expect(root.querySelector<HTMLElement>("#distribution-grid")?.hidden).toBe(
      false,
    );
    expect(
      root.querySelector('#composition-bars button[data-segment="complete:1"]'),
    ).not.toBeNull();
    expect(harness.pendingAnimationFrames()).toBe(1);

    view.reset("Loading current snapshot.");

    expect(root.querySelector<HTMLElement>("#distribution-grid")?.hidden).toBe(
      true,
    );
    expect(root.querySelector("#distribution-empty")?.textContent).toBe(
      "Loading current snapshot.",
    );
    expect(harness.pendingAnimationFrames()).toBe(0);
    expect(harness.cancelledAnimationFrames).toHaveLength(2);
  });
});
