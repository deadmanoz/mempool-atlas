import { describe, expect, it } from "vitest";

import {
  DATA_BYTES_DOMAIN,
  DATA_BYTES_TICKS,
  FEE_RATE_DOMAIN,
  FEE_RATE_TICKS,
  IO_COUNT_DOMAIN,
  IO_COUNT_TICKS,
  OUTPUT_VALUE_DOMAIN,
  OUTPUT_VALUE_TICKS,
  VSIZE_DOMAIN,
  VSIZE_TICKS,
  buildFeeSpectrum,
  buildFeeSpectrumCooperatively,
  buildJointDensity,
  buildJointDensityCooperatively,
  logDomainBinBounds,
  logDomainPosition,
  logDomainTicks,
  logDomainValue,
  transactionFeeRate,
} from "./fee-distribution";
import { mempoolTransaction } from "./test-fixtures";
import type { MempoolTransaction } from "./types";

const transaction = (
  suffix: number,
  vsize: number,
  feeSats: number,
): MempoolTransaction =>
  mempoolTransaction(suffix, {
    wtxid: (suffix + 100).toString(16).padStart(64, "0"),
    vsize,
    fee_sats: feeSats,
  });

describe("transactionFeeRate", () => {
  it("derives sat/vB from exact fee and virtual size", () => {
    expect(transactionFeeRate(transaction(1, 200, 400))).toBe(2);
  });

  it("treats a zero virtual size as a zero fee rate", () => {
    expect(transactionFeeRate(transaction(1, 0, 400))).toBe(0);
  });
});

describe("logDomainPosition", () => {
  it("maps the domain endpoints to 0 and 1", () => {
    expect(logDomainPosition(FEE_RATE_DOMAIN, 1)).toBe(0);
    expect(logDomainPosition(FEE_RATE_DOMAIN, 512)).toBe(1);
  });

  it("clamps values outside the domain", () => {
    expect(logDomainPosition(FEE_RATE_DOMAIN, 0.25)).toBe(0);
    expect(logDomainPosition(FEE_RATE_DOMAIN, 100_000)).toBe(1);
    expect(logDomainPosition(FEE_RATE_DOMAIN, 0)).toBe(0);
  });

  it("positions midpoints logarithmically", () => {
    expect(logDomainPosition(FEE_RATE_DOMAIN, 32)).toBeCloseTo(5 / 9, 10);
    expect(logDomainPosition(VSIZE_DOMAIN, 2048)).toBeCloseTo(5 / 11, 10);
  });
});

describe("log-domain axes", () => {
  it("inverts positions and builds positioned ticks from raw values", () => {
    expect(logDomainValue(FEE_RATE_DOMAIN, 0)).toBe(1);
    expect(logDomainValue(FEE_RATE_DOMAIN, 1)).toBe(512);
    expect(logDomainValue(FEE_RATE_DOMAIN, 5 / 9)).toBeCloseTo(32, 10);

    const ticks = logDomainTicks(FEE_RATE_DOMAIN, [
      { value: 1, label: "floor", priority: 3 },
      { value: 32, label: "middle", description: "reference" },
      { value: 512, label: "ceiling" },
    ]);
    expect(ticks.map(({ value, position }) => ({ value, position }))).toEqual([
      { value: 1, position: 0 },
      { value: 32, position: 5 / 9 },
      { value: 512, position: 1 },
    ]);
    expect(ticks[1]).toMatchObject({
      description: "reference",
      priority: 1,
    });
  });

  it("returns open edge bounds for values clamped into spectrum bins", () => {
    const boundary = Math.sqrt(512);
    const lower = logDomainBinBounds(FEE_RATE_DOMAIN, 0, 2);
    const upper = logDomainBinBounds(FEE_RATE_DOMAIN, 1, 2);
    expect(lower.lowerInclusive).toBeNull();
    expect(lower.upperExclusive).toBeCloseTo(boundary, 10);
    expect(upper.lowerInclusive).toBeCloseTo(boundary, 10);
    expect(upper.upperExclusive).toBeNull();
  });

  it("rejects invalid bin requests", () => {
    expect(() => logDomainBinBounds(FEE_RATE_DOMAIN, 0, 0)).toThrow(RangeError);
    expect(() => logDomainBinBounds(FEE_RATE_DOMAIN, 2, 2)).toThrow(RangeError);
  });

  it("keeps shared major ticks on their declared raw values", () => {
    const axes = [
      {
        domain: FEE_RATE_DOMAIN,
        ticks: FEE_RATE_TICKS,
        values: [1, 4, 16, 64, 256, 512],
      },
      {
        domain: VSIZE_DOMAIN,
        ticks: VSIZE_TICKS,
        values: [64, 256, 1_024, 4_096, 16_384, 65_536, 131_072],
      },
      {
        domain: DATA_BYTES_DOMAIN,
        ticks: DATA_BYTES_TICKS,
        values: [1, 8, 40, 80, 512, 4_096, 32_768, 131_072],
      },
      {
        domain: IO_COUNT_DOMAIN,
        ticks: IO_COUNT_TICKS,
        values: [1, 4, 16, 64, 256, 1_024],
      },
      {
        domain: OUTPUT_VALUE_DOMAIN,
        ticks: OUTPUT_VALUE_TICKS,
        values: [
          1_000, 100_000, 10_000_000, 100_000_000, 10_000_000_000,
          100_000_000_000,
        ],
      },
    ] as const;
    for (const { domain, ticks, values } of axes) {
      expect(ticks.map(({ value }) => value)).toEqual(values);
      for (const tick of ticks) {
        expect(tick.position).toBe(logDomainPosition(domain, tick.value));
      }
    }
  });

  it("marks 40 and 80 as payload references without a false 83-byte tick", () => {
    const historical = DATA_BYTES_TICKS.find(({ value }) => value === 40);
    const conventional = DATA_BYTES_TICKS.find(({ value }) => value === 80);
    expect(historical).toMatchObject({
      label: "40 B",
      priority: 3,
    });
    expect(historical?.description).toContain("40 pushed data bytes");
    expect(historical?.description).toContain("42 serialized script bytes");
    expect(conventional).toMatchObject({
      label: "80 B",
      priority: 3,
    });
    expect(conventional?.description).toContain("83-byte OP_RETURN script");
    expect(conventional?.description).toContain("Core 0.12–29 and BIP-110");
    expect(DATA_BYTES_TICKS.some(({ value }) => value === 83)).toBe(false);
  });
});

