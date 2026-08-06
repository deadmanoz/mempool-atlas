import { describe, expect, it } from "vitest";

import {
  classificationTotals,
  createTerrainLayout,
  hitTestTerrain,
  incompleteViolationPopulation,
  paintTerrain,
  ruleMask,
  rulePopulation,
  rulesForMask,
  signatureLabel,
  signaturePopulations,
  statusPopulation,
  terrainRegionKey,
  violationSignature,
} from "./terrain";
import { mempoolTransaction } from "./test-fixtures";
import type { Bip110Assessment, MempoolTransaction, RuleId } from "./types";

const compatible: Bip110Assessment = {
  status: "compatible",
  primary_rule: null,
  violated_rules: [],
  unknown_rules: [],
};

const violating = (
  rules: RuleId | RuleId[],
  options: {
    primary?: RuleId | null;
    unknown?: RuleId[];
  } = {},
): Bip110Assessment => {
  const violatedRules = Array.isArray(rules) ? rules : [rules];
  const hasExplicitPrimary = Object.prototype.hasOwnProperty.call(
    options,
    "primary",
  );
  return {
    status: "violating",
    primary_rule: hasExplicitPrimary
      ? (options.primary ?? null)
      : (violatedRules[0] ?? null),
    violated_rules: violatedRules,
    unknown_rules: options.unknown ?? [],
  };
};

const transaction = (
  value: number,
  overrides: Partial<MempoolTransaction> = {},
): MempoolTransaction =>
  mempoolTransaction(value, { bip110: compatible, ...overrides });

