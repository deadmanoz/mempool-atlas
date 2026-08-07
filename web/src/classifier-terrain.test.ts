import { describe, expect, it, vi } from "vitest";

import type {
  PackedPrimaryPublicationTransfer,
  PackedUnsignedColumnTransfer,
} from "./atlas-worker-protocol";
import {
  bucketTerrainRegionCanShowLabel,
  createBucketTerrainLayout,
} from "./bucket-terrain";
import {
  bip110RulePopulation,
  bip110RulePopulationSummary,
} from "./bip110-rule-index";
import {
  classifierBucketContainsLabel,
  classifierBucketDescription,
  classifierBucketForTransaction,
  classifierBucketIsSummary,
  classifierBucketLabel,
  classifierBucketPopulation,
  classifierBuckets,
  classifierLabelPopulation,
  classifierLabelSamplePopulation,
  precomputeClassifierBuckets,
  classifierTerrainGroups,
  classifierTerrainTotals,
} from "./classifier-terrain";
import { PackedPrimaryPublicationStore } from "./packed-store";
import { mempoolTransaction, txid } from "./test-fixtures";
import type {
  ClassificationResult,
  ClassifierDescriptor,
  MempoolTransaction,
} from "./types";
import { RULE_IDS } from "./types";

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

const secondDescriptor: ClassifierDescriptor = {
  ...descriptor,
  id: "second_classifier",
  title: "Second classifier",
};

const bip110Descriptor: ClassifierDescriptor = {
  id: "knots_bip110",
  version: "1",
  title: "BIP-110",
  methodology: "policy",
  semantics: "rule_set",
  required_facts: ["raw_transaction"],
  labels: [
    { key: "compatible", label: "Compatible", description: "Compatible." },
    { key: "violating", label: "Violating", description: "Violating." },
  ],
};

const packedColumn = (
  rowCount: number,
  width: number,
  valueAt: (row: number) => number,
): PackedUnsignedColumnTransfer => {
  const values = new Uint8Array(rowCount * width);
  for (let row = 0; row < rowCount; row += 1) {
    let value = valueAt(row);
    for (let byte = 0; byte < width; byte += 1) {
      values[row * width + byte] = value & 0xff;
      value = Math.floor(value / 256);
    }
  }
  return { width, values: values.buffer };
};

