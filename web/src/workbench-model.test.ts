import { describe, expect, it } from "vitest";
import type {
  BinCatalog,
  FeeRateEcdf,
  JointFeeSize,
  MempoolSummary,
  TaxonomyDescriptor,
} from "./types";
import {
  ageBinLabels,
  compositionRows,
  countBandLabels,
  ecdfPaths,
  jointViewModel,
  ramp,
  rangeBinLabels,
  squarify,
  valueBinLabels,
  verdictMeta,
} from "./workbench-model";

const behaviorTaxonomy = (): TaxonomyDescriptor => ({
  key: "behavior",
  label: "Behavior",
  verdicts: [
    { key: "payment", label: "Payment" },
    { key: "unknown", label: "Unknown" },
  ],
});

const summaryWithTaxonomyUnderived = (underived: {
  count: number;
  vsize: number;
}): MempoolSummary => ({
  source_id: "source-a",
  as_of_ms: 1_752_710_400_000,
  filter_echo: {},
  totals: {
    all: { count: 10, vsize: 1_000 },
    matching: { count: 10, vsize: 1_000 },
    awaiting_rpc: { count: 0 },
  },
  bins: {
    taxonomies: [behaviorTaxonomy()],
    feerate_sat_per_vb_edges: [],
    age_ms_edges: [],
    value_sats_edges: [],
    input_count_uppers: [],
    output_count_uppers: [],
    script_keys: [],
  },
  histograms: {
    taxonomies: [
      {
        key: "behavior",
        status: "available",
        bins: [
          { count: 6, vsize: 600 },
          { count: 2, vsize: 200 },
        ],
        underived,
      },
    ],
    script: { status: "unavailable", reason: "requires_raw_transaction" },
    value: { status: "unavailable", reason: "requires_raw_transaction" },
    inputs: { status: "unavailable", reason: "requires_raw_transaction" },
    outputs: { status: "unavailable", reason: "requires_raw_transaction" },
    age: { status: "unavailable", reason: "requires_raw_transaction" },
    feerate: { status: "unavailable", reason: "requires_raw_transaction" },
  },
  health: {
    state_cursor: { epoch_id: "epoch-a", revision: 1 },
    state_observed_at_ms: 0,
    capture: { status: "not_collected" },
  },
});

describe("bin labels", () => {
  it("labels open-ended numeric ranges", () => {
    expect(rangeBinLabels([1, 2, 4])).toEqual(["<1", "1–2", "2–4", "4+"]);
  });

  it("labels ages with duration units", () => {
    expect(
      ageBinLabels([600_000, 3_600_000, 21_600_000, 86_400_000, 259_200_000]),
    ).toEqual(["<10m", "10m–1h", "1h–6h", "6h–1d", "1d–3d", "3d+"]);
  });

  it("labels count bands with inclusive uppers", () => {
    expect(countBandLabels([1, 5, 20, 100])).toEqual([
      "1",
      "2–5",
      "6–20",
      "21–100",
      "100+",
    ]);
  });

  it("labels values in BTC", () => {
    expect(valueBinLabels([100_000, 100_000_000])).toEqual([
      "<0.001",
      "0.001–1",
      "1+",
    ]);
  });
});

describe("ramp", () => {
  it("clamps and interpolates", () => {
    expect(ramp(-1)).toBe("rgb(16,27,39)");
    expect(ramp(2)).toBe("rgb(195,245,247)");
    expect(ramp(0.5)).toBe("rgb(47,125,146)");
  });
});

describe("verdictMeta", () => {
  it("uses the named behavior colours for its known verdicts", () => {
    expect(verdictMeta(behaviorTaxonomy())).toEqual([
      { key: "payment", label: "Payment", color: "#56c7d9" },
      { key: "unknown", label: "Unknown", color: "#6b7a8d" },
    ]);
  });

  it("assigns the fallback palette deterministically for other taxonomies", () => {
    const taxonomy: TaxonomyDescriptor = {
      key: "bip110",
      label: "BIP110",
      verdicts: [
        { key: "eligible", label: "Eligible" },
        { key: "ineligible", label: "Ineligible" },
        { key: "unknown", label: "Unknown" },
      ],
    };
    const first = verdictMeta(taxonomy);
    const second = verdictMeta(taxonomy);
    expect(first).toEqual(second);
    expect(first[0]?.color).not.toBe(first[1]?.color);
    expect(first[2]?.color).toBe("#6b7a8d");
  });

  it("gives verdicts at the same index the same fallback colour across taxonomies", () => {
    const taxonomyA: TaxonomyDescriptor = {
      key: "bip110",
      label: "BIP110",
      verdicts: [
        { key: "eligible", label: "Eligible" },
        { key: "unknown", label: "Unknown" },
      ],
    };
    const taxonomyB: TaxonomyDescriptor = {
      key: "data_protocol",
      label: "Data protocol",
      verdicts: [
        { key: "ordinal", label: "Ordinal" },
        { key: "unknown", label: "Unknown" },
      ],
    };
    expect(verdictMeta(taxonomyA)[0]?.color).toBe(
      verdictMeta(taxonomyB)[0]?.color,
    );
  });
});