describe("buildJointDensity", () => {
  it("bins weight by fee rate column and virtual size row", () => {
    const density = buildJointDensity(
      [transaction(1, 64, 64), transaction(2, 131_072, 131_072 * 512)],
      "count",
      4,
      4,
    );
    expect(density.cells[0]).toBe(1);
    expect(density.cells[4 * 4 - 1]).toBe(1);
    expect(density.columnTotals).toEqual([1, 0, 0, 1]);
    expect(density.rowTotals).toEqual([1, 0, 0, 1]);
    expect(density.maxCell).toBe(1);
    expect(density.totalWeight).toBe(2);
  });

  it("clamps out-of-domain values into the edge cells", () => {
    const density = buildJointDensity(
      [transaction(1, 8, 1), transaction(2, 1_000_000, 1_000_000 * 4096)],
      "count",
      3,
      3,
    );
    expect(density.cells[0]).toBe(1);
    expect(density.cells[3 * 3 - 1]).toBe(1);
  });

  it("weights cells by virtual size when requested", () => {
    const density = buildJointDensity(
      [transaction(1, 64, 64), transaction(2, 64, 64)],
      "vsize",
      2,
      2,
    );
    expect(density.cells[0]).toBe(128);
    expect(density.totalWeight).toBe(128);
    expect(density.maxCell).toBe(128);
  });

  it("returns zeroed totals for an empty population", () => {
    const density = buildJointDensity([], "count", 2, 2);
    expect(density.cells).toEqual([0, 0, 0, 0]);
    expect(density.maxCell).toBe(0);
    expect(density.totalWeight).toBe(0);
  });

  it("matches the cooperative density builder exactly", async () => {
    const transactions = [
      transaction(1, 64, 64),
      transaction(2, 1_024, 10_240),
      transaction(3, 131_072, 131_072 * 512),
    ];
    await expect(
      buildJointDensityCooperatively(transactions, "vsize", {
        batchSize: 1,
        yieldBetweenBatches: () => Promise.resolve(),
      }),
    ).resolves.toEqual(buildJointDensity(transactions, "vsize"));
  });
});

describe("buildFeeSpectrum", () => {
  const group = (key: string, transactions: MempoolTransaction[]) => ({
    key,
    label: key,
    color: "#fff",
    transactions,
  });

  it("stacks group segments inside each fee bin in group order", () => {
    const spectrum = buildFeeSpectrum(
      [
        group("a", [transaction(1, 100, 100), transaction(2, 100, 51_200)]),
        group("b", [transaction(3, 100, 100)]),
      ],
      "count",
      2,
    );
    expect(spectrum.bins[0]?.total).toBe(2);
    expect(spectrum.bins[0]?.segments.map(({ key }) => key)).toEqual([
      "a",
      "b",
    ]);
    expect(spectrum.bins[1]?.total).toBe(1);
    expect(spectrum.maxBin).toBe(2);
    expect(spectrum.totalWeight).toBe(3);
  });

  it("omits zero-weight segments and groups", () => {
    const spectrum = buildFeeSpectrum(
      [group("a", [transaction(1, 100, 100)]), group("empty", [])],
      "vsize",
      2,
    );
    expect(spectrum.bins[0]?.segments).toHaveLength(1);
    expect(spectrum.totalWeight).toBe(100);
  });

  it("matches the cooperative spectrum builder exactly", async () => {
    const groups = [
      group("a", [transaction(1, 100, 100), transaction(2, 100, 51_200)]),
      group("b", [transaction(3, 250, 500)]),
    ];
    await expect(
      buildFeeSpectrumCooperatively(
        groups,
        "vsize",
        { batchSize: 1, yieldBetweenBatches: () => Promise.resolve() },
        2,
      ),
    ).resolves.toEqual(buildFeeSpectrum(groups, "vsize", 2));
  });
});