const packedPrimaryPublication = (
  rowCount: number,
): PackedPrimaryPublicationTransfer => {
  const txids = new Uint8Array(rowCount * 32);
  const txidView = new DataView(txids.buffer);
  for (let row = 0; row < rowCount; row += 1) {
    txidView.setUint32(row * 32 + 28, row);
  }
  const resultDictionary = [
    {
      state: "complete" as const,
      primary_label: "alpha",
      labels: ["alpha"],
      missing_facts: [],
    },
    {
      state: "partial" as const,
      primary_label: "beta",
      labels: ["beta"],
      missing_facts: ["input_script_pubkeys"],
    },
    {
      state: "complete" as const,
      primary_label: "alpha",
      labels: ["gamma", "alpha"],
      missing_facts: [],
    },
  ];
  const source = {
    source_id: "packed",
    source_label: "Packed",
    availability: "ready" as const,
    poll_interval_seconds: 300,
    last_poll_started_at_ms: 90,
    snapshot_observed_at_ms: 100,
    chain_tip: { height: 1, hash: "01".repeat(32) },
    transaction_count: rowCount,
    total_vsize: rowCount * 200,
    classification: {
      state: "complete" as const,
      revision: 1,
      classified_count: rowCount,
      unclassified_count: 0,
    },
    last_error: null,
  };
  return {
    manifest: {
      schema_version: 2,
      source,
      source_id: source.source_id,
      source_label: source.source_label,
      collection_started_at_ms: 90,
      collection_completed_at_ms: 100,
      collection_duration_ms: 10,
      observed_at_ms: 100,
      classification_revision: 1,
      chain_tip: source.chain_tip,
      transaction_count: rowCount,
      total_vsize: rowCount * 200,
      classifier_catalog: [descriptor, secondDescriptor],
      classification_summaries: [],
      bip110_summary: {
        evaluator_id: "rdts-rules",
        evaluator_version: "1",
        scope: "knots_mempool_policy",
        compatible_count: 0,
        violating_count: 0,
        indeterminate_count: 0,
        unclassified_count: rowCount,
      },
      row_count: rowCount,
      population_id: "10".repeat(32),
      classification_set_id: "11".repeat(32),
      publication_id: "12".repeat(32),
      stages: [],
    },
    population: {
      contentId: "10".repeat(32),
      txids: txids.buffer,
      vsize: packedColumn(rowCount, 2, (row) => 100 + (row % 201)),
    },
    classifiers: [descriptor, secondDescriptor].map((currentDescriptor) => ({
      contentId: currentDescriptor.id.padEnd(64, "0").slice(0, 64),
      classifierId: currentDescriptor.id,
      resultDictionary,
      resultCodes: packedColumn(rowCount, 1, (row) => row % 4),
      assessmentDictionary: null,
      assessmentCodes: null,
    })),
  };
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
  it("precomputes the BIP-110 rule index with the classifier buckets", async () => {
    let assessmentReads = 0;
    const transactions = [
      mempoolTransaction(1, {
        vsize: 100,
        bip110: {
          status: "violating",
          primary_rule: "element_size",
          violated_rules: ["element_size"],
          unknown_rules: [],
        },
        classifications: [
          {
            classifier_id: bip110Descriptor.id,
            state: "complete",
            primary_label: "violating",
            labels: ["violating"],
            missing_facts: [],
            evidence: null,
          },
        ],
      }),
      mempoolTransaction(2, {
        vsize: 300,
        bip110: {
          status: "violating",
          primary_rule: "element_size",
          violated_rules: ["element_size", "tapscript_op_if"],
          unknown_rules: [],
        },
        classifications: [
          {
            classifier_id: bip110Descriptor.id,
            state: "complete",
            primary_label: "violating",
            labels: ["violating"],
            missing_facts: [],
            evidence: null,
          },
        ],
      }),
    ].map((entry) => {
      const assessment = entry.bip110;
      Object.defineProperty(entry, "bip110", {
        configurable: true,
        get: () => {
          assessmentReads += 1;
          return assessment;
        },
      });
      return entry;
    });

    await precomputeClassifierBuckets(transactions, [bip110Descriptor], {
      batchSize: 1,
      yieldBetweenBatches: async () => Promise.resolve(),
    });

    expect(assessmentReads).toBe(transactions.length);
    expect(
      RULE_IDS.map(
        (rule) => bip110RulePopulationSummary(transactions, rule).count,
      ),
    ).toEqual([0, 2, 0, 0, 0, 0, 1]);
    const population = bip110RulePopulation(transactions, "element_size");
    expect(population).toMatchObject({ count: 2, vsize: 400 });
    expect(population.transactions[0]?.txid).toBe(transactions[1]?.txid);
    expect(bip110RulePopulation(transactions, "element_size")).toBe(population);
    expect(assessmentReads).toBe(transactions.length);
  });

  it("precomputes multiple classifiers in bounded batches over 70k packed rows", async () => {
    const store = new PackedPrimaryPublicationStore(
      packedPrimaryPublication(70_000),
    );
    const transactions = store.snapshot.transactions;
    const transactionAt = vi.spyOn(store, "transaction");
    const batchAccesses: number[] = [];
    let accessesAtLastYield = 0;

    await precomputeClassifierBuckets(
      transactions,
      [descriptor, secondDescriptor],
      {
        batchSize: 750,
        yieldBetweenBatches: async () => {
          const accesses = transactionAt.mock.calls.length;
          batchAccesses.push(accesses - accessesAtLastYield);
          accessesAtLastYield = accesses;
          await Promise.resolve();
        },
      },
    );
    batchAccesses.push(transactionAt.mock.calls.length - accessesAtLastYield);

    const populationBatches = batchAccesses.filter((count) => count > 0);
    expect(populationBatches).toHaveLength(Math.ceil(70_000 / 750));
    expect(Math.max(...populationBatches)).toBe(750);
    expect(populationBatches.reduce((total, count) => total + count, 0)).toBe(
      70_000,
    );
    expect(transactionAt).toHaveBeenCalledTimes(70_000);

    const accessesAfterPrecompute = transactionAt.mock.calls.length;
    const firstBuckets = classifierBuckets(transactions, descriptor);
    const secondBuckets = classifierBuckets(transactions, secondDescriptor);
    const groups = classifierTerrainGroups(transactions, descriptor);
    const alpha = classifierLabelPopulation(transactions, descriptor, "alpha");

    expect(transactionAt).toHaveBeenCalledTimes(accessesAfterPrecompute);
    expect(classifierBuckets(transactions, descriptor)).toBe(firstBuckets);
    expect(classifierBuckets(transactions, secondDescriptor)).toBe(
      secondBuckets,
    );
    expect(classifierTerrainGroups(transactions, descriptor)).toBe(groups);
    expect(
      firstBuckets.reduce((total, bucket) => total + bucket.count, 0),
    ).toBe(70_000);
    expect(
      groups.reduce((total, group) => total + group.transactions.length, 0),
    ).toBe(70_000);
    expect(alpha?.count).toBe(35_000);
    expect(classifierLabelPopulation(transactions, descriptor, "alpha")).toBe(
      alpha,
    );
  });

  it("matches synchronous bucket metadata, ordering, and populations", async () => {
    const transactions = [
      transaction(
        1,
        100,
        propertyResult("complete", ["version_1", "signals_rbf", "p2pkh"]),
      ),
      transaction(
        2,
        250,
        propertyResult("complete", ["version_2", "p2sh", "op_return"]),
      ),
      transaction(3, 150, propertyResult("complete", ["version_2", "p2wpkh"])),
      transaction(4, 300, propertyResult("partial", ["version_2", "p2a"])),
      transaction(5, 200, null),
    ];
    const expected = classifierBuckets([...transactions], propertyDescriptor);
    const yields = vi.fn(async () => Promise.resolve());

    await precomputeClassifierBuckets(transactions, [propertyDescriptor], {
      batchSize: 2,
      yieldBetweenBatches: yields,
    });
    const actual = classifierBuckets(transactions, propertyDescriptor);
    const comparable = (buckets: typeof actual) =>
      buckets.map(({ transactions: entries, ...bucket }) => ({
        ...bucket,
        txids: entries.map(({ txid: transactionId }) => transactionId),
      }));

    expect(yields.mock.calls.length).toBeGreaterThanOrEqual(2);
    expect(comparable(actual)).toEqual(comparable(expected));
    expect(classifierBuckets(transactions, propertyDescriptor)).toBe(actual);
  });

  it("rejects invalid cooperative batch sizes before scanning", async () => {
    const transactions = [transaction(1, 100, result("complete", ["alpha"]))];

    await expect(
      precomputeClassifierBuckets(transactions, [descriptor], { batchSize: 0 }),
    ).rejects.toThrow("positive integer");
  });

  it("does not publish a partial cache when precomputation is aborted", async () => {
    const transactions = [
      transaction(1, 100, result("complete", ["alpha"])),
      transaction(2, 200, result("complete", ["beta"])),
      transaction(3, 300, result("complete", ["gamma"])),
    ];
    const controller = new AbortController();

    await expect(
      precomputeClassifierBuckets(transactions, [descriptor], {
        batchSize: 10,
        signal: controller.signal,
        yieldBetweenBatches: () => controller.abort(),
      }),
    ).rejects.toMatchObject({ name: "AbortError" });

    const yields = vi.fn(async () => Promise.resolve());
    await precomputeClassifierBuckets(transactions, [descriptor], {
      batchSize: 1,
      yieldBetweenBatches: yields,
    });
    expect(yields.mock.calls.length).toBeGreaterThanOrEqual(2);
    expect(
      classifierBuckets(transactions, descriptor).reduce(
        (total, bucket) => total + bucket.count,
        0,
      ),
    ).toBe(3);
  });

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

  it("caches marginal label populations across property summary groups", async () => {
    const transactions = [
      transaction(
        1,
        100,
        propertyResult("complete", ["version_1", "signals_rbf", "p2pkh"]),
      ),
      transaction(
        2,
        250,
        propertyResult("complete", ["version_2", "p2sh", "op_return"]),
      ),
      transaction(3, 150, propertyResult("complete", ["version_2", "p2wpkh"])),
      transaction(4, 300, propertyResult("partial", ["version_2", "p2a"])),
      transaction(5, 200, null),
    ];

    await precomputeClassifierBuckets(transactions, [propertyDescriptor], {
      batchSize: 2,
      yieldBetweenBatches: async () => Promise.resolve(),
    });
    const versionTwo = classifierLabelPopulation(
      transactions,
      propertyDescriptor,
      "version_2",
    );

    expect(versionTwo).toMatchObject({ count: 3, vsize: 700, totalShare: 0.6 });
    expect(versionTwo?.transactions.map(({ txid }) => txid)).toEqual([
      transactions[3]?.txid,
      transactions[1]?.txid,
      transactions[2]?.txid,
    ]);
    expect(
      classifierLabelPopulation(transactions, propertyDescriptor, "version_2"),
    ).toBe(versionTwo);
    expect(
      classifierLabelSamplePopulation(
        transactions,
        propertyDescriptor,
        "version_2",
      ),
    ).toMatchObject({
      count: 3,
      vsize: 700,
      transactions: versionTwo?.transactions,
    });
    expect(
      classifierLabelPopulation(
        transactions,
        propertyDescriptor,
        "signals_rbf",
      ),
    ).toMatchObject({ count: 1, vsize: 100, totalShare: 0.2 });
    expect(
      classifierLabelPopulation(
        transactions,
        propertyDescriptor,
        "unknown_script",
      ),
    ).toMatchObject({ count: 0, vsize: 0, totalShare: 0 });
    expect(
      classifierLabelPopulation(
        transactions,
        propertyDescriptor,
        "unknown_label",
      ),
    ).toBeNull();
  });

  it("precomputes a bounded largest-first marginal-label sample", async () => {
    const transactions = Array.from({ length: 20 }, (_, index) =>
      transaction(index + 1, 100 + index, result("complete", ["alpha"])),
    );

    await precomputeClassifierBuckets(transactions, [descriptor], {
      batchSize: 5,
      yieldBetweenBatches: async () => Promise.resolve(),
    });
    const sample = classifierLabelSamplePopulation(
      transactions,
      descriptor,
      "alpha",
    );

    expect(sample).toMatchObject({ count: 20, vsize: 2_190, totalShare: 1 });
    expect(sample?.transactions).toHaveLength(8);
    expect(sample?.transactions.map(({ vsize }) => vsize)).toEqual([
      119, 118, 117, 116, 115, 114, 113, 112,
    ]);
  });

  it("counts multi-label membership independently across result states", () => {
    const transactions = [
      transaction(1, 100, result("complete", ["alpha"])),
      transaction(2, 200, result("complete", ["alpha", "gamma"])),
      transaction(3, 300, result("partial", ["alpha"])),
      transaction(4, 400, null),
    ];

    const alpha = classifierLabelPopulation(transactions, descriptor, "alpha");
    const gamma = classifierLabelPopulation(transactions, descriptor, "gamma");

    expect(alpha).toMatchObject({ count: 3, vsize: 600, totalShare: 0.75 });
    expect(alpha?.transactions.map(({ txid }) => txid)).toEqual([
      transactions[2]?.txid,
      transactions[1]?.txid,
      transactions[0]?.txid,
    ]);
    expect(gamma).toMatchObject({ count: 1, vsize: 200, totalShare: 0.25 });
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
