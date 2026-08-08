// @vitest-environment happy-dom

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { ClassifierBucketKey } from "./classifier-terrain";
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
      recognized_carried_bytes: value === 1 ? 24 : 0,
      output_sats: value * 10_000,
      witness_bytes: value * 10,
    },
    classifications: [result(label)],
  });

const snapshot = (
  transactions: MempoolTransaction[],
  observedAtMs: number = 1_700_000_010_000,
): MempoolSnapshot => ({
  source_id: "core",
  source_label: "CORE",
  collection_started_at_ms: observedAtMs - 1_000,
  collection_completed_at_ms: observedAtMs,
  collection_duration_ms: 1_000,
  observed_at_ms: observedAtMs,
  classification_revision: 1,
  chain_tip: { height: 900_000, hash: "00".repeat(32) },
  transaction_count: transactions.length,
  total_vsize: transactions.reduce((total, entry) => total + entry.vsize, 0),
  classifier_catalog: [descriptor],
  classification_summaries: [],
  bip110_summary: {
    evaluator_id: "rdts-rules",
    evaluator_version: "0.1.0",
    scope: "knots_mempool_policy",
    compatible_count: 0,
    violating_count: 0,
    indeterminate_count: 0,
    unclassified_count: transactions.length,
  },
  transactions,
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
    <div class="terrain-heading">
      <div><h2>Snapshot distributions</h2></div>
      <div class="distribution-column-control">
        <button id="distribution-columns-auto" aria-pressed="true">Auto</button>
        <button id="distribution-columns-1" aria-pressed="false">1</button>
        <button id="distribution-columns-2" aria-pressed="false">2</button>
        <button id="distribution-columns-3" aria-pressed="false">3</button>
      </div>
    </div>
    <p id="distribution-empty"></p>
    <div id="distribution-grid" data-columns="auto">
      <p id="joint-note"></p>
      <div id="joint-y-axis"></div>
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
      <div id="complexity-y-axis"></div>
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
    window.localStorage.clear();
    document.body.innerHTML = `
      <div id="composition-bars">outside sentinel</div>
      ${snapshotDistributionMarkup()}
    `;
  });

  afterEach(() => {
    vi.useRealTimers();
    window.localStorage.clear();
    harness.cleanup();
  });

  it("applies and persists an explicit panel column count", () => {
    window.localStorage.setItem(
      "mempool-atlas.snapshot-distribution-columns",
      "2",
    );
    createSnapshotDistributionsView({ onSelectBucket: vi.fn() });
    const grid = document.querySelector<HTMLElement>("#distribution-grid")!;
    const auto = document.querySelector<HTMLButtonElement>(
      "#distribution-columns-auto",
    )!;
    const one = document.querySelector<HTMLButtonElement>(
      "#distribution-columns-1",
    )!;
    const two = document.querySelector<HTMLButtonElement>(
      "#distribution-columns-2",
    )!;

    expect(grid.dataset.columns).toBe("2");
    expect(two.getAttribute("aria-pressed")).toBe("true");
    expect(auto.getAttribute("aria-pressed")).toBe("false");

    one.click();

    expect(grid.dataset.columns).toBe("1");
    expect(one.getAttribute("aria-pressed")).toBe("true");
    expect(two.getAttribute("aria-pressed")).toBe("false");
    expect(
      window.localStorage.getItem(
        "mempool-atlas.snapshot-distribution-columns",
      ),
    ).toBe("1");
  });

  it("owns root-scoped async rendering, axes, scheduling, and bucket events", async () => {
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

    const rendered = view.render(
      snapshot([transaction(1, "alpha"), transaction(2, "beta")]),
      selection(),
    );
    expect(root.getAttribute("aria-busy")).toBe("true");
    await rendered;
    expect(root.getAttribute("aria-busy")).toBe("false");

    expect(root.querySelector<HTMLElement>("#distribution-grid")?.hidden).toBe(
      false,
    );
    expect(root.querySelector("#spectrum-chart svg")).not.toBeNull();
    expect(
      root.querySelector("#complexity-chart .panel-axis-title")?.textContent,
    ).toBe("Inputs");
    expect(
      root.querySelector("#joint-chart .panel-axis-title")?.textContent,
    ).toBe("Fee rate");
    expect(
      root.querySelector("#value-chart .panel-axis-title")?.textContent,
    ).toBe("Total output value");
    expect(
      root.querySelector("#value-chart .spectrum-y-axis-title")?.textContent,
    ).toBe("transaction count");
    expect(root.querySelectorAll(".spectrum-y-axis")).toHaveLength(4);
    expect(
      root.querySelector("#value-chart .spectrum-y-axis")?.textContent,
    ).toContain("tx");
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
    await harness.flushAnimationFrames();
    await harness.flushAnimationFrames();
    await harness.flushAnimationFrames();
    expect(
      root.querySelector("#complexity-y-axis .joint-y-axis-title")?.textContent,
    ).toBe("Outputs");
    expect(
      root.querySelector("#joint-y-axis .joint-y-axis-title")?.textContent,
    ).toBe("Virtual size");
    expect(harness.canvasContext.setTransform).toHaveBeenCalled();
  });

  it("updates selection aria state without rebuilding aggregate panels", async () => {
    const view = createSnapshotDistributionsView({
      onSelectBucket: vi.fn(),
    });
    const root = document.querySelector<HTMLElement>(
      "section#snapshot-distributions",
    )!;
    await view.render(
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
    expect(first.getAttribute("aria-pressed")).toBe("false");
    expect(second.getAttribute("aria-pressed")).toBe("true");
  });

  it("makes spectra, reference ticks, and density regions inspectable", async () => {
    const view = createSnapshotDistributionsView({
      onSelectBucket: vi.fn(),
    });
    const root = document.querySelector<HTMLElement>(
      "section#snapshot-distributions",
    )!;
    await view.render(
      snapshot([transaction(1, "alpha"), transaction(2, "beta")]),
      selection(),
    );

    const spectrum = root.querySelector<SVGElement>("#spectrum-chart svg")!;
    expect(spectrum.getAttribute("role")).toBe("button");
    expect(spectrum.getAttribute("aria-label")).toContain("Fee rate:");
    const originalBin = spectrum.getAttribute(
      "data-distribution-inspection-key",
    );
    spectrum.focus();
    spectrum.dispatchEvent(
      new KeyboardEvent("keydown", { key: "ArrowRight", bubbles: true }),
    );
    expect(spectrum.getAttribute("data-distribution-inspection-key")).not.toBe(
      originalBin,
    );
    expect(
      root.querySelector<HTMLElement>(".distribution-inspection-tooltip")
        ?.hidden,
    ).toBe(false);

    spectrum.dispatchEvent(new MouseEvent("click", { bubbles: true }));
    expect(spectrum.getAttribute("data-distribution-inspection-pinned")).toBe(
      "true",
    );
    expect(spectrum.getAttribute("aria-pressed")).toBe("true");
    spectrum.dispatchEvent(
      new KeyboardEvent("keydown", { key: "Enter", bubbles: true }),
    );
    expect(spectrum.hasAttribute("data-distribution-inspection-pinned")).toBe(
      false,
    );
    expect(spectrum.getAttribute("aria-pressed")).toBe("false");

    spectrum.dispatchEvent(new MouseEvent("click", { bubbles: true }));
    const clear = root.querySelector<HTMLButtonElement>(
      ".distribution-inspection-control",
    )!;
    expect(clear.disabled).toBe(false);
    clear.click();
    expect(spectrum.hasAttribute("data-distribution-inspection-pinned")).toBe(
      false,
    );

    spectrum.focus();
    spectrum.dispatchEvent(new MouseEvent("click", { bubbles: true }));
    spectrum.dispatchEvent(
      new KeyboardEvent("keydown", { key: "Escape", bubbles: true }),
    );
    const escapedBin = spectrum.getAttribute(
      "data-distribution-inspection-key",
    );
    spectrum.dispatchEvent(
      new KeyboardEvent("keydown", { key: "ArrowRight", bubbles: true }),
    );
    expect(spectrum.getAttribute("data-distribution-inspection-key")).not.toBe(
      escapedBin,
    );
    expect(
      root.querySelector<HTMLElement>(".distribution-inspection-tooltip")
        ?.hidden,
    ).toBe(false);

    const references = root.querySelectorAll(
      "#data-chart .panel-axis [data-reference='true']",
    );
    expect([...references].map(({ textContent }) => textContent)).toEqual([
      "40 B",
      "80 B",
    ]);
    expect(
      root.querySelectorAll(
        "#data-chart .chart-gridline[data-reference='true']",
      ),
    ).toHaveLength(2);

    await harness.flushAnimationFrames();
    await harness.flushAnimationFrames();
    const canvas = root.querySelector<HTMLCanvasElement>("#joint-canvas")!;
    expect(canvas.getAttribute("role")).toBe("button");
    expect(canvas.getAttribute("aria-label")).toContain("Fee rate:");
    expect(
      root.querySelector("#joint-chart .joint-inspection-highlight"),
    ).not.toBeNull();
    expect(root.querySelector("#joint-y-axis")?.textContent).toContain(
      "1 KivB",
    );

    canvas.getBoundingClientRect = () =>
      ({
        x: 0,
        y: 0,
        left: 0,
        top: 0,
        right: 480,
        bottom: 260,
        width: 480,
        height: 260,
        toJSON: () => ({}),
      }) as DOMRect;
    const gapMove = new Event("pointermove", { bubbles: true });
    Object.defineProperties(gapMove, {
      clientX: { value: 452 },
      clientY: { value: 60 },
    });
    canvas.dispatchEvent(gapMove);
    expect(canvas.getAttribute("data-distribution-inspection-title")).toBe(
      "Between plotted regions",
    );
    canvas.focus();
    canvas.dispatchEvent(new MouseEvent("click", { bubbles: true }));
    expect(canvas.hasAttribute("data-distribution-inspection-pinned")).toBe(
      false,
    );

    const gridMove = new Event("pointermove", { bubbles: true });
    Object.defineProperties(gridMove, {
      clientX: { value: 120 },
      clientY: { value: 120 },
    });
    canvas.dispatchEvent(gridMove);
    canvas.dispatchEvent(new MouseEvent("click", { bubbles: true }));
    const pinnedCell = canvas.getAttribute("data-distribution-inspection-key");
    expect(canvas.getAttribute("data-distribution-inspection-pinned")).toBe(
      "true",
    );

    harness.resizeObservers[0]?.trigger();
    await harness.flushAnimationFrames();

    expect(canvas.getAttribute("data-distribution-inspection-key")).toBe(
      pinnedCell,
    );
    expect(canvas.getAttribute("data-distribution-inspection-pinned")).toBe(
      "true",
    );
    expect(
      root.querySelector<HTMLElement>(
        "#joint-chart .joint-inspection-highlight",
      )?.hidden,
    ).toBe(false);
  });

  it("prepares a replacement without touching the active view and commits only its exact candidate", async () => {
    const view = createSnapshotDistributionsView({
      onSelectBucket: vi.fn(),
    });
    const root = document.querySelector<HTMLElement>(
      "section#snapshot-distributions",
    )!;
    const active = snapshot([transaction(1, "alpha"), transaction(2, "beta")]);
    const candidate = snapshot(
      [transaction(10_000, "beta")],
      active.observed_at_ms + 1_000,
    );
    await view.render(active, selection());
    await harness.flushAnimationFrames();
    await harness.flushAnimationFrames();
    const activeSpectrum = root.querySelector("#spectrum-chart svg");
    const activeDensity =
      root.querySelector<HTMLCanvasElement>("#joint-canvas")!;
    expect(activeDensity.getAttribute("role")).toBe("button");

    const prepared = await view.prepare(candidate, selection("complete:1"));

    expect(root.getAttribute("aria-busy")).toBe("false");
    expect(root.querySelector("#spectrum-chart svg")).toBe(activeSpectrum);
    expect(activeDensity.getAttribute("role")).toBe("button");
    expect(root.querySelector("#value-note")?.textContent).toContain("2 of 2");
    expect(view.canCommit(prepared, candidate, selection("complete:1"))).toBe(
      true,
    );
    expect(view.canCommit(prepared, { ...candidate }, selection())).toBe(false);
    expect(
      view.commit(prepared, { ...candidate }, selection("complete:1")),
    ).toBe(false);
    expect(view.commit(prepared, candidate, selection())).toBe(false);
    expect(root.querySelector("#spectrum-chart svg")).toBe(activeSpectrum);

    expect(view.commit(prepared, candidate, selection("complete:1"))).toBe(
      true,
    );
    expect(activeDensity.getAttribute("role")).toBeNull();
    expect(activeDensity.getAttribute("aria-hidden")).toBe("true");
    expect(activeDensity.dataset.distributionInspectionKey).toBeUndefined();
    expect(root.querySelector("#value-note")?.textContent).toContain("1 of 1");
    expect(
      root.querySelector('#composition-bars button[data-segment="complete:1"]'),
    ).not.toBeNull();

    await harness.flushAnimationFrames();
    await harness.flushAnimationFrames();
    expect(activeDensity.getAttribute("role")).toBe("button");
    expect(activeDensity.hasAttribute("aria-hidden")).toBe(false);
  });

  it("removes an obsolete density empty state before deferred replacement paint", async () => {
    const view = createSnapshotDistributionsView({
      onSelectBucket: vi.fn(),
    });
    const root = document.querySelector<HTMLElement>(
      "section#snapshot-distributions",
    )!;
    const withoutStructure = {
      ...transaction(1, "alpha"),
      structure: null,
    };
    const active = snapshot([withoutStructure]);
    const candidate = snapshot(
      [transaction(2, "beta")],
      active.observed_at_ms + 1_000,
    );
    await view.render(active, selection());
    await harness.flushAnimationFrames();
    await harness.flushAnimationFrames();
    await harness.flushAnimationFrames();
    expect(root.querySelector("#complexity-chart .empty-state")).not.toBeNull();
    const prepared = await view.prepare(candidate, selection());

    expect(view.commit(prepared, candidate, selection())).toBe(true);

    expect(root.querySelector("#complexity-chart .empty-state")).toBeNull();
    expect(
      root.querySelector("#complexity-canvas")?.getAttribute("aria-hidden"),
    ).toBe("true");
    await harness.flushAnimationFrames();
    await harness.flushAnimationFrames();
    await harness.flushAnimationFrames();
    expect(root.querySelector("#complexity-canvas")?.getAttribute("role")).toBe(
      "button",
    );
  });

  it("replaces empty and nonempty owners and cancels reset lifecycle work", async () => {
    const view = createSnapshotDistributionsView({
      onSelectBucket: vi.fn(),
    });
    const root = document.querySelector<HTMLElement>(
      "section#snapshot-distributions",
    )!;
    await view.render(snapshot([transaction(1, "alpha")]), selection());
    expect(harness.pendingAnimationFrames()).toBe(1);

    await view.render(snapshot([]), selection());

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

    await view.render(
      snapshot([transaction(2, "beta")]),
      selection("complete:1"),
    );
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
    expect(root.getAttribute("aria-busy")).toBe("false");
  });

  it("lets a replacement owner supersede pending work without stale commits", async () => {
    const view = createSnapshotDistributionsView({
      onSelectBucket: vi.fn(),
    });
    const root = document.querySelector<HTMLElement>(
      "section#snapshot-distributions",
    )!;
    const first = snapshot(
      Array.from({ length: 751 }, (_, index) =>
        transaction(index + 1, "alpha"),
      ),
    );
    const second = snapshot(
      [transaction(10_000, "beta")],
      first.observed_at_ms + 1_000,
    );

    const obsoleteRender = view.render(first, selection());
    expect(root.getAttribute("aria-busy")).toBe("true");
    const currentRender = view.render(second, selection("complete:1"));
    await currentRender;

    expect(root.getAttribute("aria-busy")).toBe("false");
    expect(root.querySelector("#value-note")?.textContent).toContain("1 of 1");
    expect(
      root.querySelector('#composition-bars button[data-segment="complete:1"]'),
    ).not.toBeNull();
    expect(harness.pendingAnimationFrames()).toBe(1);

    await obsoleteRender;

    expect(root.querySelector("#value-note")?.textContent).toContain("1 of 1");
    expect(root.getAttribute("aria-busy")).toBe("false");
    expect(harness.pendingAnimationFrames()).toBe(1);
  });

  it("keeps only the latest same-owner metric render", async () => {
    const view = createSnapshotDistributionsView({
      onSelectBucket: vi.fn(),
    });
    const root = document.querySelector<HTMLElement>(
      "section#snapshot-distributions",
    )!;
    const owner = snapshot(
      Array.from({ length: 751 }, (_, index) =>
        transaction(index + 1, index % 2 === 0 ? "alpha" : "beta"),
      ),
    );

    const obsoleteRender = view.render(owner, selection());
    const currentRender = view.render(owner, {
      ...selection("complete:1"),
      metric: "vsize",
    });
    expect(root.getAttribute("aria-busy")).toBe("true");

    await Promise.all([obsoleteRender, currentRender]);

    expect(root.querySelector("#spectrum-note")?.textContent).toContain(
      "virtual size",
    );
    expect(
      root
        .querySelector<HTMLButtonElement>(
          '#composition-bars button[data-segment="complete:1"]',
        )
        ?.getAttribute("aria-pressed"),
    ).toBe("true");
    expect(root.getAttribute("aria-busy")).toBe("false");
    expect(harness.pendingAnimationFrames()).toBe(1);
  });

  it("reset aborts pending derivation without a stale DOM or canvas commit", async () => {
    const view = createSnapshotDistributionsView({
      onSelectBucket: vi.fn(),
    });
    const root = document.querySelector<HTMLElement>(
      "section#snapshot-distributions",
    )!;
    const pendingRender = view.render(
      snapshot(
        Array.from({ length: 751 }, (_, index) =>
          transaction(index + 1, "alpha"),
        ),
      ),
      selection(),
    );

    view.reset("Loading a newer snapshot.");
    await pendingRender;

    expect(root.getAttribute("aria-busy")).toBe("false");
    expect(root.querySelector<HTMLElement>("#distribution-grid")?.hidden).toBe(
      true,
    );
    expect(root.querySelector("#distribution-empty")?.textContent).toBe(
      "Loading a newer snapshot.",
    );
    expect(root.querySelector("#spectrum-chart svg")).toBeNull();
    expect(harness.pendingAnimationFrames()).toBe(0);
    expect(harness.canvasContext.setTransform).not.toHaveBeenCalled();
  });

  it("rejects genuine derivation failures and clears aria-busy", async () => {
    const view = createSnapshotDistributionsView({
      onSelectBucket: vi.fn(),
    });
    const root = document.querySelector<HTMLElement>(
      "section#snapshot-distributions",
    )!;
    const failingSnapshot = snapshot([transaction(1, "alpha")]);
    Object.defineProperty(failingSnapshot, "transactions", {
      configurable: true,
      get: () => {
        throw new Error("fixture derivation failed");
      },
    });

    await expect(view.render(failingSnapshot, selection())).rejects.toThrow(
      "fixture derivation failed",
    );

    expect(root.getAttribute("aria-busy")).toBe("false");
    expect(harness.pendingAnimationFrames()).toBe(0);
  });
});
