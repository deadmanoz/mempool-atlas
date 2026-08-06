import { describe, expect, it } from "vitest";

import {
  COMPOSITION_OVERFLOW_KEY,
  buildCompositionBar,
  buildCompositionBars,
  buildEntanglementBars,
} from "./composition";
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
    { key: "delta", label: "Delta", description: "Delta label." },
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

describe("buildCompositionBar", () => {
  it("partitions the whole population into segments summing to one", () => {
    const bar = buildCompositionBar(
      [
        transaction(1, 100, result("complete", ["alpha"])),
        transaction(2, 300, result("complete", ["beta"])),
        transaction(3, 100, null),
      ],
      descriptor,
      "count",
    );
    expect(bar.classifierId).toBe(descriptor.id);
    expect(bar.title).toBe(descriptor.title);
    expect(bar.segments.map(({ label }) => label)).toEqual([
      "Alpha",
      "Beta",
      "Result unavailable",
    ]);
    const total = bar.segments.reduce((sum, { share }) => sum + share, 0);
    expect(total).toBeCloseTo(1, 10);
  });

  it("weights shares by virtual size when requested", () => {
    const bar = buildCompositionBar(
      [
        transaction(1, 100, result("complete", ["alpha"])),
        transaction(2, 300, result("complete", ["beta"])),
      ],
      descriptor,
      "vsize",
    );
    expect(bar.segments[0]?.share).toBeCloseTo(0.25, 10);
    expect(bar.segments[1]?.share).toBeCloseTo(0.75, 10);
  });

  it("rolls the smallest buckets into one labelled overflow segment", () => {
    const bar = buildCompositionBar(
      [
        transaction(1, 500, result("complete", ["alpha"])),
        transaction(2, 400, result("complete", ["beta"])),
        transaction(3, 10, result("complete", ["gamma"])),
        transaction(4, 20, result("complete", ["delta"])),
        transaction(5, 30, result("partial", ["alpha"])),
      ],
      descriptor,
      "vsize",
      3,
    );
    expect(bar.segments).toHaveLength(3);
    expect(bar.segments.map(({ label }) => label)).toEqual([
      "Alpha",
      "Beta",
      "3 more buckets",
    ]);
    const overflow = bar.segments.at(-1);
    expect(overflow?.key).toBe(COMPOSITION_OVERFLOW_KEY);
    expect(overflow?.bucketCount).toBe(3);
    expect(overflow?.vsize).toBe(60);
    const total = bar.segments.reduce((sum, { share }) => sum + share, 0);
    expect(total).toBeCloseTo(1, 10);
  });

  it("returns zero shares for an empty population", () => {
    const bar = buildCompositionBar([], descriptor, "count");
    expect(bar.segments).toEqual([]);
  });
});

describe("buildCompositionBars", () => {
  it("builds one independent bar per catalog descriptor", () => {
    const second: ClassifierDescriptor = {
      ...descriptor,
      id: "second_classifier",
      title: "Second classifier",
    };
    const bars = buildCompositionBars(
      [transaction(1, 100, result("complete", ["alpha"]))],
      [descriptor, second],
      "count",
    );
    expect(bars.map(({ classifierId }) => classifierId)).toEqual([
      "example_classifier",
      "second_classifier",
    ]);
    expect(bars[1]?.segments.map(({ label }) => label)).toEqual([
      "Result unavailable",
    ]);
  });
});

describe("buildEntanglementBars", () => {
  const withAncestry = (
    suffix: number,
    ancestors: number,
    descendants: number,
  ): MempoolTransaction => ({
    ...transaction(suffix, 100, null),
    ancestor_count: ancestors,
    descendant_count: descendants,
  });

  it("bands transactions by relatives beyond themselves", () => {
    const bars = buildEntanglementBars(
      [
        withAncestry(1, 1, 1),
        withAncestry(2, 2, 1),
        withAncestry(3, 4, 12),
        withAncestry(4, 26, 1),
      ],
      "count",
    );
    const ancestors = bars[0];
    expect(ancestors?.title).toBe("Unconfirmed ancestors");
    expect(ancestors?.segments.map(({ label }) => label)).toEqual([
      "None",
      "1",
      "2–4",
      "10+",
    ]);
    expect(ancestors?.segments.map(({ count }) => count)).toEqual([1, 1, 1, 1]);
    const descendants = bars[1];
    expect(descendants?.segments.map(({ label }) => label)).toEqual([
      "None",
      "10+",
    ]);
    const total = descendants?.segments.reduce(
      (sum, { share }) => sum + share,
      0,
    );
    expect(total).toBeCloseTo(1, 10);
  });

  it("weights bands by virtual size when requested", () => {
    const bars = buildEntanglementBars(
      [withAncestry(1, 1, 1), withAncestry(2, 3, 1)],
      "vsize",
    );
    expect(bars[0]?.segments.map(({ share }) => share)).toEqual([0.5, 0.5]);
  });
});
