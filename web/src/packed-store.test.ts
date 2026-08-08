import { describe, expect, it, vi } from "vitest";

import type {
  PackedPrimaryPublicationTransfer,
  PackedPublicationTransfer,
  PackedSignedColumnTransfer,
  PackedUnsignedColumnTransfer,
} from "./atlas-worker-protocol";
import {
  PackedPrimaryPublicationStore,
  PackedPublicationStore,
  comparePackedSnapshotRows,
  findSnapshotTransaction,
  packedSnapshotRowCount,
  packedSnapshotRowVsize,
  packedSnapshotTransaction,
  snapshotIdentity,
  snapshotIsComplete,
} from "./packed-store";
import {
  compareCurrentSnapshots,
  lookupComparisonTransaction,
} from "./comparison-model";

const buffer = (...values: number[]): ArrayBuffer =>
  Uint8Array.from(values).buffer;

const column = (...values: number[]): PackedUnsignedColumnTransfer => ({
  width: 1,
  values: buffer(...values),
});

const signedColumn = (...values: number[]): PackedSignedColumnTransfer => {
  const width = 7;
  const bytes = new Uint8Array(values.length * width);
  values.forEach((value, row) => {
    let packed = BigInt.asUintN(width * 8, BigInt(value));
    for (let index = 0; index < width; index += 1) {
      bytes[row * width + index] = Number(packed & 0xffn);
      packed >>= 8n;
    }
  });
  return { width, values: bytes.buffer };
};

const publication = (): PackedPublicationTransfer => ({
  manifest: {
    schema_version: 2,
    source: {
      source_id: "core",
      source_label: "Core",
      availability: "ready",
      poll_interval_seconds: 300,
      last_poll_started_at_ms: 90,
      snapshot_observed_at_ms: 100,
      chain_tip: { height: 1, hash: "01".repeat(32) },
      transaction_count: 2,
      total_vsize: 3,
      classification: {
        state: "complete",
        revision: 1,
        classified_count: 1,
        unclassified_count: 1,
      },
      last_error: null,
    },
    source_id: "core",
    source_label: "Core",
    collection_started_at_ms: 90,
    collection_completed_at_ms: 100,
    collection_duration_ms: 10,
    observed_at_ms: 100,
    classification_revision: 1,
    chain_tip: { height: 1, hash: "01".repeat(32) },
    transaction_count: 2,
    total_vsize: 3,
    classifier_catalog: [
      {
        id: "knots_bip110",
        version: "1",
        title: "Policy",
        methodology: "policy",
        semantics: "rule_set",
        required_facts: [],
        labels: [
          { key: "compatible", label: "Compatible", description: "Compatible" },
        ],
      },
    ],
    classification_summaries: [
      {
        classifier_id: "knots_bip110",
        complete_count: 1,
        partial_count: 0,
        unclassified_count: 1,
        label_counts: { compatible: 1 },
      },
    ],
    bip110_summary: {
      evaluator_id: "rdts-rules",
      evaluator_version: "1",
      scope: "knots_mempool_policy",
      compatible_count: 1,
      violating_count: 0,
      indeterminate_count: 0,
      unclassified_count: 1,
    },
    row_count: 2,
    population_id: "10".repeat(32),
    classification_set_id: "11".repeat(32),
    publication_id: "12".repeat(32),
    stages: [],
  },
  population: {
    contentId: "10".repeat(32),
    txids: buffer(...Array(32).fill(0), ...Array(32).fill(1)),
    vsize: column(1, 2),
  },
  membership: {
    contentId: "13".repeat(32),
    differingWtxidBits: buffer(0b10),
    differingWtxidRanks: new Uint32Array([0, 0, 1]).buffer,
    differingWtxids: buffer(...Array(32).fill(2)),
    weight: column(4, 8),
    feeSats: column(1, 2),
    enteredAtMs: column(1, 2),
    ancestorCount: column(1, 1),
    ancestorVsize: column(1, 2),
    ancestorFeeSats: column(1, 2),
    descendantCount: column(1, 1),
    descendantVsize: column(1, 2),
    replaceableBits: buffer(0b01),
  },
  structure: {
    contentId: "14".repeat(32),
    presenceBits: buffer(0b01),
    presenceRanks: new Uint32Array([0, 1, 1]).buffer,
    inputCount: column(1),
    outputCount: column(2),
    opReturnBytes: column(0),
    outputSats: column(5),
    witnessBytes: column(3),
  },
  classifiers: [
    {
      contentId: "15".repeat(32),
      classifierId: "knots_bip110",
      resultDictionary: [
        {
          state: "complete",
          primary_label: "compatible",
          labels: ["compatible"],
          missing_facts: [],
        },
      ],
      resultCodes: column(1, 0),
      assessmentDictionary: [
        {
          status: "compatible",
          primary_rule: null,
          violated_rules: [],
          unknown_rules: [],
        },
      ],
      assessmentCodes: column(1, 0),
    },
  ],
});

