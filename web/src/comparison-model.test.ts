import { describe, expect, it } from "vitest";
import {
  anomalyView,
  regionMagnitude,
  rejectionAvailabilityMessage,
  rejectionReasonLabel,
  rejectionVerdictSegments,
  sharedView,
  stageView,
} from "./comparison-model";
import type {
  BinCatalog,
  ComparisonStage,
  RegionAggregate,
  RejectionTaxonomyBreakdown,
  SummaryHistograms,
  TaxonomyDescriptor,
} from "./types";

const behaviorTaxonomy = (): TaxonomyDescriptor => ({
  key: "behavior",
  label: "Behavior",
  verdicts: [
    { key: "payment", label: "Payment" },
    { key: "unknown", label: "Unknown" },
  ],
});

const bins = (): BinCatalog => ({
  taxonomies: [behaviorTaxonomy()],
  feerate_sat_per_vb_edges: [1, 2, 4, 8, 16, 32, 64, 128],
  age_ms_edges: [600_000, 3_600_000, 21_600_000, 86_400_000, 259_200_000],
  value_sats_edges: [
    100_000, 1_000_000, 10_000_000, 100_000_000, 1_000_000_000,
  ],
  input_count_uppers: [1, 5, 20, 100],
  output_count_uppers: [1, 2, 10, 50],
  script_keys: [
    "p2tr",
    "p2wpkh",
    "p2wsh",
    "p2sh",
    "p2pkh",
    "op_return",
    "other",
  ],
});

const histograms = (): SummaryHistograms => ({
  taxonomies: [
    {
      key: "behavior",
      status: "available",
      bins: [
        { count: 6, vsize: 600 },
        { count: 2, vsize: 200 },
      ],
      underived: { count: 0, vsize: 0 },
    },
  ],
  script: { status: "unavailable", reason: "requires_raw_transaction" },
  value: { status: "unavailable", reason: "requires_raw_transaction" },
  inputs: { status: "unavailable", reason: "requires_raw_transaction" },
  outputs: { status: "unavailable", reason: "requires_raw_transaction" },
  age: { status: "unavailable", reason: "requires_raw_transaction" },
  feerate: { status: "unavailable", reason: "requires_raw_transaction" },
});

const region = (
  present: { count: number; vsize: number },
  awaiting = 0,
): RegionAggregate => ({
  present,
  awaiting_rpc: { count: awaiting },
  bins: bins(),
  histograms: histograms(),
});

describe("regionMagnitude", () => {
  it("splits present and awaiting membership", () => {
    const magnitude = regionMagnitude(region({ count: 8, vsize: 800 }, 3));
    expect(magnitude).toEqual({
      totalCount: 11,
      presentCount: 8,
      vsize: 800,
      awaitingCount: 3,
    });
  });
});

describe("sharedView", () => {
  it("summarizes the shared region with a headline and composition rows", () => {
    const view = sharedView(region({ count: 8, vsize: 800 }), 3, "vsize");
    expect(view.headline).toBe("8 tx (800 vB) shared by all 3 sources");
    // One taxonomy row plus the six fixed shape dimensions.
    expect(view.rows.map((row) => row.key)).toEqual([
      "behavior",
      "script",
      "value",
      "inputs",
      "outputs",
      "age",
      "feerate",
    ]);
  });
});

describe("stageView", () => {
  const stage = (anomalyRegion: {
    present: { count: number; vsize: number };
    awaiting: number;
  }): ComparisonStage => ({
    from: "knots",
    to: "core",
    added: region({ count: 2, vsize: 200 }, 1),
    anomaly: {
      present: anomalyRegion.present,
      awaiting_rpc: { count: anomalyRegion.awaiting },
    },
  });

  it("describes the added region only as a membership difference", () => {
    const view = stageView(
      stage({ present: { count: 0, vsize: 0 }, awaiting: 0 }),
      "vsize",
    );
    expect(view.headline).toBe(
      "2 tx (200 vB) are present in core but not knots",
    );
    expect(view.magnitude.awaitingCount).toBe(1);
    expect(view.rows[0]?.key).toBe("behavior");
  });

  it("flags a reverse-difference anomaly when the ordering does not hold", () => {
    const view = stageView(
      stage({ present: { count: 1, vsize: 150 }, awaiting: 2 }),
      "vsize",
    );
    expect(view.anomaly.present).toBe(true);
    expect(view.anomaly.totalCount).toBe(3);
    expect(view.anomaly.note).toBe(
      "3 tx present in knots but not core (unexpected under this ordering)",
    );
  });

  it("confirms the ordering holds when there is no anomaly", () => {
    const view = stageView(
      stage({ present: { count: 0, vsize: 0 }, awaiting: 0 }),
      "vsize",
    );
    expect(view.anomaly.present).toBe(false);
    expect(view.anomaly.note).toBe(
      "No transactions are in knots but absent from core; the expected nesting holds for this pair.",
    );
  });
});

describe("anomalyView", () => {
  it("reads structurally from a stage's anomaly region", () => {
    const view = anomalyView({
      from: "core",
      to: "libre-relay",
      anomaly: {
        present: { count: 4, vsize: 900 },
        awaiting_rpc: { count: 0 },
      },
    });
    expect(view.present).toBe(true);
    expect(view.presentCount).toBe(4);
    expect(view.vsize).toBe(900);
  });
});

describe("rejectionVerdictSegments", () => {
  const breakdown = (): RejectionTaxonomyBreakdown => ({
    key: "behavior",
    label: "Behavior",
    verdicts: [
      { verdict: "payment", count: 3 },
      { verdict: "unknown", count: 0 },
    ],
  });

  it("uses catalog labels and colours and drops zero-count verdicts", () => {
    const segments = rejectionVerdictSegments(breakdown(), [
      behaviorTaxonomy(),
    ]);
    expect(segments).toHaveLength(1);
    expect(segments[0]).toEqual({
      key: "payment",
      label: "Payment",
      // The named behavior colour for the "payment" verdict.
      color: "#56c7d9",
      count: 3,
    });
  });

  it("falls back to the verdict key and a neutral colour without a catalog match", () => {
    const segments = rejectionVerdictSegments(breakdown(), []);
    expect(segments[0]).toEqual({
      key: "payment",
      label: "payment",
      color: "#6b7a8d",
      count: 3,
    });
  });
});

describe("rejectionReasonLabel", () => {
  it("labels only the synthetic rollup as Other reasons", () => {
    expect(rejectionReasonLabel({ reason: "other", is_rollup: false })).toBe(
      "other",
    );
    expect(rejectionReasonLabel({ reason: "other", is_rollup: true })).toBe(
      "Other reasons",
    );
  });
});

describe("rejectionAvailabilityMessage", () => {
  it("distinguishes unavailable evidence from an observed empty window", () => {
    expect(rejectionAvailabilityMessage({ status: "not_collected" })).toBe(
      "Rejection evidence is not collected in state-only mode.",
    );
    expect(rejectionAvailabilityMessage({ status: "available" })).toBeNull();
  });
});
