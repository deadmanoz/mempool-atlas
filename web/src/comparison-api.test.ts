import { describe, expect, it } from "vitest";
import { comparisonQuery, parseSourceComparison } from "./comparison-api";

const taxonomy = (): Record<string, unknown> => ({
  key: "behavior",
  label: "Behavior",
  verdicts: [
    { key: "payment", label: "Payment" },
    { key: "unknown", label: "Unknown" },
  ],
});

const bins = (): Record<string, unknown> => ({
  taxonomies: [taxonomy()],
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

const unavailable = (): Record<string, unknown> => ({
  status: "unavailable",
  reason: "requires_raw_transaction",
});

const histograms = (): Record<string, unknown> => ({
  taxonomies: [
    {
      key: "behavior",
      status: "available",
      bins: [
        { count: 1, vsize: 100 },
        { count: 2, vsize: 200 },
      ],
      underived: { count: 0, vsize: 0 },
    },
  ],
  script: unavailable(),
  value: unavailable(),
  inputs: unavailable(),
  outputs: unavailable(),
  age: unavailable(),
  feerate: unavailable(),
});

const region = (present: {
  count: number;
  vsize: number;
}): Record<string, unknown> => ({
  present,
  awaiting_rpc: { count: 1 },
  bins: bins(),
  histograms: histograms(),
});

const anomaly = (
  present: { count: number; vsize: number },
  awaiting = 0,
): Record<string, unknown> => ({
  present,
  awaiting_rpc: { count: awaiting },
});

const validComparison = (): Record<string, unknown> => ({
  sources: ["knots", "core", "libre-relay"],
  as_of_ms: 1_752_710_400_000,
  source_totals: [
    {
      source_id: "knots",
      present: { count: 10, vsize: 1_000 },
      awaiting_rpc: { count: 1 },
    },
    {
      source_id: "core",
      present: { count: 12, vsize: 1_200 },
      awaiting_rpc: { count: 0 },
    },
    {
      source_id: "libre-relay",
      present: { count: 15, vsize: 1_500 },
      awaiting_rpc: { count: 2 },
    },
  ],
  shared: region({ count: 8, vsize: 800 }),
  stages: [
    {
      from: "knots",
      to: "core",
      added: region({ count: 2, vsize: 200 }),
      anomaly: anomaly({ count: 0, vsize: 0 }),
    },
    {
      from: "core",
      to: "libre-relay",
      added: region({ count: 3, vsize: 300 }),
      anomaly: anomaly({ count: 1, vsize: 150 }),
    },
  ],
});

describe("parseSourceComparison", () => {
  it("accepts a full three-source comparison payload", () => {
    const comparison = parseSourceComparison(validComparison());
    expect(comparison.sources).toEqual(["knots", "core", "libre-relay"]);
    expect(comparison.stages).toHaveLength(2);
    expect(comparison.stages[0]?.from).toBe("knots");
    expect(comparison.stages[0]?.to).toBe("core");
    expect(comparison.stages[1]?.from).toBe("core");
    expect(comparison.shared.present).toEqual({ count: 8, vsize: 800 });
    expect(comparison.shared.awaiting_rpc.count).toBe(1);
    // The region reuses the summary bins/histograms shapes and parses them.
    expect(comparison.shared.bins.taxonomies).toHaveLength(1);
    expect(comparison.shared.bins.taxonomies[0]?.key).toBe("behavior");
    const behavior = comparison.shared.histograms.taxonomies[0];
    expect(behavior?.status).toBe("available");
    expect(comparison.stages[1]?.anomaly.present).toEqual({
      count: 1,
      vsize: 150,
    });
    expect(comparison.source_totals).toHaveLength(3);
  });

  it("accepts a two-source comparison with a single stage", () => {
    const payload = validComparison();
    payload.sources = ["knots", "core"];
    payload.source_totals = (payload.source_totals as unknown[]).slice(0, 2);
    payload.stages = (payload.stages as unknown[]).slice(0, 1);
    const comparison = parseSourceComparison(payload);
    expect(comparison.sources).toHaveLength(2);
    expect(comparison.stages).toHaveLength(1);
  });

  it.each([
    [
      "fewer than two sources",
      (payload: Record<string, unknown>): void => {
        payload.sources = ["knots"];
        payload.source_totals = (payload.source_totals as unknown[]).slice(
          0,
          1,
        );
        payload.stages = [];
      },
    ],
    [
      "more than four sources",
      (payload: Record<string, unknown>): void => {
        payload.sources = ["a", "b", "c", "d", "e"];
      },
    ],
    [
      "non-distinct sources",
      (payload: Record<string, unknown>): void => {
        payload.sources = ["knots", "knots", "core"];
      },
    ],
    [
      "a stage count that is not one per adjacent pair",
      (payload: Record<string, unknown>): void => {
        payload.stages = (payload.stages as unknown[]).slice(0, 1);
      },
    ],
    [
      "a stage whose from/to does not follow the order",
      (payload: Record<string, unknown>): void => {
        (payload.stages as Record<string, unknown>[])[0]!.from = "core";
      },
    ],
    [
      "a region histogram misaligned with its bin catalog",
      (payload: Record<string, unknown>): void => {
        const shared = payload.shared as Record<string, unknown>;
        const hist = shared.histograms as Record<string, unknown>;
        const taxonomies = hist.taxonomies as Record<string, unknown>[];
        // Three bins for a two-verdict taxonomy.
        taxonomies[0] = {
          ...taxonomies[0],
          bins: [
            { count: 1, vsize: 100 },
            { count: 2, vsize: 200 },
            { count: 3, vsize: 300 },
          ],
        };
      },
    ],
    [
      "a region shape histogram misaligned with its bin catalog",
      (payload: Record<string, unknown>): void => {
        const shared = payload.shared as Record<string, unknown>;
        const hist = shared.histograms as Record<string, unknown>;
        hist.age = {
          status: "available",
          bins: [{ count: 1, vsize: 100 }],
          underived: { count: 0, vsize: 0 },
        };
      },
    ],
    [
      "a malformed region missing its present aggregate",
      (payload: Record<string, unknown>): void => {
        delete (payload.shared as Record<string, unknown>).present;
      },
    ],
    [
      "a stage anomaly missing its awaiting count",
      (payload: Record<string, unknown>): void => {
        delete (payload.stages as Record<string, unknown>[])[0]!.anomaly;
      },
    ],
    [
      "source totals that do not cover every source",
      (payload: Record<string, unknown>): void => {
        payload.source_totals = (payload.source_totals as unknown[]).slice(
          0,
          2,
        );
      },
    ],
    [
      "a source total for an unrequested source",
      (payload: Record<string, unknown>): void => {
        (payload.source_totals as Record<string, unknown>[])[2]!.source_id =
          "stranger";
      },
    ],
    [
      "source totals outside the requested order",
      (payload: Record<string, unknown>): void => {
        const totals = payload.source_totals as Record<string, unknown>[];
        [totals[0], totals[1]] = [totals[1]!, totals[0]!];
      },
    ],
  ])("rejects %s", (_name, mutate) => {
    const payload = validComparison();
    mutate(payload);
    expect(() => parseSourceComparison(payload)).toThrow(TypeError);
  });
});

describe("comparisonQuery", () => {
  it("encodes the ordered sources as a comma list", () => {
    const parameters = new URLSearchParams(
      comparisonQuery(["knots", "core", "libre-relay"]),
    );
    expect(parameters.get("sources")).toBe("knots,core,libre-relay");
  });
});
