import { describe, expect, it } from "vitest";
import {
  parseMempoolSummary,
  parseSourcesResponse,
  summaryQuery,
} from "./summary-api";

const validSummary = (): Record<string, unknown> => ({
  source_id: "source-a",
  as_of_ms: 1_752_710_400_000,
  filter_echo: { classes: ["unknown"], feerate_min: 1 },
  totals: {
    all: { count: 2, vsize: 400 },
    matching: { count: 1, vsize: 200 },
    awaiting_rpc: { count: 3 },
  },
  bins: {
    feerate_sat_per_vb_edges: [1, 2, 4, 8, 16, 32, 64, 128],
    age_ms_edges: [600_000, 3_600_000, 21_600_000, 86_400_000, 259_200_000],
    value_sats_edges: [
      100_000, 1_000_000, 10_000_000, 100_000_000, 1_000_000_000,
    ],
    input_count_uppers: [1, 5, 20, 100],
    output_count_uppers: [1, 2, 10, 50],
    classification_keys: [
      "payment",
      "consolidation",
      "batch",
      "coinjoin",
      "data",
      "lightning",
      "unknown",
    ],
    script_keys: [
      "p2tr",
      "p2wpkh",
      "p2wsh",
      "p2sh",
      "p2pkh",
      "op_return",
      "other",
    ],
  },
  histograms: {
    classification: {
      status: "available",
      bins: Array.from({ length: 7 }, (_, index) =>
        index === 6 ? { count: 1, vsize: 200 } : { count: 0, vsize: 0 },
      ),
      underived: { count: 0, vsize: 0 },
    },
    script: { status: "unavailable", reason: "requires_raw_transaction" },
    value: { status: "unavailable", reason: "requires_raw_transaction" },
    inputs: { status: "unavailable", reason: "requires_raw_transaction" },
    outputs: { status: "unavailable", reason: "requires_raw_transaction" },
    age: {
      status: "available",
      bins: Array.from({ length: 6 }, () => ({ count: 0, vsize: 0 })),
      underived: { count: 2, vsize: 150 },
    },
    feerate: {
      status: "available",
      bins: Array.from({ length: 9 }, () => ({ count: 0, vsize: 0 })),
      underived: { count: 0, vsize: 0 },
    },
  },
  ecdf: {
    fee_edges: [0.5, 1, 2, 512],
    series: [{ key: "unknown", cum_vsize: [0, 100, 200] }],
  },
  joint_fee_size: {
    fee_edges: [1, 2, 512],
    size_edges: [100, 1_000, 100_000],
    grid: [
      [10, 0],
      [0, 20],
    ],
  },
  health: {
    last_seen_at_ms: 1_752_710_000_000,
    capture: { status: "no_reported_gaps" },
  },
});

describe("parseMempoolSummary", () => {
  it("accepts a full summary payload", () => {
    const summary = parseMempoolSummary(validSummary());
    expect(summary.source_id).toBe("source-a");
    expect(summary.totals.awaiting_rpc.count).toBe(3);
    expect(summary.filter_echo.classes).toEqual(["unknown"]);
    expect(summary.histograms.script.status).toBe("unavailable");
    const age = summary.histograms.age;
    expect(age.status).toBe("available");
    expect(age.status === "available" && age.underived).toEqual({
      count: 2,
      vsize: 150,
    });
    const classification = summary.histograms.classification;
    expect(classification.status).toBe("available");
    expect(
      classification.status === "available" && classification.underived,
    ).toEqual({ count: 0, vsize: 0 });
    expect(summary.ecdf?.series[0]?.key).toBe("unknown");
    expect(summary.joint_fee_size?.grid).toHaveLength(2);
  });

  it("accepts a summary without detail blocks", () => {
    const payload = validSummary();
    delete payload.ecdf;
    delete payload.joint_fee_size;
    const summary = parseMempoolSummary(payload);
    expect(summary.ecdf).toBeUndefined();
    expect(summary.joint_fee_size).toBeUndefined();
  });

  it.each([
    [
      "unknown classification key",
      (payload: Record<string, unknown>): void => {
        (payload.bins as Record<string, unknown>).classification_keys = [
          "snazzy",
        ];
      },
    ],
    [
      "missing histogram dimension",
      (payload: Record<string, unknown>): void => {
        delete (payload.histograms as Record<string, unknown>).age;
      },
    ],
    [
      "negative bin count",
      (payload: Record<string, unknown>): void => {
        (
          (payload.histograms as Record<string, Record<string, unknown>>)
            .feerate as { bins: { count: number }[] }
        ).bins[0]!.count = -1;
      },
    ],
    [
      "malformed ecdf series",
      (payload: Record<string, unknown>): void => {
        (payload.ecdf as Record<string, unknown>).series = [
          { key: "unknown", cum_vsize: ["many"] },
        ];
      },
    ],
    [
      "unavailable histogram without reason",
      (payload: Record<string, unknown>): void => {
        (payload.histograms as Record<string, unknown>).script = {
          status: "unavailable",
        };
      },
    ],
    [
      "available histogram missing underived",
      (payload: Record<string, unknown>): void => {
        const age = (payload.histograms as Record<string, unknown>)
          .age as Record<string, unknown>;
        delete age.underived;
      },
    ],
  ])("rejects %s", (_name, mutate) => {
    const payload = validSummary();
    mutate(payload);
    expect(() => parseMempoolSummary(payload)).toThrow(TypeError);
  });
});

describe("parseSourcesResponse", () => {
  it("accepts a source listing", () => {
    const sources = parseSourcesResponse({
      sources: [
        { source_id: "source-a", last_seen_at_ms: 5, membership_count: 2 },
      ],
    });
    expect(sources).toHaveLength(1);
    expect(sources[0]?.source_id).toBe("source-a");
  });

  it("rejects descriptors with missing counts", () => {
    expect(() =>
      parseSourcesResponse({
        sources: [{ source_id: "source-a", last_seen_at_ms: 5 }],
      }),
    ).toThrow(TypeError);
  });
});

describe("summaryQuery", () => {
  it("always requests detail blocks and encodes facets", () => {
    const query = summaryQuery({
      classes: ["payment", "data"],
      feerate_min: 4,
    });
    const parameters = new URLSearchParams(query);
    expect(parameters.get("class")).toBe("payment,data");
    expect(parameters.get("feerate_min")).toBe("4");
    expect(parameters.get("detail")).toBe("ecdf,joint");
    expect(parameters.get("script")).toBeNull();
  });
});
