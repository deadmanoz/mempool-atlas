import { describe, expect, it } from "vitest";

import {
  classificationPopulation,
  classificationResult,
} from "./classification-view";
import { mempoolTransaction } from "./test-fixtures";
import type { MempoolTransaction } from "./types";

const transaction = (
  value: number,
  labels: string[],
  vsize = 100,
): MempoolTransaction =>
  mempoolTransaction(value, {
    vsize,
    fee_sats: vsize,
    classifications: [
      {
        classifier_id: "transaction_properties",
        state: "complete",
        primary_label: null,
        labels,
        missing_facts: [],
        evidence: null,
      },
    ],
  });

describe("classification view model", () => {
  it("finds one classifier without interpreting another taxonomy", () => {
    expect(
      classificationResult(transaction(1, ["p2tr"]), "transaction_properties")
        ?.labels,
    ).toEqual(["p2tr"]);
    expect(
      classificationResult(transaction(1, ["p2tr"]), "data_protocols"),
    ).toBeNull();
  });

  it("keeps marginal multi-label populations overlapping", () => {
    const transactions = [
      transaction(1, ["p2tr", "signals_rbf"], 300),
      transaction(2, ["p2tr"], 200),
      transaction(3, ["p2wpkh", "signals_rbf"], 100),
    ];
    const p2tr = classificationPopulation(
      transactions,
      "transaction_properties",
      "p2tr",
    );
    expect(p2tr).toMatchObject({ count: 2, vsize: 500, totalShare: 2 / 3 });
    expect(p2tr.transactions.map(({ txid }) => txid)).toEqual([
      transactions[0]?.txid,
      transactions[1]?.txid,
    ]);
    expect(
      classificationPopulation(
        transactions,
        "transaction_properties",
        "signals_rbf",
      ).count,
    ).toBe(2);
  });
});
