import { describe, expect, it } from "vitest";

import { compareCurrentSnapshots } from "./comparison-model";
import { loadedSource } from "./comparison-test-fixtures";
import { mempoolTransaction, txid } from "./test-fixtures";
import {
  SnapshotDistributionCache,
  buildSnapshotDistributionModel,
  type SnapshotDistributionInput,
  type SnapshotDistributionModel,
} from "./snapshot-distributions";
import type {
  ClassificationResult,
  ClassifierDescriptor,
  MempoolTransaction,
} from "./types";

const propertyDescriptor: ClassifierDescriptor = {
  id: "transaction_properties",
  version: "1",
  title: "Transaction properties",
  methodology: "exact",
  semantics: "multi_label",
  required_facts: ["raw_transaction"],
  labels: [
    { key: "p2pkh", label: "P2PKH", description: "Legacy payment." },
    { key: "p2tr", label: "P2TR", description: "Taproot payment." },
    { key: "op_return", label: "OP_RETURN", description: "Data output." },
  ],
};

const dataDescriptor: ClassifierDescriptor = {
  id: "data_protocols",
  version: "1",
  title: "Data protocols",
  methodology: "fingerprint",
  semantics: "multi_label",
  required_facts: ["raw_transaction"],
  labels: [
    { key: "ordinals", label: "Ordinals", description: "Ordinals data." },
    { key: "other", label: "Other", description: "Other data." },
  ],
};

const result = (
  classifierId: string,
  labels: string[],
): ClassificationResult => ({
  classifier_id: classifierId,
  state: "complete",
  primary_label: labels[0] ?? null,
  labels,
  missing_facts: [],
  evidence: null,
});

interface TransactionOptions {
  vsize: number;
  structured?: boolean;
  carrierBytes?: number;
  replaceable?: boolean;
  propertyLabels?: string[];
  dataLabels?: string[];
  wtxid?: string;
}

const transaction = (
  suffix: number,
  {
    vsize,
    structured = true,
    carrierBytes = 0,
    replaceable = false,
    propertyLabels = ["p2pkh"],
    dataLabels = [],
    wtxid = txid(suffix + 100),
  }: TransactionOptions,
): MempoolTransaction =>
  mempoolTransaction(suffix, {
    wtxid,
    vsize,
    weight: vsize * 4,
    fee_sats: vsize * 2,
    entered_at_ms: 1_700_000_000_000 + suffix,
    ancestor_count: suffix === 3 ? 2 : 1,
    ancestor_vsize: vsize,
    ancestor_fee_sats: vsize * 2,
    descendant_vsize: vsize,
    replaceable,
    structure: structured
      ? {
          input_count: suffix,
          output_count: suffix + 1,
          op_return_bytes: carrierBytes,
          output_sats: suffix * 10_000,
          witness_bytes: 0,
        }
      : null,
    classifications: [
      result(propertyDescriptor.id, propertyLabels),
      result(dataDescriptor.id, dataLabels),
    ],
  });

const transactions = [
  transaction(1, {
    vsize: 100,
    carrierBytes: 40,
    replaceable: true,
    propertyLabels: ["p2pkh", "op_return"],
    dataLabels: ["ordinals"],
  }),
  transaction(2, { vsize: 250, propertyLabels: ["p2tr"] }),
  transaction(3, { vsize: 400, structured: false }),
];

const input = (
  metric: "count" | "vsize",
  population: readonly MempoolTransaction[] = transactions,
): SnapshotDistributionInput => ({
  transactions: population,
  classifierCatalog: [propertyDescriptor, dataDescriptor],
  selectedClassifier: propertyDescriptor,
  observedAtMs: 1_700_000_010_000,
  metric,
  groupLimit: 6,
  dataGroupLimit: 5,
});

const segmentWeight = (
  model: SnapshotDistributionModel,
  barIndex: number,
  metric: "count" | "vsize",
): number =>
  model.entanglement[barIndex]?.segments.reduce(
    (total, segment) =>
      total + (metric === "count" ? segment.count : segment.vsize),
    0,
  ) ?? 0;

