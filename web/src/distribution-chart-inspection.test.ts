import { describe, expect, it } from "vitest";

import {
  JOINT_CHART_GAP_CSS_PX,
  JOINT_CHART_MARGIN_CSS_PX,
  buildJointDensityCellInspection,
  buildSpectrumBinInspection,
  formatLogBinRange,
  hitTestJointChartCss,
  hitTestSpectrumX,
} from "./distribution-chart-inspection";
import {
  type FeeSpectrum,
  type JointDensity,
  type LogDomainBinBounds,
  FEE_RATE_DOMAIN,
  VSIZE_DOMAIN,
} from "./fee-distribution";

const fixed = (value: number): string => value.toFixed(2);
const metric = (value: number): string => `${value} tx`;
const share = (value: number): string => `${(value * 100).toFixed(1)}%`;

describe("formatLogBinRange", () => {
  it.each([
    [{ lowerInclusive: null, upperExclusive: 4 }, "less than 4.00"],
    [{ lowerInclusive: 4, upperExclusive: 16 }, "4.00 to less than 16.00"],
    [{ lowerInclusive: 16, upperExclusive: null }, "16.00 or more"],
    [{ lowerInclusive: null, upperExclusive: null }, "all values"],
  ] satisfies ReadonlyArray<[LogDomainBinBounds, string]>)(
    "formats open and half-open bounds %#",
    (bounds, expected) => {
      expect(formatLogBinRange(bounds, fixed)).toBe(expected);
    },
  );
});

describe("buildSpectrumBinInspection", () => {
  const spectrum: FeeSpectrum = {
    bins: [
      {
        total: 25,
        segments: [
          { key: "alpha", label: "Alpha", color: "#111", weight: 10 },
          { key: "beta/value", label: "Beta", color: "#222", weight: 15 },
        ],
      },
      { total: 75, segments: [] },
    ],
    maxBin: 75,
    totalWeight: 100,
  };

  it("returns stable metadata, exact range semantics, and series shares", () => {
    const inspection = buildSpectrumBinInspection({
      spectrum,
      domain: FEE_RATE_DOMAIN,
      bin: 0,
      keyPrefix: "fee",
      axisLabel: "Fee rate",
      metricLabel: "Transactions",
      formatRangeValue: (value) => `${fixed(value)} sat/vB`,
      formatMetric: metric,
      formatShare: share,
    });

    expect(inspection).toMatchObject({
      kind: "spectrum-bin",
      key: "fee:bin:0",
      bin: 0,
      rangeLabel: "less than 22.63 sat/vB",
      title: "Fee rate: less than 22.63 sat/vB",
      metric: {
        total: 25,
        totalLabel: "25 tx",
        share: 0.25,
        shareLabel: "25.0%",
      },
    });
    expect(inspection.detail).toBe(
      "Transactions: 25 tx (25.0% of population) · Alpha: 10 tx (40.0% of bin) · Beta: 15 tx (60.0% of bin)",
    );
    expect(inspection.series).toEqual([
      {
        key: "fee:bin:0:series:alpha",
        label: "Alpha",
        color: "#111",
        metric: {
          total: 10,
          totalLabel: "10 tx",
          share: 0.4,
          shareLabel: "40.0%",
        },
        populationShare: 0.1,
        populationShareLabel: "10.0%",
      },
      {
        key: "fee:bin:0:series:beta%2Fvalue",
        label: "Beta",
        color: "#222",
        metric: {
          total: 15,
          totalLabel: "15 tx",
          share: 0.6,
          shareLabel: "60.0%",
        },
        populationShare: 0.15,
        populationShareLabel: "15.0%",
      },
    ]);
  });

  it("formats the last clamped bin with an open upper bound", () => {
    const inspection = buildSpectrumBinInspection({
      spectrum,
      domain: FEE_RATE_DOMAIN,
      bin: 1,
      keyPrefix: "fee",
      axisLabel: "Fee rate",
      metricLabel: "Transactions",
      formatRangeValue: fixed,
      formatMetric: metric,
      formatShare: share,
    });

    expect(inspection.bounds.upperExclusive).toBeNull();
    expect(inspection.rangeLabel).toBe("22.63 or more");
  });

  it("names a subset denominator without claiming the whole population", () => {
    const inspection = buildSpectrumBinInspection({
      spectrum,
      domain: FEE_RATE_DOMAIN,
      bin: 0,
      keyPrefix: "data",
      axisLabel: "Carried bytes",
      metricLabel: "Transactions",
      shareDenominatorLabel: "OP_RETURN carriers",
      formatRangeValue: fixed,
      formatMetric: metric,
      formatShare: share,
    });

    expect(inspection.detail).toContain("25.0% of OP_RETURN carriers");
    expect(inspection.detail).not.toContain("of population");
  });

  it("rejects invalid bins and empty stable-key prefixes", () => {
    expect(() =>
      buildSpectrumBinInspection({
        spectrum,
        domain: FEE_RATE_DOMAIN,
        bin: 2,
        keyPrefix: "fee",
        axisLabel: "Fee rate",
        metricLabel: "Transactions",
        formatRangeValue: fixed,
        formatMetric: metric,
      }),
    ).toThrow(RangeError);
    expect(() =>
      buildSpectrumBinInspection({
        spectrum,
        domain: FEE_RATE_DOMAIN,
        bin: 0,
        keyPrefix: "",
        axisLabel: "Fee rate",
        metricLabel: "Transactions",
        formatRangeValue: fixed,
        formatMetric: metric,
      }),
    ).toThrow(RangeError);
  });
});

