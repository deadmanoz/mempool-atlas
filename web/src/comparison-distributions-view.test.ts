// @vitest-environment happy-dom

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import {
  createComparisonDistributionsView,
  comparisonDistributionScopeSuffix,
  comparisonDistributionTransactions,
  type ComparisonDistributionScope,
} from "./comparison-distributions-view";
import { compareCurrentSnapshots } from "./comparison-model";
import { loadedSource } from "./comparison-test-fixtures";
import {
  installDistributionViewTestHarness,
  type DistributionViewTestHarness,
} from "./distribution-view-test-harness";
import { mempoolTransaction, txid } from "./test-fixtures";
import type { MempoolTransaction } from "./types";

const transaction = (
  value: number,
  wtxidValue: number = value,
): MempoolTransaction =>
  mempoolTransaction(value, {
    wtxid: txid(wtxidValue),
    vsize: 100 + value,
    weight: (100 + value) * 4,
    fee_sats: 200 + value,
    entered_at_ms: 1_700_000_000_000 + value,
    ancestor_vsize: 100 + value,
    ancestor_fee_sats: 200 + value,
    descendant_vsize: 100 + value,
  });

const comparison = compareCurrentSnapshots(
  loadedSource("core", [transaction(1, 101), transaction(2), transaction(3)]),
  loadedSource("knots", [transaction(1, 201), transaction(2), transaction(4)]),
);

const selectedTxids = (
  side: "left" | "right",
  scope: ComparisonDistributionScope,
): string[] =>
  comparisonDistributionTransactions(comparison, side, scope).map(
    ({ txid: transactionId }) => transactionId,
  );

describe("comparisonDistributionTransactions", () => {
  it.each([
    ["left", "all", [txid(1), txid(2), txid(3)]],
    ["right", "all", [txid(1), txid(2), txid(4)]],
    ["left", "left_only", [txid(3)]],
    ["right", "left_only", []],
    ["left", "right_only", []],
    ["right", "right_only", [txid(4)]],
  ] as const)(
    "selects %s members for the %s scope",
    (side, scope, expected) => {
      expect(selectedTxids(side, scope)).toEqual(expected);
    },
  );

  it("preserves both source-local variants for the common scope", () => {
    const left = comparisonDistributionTransactions(
      comparison,
      "left",
      "common",
    );
    const right = comparisonDistributionTransactions(
      comparison,
      "right",
      "common",
    );

    expect(left.map(({ txid: transactionId }) => transactionId)).toEqual([
      txid(1),
      txid(2),
    ]);
    expect(right.map(({ txid: transactionId }) => transactionId)).toEqual([
      txid(1),
      txid(2),
    ]);
    expect(left[0]?.wtxid).toBe(txid(101));
    expect(right[0]?.wtxid).toBe(txid(201));
  });
});

describe("comparisonDistributionScopeSuffix", () => {
  it.each([
    ["all", ""],
    ["common", " · present in both"],
    ["left_only", " · only in Source A"],
    ["right_only", " · only in Source B"],
  ] as const)("maps %s to its source-local title suffix", (scope, suffix) => {
    expect(comparisonDistributionScopeSuffix(scope)).toBe(suffix);
  });
});

const comparisonDistributionMarkup = (): string => {
  const simplePanels = [
    ["comp", "composition"],
    ["spectrum", "spectrum"],
    ["package", "package"],
    ["mosaic", "mosaic"],
    ["data", "data"],
    ["entanglement", "entanglement"],
    ["value", "value"],
  ] as const;
  const panels = simplePanels
    .flatMap(([title, container]) =>
      (["left", "right"] as const).map(
        (side) =>
          `<h3 id="dist-${title}-${side}-title"></h3><div id="${container}-${side}"></div>`,
      ),
    )
    .join("");
  const canvasPanels = (["joint", "complexity"] as const)
    .flatMap((panel) =>
      (["left", "right"] as const).map(
        (side) =>
          `<h3 id="dist-${panel}-${side}-title"></h3><div id="${panel}-${side}-y-axis"></div><div id="${panel}-${side}"><canvas id="${panel}-${side}-canvas"></canvas></div>`,
      ),
    )
    .join("");
  return `
    <section id="comparison-distributions" hidden>
      <div class="comparison-distributions-heading"><div><h2>Snapshot distributions</h2></div></div>
      <button id="dist-scope-all" aria-pressed="true"></button>
      <button id="dist-scope-common" aria-pressed="false"></button>
      <button id="dist-scope-left" aria-pressed="false"></button>
      <button id="dist-scope-right" aria-pressed="false"></button>
      ${panels}
      ${canvasPanels}
    </section>
  `;
};

