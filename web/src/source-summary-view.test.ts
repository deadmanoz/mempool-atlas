// @vitest-environment happy-dom

import { beforeEach, describe, expect, it } from "vitest";
import {
  createNodeSourceSummaryView,
  createSourceCardView,
  setAtlasLoadPhase,
} from "./source-summary-view";
import type { MempoolSnapshot, SourceSummary } from "./types";

const source = (
  availability: SourceSummary["availability"] = "ready",
): SourceSummary => ({
  source_id: "core",
  source_label: "Bitcoin Core",
  availability,
  poll_interval_seconds: 60,
  last_poll_started_at_ms: 1_700_000_000_000,
  snapshot_observed_at_ms: 1_700_000_001_000,
  chain_tip: { height: 917_432, hash: "ab".repeat(32) },
  transaction_count: 2,
  total_vsize: 420,
  classification: {
    state: "classifying",
    revision: 3,
    classified_count: 1,
    unclassified_count: 1,
  },
  last_error: availability === "stale" ? "fixture failure" : null,
});

const snapshot = (): MempoolSnapshot => ({
  source_id: "core",
  source_label: "Bitcoin Core",
  collection_started_at_ms: 1_700_000_000_000,
  collection_completed_at_ms: 1_700_000_001_000,
  collection_duration_ms: 1_000,
  observed_at_ms: 1_700_000_001_000,
  classification_revision: 3,
  chain_tip: { height: 917_432, hash: "ab".repeat(32) },
  transaction_count: 2,
  total_vsize: 420,
  classifier_catalog: [],
  classification_summaries: [],
  bip110_summary: {
    evaluator_id: "rdts-rules",
    evaluator_version: "1.0.0",
    scope: "knots_mempool_policy",
    compatible_count: 1,
    violating_count: 0,
    indeterminate_count: 0,
    unclassified_count: 1,
  },
  transactions: [],
});

