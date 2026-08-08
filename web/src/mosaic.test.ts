import { describe, expect, it } from "vitest";

import {
  AGE_BANDS,
  buildAgeMosaic,
  buildAgeMosaicCooperatively,
} from "./mosaic";
import { mempoolTransaction } from "./test-fixtures";
import type { MempoolTransaction } from "./types";

const OBSERVED = 1_700_000_000_000;

const transaction = (
  suffix: number,
  vsize: number,
  ageMs: number,
): MempoolTransaction =>
  mempoolTransaction(suffix, {
    wtxid: (suffix + 100).toString(16).padStart(64, "0"),
    vsize,
    fee_sats: vsize,
    entered_at_ms: OBSERVED - ageMs,
  });

const group = (key: string, transactions: MempoolTransaction[]) => ({
  key,
  label: key,
  color: "#fff",
  transactions,
});

describe("buildAgeMosaic", () => {
  it("splits each column into observation-relative age bands", () => {
    const mosaic = buildAgeMosaic(
      [
        group("a", [
          transaction(1, 100, 60_000),
          transaction(2, 100, 7_200_000),
        ]),
      ],
      OBSERVED,
      "count",
    );
    const column = mosaic.columns[0];
    expect(column?.share).toBe(1);
    expect(column?.cells.map(({ bandKey }) => bandKey)).toEqual([
      "under_10m",
      "under_6h",
    ]);
    expect(column?.cells.every(({ share }) => share === 0.5)).toBe(true);
  });

  it("sizes column shares by the requested metric", () => {
    const mosaic = buildAgeMosaic(
      [
        group("small", [transaction(1, 100, 60_000)]),
        group("large", [transaction(2, 300, 60_000)]),
      ],
      OBSERVED,
      "vsize",
    );
    expect(mosaic.columns[0]?.share).toBeCloseTo(0.25, 10);
    expect(mosaic.columns[1]?.share).toBeCloseTo(0.75, 10);
    expect(mosaic.totalWeight).toBe(400);
  });

  it("drops empty groups and clamps future entries to the youngest band", () => {
    const mosaic = buildAgeMosaic(
      [group("empty", []), group("future", [transaction(1, 100, -10_000)])],
      OBSERVED,
      "count",
    );
    expect(mosaic.columns).toHaveLength(1);
    expect(mosaic.columns[0]?.cells[0]?.bandKey).toBe(AGE_BANDS[0]?.key);
  });

  it("places very old transactions into the open last band", () => {
    const mosaic = buildAgeMosaic(
      [group("old", [transaction(1, 100, 500_000_000)])],
      OBSERVED,
      "count",
    );
    expect(mosaic.columns[0]?.cells[0]?.bandKey).toBe("over_24h");
  });

  it("labels the inclusive 24-hour boundary precisely", () => {
    const mosaic = buildAgeMosaic(
      [group("boundary", [transaction(1, 100, 86_400_000)])],
      OBSERVED,
      "count",
    );
    expect(mosaic.columns[0]?.cells[0]).toMatchObject({
      bandKey: "over_24h",
      bandLabel: "≥ 24 h",
    });
    expect(AGE_BANDS.at(-1)?.label).toBe("≥ 24 h");
  });

  it("matches the cooperative mosaic builder exactly", async () => {
    const groups = [
      group("a", [transaction(1, 100, 60_000), transaction(2, 200, 7_200_000)]),
      group("b", [transaction(3, 300, 500_000_000)]),
    ];
    await expect(
      buildAgeMosaicCooperatively(groups, OBSERVED, "vsize", {
        batchSize: 1,
        yieldBetweenBatches: () => Promise.resolve(),
      }),
    ).resolves.toEqual(buildAgeMosaic(groups, OBSERVED, "vsize"));
  });
});