describe("createComparisonDistributionsView", () => {
  let harness: DistributionViewTestHarness;

  beforeEach(() => {
    harness = installDistributionViewTestHarness();
    document.body.innerHTML = `
      <h3 id="dist-comp-left-title">outside sentinel</h3>
      ${comparisonDistributionMarkup()}
    `;
  });

  afterEach(() => {
    harness.cleanup();
  });

  it("owns descendant lookup, axes, rendering, and density scheduling", async () => {
    const root = document.querySelector<HTMLElement>(
      "section#comparison-distributions",
    );
    expect(root).not.toBeNull();
    const view = createComparisonDistributionsView(root!);

    expect(harness.resizeObservers).toHaveLength(1);
    expect(harness.resizeObservers[0]?.observed).toHaveLength(4);
    expect(root!.querySelectorAll("#joint-left .panel-axis")).toHaveLength(1);
    expect(root!.querySelectorAll("#complexity-left .panel-axis")).toHaveLength(
      1,
    );

    await view.render(comparison);

    expect(root!.hidden).toBe(false);
    expect(root!.querySelector("#dist-comp-left-title")?.textContent).toContain(
      "CORE node",
    );
    expect(document.body.firstElementChild?.textContent).toBe(
      "outside sentinel",
    );
    expect(root!.querySelector("#spectrum-left svg")).not.toBeNull();
    expect(
      root!.querySelectorAll(
        "#composition-left .composition-segment[tabindex]",
      ),
    ).toHaveLength(0);
    const passiveTrack = root!.querySelector<HTMLElement>(
      "#entanglement-left .composition-track[tabindex='0']",
    );
    expect(passiveTrack).not.toBeNull();
    expect(
      root!.querySelectorAll("#mosaic-left .mosaic-column[tabindex]"),
    ).toHaveLength(0);
    expect(
      root!
        .querySelector("#mosaic-left .mosaic-board")
        ?.getAttribute("tabindex"),
    ).toBe("0");
    expect(harness.pendingAnimationFrames()).toBe(1);
    harness.resizeObservers[0]?.trigger();
    expect(harness.pendingAnimationFrames()).toBe(1);
    harness.flushAnimationFrames();
    harness.flushAnimationFrames();
    expect(harness.canvasContext.setTransform).toHaveBeenCalled();
    expect(
      root!.querySelector("#joint-left-canvas")?.getAttribute("aria-pressed"),
    ).toBe("false");
  });

  it("rerenders source-local empty state when a one-sided scope is selected", async () => {
    const root = document.querySelector<HTMLElement>(
      "section#comparison-distributions",
    )!;
    const view = createComparisonDistributionsView(root);
    await view.render(comparison);

    root.querySelector<HTMLButtonElement>("#dist-scope-left")?.click();

    await vi.waitFor(() => {
      expect(
        root.querySelector("#spectrum-right .empty-state")?.textContent,
      ).toBe("This population has no members on this source.");
    });

    expect(
      root.querySelector("#dist-scope-left")?.getAttribute("aria-pressed"),
    ).toBe("true");
    expect(
      root.querySelector("#dist-scope-all")?.getAttribute("aria-pressed"),
    ).toBe("false");
    expect(
      root.querySelector("#dist-spectrum-right-title")?.textContent,
    ).toContain("only in Source A");
    expect(root.querySelector("#spectrum-left svg")).not.toBeNull();
    expect(
      root.querySelector("#spectrum-right .empty-state")?.textContent,
    ).toBe("This population has no members on this source.");
  });

  it("prepares a replacement off-view and synchronously commits only its exact identity and scope", async () => {
    const root = document.querySelector<HTMLElement>(
      "section#comparison-distributions",
    )!;
    const view = createComparisonDistributionsView(root);
    await view.render(comparison);
    for (let frame = 0; frame < 6; frame += 1) {
      harness.flushAnimationFrames();
    }
    const activeSpectrum = root.querySelector("#spectrum-left svg");
    const activeDensity =
      root.querySelector<HTMLCanvasElement>("#joint-left-canvas")!;
    expect(activeDensity.getAttribute("role")).toBe("button");
    const candidate = compareCurrentSnapshots(
      loadedSource("candidate-a", [transaction(50)]),
      loadedSource("candidate-b", [transaction(50)]),
    );

    const prepared = await view.prepare(candidate);

    expect(prepared.prefetchedModels.common).toBeDefined();
    expect(prepared.prefetchedModels.common?.left.totals.population.count).toBe(
      1,
    );
    expect(
      prepared.prefetchedModels.common?.right.totals.population.count,
    ).toBe(1);
    expect(root.getAttribute("aria-busy")).toBe("false");
    expect(root.querySelector("#spectrum-left svg")).toBe(activeSpectrum);
    expect(activeDensity.getAttribute("role")).toBe("button");
    expect(root.querySelector("#dist-comp-left-title")?.textContent).toContain(
      "CORE node",
    );
    expect(view.canCommit(prepared, candidate)).toBe(true);
    expect(view.canCommit(prepared, { ...candidate })).toBe(false);
    expect(view.commit(prepared, { ...candidate })).toBe(false);

    root.querySelector<HTMLButtonElement>("#dist-scope-left")?.click();
    expect(view.canCommit(prepared, candidate)).toBe(false);
    expect(view.commit(prepared, candidate)).toBe(false);
    await vi.waitFor(() => {
      expect(root.getAttribute("aria-busy")).toBe("false");
    });
    root.querySelector<HTMLButtonElement>("#dist-scope-all")?.click();
    await vi.waitFor(() => {
      expect(root.getAttribute("aria-busy")).toBe("false");
    });

    expect(view.commit(prepared, candidate)).toBe(true);
    expect(activeDensity.getAttribute("role")).toBeNull();
    expect(activeDensity.getAttribute("aria-hidden")).toBe("true");
    expect(activeDensity.dataset.distributionInspectionKey).toBeUndefined();
    expect(root.querySelector("#dist-comp-left-title")?.textContent).toContain(
      "CANDIDATE-A node",
    );
    expect(root.querySelector("#spectrum-left svg")).not.toBe(activeSpectrum);
    harness.flushAnimationFrames();
    harness.flushAnimationFrames();
    expect(activeDensity.getAttribute("role")).toBe("button");
    expect(activeDensity.hasAttribute("aria-hidden")).toBe(false);

    root.querySelector<HTMLButtonElement>("#dist-scope-common")?.click();
    await vi.waitFor(() => {
      expect(root.getAttribute("aria-busy")).toBe("false");
      expect(
        root.querySelector("#dist-comp-left-title")?.textContent,
      ).toContain("present in both");
    });
  });

  it("replaces a nonempty owner with empty aggregates and resets its lifecycle", async () => {
    const root = document.querySelector<HTMLElement>(
      "section#comparison-distributions",
    )!;
    const view = createComparisonDistributionsView(root);
    await view.render(comparison);
    const emptyComparison = compareCurrentSnapshots(
      loadedSource("core", []),
      loadedSource("knots", []),
    );

    await view.render(emptyComparison);

    expect(root.querySelector("#spectrum-left svg")).toBeNull();
    expect(root.querySelector("#spectrum-left .empty-state")).not.toBeNull();
    expect(harness.pendingAnimationFrames()).toBe(1);

    view.reset();

    expect(root.hidden).toBe(true);
    expect(root.querySelector("#spectrum-left")?.childElementCount).toBe(0);
    expect(
      root.querySelector("#dist-scope-all")?.getAttribute("aria-pressed"),
    ).toBe("true");
    expect(harness.pendingAnimationFrames()).toBe(0);
    expect(harness.cancelledAnimationFrames).toHaveLength(2);
  });

  it("keeps only the latest scope when cooperative derivations overlap", async () => {
    const root = document.querySelector<HTMLElement>(
      "section#comparison-distributions",
    )!;
    const view = createComparisonDistributionsView(root);
    const transactions = Array.from({ length: 900 }, (_, index) =>
      transaction(index + 10_000),
    );
    const largeComparison = compareCurrentSnapshots(
      loadedSource("core", transactions),
      loadedSource("knots", transactions),
    );

    const obsolete = view.render(largeComparison);
    root.querySelector<HTMLButtonElement>("#dist-scope-left")?.click();
    await obsolete;

    await vi.waitFor(() => {
      expect(
        root.querySelector("#dist-spectrum-right-title")?.textContent,
      ).toContain("only in Source A");
      expect(
        root.querySelector("#spectrum-right .empty-state")?.textContent,
      ).toBe("This population has no members on this source.");
    });
    expect(root.querySelector("#spectrum-right svg")).toBeNull();
  });

  it("aborts obsolete owners and reset work without stale DOM or density frames", async () => {
    const root = document.querySelector<HTMLElement>(
      "section#comparison-distributions",
    )!;
    const view = createComparisonDistributionsView(root);
    const transactions = Array.from({ length: 900 }, (_, index) =>
      transaction(index + 20_000),
    );
    const largeComparison = compareCurrentSnapshots(
      loadedSource("core", transactions),
      loadedSource("knots", transactions),
    );
    const emptyComparison = compareCurrentSnapshots(
      loadedSource("core", []),
      loadedSource("knots", []),
    );

    const obsoleteOwner = view.render(largeComparison);
    const currentOwner = view.render(emptyComparison);
    await Promise.all([obsoleteOwner, currentOwner]);

    expect(root.querySelector("#spectrum-left svg")).toBeNull();
    expect(root.querySelector("#spectrum-left .empty-state")).not.toBeNull();
    expect(harness.pendingAnimationFrames()).toBe(1);

    harness.flushAnimationFrames();
    const obsoleteReset = view.render(largeComparison);
    view.reset();
    await obsoleteReset;

    expect(root.hidden).toBe(true);
    expect(root.getAttribute("aria-busy")).toBe("false");
    expect(root.querySelector("#spectrum-left")?.childElementCount).toBe(0);
    expect(harness.pendingAnimationFrames()).toBe(0);
  });
});