describe("PackedPublicationStore", () => {
  it("identifies snapshot content independently of source lifecycle metadata", () => {
    const current = publication().manifest;
    const lifecycleChange = structuredClone(current);
    lifecycleChange.publication_id = "99".repeat(32);
    lifecycleChange.source = {
      ...lifecycleChange.source,
      availability: "stale",
      last_poll_started_at_ms: 110,
      last_error: "next poll failed",
    };

    expect(snapshotIdentity(lifecycleChange)).toBe(snapshotIdentity(current));

    const snapshotChange = structuredClone(current);
    snapshotChange.stages = [
      {
        kind: "population",
        content_id: "88".repeat(32),
        uncompressed_bytes: 64,
        row_count: 2,
        dependency_ids: [],
      },
    ];
    expect(snapshotIdentity(snapshotChange)).not.toBe(
      snapshotIdentity(current),
    );
  });

  it("decodes safe seven-byte unsigned columns without BigInt materialization", () => {
    const current = publication();
    current.membership.feeSats = signedColumn(
      Number.MAX_SAFE_INTEGER,
      Number.MAX_SAFE_INTEGER,
    );
    const store = new PackedPublicationStore(current);

    expect(store.snapshot.transactions[0]?.fee_sats).toBe(
      Number.MAX_SAFE_INTEGER,
    );
  });

  it("decodes safe negative seven-byte signed columns exactly", () => {
    const current = publication();
    current.membership.ancestorFeeSats = signedColumn(
      -Number.MAX_SAFE_INTEGER,
      Number.MAX_SAFE_INTEGER,
    );
    const store = new PackedPublicationStore(current);

    expect(store.snapshot.transactions[0]?.ancestor_fee_sats).toBe(
      -Number.MAX_SAFE_INTEGER,
    );
    expect(store.snapshot.transactions[1]?.ancestor_fee_sats).toBe(
      Number.MAX_SAFE_INTEGER,
    );
  });

  it("exposes a bounded non-owning row view over transferred columns", () => {
    const store = new PackedPublicationStore(publication());

    expect(store.snapshot.transactions).toHaveLength(2);
    expect(store.snapshot.transactions[0]).toMatchObject({
      txid: "00".repeat(32),
      wtxid: "00".repeat(32),
      vsize: 1,
      replaceable: true,
      structure: { input_count: 1, output_count: 2 },
      bip110: { status: "compatible" },
    });
    expect(store.snapshot.transactions[1]).toMatchObject({
      txid: "01".repeat(32),
      wtxid: "02".repeat(32),
      structure: null,
      classifications: [],
      bip110: null,
    });
    expect([...store.snapshot.transactions].map(({ vsize }) => vsize)).toEqual([
      1, 2,
    ]);
  });

  it("performs exact packed-byte txid lookup without a union-sized map", () => {
    const store = new PackedPublicationStore(publication());
    expect(
      findSnapshotTransaction(store.snapshot, "01".repeat(32))?.wtxid,
    ).toBe("02".repeat(32));
    expect(findSnapshotTransaction(store.snapshot, "03".repeat(32))).toBe(
      undefined,
    );
  });

  it("exposes bounded row access and packed-byte comparison helpers", () => {
    const store = new PackedPublicationStore(publication());
    const detachedSnapshot = { ...store.snapshot };

    expect(packedSnapshotRowCount(store.snapshot)).toBe(2);
    expect(packedSnapshotRowVsize(store.snapshot, 1)).toBe(2);
    expect(packedSnapshotRowVsize(store.snapshot, 2)).toBeUndefined();
    expect(packedSnapshotTransaction(store.snapshot, -1)).toBeUndefined();
    expect(
      comparePackedSnapshotRows(store.snapshot, 0, store.snapshot, 1),
    ).toBe(-1);
    expect(
      comparePackedSnapshotRows(store.snapshot, 1, store.snapshot, 0),
    ).toBe(1);
    expect(
      comparePackedSnapshotRows(store.snapshot, 1, store.snapshot, 1),
    ).toBe(0);
    expect(() =>
      comparePackedSnapshotRows(store.snapshot, 2, store.snapshot, 0),
    ).toThrow("Packed comparison row is unavailable");
    expect(() => packedSnapshotRowCount(detachedSnapshot)).toThrow(
      "Snapshot is not a packed v2 publication",
    );
    expect(() =>
      comparePackedSnapshotRows(store.snapshot, 0, detachedSnapshot, 0),
    ).toThrow("Packed comparison row is unavailable");
    expect(() =>
      findSnapshotTransaction(detachedSnapshot, "00".repeat(32)),
    ).toThrow("Snapshot is not a packed v2 publication");
  });

  it("merge-joins packed rows before lazily materializing comparison entries", () => {
    const leftPublication = publication();
    leftPublication.population.txids = buffer(
      ...Array(32).fill(0),
      ...Array(32).fill(2),
    );
    leftPublication.population.vsize = column(1, 3);
    leftPublication.manifest.total_vsize = 4;
    const rightPublication = publication();
    rightPublication.manifest.source = {
      ...rightPublication.manifest.source,
      source_id: "knots",
      source_label: "Knots",
    };
    rightPublication.manifest.source_id = "knots";
    rightPublication.manifest.source_label = "Knots";
    rightPublication.population.txids = buffer(
      ...Array(32).fill(0),
      ...Array(32).fill(1),
    );

    const leftStore = new PackedPublicationStore(leftPublication);
    const rightStore = new PackedPublicationStore(rightPublication);
    const leftTransaction = vi.spyOn(leftStore, "transaction");
    const rightTransaction = vi.spyOn(rightStore, "transaction");
    const comparison = compareCurrentSnapshots(
      {
        source: leftPublication.manifest.source,
        snapshot: leftStore.snapshot,
      },
      {
        source: rightPublication.manifest.source,
        snapshot: rightStore.snapshot,
      },
    );

    expect(leftTransaction).not.toHaveBeenCalled();
    expect(rightTransaction).not.toHaveBeenCalled();
    expect(Array.isArray(comparison.common)).toBe(true);
    expect(comparison.totals).toMatchObject({
      union_count: 3,
      common_count: 1,
      common_left_vsize: 1,
      common_right_vsize: 1,
      left_only_count: 1,
      left_only_vsize: 3,
      right_only_count: 1,
      right_only_vsize: 2,
    });

    const common = comparison.common[0];
    expect(common).toMatchObject({
      txid: "00".repeat(32),
      witness_relation: "same",
      left: { vsize: 1 },
      right: { vsize: 1 },
    });
    expect(comparison.common[0]).toBe(common);
    expect(leftTransaction).toHaveBeenCalledTimes(1);
    expect(rightTransaction).toHaveBeenCalledTimes(1);
    expect(comparison.left_only.map(({ txid }) => txid)).toEqual([
      "02".repeat(32),
    ]);
    expect(comparison.right_only.map(({ txid }) => txid)).toEqual([
      "01".repeat(32),
    ]);
    expect(lookupComparisonTransaction(comparison, "01".repeat(32))).toEqual({
      region: "right_only",
      index: 0,
      entry: comparison.right_only[0],
    });
  });
});