describe("buildSnapshotDistributionModel", () => {
  it.each([
    ["count", 3, 2, 1],
    ["vsize", 750, 350, 100],
  ] as const)(
    "conserves %s across whole and subset distributions",
    (metric, populationWeight, structuredWeight, carrierWeight) => {
      const model = buildSnapshotDistributionModel(input(metric));

      expect(model.feeSpectrum.totalWeight).toBe(populationWeight);
      expect(model.packageSpectrum.totalWeight).toBe(populationWeight);
      expect(model.jointDensity.totalWeight).toBe(populationWeight);
      expect(model.ageMosaic.totalWeight).toBe(populationWeight);
      expect(model.complexityDensity.totalWeight).toBe(structuredWeight);
      expect(model.valueSpectrum.totalWeight).toBe(structuredWeight);
      expect(model.dataSpectrum.totalWeight).toBe(carrierWeight);
      expect(segmentWeight(model, 0, metric)).toBe(populationWeight);
      expect(segmentWeight(model, 1, metric)).toBe(populationWeight);
      expect(model.totals).toEqual({
        population: { count: 3, vsize: 750 },
        structured: { count: 2, vsize: 350 },
        carrier: { count: 1, vsize: 100 },
        replaceable: { count: 1, vsize: 100 },
      });
    },
  );

  it("keeps each classifier composition bar independent", () => {
    const model = buildSnapshotDistributionModel(input("count"));

    expect(model.composition.map(({ classifierId }) => classifierId)).toEqual([
      propertyDescriptor.id,
      dataDescriptor.id,
    ]);
    expect(
      model.composition[0]?.segments.reduce(
        (total, segment) => total + segment.count,
        0,
      ),
    ).toBe(transactions.length);
    expect(
      model.composition[1]?.segments.reduce(
        (total, segment) => total + segment.count,
        0,
      ),
    ).toBe(transactions.length);
    expect(
      model.composition[0]?.segments.map(({ label }) => label),
    ).not.toEqual(model.composition[1]?.segments.map(({ label }) => label));
  });

  it("preserves source-local variants for common comparison transactions", () => {
    const leftTransaction = transaction(9, {
      vsize: 120,
      carrierBytes: 32,
      wtxid: txid(109),
    });
    const rightTransaction = transaction(9, {
      vsize: 300,
      structured: false,
      wtxid: txid(209),
    });
    const comparison = compareCurrentSnapshots(
      loadedSource("left", [leftTransaction]),
      loadedSource("right", [rightTransaction]),
    );
    const leftPopulation = comparison.common.flatMap(({ left }) =>
      left === null ? [] : [left],
    );
    const rightPopulation = comparison.common.flatMap(({ right }) =>
      right === null ? [] : [right],
    );

    const leftModel = buildSnapshotDistributionModel(
      input("vsize", leftPopulation),
    );
    const rightModel = buildSnapshotDistributionModel(
      input("vsize", rightPopulation),
    );

    expect(comparison.common[0]?.same_wtxid).toBe(false);
    expect(leftModel.totals.population.vsize).toBe(120);
    expect(rightModel.totals.population.vsize).toBe(300);
    expect(leftModel.totals.carrier).toEqual({ count: 1, vsize: 120 });
    expect(rightModel.totals.carrier).toEqual({ count: 0, vsize: 0 });
  });

  it("retains no transaction collections or transaction objects", () => {
    const model = buildSnapshotDistributionModel(input("count"));
    const sourceObjects = new Set<object>(transactions);
    const visited = new Set<object>();

    const inspect = (value: unknown): void => {
      if (typeof value !== "object" || value === null || visited.has(value)) {
        return;
      }
      expect(sourceObjects.has(value)).toBe(false);
      expect(Object.hasOwn(value, "transactions")).toBe(false);
      visited.add(value);
      for (const child of Object.values(value)) {
        inspect(child);
      }
    };

    inspect(model);
  });
});

describe("SnapshotDistributionCache", () => {
  it("reuses a semantic variant for one owner and separates variants", () => {
    const cache = new SnapshotDistributionCache();
    const owner = {};
    let builds = 0;
    const build = () => {
      builds += 1;
      return buildSnapshotDistributionModel(input("count"));
    };

    const first = cache.get(owner, "classifier=properties;metric=count", build);
    const repeated = cache.get(
      owner,
      "classifier=properties;metric=count",
      build,
    );
    const otherVariant = cache.get(
      owner,
      "classifier=data;metric=count",
      build,
    );

    expect(repeated).toBe(first);
    expect(otherVariant).not.toBe(first);
    expect(builds).toBe(2);
  });

  it("invalidates variants when the owner changes or the cache resets", () => {
    const cache = new SnapshotDistributionCache();
    const firstOwner = {};
    const secondOwner = {};
    let builds = 0;
    const build = () => {
      builds += 1;
      return buildSnapshotDistributionModel(input("count"));
    };

    const first = cache.get(firstOwner, "metric=count", build);
    const replacement = cache.get(secondOwner, "metric=count", build);
    const returnedOwner = cache.get(firstOwner, "metric=count", build);
    cache.reset();
    const afterReset = cache.get(firstOwner, "metric=count", build);

    expect(replacement).not.toBe(first);
    expect(returnedOwner).not.toBe(first);
    expect(afterReset).not.toBe(returnedOwner);
    expect(builds).toBe(4);
  });
});