describe("source summary views", () => {
  beforeEach(() => {
    document.body.innerHTML = `
      <dl id="source-summary">
        <strong id="source-label"></strong><code id="source-id"></code>
        <dd id="observed-value"></dd><dd id="tip-value"></dd>
        <span id="transaction-count"></span><small id="total-vsize"></small>
        <small id="source-classification-summary"></small>
      </dl>
      <aside id="honesty-banner" hidden><p id="honesty-message"></p></aside>
      <article id="card"></article><div id="status"></div>`;
  });

  it("renders source metadata before a node snapshot body is available", () => {
    const view = createNodeSourceSummaryView();
    view.renderMetadata(source());

    expect(
      document.querySelector("#source-summary")?.getAttribute("aria-busy"),
    ).toBe("true");
    expect(
      document.querySelector("#source-summary")?.getAttribute("data-phase"),
    ).toBe("metadata-ready");
    expect(document.querySelector("#source-label")?.textContent).toBe(
      "Bitcoin Core",
    );
    expect(document.querySelector("#transaction-count")?.textContent).toBe("2");
    expect(
      document.querySelector("#source-classification-summary")?.textContent,
    ).toContain("1 of 2 assessed");
  });

  it("replaces metadata in place when the full snapshot validates", () => {
    const view = createNodeSourceSummaryView();
    view.renderMetadata(source());
    view.renderSnapshot(source(), snapshot());

    expect(
      document.querySelector("#source-summary")?.getAttribute("aria-busy"),
    ).toBe("false");
    expect(
      document.querySelector("#source-summary")?.getAttribute("data-phase"),
    ).toBe("interactive");
    expect(document.querySelector("#total-vsize")?.textContent).toBe("420 vB");
  });

  it("keeps stale metadata and its last error visible on comparison cards", () => {
    const card = document.querySelector<HTMLElement>("#card");
    if (card === null) throw new Error("missing fixture card");
    const view = createSourceCardView(card, "Source A");
    view.renderMetadata(source("stale"));

    expect(card.dataset.state).toBe("stale");
    expect(card.dataset.phase).toBe("metadata-ready");
    expect(card.textContent).toContain("fixture failure");
    expect(card.textContent).toContain("1/2 assessed");
  });

  it("keeps evaluator identity out of user-facing source details", () => {
    const card = document.querySelector<HTMLElement>("#card");
    if (card === null) throw new Error("missing fixture card");
    createSourceCardView(card, "Source A").renderSnapshot(source(), snapshot());

    expect(card.querySelector(".source-card-classification")?.textContent).toBe(
      "Classifying · 1/2 assessed. 1 of 2 assessed; 1 transaction has an assessment pending while classification continues. Refresh to read newer progress.",
    );
    expect(card.querySelector(".source-card-policy")).toBeNull();
    expect(card.textContent).not.toContain("rdts-rules");
  });

  it("preserves comparison card nodes and focus when the snapshot completes", () => {
    const card = document.querySelector<HTMLElement>("#card");
    if (card === null) throw new Error("missing fixture card");
    const view = createSourceCardView(card, "Source A");
    view.renderMetadata(source());
    const identifier = card.querySelector(":scope > code");
    const link = card.querySelector<HTMLAnchorElement>(".source-card-link");
    if (identifier === null || link === null) {
      throw new Error("missing persistent source card nodes");
    }

    link.focus();
    view.renderSnapshot(source(), snapshot());

    expect(card.querySelector(":scope > code")).toBe(identifier);
    expect(card.querySelector(".source-card-link")).toBe(link);
    expect(document.activeElement).toBe(link);
    expect(link.textContent).toBe("Explore this node");
    expect(link.href).toContain("?source=core");
    expect(card.dataset.phase).toBe("interactive");
  });

  it("shows stale observation honesty as soon as node metadata arrives", () => {
    createNodeSourceSummaryView().renderMetadata(source("stale"));

    const banner = document.querySelector<HTMLElement>("#honesty-banner");
    expect(banner?.hidden).toBe(false);
    expect(banner?.dataset.tone).toBe("stale");
    expect(document.querySelector("#honesty-message")?.textContent).toContain(
      "fixture failure",
    );
  });

  it("does not imply transaction availability before a first snapshot", () => {
    const card = document.querySelector<HTMLElement>("#card");
    if (card === null) throw new Error("missing fixture card");
    const unavailable = source("error");
    unavailable.snapshot_observed_at_ms = null;
    unavailable.chain_tip = null;
    unavailable.transaction_count = null;
    unavailable.total_vsize = null;
    unavailable.classification = null;
    unavailable.last_error = "RPC unavailable";

    createSourceCardView(card, "Source A").renderMetadata(unavailable);

    expect(card.dataset.state).toBe("error");
    expect(card.getAttribute("aria-busy")).toBe("false");
    expect(card.textContent).toContain("No complete snapshot");
    expect(card.textContent).toContain("Classification begins after");
    expect(card.textContent).not.toContain("0 tx");
  });

  it("records page lifecycle phases without changing availability state", () => {
    const status = document.querySelector<HTMLElement>("#status");
    if (status === null) throw new Error("missing fixture status");
    status.dataset.state = "waiting";
    setAtlasLoadPhase(status, "loading-snapshot");
    expect(status.dataset.phase).toBe("loading-snapshot");
    expect(status.dataset.state).toBe("waiting");
  });

  it("announces refresh and terminal request states without rebuilding views", () => {
    const node = createNodeSourceSummaryView();
    const card = document.querySelector<HTMLElement>("#card");
    if (card === null) throw new Error("missing fixture card");
    const comparison = createSourceCardView(card, "Source A");

    node.renderSnapshot(source(), snapshot());
    comparison.renderSnapshot(source(), snapshot());
    node.setBusy(true);
    comparison.setBusy(true);
    expect(
      document.querySelector("#source-summary")?.getAttribute("aria-busy"),
    ).toBe("true");
    expect(card.getAttribute("aria-busy")).toBe("true");

    node.setBusy(false);
    comparison.setBusy(false);
    expect(
      document.querySelector("#source-summary")?.getAttribute("aria-busy"),
    ).toBe("false");
    expect(card.getAttribute("aria-busy")).toBe("false");
  });

  it("stops announcing source discovery as busy after discovery fails", () => {
    const view = createNodeSourceSummaryView();
    view.renderDiscovering();
    view.renderDiscoveryFailure();

    const root = document.querySelector<HTMLElement>("#source-summary");
    expect(root?.dataset.phase).toBe("discovering-sources");
    expect(root?.dataset.state).toBe("error");
    expect(root?.getAttribute("aria-busy")).toBe("false");
  });
});
