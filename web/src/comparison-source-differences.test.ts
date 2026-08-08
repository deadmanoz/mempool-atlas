import { describe, expect, it } from "vitest";

import { sourceDifferenceNavigatorDescription } from "./comparison-source-differences";
import type { ComparedTransaction } from "./comparison-model";
import { mempoolTransaction, txid } from "./test-fixtures";

describe("sourceDifferenceNavigatorDescription", () => {
  it("keeps loading and one-sided membership distinct", () => {
    const transaction = mempoolTransaction(1);
    expect(
      sourceDifferenceNavigatorDescription({
        txid: transaction.txid,
        left: transaction,
        right: transaction,
        witness_relation: "loading",
      }),
    ).toBe("Witness variants are still loading");
    expect(
      sourceDifferenceNavigatorDescription({
        txid: transaction.txid,
        left: transaction,
        right: null,
        witness_relation: "one_sided",
      }),
    ).toBe("Observed in one snapshot");
  });

  it("names every overlapping source-local difference", () => {
    const left = mempoolTransaction(1, {
      wtxid: txid(10),
      ancestor_vsize: 200,
      ancestor_fee_sats: 2_000,
      replaceable: false,
    });
    const right = mempoolTransaction(1, {
      wtxid: txid(11),
      ancestor_vsize: 300,
      ancestor_fee_sats: 4_000,
      replaceable: true,
    });
    const entry: ComparedTransaction = {
      txid: left.txid,
      left,
      right,
      witness_relation: "different",
    };

    expect(sourceDifferenceNavigatorDescription(entry)).toBe(
      "Different witness variant, ancestor package, and replaceability",
    );
  });

  it("states when the compared source-local facts match", () => {
    const transaction = mempoolTransaction(1);
    expect(
      sourceDifferenceNavigatorDescription({
        txid: transaction.txid,
        left: transaction,
        right: transaction,
        witness_relation: "same",
      }),
    ).toBe("Same source-local facts");
  });
});
