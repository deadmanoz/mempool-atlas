import { describe, expect, it } from "vitest";

import {
  bucketTerrainRegionCanShowLabel,
  createBucketTerrainLayout,
} from "./bucket-terrain";
import {
  classifierBucketContainsLabel,
  classifierBucketDescription,
  classifierBucketForTransaction,
  classifierBucketIsSummary,
  classifierBucketLabel,
  classifierBucketPopulation,
  classifierBuckets,
  classifierTerrainGroups,
  classifierTerrainTotals,
} from "./classifier-terrain";
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

const propertyDescriptor: ClassifierDescriptor = {
  id: "transaction_properties",
  version: "1",
  title: "Transaction properties",
  methodology: "exact",
  semantics: "multi_label",
  required_facts: ["raw_transaction", "input_script_pubkeys"],
  labels: [
    { key: "version_1", label: "Version 1", description: "Version 1." },
    { key: "version_2", label: "Version 2", description: "Version 2." },
    { key: "signals_rbf", label: "Signals RBF", description: "RBF." },
    { key: "has_witness", label: "Has witness", description: "Witness." },
    { key: "p2pkh", label: "P2PKH", description: "P2PKH." },
    { key: "p2sh", label: "P2SH", description: "P2SH." },
    { key: "p2wpkh", label: "P2WPKH", description: "P2WPKH." },
    { key: "p2wsh", label: "P2WSH", description: "P2WSH." },
    { key: "p2tr", label: "P2TR", description: "P2TR." },
    { key: "p2a", label: "P2A", description: "P2A." },
    { key: "op_return", label: "OP_RETURN", description: "OP_RETURN." },
    {
      key: "unknown_script",
      label: "Other script",
      description: "Other script.",
    },
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

const propertyResult = (
  state: "complete" | "partial",
  labels: string[],
): ClassificationResult => ({
  classifier_id: propertyDescriptor.id,
  state,
  primary_label: null,
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
    fee_sats: vsize * 2,
    classifications: classification === null ? [] : [classification],
  });

describe("classifier terrain", () => {
  it("uses descriptor order for canonical exact label-set keys", () => {
    const left = transaction(1, 100, result("complete", ["gamma", "alpha"]));
    const right = transaction(2, 100, result("complete", ["alpha", "gamma"]));

    expect(classifierBucketForTransaction(left, descriptor)).toEqual({
      key: "complete:0.2",
      state: "complete",
      labelKeys: ["alpha", "gamma"],
    });
    expect(classifierBucketForTransaction(right, descriptor)).toEqual(
      classifierBucketForTransaction(left, descriptor),
    );
  });

  it("summarizes transaction properties into broad script-profile groups", () => {
    const transactions = [
      transaction(
        1,
        100,
        propertyResult("complete", ["version_1", "signals_rbf", "p2pkh"]),
      ),
      transaction(
        2,
        110,
        propertyResult("complete", ["version_2", "p2sh", "op_return"]),
      ),
      transaction(
        3,
        120,
        propertyResult("complete", ["version_2", "has_witness", "p2wpkh"]),
      ),
      transaction(
        4,
        130,
        propertyResult("complete", ["version_2", "p2wpkh", "p2tr"]),
      ),
      transaction(5, 140, propertyResult("partial", ["version_2", "p2a"])),
      transaction(6, 150, null),
    ];

    expect(
      classifierBuckets(transactions, propertyDescriptor).map(
        ({ key, count }) => [key, count],
      ),
    ).toEqual([
      ["complete:profile_legacy", 2],
      ["complete:profile_segwit_v0", 1],
      ["complete:profile_mixed", 1],
      ["partial:profile_other", 1],
      ["unavailable", 1],
    ]);
  });

  it("keeps exact property labels available inside summary groups", () => {
    const transactions = [
      transaction(
        1,
        100,
        propertyResult("complete", ["version_1", "signals_rbf", "p2pkh"]),
      ),
      transaction(
        2,
        110,
        propertyResult("complete", ["version_2", "p2sh", "op_return"]),
      ),
    ];
    const bucket = classifierBuckets(transactions, propertyDescriptor)[0];

    expect(bucket).toBeDefined();
    expect(classifierBucketIsSummary(bucket!)).toBe(true);
    expect(classifierBucketLabel(propertyDescriptor, bucket!)).toBe(
      "Legacy / P2SH",
    );
    expect(classifierBucketDescription(bucket!)).toContain("legacy or P2SH");
    expect(classifierBucketContainsLabel(bucket!, "signals_rbf")).toBe(true);
    expect(classifierBucketContainsLabel(bucket!, "op_return")).toBe(true);
    expect(classifierBucketContainsLabel(bucket!, "p2tr")).toBe(false);
  });

  it("separates complete, partial, and unavailable results", () => {
    const transactions = [
      transaction(1, 100, result("complete", ["alpha"])),
      transaction(2, 200, result("complete", ["alpha", "gamma"])),
      transaction(3, 300, result("partial", ["alpha"])),
      transaction(4, 400, null),
    ];

    expect(classifierTerrainTotals(transactions, descriptor)).toEqual({
      complete: 2,
      partial: 1,
      unavailable: 1,
    });
    expect(
      classifierBuckets(transactions, descriptor).map(({ key, count }) => [
        key,
        count,
      ]),
    ).toEqual([
      ["complete:0", 1],
      ["complete:0.2", 1],
      ["partial:0", 1],
      ["unavailable", 1],
    ]);
  });

  it("places every transaction in exactly one observed region", () => {
    const transactions = [
      transaction(1, 100, result("complete", ["alpha"])),
      transaction(2, 200, result("complete", ["alpha"])),
      transaction(3, 300, result("partial", ["beta"])),
      transaction(4, 400, null),
    ];
    const groups = classifierTerrainGroups(transactions, descriptor);
    const members = groups.flatMap(({ regions }) =>
      regions.flatMap(({ transactions: entries }) =>
        entries.map(({ txid }) => txid),
      ),
    );

    expect(members).toHaveLength(transactions.length);
    expect(new Set(members)).toEqual(
      new Set(transactions.map(({ txid }) => txid)),
    );
    expect(groups.map(({ key }) => key)).toEqual([
      "complete",
      "partial",
      "unavailable",
    ]);
  });

  it("reuses the classified bucket model for repeated interactions", () => {
    const transactions = [
      transaction(1, 100, result("complete", ["alpha"])),
      transaction(2, 200, result("complete", ["beta"])),
      transaction(3, 300, null),
    ];
    const buckets = classifierBuckets(transactions, descriptor);
    const groups = classifierTerrainGroups(transactions, descriptor);

    expect(classifierBuckets(transactions, descriptor)).toBe(buckets);
    expect(classifierTerrainGroups(transactions, descriptor)).toBe(groups);
  });

  it("keeps coverage area proportional to the selected metric", () => {
    const transactions = [
      ...Array.from({ length: 75 }, (_, index) =>
        transaction(index + 1, 100, result("complete", ["alpha"])),
      ),
      ...Array.from({ length: 25 }, (_, index) =>
        transaction(index + 76, 100, null),
      ),
    ];
    const layout = createBucketTerrainLayout(
      classifierTerrainGroups(transactions, descriptor),
      1_000,
      600,
      "count",
    );
    const complete = layout.sections.find(({ key }) => key === "complete");
    const unavailable = layout.sections.find(
      ({ key }) => key === "unavailable",
    );

    expect(complete).toBeDefined();
    expect(unavailable).toBeDefined();
    expect(
      (complete?.rect.width ?? 0) / (unavailable?.rect.width ?? 1),
    ).toBeCloseTo(3, 1);
  });

  it("avoids a duplicate nested bucket for singleton coverage sections", () => {
    const transactions = [
      transaction(1, 100, result("complete", ["alpha"])),
      transaction(2, 200, result("complete", ["beta"])),
      transaction(3, 300, result("partial", ["alpha"])),
      transaction(4, 400, null),
    ];
    const groups = classifierTerrainGroups(transactions, descriptor);

    expect(groups.find(({ key }) => key === "complete")?.nested).toBe(true);
    expect(groups.find(({ key }) => key === "partial")?.nested).toBe(false);
    expect(groups.find(({ key }) => key === "unavailable")?.nested).toBe(false);
  });

  it("only exposes labels when a terrain region can display them cleanly", () => {
    expect(
      bucketTerrainRegionCanShowLabel({
        rect: { x: 0, y: 0, width: 180, height: 80 },
        labelHeight: 30,
      }),
    ).toBe(true);
    expect(
      bucketTerrainRegionCanShowLabel({
        rect: { x: 0, y: 0, width: 100, height: 80 },
        labelHeight: 30,
      }),
    ).toBe(false);
    expect(
      bucketTerrainRegionCanShowLabel({
        rect: { x: 0, y: 0, width: 180, height: 40 },
        labelHeight: 20,
      }),
    ).toBe(false);
  });

  it("keeps marginal label membership independent of exact buckets", () => {
    const signature = classifierBucketForTransaction(
      transaction(1, 100, result("complete", ["alpha", "gamma"])),
      descriptor,
    );

    expect(classifierBucketContainsLabel(signature, "alpha")).toBe(true);
    expect(classifierBucketContainsLabel(signature, "gamma")).toBe(true);
    expect(classifierBucketContainsLabel(signature, "beta")).toBe(false);
  });

  it("sorts exact-bucket samples by virtual size then txid", () => {
    const transactions = [
      transaction(2, 200, result("complete", ["alpha"])),
      transaction(3, 300, result("complete", ["alpha"])),
      transaction(1, 200, result("complete", ["alpha"])),
    ];
    const population = classifierBucketPopulation(
      transactions,
      descriptor,
      "complete:0",
    );

    expect(population?.transactions.map(({ txid }) => txid)).toEqual([
      transactions[1]?.txid,
      transactions[2]?.txid,
      transactions[0]?.txid,
    ]);
    expect(population?.totalShare).toBe(1);
  });
});
