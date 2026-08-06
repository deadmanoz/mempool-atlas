import { describe, expect, it } from "vitest";

import { panelBucketGroups } from "./panel-groups";
import { mempoolTransaction, txid } from "./test-fixtures";
import type {
  ClassificationResult,
  ClassifierDescriptor,
  MempoolTransaction,
} from "./types";

const descriptor: ClassifierDescriptor = {
  id: "example_classifier",
  version: "1",
  title: "Example classifier",
  methodology: "exact",
  semantics: "multi_label",
  required_facts: ["raw_transaction"],
  labels: [
    { key: "alpha", label: "Alpha", description: "Alpha label." },
    { key: "beta", label: "Beta", description: "Beta label." },
    { key: "gamma", label: "Gamma", description: "Gamma label." },
  ],
};

const result = (
  state: "complete" | "partial",
  labels: string[],
): ClassificationResult => ({
  classifier_id: descriptor.id,
  state,
  primary_label: labels[0] ?? null,
  labels,
  missing_facts: state === "partial" ? ["input_script_pubkeys"] : [],
  evidence: null,
});

const transaction = (
  suffix: number,
  vsize: number,
  classification: ClassificationResult | null,
): MempoolTransaction =>
  mempoolTransaction(suffix, {
    wtxid: txid(suffix + 100),
    vsize,
    fee_sats: vsize,
    classifications: classification === null ? [] : [classification],
  });

describe("panelBucketGroups", () => {
  it("keeps the largest buckets and aggregates the remainder", () => {
    const transactions = [
      transaction(1, 500, result("complete", ["alpha"])),
      transaction(2, 400, result("complete", ["beta"])),
      transaction(3, 10, result("complete", ["gamma"])),
      transaction(4, 20, result("partial", ["alpha"])),
    ];
    const groups = panelBucketGroups(transactions, descriptor, "vsize", 2);
    expect(groups.map(({ label }) => label)).toEqual([
      "Alpha",
      "Beta",
      "2 more buckets",
    ]);
    expect(groups.at(-1)?.bucketKey).toBeNull();
    expect(groups.at(-1)?.transactions).toHaveLength(2);
    const covered = groups.reduce(
      (total, group) => total + group.transactions.length,
      0,
    );
    expect(covered).toBe(transactions.length);
  });

  it("suffixes partial buckets and keeps their exact bucket key", () => {
    const groups = panelBucketGroups(
      [transaction(1, 100, result("partial", ["alpha"]))],
      descriptor,
      "count",
    );
    expect(groups[0]?.label).toBe("Alpha · partial");
    expect(groups[0]?.bucketKey).toBe("partial:0");
  });

  it("falls back to one all-transactions group without a descriptor", () => {
    const groups = panelBucketGroups(
      [transaction(1, 100, null)],
      null,
      "count",
    );
    expect(groups).toHaveLength(1);
    expect(groups[0]?.key).toBe("all");
    expect(groups[0]?.bucketKey).toBeNull();
  });
});
