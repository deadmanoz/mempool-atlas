import { describe, expect, it } from "vitest";

import { DEFAULT_FILTERS, filterTransactions } from "./filters";
import type { MempoolTransaction } from "./types";

const OBSERVED_AT_MS = 1_700_000_000_000;
const HOUR_MS = 60 * 60_000;

const transaction = (
  value: number,
  overrides: Partial<MempoolTransaction> = {},
): MempoolTransaction => ({
  txid: value.toString(16).padStart(64, "0"),
  wtxid: value.toString(16).padStart(64, "0"),
  vsize: 200,
  fee_sats: 2_000,
  entered_at_ms: OBSERVED_AT_MS - HOUR_MS,
  bip110: {
    status: "compatible",
    primary_rule: null,
    violated_rules: [],
    unknown_rules: [],
  },
  ...overrides,
});

describe("filterTransactions", () => {
  it("keeps the complete snapshot by default", () => {
    const transactions = [transaction(1), transaction(2)];

    expect(
      filterTransactions(transactions, DEFAULT_FILTERS, OBSERVED_AT_MS),
    ).toEqual(transactions);
  });

  it("combines fee-rate, age, and vsize filters", () => {
    const transactions = [
      transaction(1),
      transaction(2, { fee_sats: 1_000 }),
      transaction(3, { entered_at_ms: OBSERVED_AT_MS - 2 * HOUR_MS }),
      transaction(4, { vsize: 100, fee_sats: 1_000 }),
    ];

    expect(
      filterTransactions(
        transactions,
        {
          minimumFeeRate: 8,
          maximumAgeMs: HOUR_MS,
          minimumVsize: 150,
        },
        OBSERVED_AT_MS,
      ).map(({ txid }) => txid),
    ).toEqual([transaction(1).txid]);
  });

  it("treats future node entry times as zero age", () => {
    const future = transaction(1, {
      entered_at_ms: OBSERVED_AT_MS + HOUR_MS,
    });

    expect(
      filterTransactions(
        [future],
        { ...DEFAULT_FILTERS, maximumAgeMs: 0 },
        OBSERVED_AT_MS,
      ),
    ).toEqual([future]);
  });
});
