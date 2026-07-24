import { describe, expect, it } from "vitest";

import {
  classificationTotals,
  createTerrainLayout,
  hitTestTerrain,
  selectedRulePopulation,
  unresolvedPrimaryPopulation,
} from "./terrain";
import type { Bip110Assessment, MempoolTransaction, RuleId } from "./types";

const compatible: Bip110Assessment = {
  status: "compatible",
  primary_rule: null,
  violated_rules: [],
  unknown_rules: [],
};

const violating = (rule: RuleId): Bip110Assessment => ({
  status: "violating",
  primary_rule: rule,
  violated_rules: [rule],
  unknown_rules: [],
});

const transaction = (
  value: number,
  overrides: Partial<MempoolTransaction> = {},
): MempoolTransaction => ({
  txid: value.toString(16).padStart(64, "0"),
  wtxid: value.toString(16).padStart(64, "0"),
  vsize: 200,
  fee_sats: 2_000,
  entered_at_ms: 1_700_000_000_000,
  bip110: compatible,
  ...overrides,
});

describe("classificationTotals", () => {
  it("keeps status and coverage as independent claims", () => {
    const transactions = [
      transaction(1),
      transaction(2, { bip110: violating("element_size"), vsize: 400 }),
      transaction(3, {
        bip110: {
          status: "violating",
          primary_rule: "op_success",
          violated_rules: ["op_success"],
          unknown_rules: ["taproot_annex"],
        },
        vsize: 300,
      }),
      transaction(4, {
        bip110: {
          status: "indeterminate",
          primary_rule: null,
          violated_rules: [],
          unknown_rules: ["undefined_version"],
        },
      }),
      transaction(5, { bip110: null }),
    ];

    expect(classificationTotals(transactions)).toEqual({
      compatible: { count: 1, vsize: 200 },
      violating: { count: 2, vsize: 700 },
      indeterminate: { count: 1, vsize: 200 },
      unclassified: { count: 1, vsize: 200 },
      completeCoverage: { count: 2, vsize: 600 },
      incompleteCoverage: { count: 2, vsize: 500 },
    });
  });
});

describe("selectedRulePopulation", () => {
  it("uses the primary rule once and sorts samples by virtual size", () => {
    const transactions = [
      transaction(1, {
        vsize: 220,
        bip110: violating("element_size"),
      }),
      transaction(2, {
        vsize: 520,
        bip110: {
          status: "violating",
          primary_rule: "element_size",
          violated_rules: ["element_size", "op_success"],
          unknown_rules: [],
        },
      }),
      transaction(3, { bip110: violating("op_success") }),
    ];

    const population = selectedRulePopulation(transactions, "element_size");

    expect(population.count).toBe(2);
    expect(population.vsize).toBe(740);
    expect(population.totalShare).toBeCloseTo(2 / 3);
    expect(population.transactions.map(({ txid }) => txid)).toEqual([
      transaction(2).txid,
      transaction(1).txid,
    ]);
  });
});

describe("unresolvedPrimaryPopulation", () => {
  it("keeps definite violations whose first rejecting rule is unresolved", () => {
    const unresolved = transaction(1, {
      vsize: 420,
      bip110: {
        status: "violating",
        primary_rule: null,
        violated_rules: ["op_success"],
        unknown_rules: ["element_size"],
      },
    });

    expect(
      unresolvedPrimaryPopulation([
        transaction(2),
        unresolved,
        transaction(3, { bip110: violating("op_success") }),
      ]),
    ).toMatchObject({
      transactions: [unresolved],
      count: 1,
      vsize: 420,
      totalShare: 1 / 3,
    });
  });
});