describe("PackedPrimaryPublicationStore", () => {
  it("exposes only population and selected-classifier facts before membership", () => {
    const complete = publication();
    const primary: PackedPrimaryPublicationTransfer = {
      manifest: complete.manifest,
      population: complete.population,
      classifiers: complete.classifiers,
    };
    const store = new PackedPrimaryPublicationStore(primary);
    const transaction = store.snapshot.transactions[0];

    expect(snapshotIsComplete(store.snapshot)).toBe(false);
    expect(transaction).toMatchObject({
      txid: "00".repeat(32),
      vsize: 1,
      bip110: { status: "compatible" },
    });
    expect(() => transaction?.fee_sats).toThrow(
      "Membership stage is still loading",
    );
    expect(findSnapshotTransaction(store.snapshot, "01".repeat(32))?.txid).toBe(
      "01".repeat(32),
    );
  });

  it("shares non-enumerable readonly membership guards across primary rows", () => {
    const complete = publication();
    const store = new PackedPrimaryPublicationStore({
      manifest: complete.manifest,
      population: complete.population,
      classifiers: complete.classifiers,
    });
    const first = store.snapshot.transactions[0];
    const second = store.snapshot.transactions[1];
    expect(first).toBeDefined();
    expect(second).toBeDefined();
    if (first === undefined || second === undefined) return;

    expect(Object.getPrototypeOf(first)).toBe(Object.getPrototypeOf(second));
    expect(Object.keys(first)).toEqual([
      "txid",
      "vsize",
      "classifications",
      "bip110",
    ]);
    for (const field of [
      "wtxid",
      "weight",
      "fee_sats",
      "entered_at_ms",
      "ancestor_count",
      "ancestor_vsize",
      "ancestor_fee_sats",
      "descendant_count",
      "descendant_vsize",
      "replaceable",
      "structure",
    ] as const) {
      expect(Object.hasOwn(first, field), field).toBe(false);
      expect(
        Object.getOwnPropertyDescriptor(Object.getPrototypeOf(first), field),
        field,
      ).toMatchObject({ configurable: false, enumerable: false });
      expect(() => first[field], field).toThrow(
        "Membership stage is still loading",
      );
    }
    expect(Reflect.set(first, "fee_sats", 42)).toBe(false);
    expect(() => first.fee_sats).toThrow("Membership stage is still loading");
  });

  it("compares primary publications without reading unavailable membership", () => {
    const leftComplete = publication();
    const rightComplete = publication();
    rightComplete.manifest.source = {
      ...rightComplete.manifest.source,
      source_id: "knots",
      source_label: "Knots",
    };
    rightComplete.manifest.source_id = "knots";
    rightComplete.manifest.source_label = "Knots";
    const left = new PackedPrimaryPublicationStore({
      manifest: leftComplete.manifest,
      population: leftComplete.population,
      classifiers: leftComplete.classifiers,
    });
    const right = new PackedPrimaryPublicationStore({
      manifest: rightComplete.manifest,
      population: rightComplete.population,
      classifiers: rightComplete.classifiers,
    });

    const comparison = compareCurrentSnapshots(
      {
        source: leftComplete.manifest.source,
        snapshot: left.snapshot,
      },
      {
        source: rightComplete.manifest.source,
        snapshot: right.snapshot,
      },
      false,
    );

    expect(comparison.common).toHaveLength(2);
    expect(comparison.common[0]).toMatchObject({
      txid: "00".repeat(32),
      witness_relation: "loading",
      left: { vsize: 1 },
      right: { vsize: 1 },
    });
  });
});