describe("buildJointDensityCellInspection", () => {
  const density: JointDensity = {
    columns: 2,
    rows: 2,
    cells: [5, 15, 20, 60],
    columnTotals: [25, 75],
    rowTotals: [20, 80],
    maxCell: 60,
    totalWeight: 100,
  };

  it("describes exact x/y bounds, stable cell identity, and population share", () => {
    const inspection = buildJointDensityCellInspection({
      density,
      xDomain: FEE_RATE_DOMAIN,
      yDomain: VSIZE_DOMAIN,
      column: 1,
      row: 0,
      keyPrefix: "fee-size",
      xAxisLabel: "Fee rate",
      yAxisLabel: "Virtual size",
      metricLabel: "Transactions",
      formatXValue: (value) => `${fixed(value)} sat/vB`,
      formatYValue: (value) => `${fixed(value)} vB`,
      formatMetric: metric,
      formatShare: share,
    });

    expect(inspection).toMatchObject({
      kind: "joint-cell",
      key: "fee-size:cell:1:0",
      column: 1,
      row: 0,
      xRangeLabel: "22.63 sat/vB or more",
      yRangeLabel: "less than 2896.31 vB",
      metric: {
        total: 15,
        totalLabel: "15 tx",
        share: 0.15,
        shareLabel: "15.0%",
      },
    });
    expect(inspection.title).toBe(
      "Fee rate: 22.63 sat/vB or more · Virtual size: less than 2896.31 vB",
    );
    expect(inspection.detail).toBe("Transactions: 15 tx (15.0% of population)");
  });

  it("names the structure-fact subset used by a density", () => {
    const inspection = buildJointDensityCellInspection({
      density,
      xDomain: FEE_RATE_DOMAIN,
      yDomain: VSIZE_DOMAIN,
      column: 1,
      row: 0,
      keyPrefix: "complexity",
      xAxisLabel: "Inputs",
      yAxisLabel: "Outputs",
      metricLabel: "Transactions",
      shareDenominatorLabel: "transactions with structure facts",
      formatXValue: fixed,
      formatYValue: fixed,
      formatMetric: metric,
      formatShare: share,
    });

    expect(inspection.detail).toContain(
      "15.0% of transactions with structure facts",
    );
  });
});

describe("hitTestSpectrumX", () => {
  it("maps the full chart width to bounded bins, including thin regions", () => {
    expect(hitTestSpectrumX({ x: 0, width: 220, binCount: 22 })).toBe(0);
    expect(hitTestSpectrumX({ x: 9.99, width: 220, binCount: 22 })).toBe(0);
    expect(hitTestSpectrumX({ x: 10, width: 220, binCount: 22 })).toBe(1);
    expect(hitTestSpectrumX({ x: 219.999, width: 220, binCount: 22 })).toBe(21);
  });

  it("returns null outside the chart or for invalid geometry", () => {
    for (const input of [
      { x: -1, width: 220, binCount: 22 },
      { x: 220, width: 220, binCount: 22 },
      { x: 0, width: 0, binCount: 22 },
      { x: 0, width: 220, binCount: 0 },
      { x: Number.NaN, width: 220, binCount: 22 },
    ]) {
      expect(hitTestSpectrumX(input)).toBeNull();
    }
  });
});

describe("hitTestJointChartCss", () => {
  const width = 249;
  const input = { width, columns: 2, rows: 2 } as const;
  const gridWidth = width - JOINT_CHART_MARGIN_CSS_PX - JOINT_CHART_GAP_CSS_PX;
  const gridTop = JOINT_CHART_MARGIN_CSS_PX + JOINT_CHART_GAP_CSS_PX;
  const cellSpanY = (gridWidth / input.columns) * 0.82;

  it("distinguishes the grid and reverses visual y into model row order", () => {
    expect(hitTestJointChartCss({ ...input, x: 10, y: gridTop + 1 })).toEqual({
      kind: "grid",
      column: 0,
      row: 1,
    });
    expect(
      hitTestJointChartCss({ ...input, x: 150, y: gridTop + cellSpanY + 1 }),
    ).toEqual({ kind: "grid", column: 1, row: 0 });
  });

  it("distinguishes top and right marginals from their separating gaps", () => {
    expect(hitTestJointChartCss({ ...input, x: 150, y: 10 })).toEqual({
      kind: "x-marginal",
      column: 1,
    });
    expect(
      hitTestJointChartCss({ ...input, x: gridWidth + 4, y: gridTop + 1 }),
    ).toEqual({ kind: "y-marginal", row: 1 });
    expect(
      hitTestJointChartCss({
        ...input,
        x: 10,
        y: JOINT_CHART_MARGIN_CSS_PX + 1,
      }),
    ).toBeNull();
    expect(
      hitTestJointChartCss({
        ...input,
        x: gridWidth + 1,
        y: gridTop + 1,
      }),
    ).toBeNull();
  });

  it("returns null outside the computed canvas regions and for invalid geometry", () => {
    const gridBottom = gridTop + cellSpanY * input.rows;
    for (const candidate of [
      { ...input, x: -1, y: 0 },
      { ...input, x: width, y: 0 },
      { ...input, x: 0, y: gridBottom },
      { ...input, x: 0, y: -1 },
      { ...input, x: 0, y: 0, columns: 0 },
      { ...input, x: 0, y: 0, width: 20 },
    ]) {
      expect(hitTestJointChartCss(candidate)).toBeNull();
    }
  });
});
