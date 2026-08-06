import { describe, expect, it } from "vitest";

import {
  FEE_RATE_DOMAIN,
  VSIZE_DOMAIN,
  buildFeeSpectrum,
  buildJointDensity,
  logDomainPosition,
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
});
