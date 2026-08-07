import { describe, expect, it } from "vitest";

import {
  DEFAULT_FILTERS,
  filterTransactions,
  filterTransactionsCooperatively,
} from "./filters";
import { mempoolTransaction } from "./test-fixtures";
import type { MempoolTransaction } from "./types";

const OBSERVED_AT_MS = 1_700_000_000_000;
const HOUR_MS = 60 * 60_000;

const transaction = (
  value: number,
  overrides: Partial<MempoolTransaction> = {},
): MempoolTransaction =>
  mempoolTransaction(value, {
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
    ).toBe(transactions);
    expect(
      Array.from(
        filterTransactions(transactions, DEFAULT_FILTERS, OBSERVED_AT_MS),
      ),
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
      Array.from(
        filterTransactions(
          [future],
          { ...DEFAULT_FILTERS, maximumAgeMs: 0 },
          OBSERVED_AT_MS,
        ),
      ),
    ).toEqual([future]);
  });

  it("cooperatively preserves the complete filter result", async () => {
    const transactions = Array.from({ length: 257 }, (_, index) =>
      transaction(index + 1, {
        fee_sats: 100 + index,
        vsize: 100 + (index % 100),
        entered_at_ms: OBSERVED_AT_MS - (index % 3) * HOUR_MS,
      }),
    );
    const filters = {
      minimumFeeRate: 1,
      maximumAgeMs: HOUR_MS,
      minimumVsize: 150,
    };
    const expected = filterTransactions(transactions, filters, OBSERVED_AT_MS);

    const actual = await filterTransactionsCooperatively(
      transactions,
      filters,
      OBSERVED_AT_MS,
      { batchSize: 32 },
    );

    expect(actual.map(({ txid }) => txid)).toEqual(
      expected.map(({ txid }) => txid),
    );
  });

  it("preserves default-array identity without scheduling work", async () => {
    const transactions = [transaction(1), transaction(2)];
    let yielded = false;

    await expect(
      filterTransactionsCooperatively(
        transactions,
        DEFAULT_FILTERS,
        OBSERVED_AT_MS,
        { yieldBetweenBatches: () => void (yielded = true) },
      ),
    ).resolves.toBe(transactions);
    expect(yielded).toBe(false);
  });
});