describe("createTerrainLayout", () => {
  it("lays every transaction inside its source-scoped classification region", () => {
    const transactions = [
      transaction(1),
      transaction(2, { bip110: violating("element_size") }),
      transaction(3, {
        bip110: {
          status: "indeterminate",
          primary_rule: null,
          violated_rules: [],
          unknown_rules: ["op_success"],
        },
      }),
      transaction(4, { bip110: null }),
      transaction(5, {
        bip110: {
          status: "violating",
          primary_rule: null,
          violated_rules: ["op_success"],
          unknown_rules: ["element_size"],
        },
      }),
    ];
    const layout = createTerrainLayout(transactions, 1_000, 640, "count");

    expect(layout.glyphs).toHaveLength(transactions.length);
    for (const glyph of layout.glyphs) {
      const region = layout.regions.find(({ key }) => key === glyph.regionKey);
      expect(region).toBeDefined();
      expect(glyph.rect.x).toBeGreaterThanOrEqual(region?.contentRect.x ?? -1);
      expect(glyph.rect.y).toBeGreaterThanOrEqual(region?.contentRect.y ?? -1);
      expect(glyph.rect.x + glyph.rect.width).toBeLessThanOrEqual(
        (region?.contentRect.x ?? 0) + (region?.contentRect.width ?? 0),
      );
      expect(glyph.rect.y + glyph.rect.height).toBeLessThanOrEqual(
        (region?.contentRect.y ?? 0) + (region?.contentRect.height ?? 0),
      );
    }
    expect(
      layout.glyphs.find(({ txid }) => txid === transaction(5).txid)?.regionKey,
    ).toBe("violating_unresolved");
  });

  it("switches transaction area between count and virtual-size modes", () => {
    const transactions = [
      transaction(1, { vsize: 100 }),
      transaction(2, { vsize: 900 }),
    ];
    const countLayout = createTerrainLayout(transactions, 1_000, 640, "count");
    const vsizeLayout = createTerrainLayout(transactions, 1_000, 640, "vsize");
    const area = (
      layout: ReturnType<typeof createTerrainLayout>,
      txid: string,
    ): number => {
      const rect = layout.glyphs.find((glyph) => glyph.txid === txid)?.rect;
      return (rect?.width ?? 0) * (rect?.height ?? 0);
    };

    expect(area(countLayout, transactions[1]?.txid ?? "")).toBeCloseTo(
      area(countLayout, transactions[0]?.txid ?? ""),
      -1,
    );
    expect(area(vsizeLayout, transactions[1]?.txid ?? "")).toBeGreaterThan(
      area(vsizeLayout, transactions[0]?.txid ?? "") * 7,
    );
  });

  it("hit-tests transaction glyphs and empty region space", () => {
    const layout = createTerrainLayout(
      [transaction(1, { bip110: violating("element_size") })],
      900,
      600,
      "count",
    );
    const glyph = layout.glyphs[0];
    expect(glyph).toBeDefined();
    if (glyph === undefined) {
      return;
    }
    const glyphHit = hitTestTerrain(
      layout,
      glyph.rect.x + glyph.rect.width / 2,
      glyph.rect.y + glyph.rect.height / 2,
    );
    expect(glyphHit).toMatchObject({
      kind: "transaction",
      glyph: { txid: transaction(1).txid },
    });

    const region = layout.regions.find(({ key }) => key === "element_size");
    expect(region).toBeDefined();
    const regionHit = hitTestTerrain(
      layout,
      (region?.rect.x ?? 0) + 2,
      (region?.rect.y ?? 0) + 2,
    );
    expect(regionHit).toMatchObject({
      kind: "region",
      region: { key: "element_size" },
    });
  });

  it("keeps one Canvas glyph per entry at the supported snapshot cap", () => {
    const transactions = Array.from({ length: 200_000 }, (_, index) =>
      transaction(index + 1, {
        vsize: 120 + (index % 4_000),
        bip110:
          index % 10 === 0
            ? violating("element_size")
            : index % 17 === 0
              ? null
              : compatible,
      }),
    );

    const layout = createTerrainLayout(transactions, 1_200, 720, "vsize");

    expect(layout.glyphs).toHaveLength(200_000);
    expect(layout.regions).toHaveLength(11);
  });
});
