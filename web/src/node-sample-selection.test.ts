import { describe, expect, it } from "vitest";

import { classifierBucketForTransaction } from "./classifier-terrain";
import {
  nodeSampleSelectionContains,
  resolveNodeSampleSelection,
  type NodeSampleSelection,
} from "./node-sample-selection";
import { mempoolTransaction } from "./test-fixtures";
import { terrainRegionKey } from "./terrain";
import type { ClassificationResult, ClassifierDescriptor } from "./types";

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
  ],
};

const result = (labels: string[]): ClassificationResult => ({
  classifier_id: descriptor.id,
  state: "complete",
  primary_label: labels[0] ?? null,
  labels,
  missing_facts: [],
  evidence: null,
});

describe("nodeSampleSelectionContains", () => {
  it("resolves terrain selections before the marginal label fallback", () => {
    expect(
      resolveNodeSampleSelection({
        terrain: true,
        bip110: true,
        inspector: { kind: "rule", rule: "element_size" },
        descriptor,
        bucketKey: "complete:0",
        classifierId: descriptor.id,
        label: "alpha",
      }),
    ).toEqual({ kind: "bip110-rule", rule: "element_size" });
    expect(
      resolveNodeSampleSelection({
        terrain: true,
        bip110: false,
        inspector: { kind: "rule", rule: "element_size" },
        descriptor,
        bucketKey: "complete:0",
        classifierId: descriptor.id,
        label: "alpha",
      }),
    ).toEqual({
      kind: "classifier-bucket",
      descriptor,
      bucketKey: "complete:0",
    });
    expect(
      resolveNodeSampleSelection({
        terrain: false,
        bip110: false,
        inspector: { kind: "rule", rule: "element_size" },
        descriptor,
        bucketKey: null,
        classifierId: descriptor.id,
        label: "alpha",
      }),
    ).toEqual({
      kind: "classifier-label",
      classifierId: descriptor.id,
      label: "alpha",
    });
  });

  it("matches a BIP-110 transaction only to its canonical terrain region", () => {
    const transaction = mempoolTransaction(1, {
      bip110: {
        status: "violating",
        primary_rule: "element_size",
        violated_rules: ["element_size"],
        unknown_rules: ["output_size"],
      },
    });
    const selection: NodeSampleSelection = {
      kind: "bip110-region",
      regionKey: terrainRegionKey(transaction),
    };

    expect(nodeSampleSelectionContains(transaction, selection)).toBe(true);
    expect(
      nodeSampleSelectionContains(transaction, {
        kind: "bip110-region",
        regionKey: "compatible",
      }),
    ).toBe(false);
  });

  it("matches BIP-110 rules from the proven violated rule set", () => {
    const transaction = mempoolTransaction(1, {
      bip110: {
        status: "violating",
        primary_rule: "element_size",
        violated_rules: ["element_size", "output_size"],
        unknown_rules: ["tapscript_op_if"],
      },
    });

    expect(
      nodeSampleSelectionContains(transaction, {
        kind: "bip110-rule",
        rule: "output_size",
      }),
    ).toBe(true);
    expect(
      nodeSampleSelectionContains(transaction, {
        kind: "bip110-rule",
        rule: "tapscript_op_if",
      }),
    ).toBe(false);
  });

  it("matches one exact classifier bucket without scanning its population", () => {
    const transaction = mempoolTransaction(1, {
      classifications: [result(["alpha"])],
    });
    const bucketKey = classifierBucketForTransaction(
      transaction,
      descriptor,
    ).key;

    expect(
      nodeSampleSelectionContains(transaction, {
        kind: "classifier-bucket",
        descriptor,
        bucketKey,
      }),
    ).toBe(true);
    expect(
      nodeSampleSelectionContains(transaction, {
        kind: "classifier-bucket",
        descriptor,
        bucketKey: "complete:1",
      }),
    ).toBe(false);
  });

  it("matches marginal labels within the selected classifier only", () => {
    const transaction = mempoolTransaction(1, {
      classifications: [result(["alpha"])],
    });

    expect(
      nodeSampleSelectionContains(transaction, {
        kind: "classifier-label",
        classifierId: descriptor.id,
        label: "alpha",
      }),
    ).toBe(true);
    expect(
      nodeSampleSelectionContains(transaction, {
        kind: "classifier-label",
        classifierId: descriptor.id,
        label: "beta",
      }),
    ).toBe(false);
    expect(
      nodeSampleSelectionContains(transaction, {
        kind: "classifier-label",
        classifierId: "other_classifier",
        label: "alpha",
      }),
    ).toBe(false);
  });
});
