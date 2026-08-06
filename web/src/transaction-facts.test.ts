import { describe, expect, it } from "vitest";

import {
  ancestorFeeRate,
  transactionFactPairs,
  transactionFactSummary,
} from "./transaction-facts";
import { mempoolTransaction } from "./test-fixtures";
import type { MempoolTransaction } from "./types";

const transaction = (
  overrides: Partial<MempoolTransaction> = {},
): MempoolTransaction =>
  mempoolTransaction(0, {
    wtxid: "11".repeat(32),
    weight: 800,
    fee_sats: 400,
    ancestor_count: 2,
    ancestor_vsize: 500,
    ancestor_fee_sats: 1_500,
    replaceable: true,
    structure: {
      input_count: 1,
      output_count: 2,
      op_return_bytes: 80,
      output_sats: 250_000_000,
      witness_bytes: 107,
    },
    ...overrides,
  });

describe("ancestorFeeRate", () => {
  it("divides delta-adjusted ancestor fees by ancestor vsize", () => {
    expect(ancestorFeeRate(transaction())).toBe(3);
  });

  it("returns zero for a zero ancestor vsize", () => {
    expect(ancestorFeeRate(transaction({ ancestor_vsize: 0 }))).toBe(0);
  });

  it("keeps a prioritised-down ancestor fee negative", () => {
    expect(ancestorFeeRate(transaction({ ancestor_fee_sats: -1_500 }))).toBe(
      -3,
    );
  });
});

describe("transactionFactPairs", () => {
  it("lists membership and structure facts with exact copy", () => {
    const pairs = transactionFactPairs(transaction());
    expect(pairs.map(([label]) => label)).toEqual([
      "Weight",
      "Ancestors",
      "Descendants",
      "Replaceable",
      "Shape",
      "Output value",
      "Witness",
      "OP_RETURN bytes",
    ]);
    expect(pairs[1]?.[1]).toBe("2 tx incl. self · 500 vB · 3 sat/vB");
    expect(pairs[3]?.[1]).toBe("Yes, as reported by the source");
    expect(pairs[4]?.[1]).toBe("1 input → 2 outputs");
    expect(pairs[5]?.[1]).toBe("2.5 BTC");
  });

  it("renders a negative ancestor fee rate with its sign", () => {
    const pairs = transactionFactPairs(
      transaction({ ancestor_fee_sats: -1_250 }),
    );
    expect(pairs[1]?.[1]).toBe("2 tx incl. self · 500 vB · -2.5 sat/vB");
  });

  it("marks structure facts as pending when null", () => {
    const pairs = transactionFactPairs(transaction({ structure: null }));
    expect(pairs.at(-1)).toEqual([
      "Structure",
      "Facts arrive with classification",
    ]);
  });

  it("omits the OP_RETURN row when the payload is empty", () => {
    const pairs = transactionFactPairs(
      transaction({
        structure: {
          input_count: 1,
          output_count: 1,
          op_return_bytes: 0,
          output_sats: 1_000,
          witness_bytes: 0,
        },
      }),
    );
    expect(pairs.some(([label]) => label === "OP_RETURN bytes")).toBe(false);
  });
});

describe("transactionFactSummary", () => {
  it("joins the same facts into one compact line", () => {
    const summary = transactionFactSummary(transaction());
    expect(summary).toContain("800 wu");
    expect(summary).toContain("ancestors 2 tx · 500 vB @ 3 sat/vB");
    expect(summary).toContain("replaceable");
    expect(summary).toContain("1 in → 2 out");
    expect(summary).toContain("moves 2.5 BTC");
    expect(summary).toContain("OP_RETURN 80 B");
  });
});