describe("squarify", () => {
  it("tiles cover the canvas area proportionally", () => {
    const tiles = squarify(
      [
        { key: "a", label: "A", color: "#111111", count: 1, vsize: 600 },
        { key: "b", label: "B", color: "#222222", count: 1, vsize: 300 },
        { key: "c", label: "C", color: "#333333", count: 1, vsize: 100 },
      ],
      100,
      100,
    );
    expect(tiles).toHaveLength(3);
    const area = tiles.reduce((sum, tile) => sum + tile.width * tile.height, 0);
    expect(area).toBeCloseTo(10_000, 5);
    for (const tile of tiles) {
      expect(tile.x).toBeGreaterThanOrEqual(0);
      expect(tile.y).toBeGreaterThanOrEqual(0);
      expect(tile.x + tile.width).toBeLessThanOrEqual(100.000001);
      expect(tile.y + tile.height).toBeLessThanOrEqual(100.000001);
    }
    const largest = tiles.find((tile) => tile.key === "a");
    expect(largest && largest.width * largest.height).toBeCloseTo(6_000, 5);
  });

  it("drops zero-weight nodes", () => {
    expect(
      squarify(
        [{ key: "a", label: "A", color: "#111111", count: 0, vsize: 0 }],
        100,
        100,
      ),
    ).toEqual([]);
  });
});

describe("ecdfPaths", () => {
  it("normalizes each series to its own total and skips empty series", () => {
    const bins: BinCatalog = {
      taxonomies: [behaviorTaxonomy()],
      feerate_sat_per_vb_edges: [],
      age_ms_edges: [],
      value_sats_edges: [],
      input_count_uppers: [],
      output_count_uppers: [],
      script_keys: [],
    };
    const ecdf: FeeRateEcdf = {
      taxonomy: "behavior",
      fee_edges: [0.5, 1, 2, 4],
      series: [
        { key: "unknown", cum_vsize: [0, 50, 100] },
        { key: "payment", cum_vsize: [0, 0, 0] },
      ],
    };
    const paths = ecdfPaths(ecdf, bins, {
      left: 0,
      top: 0,
      width: 100,
      height: 100,
    });
    expect(paths).toHaveLength(1);
    expect(paths[0]?.key).toBe("unknown");
    // Starts at zero share (bottom) and ends at full share (top).
    expect(paths[0]?.d.startsWith("M0.0 100.0")).toBe(true);
    expect(paths[0]?.d.endsWith("L100.0 0.0")).toBe(true);
  });
});

describe("compositionRows", () => {
  it("appends a trailing underived segment when the active metric is nonzero", () => {
    const summary = summaryWithTaxonomyUnderived({
      count: 2,
      vsize: 200,
    });
    const rows = compositionRows(summary, "vsize");
    const behavior = rows.find((row) => row.key === "behavior");
    expect(behavior?.status).toBe("available");
    if (behavior?.status !== "available") {
      throw new Error("expected available behavior row");
    }
    expect(behavior.segments).toHaveLength(3);
    const underived = behavior.segments[2]!;
    expect(underived.key).toBe("underived");
    expect(underived.label).toBe("Underived");
    expect(underived.color).toBe("#38434f");
    expect(underived.fraction).toBeCloseTo(0.2, 5);
    expect(underived.count).toBe(2);
    expect(underived.vsize).toBe(200);
    // Denominator includes underived: bin fractions shrink accordingly.
    expect(behavior.segments[0]!.fraction).toBeCloseTo(0.6, 5);
    expect(behavior.segments[1]!.fraction).toBeCloseTo(0.2, 5);
  });

  it("omits the underived segment when underived is zero", () => {
    const summary = summaryWithTaxonomyUnderived({
      count: 0,
      vsize: 0,
    });
    const rows = compositionRows(summary, "vsize");
    const behavior = rows.find((row) => row.key === "behavior");
    expect(behavior?.status).toBe("available");
    if (behavior?.status !== "available") {
      throw new Error("expected available behavior row");
    }
    expect(behavior.segments).toHaveLength(2);
    expect(
      behavior.segments.some((segment) => segment.key === "underived"),
    ).toBe(false);
  });

  it("lists every taxonomy ahead of the six fixed shape dimensions", () => {
    const summary = summaryWithTaxonomyUnderived({ count: 0, vsize: 0 });
    const rows = compositionRows(summary, "vsize");
    expect(rows.map((row) => row.key)).toEqual([
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

describe("jointViewModel", () => {
  it("normalizes cells and flips rows so the largest sizes render on top", () => {
    const joint: JointFeeSize = {
      fee_edges: [1, 2, 4],
      size_edges: [100, 1_000, 10_000],
      grid: [
        [100, 0],
        [0, 25],
      ],
    };
    const view = jointViewModel(joint);
    expect(view.feeBinCount).toBe(2);
    expect(view.sizeBinCount).toBe(2);
    // Top display row is the larger-size bin (grid row 1).
    expect(view.rows[0]).toEqual([0, Math.sqrt(25 / 100)]);
    expect(view.rows[1]).toEqual([1, 0]);
    expect(view.top).toEqual([1, 0.25]);
    expect(view.right).toEqual([0.25, 1]);
  });
});
