import { describe, expect, it } from "vitest";

import { mempoolTransaction } from "./test-fixtures";
import {
  concatenateTransactionViews,
  filterTransactionView,
  filterTransactionViewCooperatively,
  sortTransactionView,
  sortTransactionViewByVsize,
  sortTransactionViewByVsizeCooperatively,
  transactionIndexView,
} from "./transaction-view";

describe("transaction index views", () => {
  it("behaves like an ordinary array without retaining selected objects", () => {
    const transactions = [
      mempoolTransaction(1, { vsize: 100 }),
      mempoolTransaction(2, { vsize: 200 }),
      mempoolTransaction(3, { vsize: 300 }),
    ];
    const view = transactionIndexView(transactions, [2, 0]);

    expect(Array.isArray(view)).toBe(true);
    expect(view).toHaveLength(2);
    expect(view[0]).toBe(transactions[2]);
    expect(view.map(({ vsize }) => vsize)).toEqual([300, 100]);
    expect([...view]).toEqual([transactions[2], transactions[0]]);

    const replacement = mempoolTransaction(4, { vsize: 400 });
    transactions[2] = replacement;
    expect(view[0]).toBe(replacement);
  });

  it("flattens filtered and sorted views onto their original source", () => {
    const transactions = [
      mempoolTransaction(1, { vsize: 100 }),
      mempoolTransaction(2, { vsize: 300 }),
      mempoolTransaction(3, { vsize: 200 }),
      mempoolTransaction(4, { vsize: 400 }),
    ];
    const filtered = filterTransactionView(
      transactions,
      ({ vsize }) => vsize >= 200,
    );
    const sorted = sortTransactionView(
      filtered,
      (left, right) => right.vsize - left.vsize,
    );

    expect(sorted.map(({ vsize }) => vsize)).toEqual([400, 300, 200]);
    transactions[3] = mempoolTransaction(5, { vsize: 450 });
    expect(sorted[0]?.vsize).toBe(450);
  });

  it("cooperatively preserves filtered view semantics", async () => {
    const transactions = Array.from({ length: 257 }, (_, index) =>
      mempoolTransaction(index + 1, { vsize: 100 + (index % 7) }),
    );
    const expected = filterTransactionView(
      transactions,
      ({ vsize }) => vsize >= 104,
    );
    const yields: number[] = [];

    const actual = await filterTransactionViewCooperatively(
      transactions,
      ({ vsize }) => vsize >= 104,
      {
        batchSize: 32,
        yieldBetweenBatches: () => {
          yields.push(1);
        },
      },
    );

    expect(actual.map(({ txid }) => txid)).toEqual(
      expected.map(({ txid }) => txid),
    );
    expect(yields.length).toBeGreaterThan(0);
  });

  it("does not expose an aborted cooperative filter", async () => {
    const controller = new AbortController();
    const transactions = Array.from({ length: 100 }, (_, index) =>
      mempoolTransaction(index + 1),
    );

    await expect(
      filterTransactionViewCooperatively(transactions, () => true, {
        batchSize: 10,
        signal: controller.signal,
        yieldBetweenBatches: () => controller.abort(),
      }),
    ).rejects.toMatchObject({ name: "AbortError" });
  });

  it("sorts the standard terrain order without changing array behavior", () => {
    const transactions = [
      mempoolTransaction(2, { vsize: 300 }),
      mempoolTransaction(3, { vsize: 200 }),
      mempoolTransaction(1, { vsize: 300 }),
    ];

    expect(
      sortTransactionViewByVsize(transactions).map(({ txid }) => txid),
    ).toEqual([
      transactions[2]?.txid,
      transactions[0]?.txid,
      transactions[1]?.txid,
    ]);
  });

  it("cooperatively preserves the standard terrain order", async () => {
    const transactions = Array.from({ length: 257 }, (_, index) =>
      mempoolTransaction(index + 1, { vsize: 100 + ((index * 37) % 211) }),
    );
    const expected = sortTransactionViewByVsize(transactions).map(
      ({ txid }) => txid,
    );
    const yields: number[] = [];

    const actual = await sortTransactionViewByVsizeCooperatively(transactions, {
      timeBudgetMs: Number.MIN_VALUE,
      yieldBetweenBatches: () => {
        yields.push(1);
      },
    });

    expect(actual.map(({ txid }) => txid)).toEqual(expected);
    expect(yields.length).toBeGreaterThan(0);
  });

  it("does not expose an aborted cooperative sort", async () => {
    const controller = new AbortController();
    const transactions = Array.from({ length: 1_000 }, (_, index) =>
      mempoolTransaction(index + 1, { vsize: 1_000 - index }),
    );

    await expect(
      sortTransactionViewByVsizeCooperatively(transactions, {
        timeBudgetMs: Number.MIN_VALUE,
        signal: controller.signal,
        yieldBetweenBatches: () => controller.abort(),
      }),
    ).rejects.toMatchObject({ name: "AbortError" });
  });

  it("concatenates populations that share a source", () => {
    const transactions = [
      mempoolTransaction(1),
      mempoolTransaction(2),
      mempoolTransaction(3),
      mempoolTransaction(4),
    ];
    const even = transactionIndexView(transactions, [1, 3]);
    const odd = transactionIndexView(transactions, [0, 2]);

    expect(
      concatenateTransactionViews(transactions, [even, odd]).map(
        ({ txid }) => txid,
      ),
    ).toEqual([
      transactions[1]?.txid,
      transactions[3]?.txid,
      transactions[0]?.txid,
      transactions[2]?.txid,
    ]);
  });
});