describe("classificationTotals", () => {
  it("keeps status and coverage as independent claims", () => {
    const transactions = [
      transaction(1),
      transaction(2, {
        bip110: violating("element_size"),
        vsize: 400,
      }),
      transaction(3, {
        bip110: violating("op_success", {
          unknown: ["taproot_annex"],
        }),
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

describe("rule signatures", () => {
  it("uses canonical seven-bit identity independent of API rule order", () => {
    const forward = violationSignature(
      violating(["element_size", "tapscript_op_if"], {
        primary: "tapscript_op_if",
      }),
    );
    const reverse = violationSignature(
      violating(["tapscript_op_if", "element_size"], {
        primary: "element_size",
      }),
    );
    const unresolvedPrimary = violationSignature(
      violating(["element_size", "tapscript_op_if"], { primary: null }),
    );

    expect(forward).toMatchObject({
      key: "exact:42",
      violatedRules: ["element_size", "tapscript_op_if"],
      foundationRule: "tapscript_op_if",
    });
    expect(reverse).toEqual(forward);
    expect(unresolvedPrimary).toEqual(forward);
    expect(signatureLabel(forward!)).toBe("R2 + R7");
  });

  it("keeps proven and unresolved facts for the same rule", () => {
    expect(
      violationSignature(
        violating("element_size", { unknown: ["element_size"] }),
      ),
    ).toMatchObject({
      key: "partial:02:02",
      completeness: "partial",
      violatedRules: ["element_size"],
      unknownRules: ["element_size"],
    });
  });

  it("round-trips every non-empty rule mask without collisions", () => {
    const keys = new Set<string>();
    for (let mask = 1; mask < 1 << 7; mask += 1) {
      const rules = rulesForMask(mask);
      expect(ruleMask(rules)).toBe(mask);
      const signature = violationSignature(violating(rules));
      expect(signature).not.toBeNull();
      keys.add(signature?.key ?? "");
    }
    expect(keys).toHaveLength(127);
  });
});

describe("terrainRegionKey", () => {
  it("maps every assessment shape to its canonical terrain region", () => {
    expect(terrainRegionKey(transaction(1))).toBe("compatible");
    expect(terrainRegionKey(transaction(2, { bip110: null }))).toBe(
      "unclassified",
    );
    expect(
      terrainRegionKey(
        transaction(3, {
          bip110: {
            status: "indeterminate",
            primary_rule: null,
            violated_rules: [],
            unknown_rules: ["undefined_version"],
          },
        }),
      ),
    ).toBe("indeterminate");
    expect(
      terrainRegionKey(
        transaction(4, {
          bip110: violating(["element_size", "tapscript_op_if"]),
        }),
      ),
    ).toBe("exact:42");
    expect(
      terrainRegionKey(
        transaction(5, {
          bip110: violating("element_size", {
            unknown: ["tapscript_op_if"],
          }),
        }),
      ),
    ).toBe("partial:02:40");
  });
});

describe("rulePopulation", () => {
  it("counts every proven rule occurrence and sorts samples by vsize", () => {
    const transactions = [
      transaction(1, {
        vsize: 220,
        bip110: violating("element_size"),
      }),
      transaction(2, {
        vsize: 520,
        bip110: violating(["element_size", "tapscript_op_if"], {
          primary: "tapscript_op_if",
        }),
      }),
      transaction(3, { bip110: violating("tapscript_op_if") }),
    ];

    const r2 = rulePopulation(transactions, "element_size");
    const r7 = rulePopulation(transactions, "tapscript_op_if");

    expect(r2).toMatchObject({ count: 2, vsize: 740 });
    expect(r2.totalShare).toBeCloseTo(2 / 3);
    expect(r2.transactions.map(({ txid }) => txid)).toEqual([
      transaction(2).txid,
      transaction(1).txid,
    ]);
    expect(r7).toMatchObject({ count: 2, vsize: 720 });
  });
});

describe("statusPopulation", () => {
  it("partitions selectable non-violating status buckets", () => {
    const indeterminate: Bip110Assessment = {
      status: "indeterminate",
      primary_rule: null,
      violated_rules: [],
      unknown_rules: ["taproot_annex"],
    };
    const transactions = [
      transaction(1, { vsize: 100 }),
      transaction(2, { bip110: indeterminate, vsize: 200 }),
      transaction(3, { bip110: null, vsize: 300 }),
    ];

    expect(statusPopulation(transactions, "compatible")).toMatchObject({
      count: 1,
      vsize: 100,
      totalShare: 1 / 3,
    });
    expect(statusPopulation(transactions, "indeterminate")).toMatchObject({
      count: 1,
      vsize: 200,
      totalShare: 1 / 3,
    });
    expect(statusPopulation(transactions, "unclassified")).toMatchObject({
      count: 1,
      vsize: 300,
      totalShare: 1 / 3,
    });
  });
});

describe("signaturePopulations", () => {
  it("co-buckets equal rule sets despite different first rejections", () => {
    const transactions = [
      transaction(1, {
        vsize: 300,
        bip110: violating(["element_size", "tapscript_op_if"], {
          primary: "tapscript_op_if",
        }),
      }),
      transaction(2, {
        vsize: 700,
        bip110: violating(["tapscript_op_if", "element_size"], {
          primary: "element_size",
        }),
      }),
      transaction(3, {
        bip110: violating("tapscript_op_if"),
      }),
    ];

    const populations = signaturePopulations(transactions);
    const overlap = populations.find(
      ({ signature }) => signature.key === "exact:42",
    );

    expect(populations).toHaveLength(2);
    expect(overlap).toMatchObject({ count: 2, vsize: 1_000 });
    expect(overlap?.transactions.map(({ txid }) => txid)).toEqual([
      transaction(2).txid,
      transaction(1).txid,
    ]);
  });

  it("keeps partial violations outside exact buckets", () => {
    const exact = transaction(1, {
      bip110: violating(["element_size", "tapscript_op_if"]),
    });
    const partial = transaction(2, {
      bip110: violating(["element_size", "tapscript_op_if"], {
        unknown: ["undefined_version"],
      }),
    });
    const populations = signaturePopulations([exact, partial]);

    expect(populations.map(({ signature }) => signature.key)).toEqual([
      "exact:42",
      "partial:42:04",
    ]);
    expect(incompleteViolationPopulation([exact, partial])).toMatchObject({
      transactions: [partial],
      count: 1,
      vsize: 200,
      totalShare: 0.5,
    });
  });
});

describe("createTerrainLayout", () => {
  it("lays every transaction once inside its dynamic status or signature bucket", () => {
    const transactions = [
      transaction(1),
      transaction(2, {
        bip110: violating(["element_size", "tapscript_op_if"], {
          primary: "tapscript_op_if",
        }),
      }),
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
        bip110: violating("op_success", {
          primary: null,
          unknown: ["element_size"],
        }),
      }),
    ];
    const layout = createTerrainLayout(transactions, 1_000, 640, "count");

    expect(layout.glyphs).toHaveLength(transactions.length);
    expect(new Set(layout.glyphs.map(({ txid }) => txid))).toHaveLength(
      transactions.length,
    );
    expect(layout.regions.map(({ key }) => key)).toEqual([
      "compatible",
      "indeterminate",
      "exact:42",
      "partial:20:02",
      "unclassified",
    ]);
    expect(layout.sections.map(({ key }) => key)).toEqual([
      "compatible",
      "indeterminate",
      "violating_exact",
      "violating_incomplete",
      "unclassified",
    ]);
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
  });

  it("orders observed exact buckets by stable rule-set similarity", () => {
    const transactions = [
      transaction(1, { bip110: violating("tapscript_op_if") }),
      transaction(2, {
        bip110: violating(["element_size", "tapscript_op_if"]),
      }),
      transaction(3, { bip110: violating("output_size") }),
    ];
    const countLayout = createTerrainLayout(transactions, 1_000, 640, "count");
    const vsizeLayout = createTerrainLayout(transactions, 1_200, 720, "vsize");
    const keys = (layout: ReturnType<typeof createTerrainLayout>): string[] =>
      layout.regions
        .filter(({ signature }) => signature?.completeness === "exact")
        .map(({ key }) => key);

    expect(keys(countLayout)).toEqual(["exact:01", "exact:42", "exact:40"]);
    expect(keys(vsizeLayout)).toEqual(keys(countLayout));
  });

  it("switches transaction area between count and virtual-size modes", () => {
    const transactions = [
      transaction(1, {
        vsize: 100,
        bip110: violating(["element_size", "tapscript_op_if"]),
      }),
      transaction(2, {
        vsize: 900,
        bip110: violating(["element_size", "tapscript_op_if"]),
      }),
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

  it("hit-tests exact-combination glyphs and bucket headers", () => {
    const layout = createTerrainLayout(
      [
        transaction(1, {
          bip110: violating(["element_size", "tapscript_op_if"]),
        }),
      ],
      900,
      600,
      "count",
    );
    const glyph = layout.glyphs[0];
    expect(glyph).toBeDefined();
    if (glyph === undefined) {
      return;
    }
    expect(
      hitTestTerrain(
        layout,
        glyph.rect.x + glyph.rect.width / 2,
        glyph.rect.y + glyph.rect.height / 2,
      ),
    ).toMatchObject({
      kind: "transaction",
      glyph: { txid: transaction(1).txid, regionKey: "exact:42" },
    });

    const region = layout.regions.find(({ key }) => key === "exact:42");
    expect(region).toBeDefined();
    expect(
      hitTestTerrain(
        layout,
        (region?.rect.x ?? 0) + 2,
        (region?.rect.y ?? 0) + 2,
      ),
    ).toMatchObject({
      kind: "region",
      region: { key: "exact:42" },
    });
  });

  it("outlines only the selected transaction glyph", () => {
    const transactions = [transaction(1), transaction(2)];
    const layout = createTerrainLayout(transactions, 900, 600, "count");
    const operations: string[] = [];
    const strokeCalls: Array<{
      style: string;
      lineWidth: number;
      x: number;
      y: number;
      width: number;
      height: number;
    }> = [];
    const contextState = {
      fillStyle: "",
      strokeStyle: "",
      lineWidth: 1,
      globalAlpha: 1,
      clearRect: () => undefined,
      fillRect: () => operations.push("fill"),
      strokeRect(x: number, y: number, width: number, height: number): void {
        operations.push(`stroke:${contextState.strokeStyle}`);
        strokeCalls.push({
          style: contextState.strokeStyle,
          lineWidth: contextState.lineWidth,
          x,
          y,
          width,
          height,
        });
      },
    };
    const context = contextState as unknown as CanvasRenderingContext2D;

    paintTerrain(
      context,
      layout,
      { kind: "region", regionKey: "compatible" },
      transactions[0]?.txid,
    );

    const selectedGlyph = layout.glyphs.find(
      ({ txid }) => txid === transactions[0]?.txid,
    );
    expect(selectedGlyph).toBeDefined();
    expect(strokeCalls.filter(({ style }) => style === "#f7ff6a")).toEqual([
      {
        style: "#f7ff6a",
        lineWidth: 2.5,
        ...selectedGlyph?.rect,
      },
    ]);
    expect(operations.at(-1)).toBe("stroke:#f7ff6a");
  });

  it("paints one visible Canvas block per transaction", () => {
    const transactions = Array.from({ length: 1_000 }, (_, index) =>
      transaction(index + 1),
    );
    const layout = createTerrainLayout(transactions, 900, 600, "count");
    let fillCount = 0;
    const context = {
      fillStyle: "",
      strokeStyle: "",
      lineWidth: 1,
      globalAlpha: 1,
      clearRect: () => undefined,
      fillRect: () => {
        fillCount += 1;
      },
      strokeRect: () => undefined,
    } as unknown as CanvasRenderingContext2D;

    paintTerrain(context, layout, { kind: "region", regionKey: "compatible" });

    expect(layout.glyphs).toHaveLength(transactions.length);
    expect(fillCount).toBeGreaterThan(transactions.length);
  });

  it("keeps one Canvas glyph per entry without theoretical bucket allocation", () => {
    const transactions = Array.from({ length: 200_000 }, (_, index) =>
      transaction(index + 1, {
        vsize: 120 + (index % 4_000),
        bip110:
          index % 31 === 0
            ? violating(["element_size", "tapscript_op_if"])
            : index % 23 === 0
              ? violating("op_success", { unknown: ["tapscript_op_if"] })
              : index % 17 === 0
                ? null
                : index % 10 === 0
                  ? violating("element_size")
                  : compatible,
      }),
    );

    const layout = createTerrainLayout(transactions, 1_200, 720, "vsize");

    expect(layout.glyphs).toHaveLength(200_000);
    expect(layout.regions.map(({ key }) => key)).toEqual([
      "compatible",
      "exact:02",
      "exact:42",
      "partial:20:40",
      "unclassified",
    ]);
    expect(layout.regions).toHaveLength(5);
  });

  it("preserves glyphs for singleton status and signature buckets under heavy skew", () => {
    const common = Array.from({ length: 20_000 }, (_, index) =>
      transaction(index + 1),
    );
    const exact = Array.from({ length: 127 }, (_, index) => {
      const rules = rulesForMask(index + 1);
      return transaction(30_000 + index, {
        bip110: violating(rules, {
          primary: index % 2 === 0 ? null : (rules[0] ?? null),
        }),
      });
    });
    const rare = [
      transaction(40_001, {
        bip110: {
          status: "indeterminate",
          primary_rule: null,
          violated_rules: [],
          unknown_rules: ["undefined_version"],
        },
      }),
      transaction(40_002, {
        bip110: violating("output_size", { unknown: ["element_size"] }),
      }),
      transaction(40_003, { bip110: null }),
    ];
    const transactions = [...common, ...exact, ...rare];

    const layout = createTerrainLayout(transactions, 1_200, 720, "count");

    expect(layout.glyphs).toHaveLength(transactions.length);
    expect(layout.regions).toHaveLength(131);
    for (const region of layout.regions) {
      const glyphs = layout.glyphs.filter(
        ({ regionKey }) => regionKey === region.key,
      );
      expect(glyphs.length).toBe(region.transactionCount);
      expect(region.contentRect.width).toBeGreaterThan(0);
      expect(region.contentRect.height).toBeGreaterThan(0);
      for (const glyph of glyphs) {
        expect(glyph.rect.width).toBeGreaterThan(0);
        expect(glyph.rect.height).toBeGreaterThan(0);
      }
    }
  });
});
