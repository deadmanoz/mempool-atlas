// @vitest-environment happy-dom

import { beforeEach, describe, expect, it } from "vitest";

import { compareCurrentSnapshots } from "./comparison-model";
import { createComparisonSamplingView } from "./comparison-sampling-view";
import { loadedSource } from "./comparison-test-fixtures";

const element = (id: string): HTMLElement => {
  const value = document.querySelector<HTMLElement>(`#${id}`);
  if (value === null) throw new Error(`missing #${id}`);
  return value;
};

const view = () =>
  createComparisonSamplingView({
    panel: element("sampling-panel"),
    summary: element("sampling-summary"),
    chainSummary: element("chain-summary"),
    note: element("sampling-note"),
  });

describe("comparison sampling view", () => {
  beforeEach(() => {
    document.body.innerHTML = `
      <section id="sampling-panel" hidden>
        <strong id="sampling-summary"></strong>
        <span id="chain-summary"></span>
        <p id="sampling-note"></p>
      </section>`;
  });

  it("states which source was observed later and by how much", () => {
    const left = loadedSource("core", [], 1_700_000_001_000);
    const right = loadedSource("knots", [], 1_700_000_004_000);

    view().render(compareCurrentSnapshots(left, right));

    expect(element("sampling-summary").textContent).toBe(
      "Source B was observed 3 s after Source A.",
    );
    expect(element("sampling-note").textContent).toContain(
      "collection windows were separated by 2 s",
    );
  });

  it("preserves the direction when source A is newer", () => {
    const left = loadedSource("core", [], 1_700_000_004_000);
    const right = loadedSource("knots", [], 1_700_000_001_000);

    view().render(compareCurrentSnapshots(left, right));

    expect(element("sampling-summary").textContent).toBe(
      "Source A was observed 3 s after Source B.",
    );
  });

  it("describes simultaneous observations and overlapping collection", () => {
    const left = loadedSource("core", [], 1_700_000_001_000);
    const right = loadedSource("knots", [], 1_700_000_001_000);

    const samplingView = view();
    samplingView.render(compareCurrentSnapshots(left, right));

    expect(element("sampling-summary").textContent).toBe(
      "Both snapshots were observed at the same time.",
    );
    expect(element("sampling-note").textContent).toContain(
      "collection windows overlapped by 1 s",
    );
    expect(element("chain-summary").textContent).toBe(
      "Same chain tip · 900,000 · 0000000000…",
    );
    expect(element("sampling-panel").hidden).toBe(false);

    samplingView.reset();
    expect(element("sampling-panel").hidden).toBe(true);
  });

  it("warns when the snapshots report different chain tips", () => {
    const left = loadedSource("core", [], 1_700_000_001_000);
    const right = loadedSource("knots", [], 1_700_000_001_000);
    right.snapshot.chain_tip = {
      height: 899_999,
      hash: "11".repeat(32),
    };

    view().render(compareCurrentSnapshots(left, right));

    expect(element("chain-summary").dataset.state).toBe("different");
    expect(element("chain-summary").textContent).toBe(
      "Different chain tips · A 900,000 · 0000000000… / B 899,999 · 1111111111…",
    );
    expect(element("chain-summary").title).toContain(
      `Source B: height 899,999, block ${"11".repeat(32)}`,
    );
  });

  it("distinguishes different hashes reported at the same height", () => {
    const left = loadedSource("core", [], 1_700_000_001_000);
    const right = loadedSource("knots", [], 1_700_000_001_000);
    right.snapshot.chain_tip = {
      height: 900_000,
      hash: "11".repeat(32),
    };

    view().render(compareCurrentSnapshots(left, right));

    expect(element("chain-summary").textContent).toBe(
      "Different chain tips at height 900,000 · A 0000000000… / B 1111111111…",
    );
  });
});
