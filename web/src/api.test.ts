import { afterEach, describe, expect, it, vi } from "vitest";

import {
  fetchSourceSnapshot,
  fetchTransactionDetail,
  parseSourceSnapshotResponse,
  parseSourcesResponse,
  parseTransactionDetailResponse,
  transactionDetailMatchesSnapshot,
} from "./api";
import type {
  MempoolSnapshot,
  MempoolTransaction,
  TransactionDetailResponse,
} from "./types";

const TXID = "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f";
const BLOCK_HASH = "00".repeat(32);

const classifierCatalog = () => [
  {
    id: "transaction_properties",
    version: "1",
    title: "Transaction properties",
    methodology: "exact" as const,
    semantics: "multi_label" as const,
    required_facts: ["raw_transaction", "input_script_pubkeys"],
    labels: [
      { key: "version_2", label: "Version 2", description: "Version 2." },
      { key: "p2wpkh", label: "P2WPKH", description: "Uses P2WPKH." },
    ],
  },
  {
    id: "transaction_shape",
    version: "1",
    title: "Transaction shape",
    methodology: "heuristic" as const,
    semantics: "multi_label" as const,
    required_facts: ["raw_transaction", "input_script_pubkeys"],
    labels: [
      {
        key: "other_shape",
        label: "Other shape",
        description: "No shape matched.",
      },
    ],
  },
  {
    id: "data_protocols",
    version: "1",
    title: "Data protocols",
    methodology: "fingerprint" as const,
    semantics: "multi_label" as const,
    required_facts: ["raw_transaction"],
    labels: [
      {
        key: "no_detected_protocol",
        label: "No detected protocol",
        description: "No fingerprint fired.",
      },
    ],
  },
  {
    id: "knots_bip110",
    version: "1",
    title: "Knots BIP-110 compatibility",
    methodology: "policy" as const,
    semantics: "rule_set" as const,
    required_facts: ["raw_transaction", "input_script_pubkeys"],
    labels: [
      {
        key: "violating",
        label: "Would violate",
        description: "A violation was proven.",
      },
    ],
  },
];

const compactClassifications = () => [
  {
    classifier_id: "transaction_properties",
    state: "complete" as const,
    primary_label: null,
    labels: ["version_2", "p2wpkh"],
    missing_facts: [],
    evidence: null,
  },
  {
    classifier_id: "transaction_shape",
    state: "complete" as const,
    primary_label: "other_shape",
    labels: ["other_shape"],
    missing_facts: [],
    evidence: null,
  },
  {
    classifier_id: "data_protocols",
    state: "complete" as const,
    primary_label: "no_detected_protocol",
    labels: ["no_detected_protocol"],
    missing_facts: [],
    evidence: null,
  },
  {
    classifier_id: "knots_bip110",
    state: "complete" as const,
    primary_label: "violating",
    labels: ["violating"],
    missing_facts: [],
    evidence: null,
  },
];

const classificationSummaries = () => [
  {
    classifier_id: "transaction_properties",
    complete_count: 1,
    partial_count: 0,
    unclassified_count: 0,
    label_counts: { version_2: 1, p2wpkh: 1 },
  },
  {
    classifier_id: "transaction_shape",
    complete_count: 1,
    partial_count: 0,
    unclassified_count: 0,
    label_counts: { other_shape: 1 },
  },
  {
    classifier_id: "data_protocols",
    complete_count: 1,
    partial_count: 0,
    unclassified_count: 0,
    label_counts: { no_detected_protocol: 1 },
  },
  {
    classifier_id: "knots_bip110",
    complete_count: 1,
    partial_count: 0,
    unclassified_count: 0,
    label_counts: { violating: 1 },
  },
];

const makeUnclassified = (value: MempoolSnapshot): void => {
  value.transactions[0]!.classifications = [];
  value.transactions[0]!.bip110 = null;
  value.transactions[0]!.structure = null;
  value.classification_summaries = value.classification_summaries.map(
    (summary) => ({
      ...summary,
      complete_count: 0,
      partial_count: 0,
      unclassified_count: 1,
      label_counts: Object.fromEntries(
        Object.keys(summary.label_counts).map((label) => [label, 0]),
      ),
    }),
  );
  value.bip110_summary = {
    ...value.bip110_summary,
    violating_count: 0,
    unclassified_count: 1,
  };
};

const source = () => ({
  source_id: "core",
  source_label: "Bitcoin Core",
  availability: "ready",
  poll_interval_seconds: 300,
  last_poll_started_at_ms: 1_700_000_000_000,
  snapshot_observed_at_ms: 1_700_000_001_000,
  chain_tip: { height: 900_000, hash: BLOCK_HASH },
  transaction_count: 1,
  total_vsize: 141,
  classification: {
    state: "complete",
    revision: 3,
    classified_count: 1,
    unclassified_count: 0,
  },
  last_error: null,
});

const snapshot = (): MempoolSnapshot => ({
  source_id: "core",
  source_label: "Bitcoin Core",
  collection_started_at_ms: 1_700_000_000_000,
  collection_completed_at_ms: 1_700_000_001_000,
  collection_duration_ms: 1_000,
  observed_at_ms: 1_700_000_001_000,
  classification_revision: 3,
  chain_tip: { height: 900_000, hash: BLOCK_HASH },
  transaction_count: 1,
  total_vsize: 141,
  classifier_catalog: classifierCatalog(),
  classification_summaries: classificationSummaries(),
  bip110_summary: {
    evaluator_id: "rdts",
    evaluator_version: "0.1.0",
    scope: "knots_mempool_policy",
    compatible_count: 0,
    violating_count: 1,
    indeterminate_count: 0,
    unclassified_count: 0,
  },
  transactions: [
    {
      txid: TXID,
      wtxid: TXID,
      vsize: 141,
      weight: 561,
      fee_sats: 423,
      entered_at_ms: 1_699_999_000_000,
      ancestor_count: 1,
      ancestor_vsize: 141,
      ancestor_fee_sats: 423,
      descendant_count: 1,
      descendant_vsize: 141,
      replaceable: false,
      structure: {
        input_count: 1,
        output_count: 2,
        op_return_bytes: 0,
        output_sats: 50_000,
        witness_bytes: 107,
      },
      classifications: compactClassifications(),
      bip110: {
        status: "violating",
        primary_rule: "element_size",
        violated_rules: ["element_size"],
        unknown_rules: [],
      },
    },
  ],
});

const transactionDetail = (): TransactionDetailResponse => ({
  source_id: "core",
  snapshot_observed_at_ms: 1_700_000_001_000,
  classification_revision: 3,
  txid: TXID,
  wtxid: TXID,
  classifications: compactClassifications().map((result) => ({
    ...result,
    evidence: { fixture: true },
  })),
  assessment: {
    status: "violating",
    primary_rule: "element_size",
    violated_rules: ["element_size"],
    unknown_rules: [],
  },
  rules: [
    {
      rule: "output_size",
      number: 1,
      verdict: "pass",
      evidence_count: 0,
      evidence: [],
      missing_count: 0,
      missing: [],
    },
    {
      rule: "element_size",
      number: 2,
      verdict: "violate",
      evidence_count: 3,
      evidence: [{ location: "witness[0]" }],
      missing_count: 0,
      missing: [],
    },
    {
      rule: "undefined_version",
      number: 3,
      verdict: "pass",
      evidence_count: 0,
      evidence: [],
      missing_count: 0,
      missing: [],
    },
    {
      rule: "taproot_annex",
      number: 4,
      verdict: "pass",
      evidence_count: 0,
      evidence: [],
      missing_count: 0,
      missing: [],
    },
    {
      rule: "control_block_size",
      number: 5,
      verdict: "pass",
      evidence_count: 0,
      evidence: [],
      missing_count: 0,
      missing: [],
    },
    {
      rule: "op_success",
      number: 6,
      verdict: "pass",
      evidence_count: 0,
      evidence: [],
      missing_count: 0,
      missing: [],
    },
    {
      rule: "tapscript_op_if",
      number: 7,
      verdict: "pass",
      evidence_count: 0,
      evidence: [],
      missing_count: 0,
      missing: [],
    },
  ],
});

afterEach(() => {
  vi.unstubAllGlobals();
});

describe("parseSourceSnapshotResponse", () => {
  it("validates and retains the API object without rebuilding it", () => {
    const value = { source: source(), snapshot: snapshot() };
    const response = parseSourceSnapshotResponse(value);

    expect(response).toBe(value);
    expect(response.source.availability).toBe("ready");
    expect(response.snapshot?.transactions[0]?.fee_sats).toBe(423);
  });

  it("accepts waiting state before the first snapshot", () => {
    const waiting = {
      ...source(),
      availability: "waiting",
      last_poll_started_at_ms: null,
      snapshot_observed_at_ms: null,
      chain_tip: null,
      transaction_count: null,
      total_vsize: null,
      classification: null,
    };

    expect(
      parseSourceSnapshotResponse({ source: waiting, snapshot: null }),
    ).toMatchObject({
      source: { availability: "waiting" },
      snapshot: null,
    });
  });

  it("accepts stale state only with a last error and retained snapshot", () => {
    const stale = {
      ...source(),
      availability: "stale",
      last_error: "node unavailable",
    };

    expect(
      parseSourceSnapshotResponse({ source: stale, snapshot: snapshot() }),
    ).toMatchObject({
      source: { availability: "stale", last_error: "node unavailable" },
    });
  });

  it("rejects inconsistent totals and source metadata", () => {
    expect(() =>
      parseSourceSnapshotResponse({
        source: source(),
        snapshot: { ...snapshot(), total_vsize: 142 },
      }),
    ).toThrow("Snapshot total vsize does not match payload");

    expect(() =>
      parseSourceSnapshotResponse({
        source: source(),
        snapshot: { ...snapshot(), source_id: "knots" },
      }),
    ).toThrow("Source summary does not match its snapshot");
  });

  it("requires classification progress exactly when a snapshot exists", () => {
    expect(() =>
      parseSourceSnapshotResponse({
        source: { ...source(), classification: null },
        snapshot: snapshot(),
      }),
    ).toThrow(
      "Source summary classification does not match its snapshot metadata",
    );

    const waiting = {
      ...source(),
      availability: "waiting",
      last_poll_started_at_ms: null,
      snapshot_observed_at_ms: null,
      chain_tip: null,
      transaction_count: null,
      total_vsize: null,
    };
    expect(() =>
      parseSourceSnapshotResponse({ source: waiting, snapshot: null }),
    ).toThrow(
      "Source without a snapshot unexpectedly contains classification progress",
    );
  });

  it("requires strict and conserving classification progress", () => {
    expect(() =>
      parseSourceSnapshotResponse({
        source: {
          ...source(),
          classification: {
            ...source().classification,
            state: "unknown",
          },
        },
        snapshot: snapshot(),
      }),
    ).toThrow("Invalid classification progress");

    expect(() =>
      parseSourceSnapshotResponse({
        source: {
          ...source(),
          classification: {
            ...source().classification,
            classified_count: 0,
          },
        },
        snapshot: snapshot(),
      }),
    ).toThrow(
      "Source summary classification does not match its snapshot metadata",
    );

    expect(() =>
      parseSourceSnapshotResponse({
        source: {
          ...source(),
          classification: {
            ...source().classification,
            extra: true,
          },
        },
        snapshot: snapshot(),
      }),
    ).toThrow("Invalid classification progress");
  });

  it("binds classification revision and counts to the returned snapshot", () => {
    expect(() =>
      parseSourceSnapshotResponse({
        source: {
          ...source(),
          classification: {
            ...source().classification,
            revision: 4,
          },
        },
        snapshot: snapshot(),
      }),
    ).toThrow("Source summary does not match its snapshot");

    const unclassifiedSnapshot = snapshot();
    makeUnclassified(unclassifiedSnapshot);
    expect(() =>
      parseSourceSnapshotResponse({
        source: source(),
        snapshot: unclassifiedSnapshot,
      }),
    ).toThrow("Source summary does not match its snapshot");
  });

  it("accepts every lifecycle state and complete snapshots with coverage gaps", () => {
    const noAssessmentSnapshot = snapshot();
    makeUnclassified(noAssessmentSnapshot);

    for (const state of ["classifying", "complete", "paused"] as const) {
      const value = {
        source: {
          ...source(),
          classification: {
            state,
            revision: 3,
            classified_count: 0,
            unclassified_count: 1,
          },
        },
        snapshot: noAssessmentSnapshot,
      };
      expect(parseSourceSnapshotResponse(value)).toBe(value);
    }
  });

  it("rejects membership facts and structure that break the contract", () => {
    const withTransaction = (patch: Partial<MempoolTransaction>) => {
      const value = snapshot();
      Object.assign(value.transactions[0]!, patch);
      return { source: source(), snapshot: value };
    };

    expect(() =>
      parseSourceSnapshotResponse(withTransaction({ weight: 141 * 4 + 1 })),
    ).toThrow("inconsistent weight");
    expect(() =>
      parseSourceSnapshotResponse(withTransaction({ weight: 0 })),
    ).toThrow("inconsistent weight");
    expect(() =>
      parseSourceSnapshotResponse(withTransaction({ ancestor_vsize: 140 })),
    ).toThrow("inconsistent ancestry");
    expect(() =>
      parseSourceSnapshotResponse(withTransaction({ descendant_count: 0 })),
    ).toThrow("inconsistent ancestry");
    expect(() =>
      parseSourceSnapshotResponse(withTransaction({ structure: null })),
    ).toThrow("couples structure and assessment");
    expect(() =>
      parseSourceSnapshotResponse(
        withTransaction({
          structure: {
            input_count: 0,
            output_count: 1,
            op_return_bytes: 0,
            output_sats: 0,
            witness_bytes: 0,
          },
        }),
      ),
    ).toThrow("Invalid transaction structure");
  });

  it("accepts a delta-adjusted ancestor fee below zero", () => {
    const value = snapshot();
    value.transactions[0]!.ancestor_fee_sats = -12_500;

    expect(
      parseSourceSnapshotResponse({ source: source(), snapshot: value })
        .snapshot?.transactions[0]?.ancestor_fee_sats,
    ).toBe(-12_500);
  });

  it("still requires unsigned base fees and ancestry sizes", () => {
    const withTransaction = (patch: Partial<MempoolTransaction>) => {
      const value = snapshot();
      Object.assign(value.transactions[0]!, patch);
      return { source: source(), snapshot: value };
    };

    expect(() =>
      parseSourceSnapshotResponse(withTransaction({ fee_sats: -1 })),
    ).toThrow("Invalid transaction at index 0");
    expect(() =>
      parseSourceSnapshotResponse(withTransaction({ ancestor_vsize: -141 })),
    ).toThrow("Invalid transaction at index 0");
    expect(() =>
      parseSourceSnapshotResponse(withTransaction({ ancestor_fee_sats: -0.5 })),
    ).toThrow("Invalid transaction at index 0");
  });

  it("rejects a classifier catalog that repeats an ID", () => {
    const value = snapshot();
    value.classifier_catalog = [
      ...classifierCatalog(),
      classifierCatalog()[0]!,
    ];

    expect(() =>
      parseSourceSnapshotResponse({ source: source(), snapshot: value }),
    ).toThrow("Invalid classifier descriptor at index 4");
  });

  it("rejects a classifier that repeats a label key", () => {
    const value = snapshot();
    const descriptor = value.classifier_catalog[0]!;
    descriptor.labels = [descriptor.labels[0]!, descriptor.labels[0]!];

    expect(() =>
      parseSourceSnapshotResponse({ source: source(), snapshot: value }),
    ).toThrow("Invalid classifier label at 0:1");
  });

  it("rejects unsafe, negative, and fractional lifecycle numbers", () => {
    const invalidNumbers = [
      ["revision", Number.MAX_SAFE_INTEGER + 1],
      ["classified_count", -1],
      ["unclassified_count", 0.5],
    ] as const;

    for (const [field, invalid] of invalidNumbers) {
      expect(() =>
        parseSourceSnapshotResponse({
          source: {
            ...source(),
            classification: {
              ...source().classification,
              [field]: invalid,
            },
          },
          snapshot: snapshot(),
        }),
      ).toThrow("Invalid classification progress");
    }
  });

  it("requires a reader-visible classification revision", () => {
    const value = snapshot() as unknown as Record<string, unknown>;
    delete value.classification_revision;

    expect(() =>
      parseSourceSnapshotResponse({ source: source(), snapshot: value }),
    ).toThrow("Invalid mempool snapshot");
  });

  it("requires internally consistent snapshot collection timing", () => {
    expect(() =>
      parseSourceSnapshotResponse({
        source: source(),
        snapshot: { ...snapshot(), collection_duration_ms: 999 },
      }),
    ).toThrow("Invalid mempool snapshot collection timing");

    expect(() =>
      parseSourceSnapshotResponse({
        source: source(),
        snapshot: {
          ...snapshot(),
          collection_started_at_ms: 1_700_000_002_000,
        },
      }),
    ).toThrow("Invalid mempool snapshot collection timing");

    expect(() =>
      parseSourceSnapshotResponse({
        source: source(),
        snapshot: { ...snapshot(), observed_at_ms: 1_700_000_001_001 },
      }),
    ).toThrow("Invalid mempool snapshot collection timing");
  });

  it("rejects malformed or unordered transactions", () => {
    expect(() =>
      parseSourceSnapshotResponse({
        source: source(),
        snapshot: {
          ...snapshot(),
          transaction_count: 2,
          total_vsize: 282,
          transactions: [
            snapshot().transactions[0],
            snapshot().transactions[0],
          ],
        },
      }),
    ).toThrow("Transactions are not strictly ordered by txid");

    expect(() =>
      parseSourceSnapshotResponse({
        source: source(),
        snapshot: {
          ...snapshot(),
          transactions: [{ ...snapshot().transactions[0], vsize: 0 }],
        },
      }),
    ).toThrow("Invalid transaction at index 0");
  });

  it("rejects inconsistent classification summaries and assessments", () => {
    expect(() =>
      parseSourceSnapshotResponse({
        source: source(),
        snapshot: {
          ...snapshot(),
          bip110_summary: {
            ...snapshot().bip110_summary,
            compatible_count: 1,
            violating_count: 0,
          },
        },
      }),
    ).toThrow("BIP-110 summary does not match payload");

    expect(() =>
      parseSourceSnapshotResponse({
        source: source(),
        snapshot: {
          ...snapshot(),
          transactions: [
            {
              ...snapshot().transactions[0],
              bip110: {
                status: "violating",
                primary_rule: "element_size",
                violated_rules: [],
                unknown_rules: [],
              },
            },
          ],
        },
      }),
    ).toThrow("Inconsistent BIP-110 assessment");
  });

  it("accepts a partial shape result that proves no labels yet", () => {
    const value = snapshot();
    const shape = value.transactions[0]!.classifications.find(
      ({ classifier_id }) => classifier_id === "transaction_shape",
    )!;
    shape.state = "partial";
    shape.primary_label = null;
    shape.labels = [];
    shape.missing_facts = ["input_script_pubkeys"];
    const shapeSummary = value.classification_summaries.find(
      ({ classifier_id }) => classifier_id === "transaction_shape",
    )!;
    shapeSummary.complete_count = 0;
    shapeSummary.partial_count = 1;
    shapeSummary.label_counts = { other_shape: 0 };

    expect(
      parseSourceSnapshotResponse({ source: source(), snapshot: value })
        .snapshot?.transactions[0]?.classifications,
    ).toBeDefined();
  });

  it("rejects a complete result without labels", () => {
    const value = snapshot();
    const shape = value.transactions[0]!.classifications.find(
      ({ classifier_id }) => classifier_id === "transaction_shape",
    )!;
    shape.primary_label = null;
    shape.labels = [];

    expect(() =>
      parseSourceSnapshotResponse({ source: source(), snapshot: value }),
    ).toThrow("Inconsistent transaction_shape classification result");
  });

  it("accepts a definite violation without a deterministic primary rule", () => {
    const value = snapshot();
    value.transactions[0]!.bip110 = {
      status: "violating",
      primary_rule: null,
      violated_rules: ["element_size"],
      unknown_rules: ["element_size", "undefined_version"],
    };
    const policy = value.transactions[0]!.classifications.find(
      ({ classifier_id }) => classifier_id === "knots_bip110",
    )!;
    policy.state = "partial";
    policy.missing_facts = ["policy_facts"];
    const policySummary = value.classification_summaries.find(
      ({ classifier_id }) => classifier_id === "knots_bip110",
    )!;
    policySummary.complete_count = 0;
    policySummary.partial_count = 1;

    expect(
      parseSourceSnapshotResponse({ source: source(), snapshot: value })
        .snapshot?.transactions[0]?.bip110,
    ).toMatchObject({
      status: "violating",
      primary_rule: null,
      violated_rules: ["element_size"],
      unknown_rules: ["element_size", "undefined_version"],
    });
  });
});

describe("parseTransactionDetailResponse", () => {
  it("validates the canonical seven-rule detail", () => {
    const value = transactionDetail();

    expect(parseTransactionDetailResponse(value)).toBe(value);
  });

  it("rejects rule order or verdicts that disagree with the assessment", () => {
    const wrongOrder = transactionDetail();
    wrongOrder.rules[0] = {
      ...wrongOrder.rules[0]!,
      rule: "element_size",
    };
    expect(() => parseTransactionDetailResponse(wrongOrder)).toThrow(
      "Invalid rule assessment at index 0",
    );

    const wrongVerdict = transactionDetail();
    wrongVerdict.rules[1] = {
      ...wrongVerdict.rules[1]!,
      verdict: "pass",
    };
    expect(() => parseTransactionDetailResponse(wrongVerdict)).toThrow(
      "Inconsistent rule verdict at index 1",
    );
  });

  it("accepts a rule that is both proven and unresolved for other inputs", () => {
    const value = transactionDetail();
    if (value.assessment === null) {
      throw new Error("Fixture unexpectedly lacks an assessment");
    }
    value.assessment.primary_rule = null;
    value.assessment.unknown_rules = ["element_size"];
    const policy = value.classifications.find(
      ({ classifier_id }) => classifier_id === "knots_bip110",
    )!;
    policy.state = "partial";
    policy.missing_facts = ["policy_facts"];
    value.rules[1]!.missing_count = 2;
    value.rules[1]!.missing = [{ location: "witness[1]" }];

    expect(parseTransactionDetailResponse(value)).toBe(value);
  });

  it("accepts a detailed partial shape result without labels", () => {
    const value = transactionDetail();
    const shape = value.classifications.find(
      ({ classifier_id }) => classifier_id === "transaction_shape",
    )!;
    shape.state = "partial";
    shape.primary_label = null;
    shape.labels = [];
    shape.missing_facts = ["input_script_pubkeys"];

    expect(parseTransactionDetailResponse(value)).toBe(value);
  });

  it("requires exact counts with at most one bounded exemplar", () => {
    const tooMany = transactionDetail();
    tooMany.rules[1]!.evidence = [
      { location: "witness[0]" },
      { location: "witness[1]" },
    ];
    expect(() => parseTransactionDetailResponse(tooMany)).toThrow(
      "Invalid rule assessment at index 1",
    );

    const missingExemplar = transactionDetail();
    missingExemplar.rules[1]!.evidence = [];
    expect(() => parseTransactionDetailResponse(missingExemplar)).toThrow(
      "Invalid rule assessment at index 1",
    );
  });

  it("rejects an unclassified payload on the classified detail route", () => {
    const value = transactionDetail() as unknown as Record<string, unknown>;
    value.assessment = null;

    expect(() => parseTransactionDetailResponse(value)).toThrow(
      "Classified transaction detail is missing its assessment",
    );
  });

  it("rejects detailed classifications that repeat a classifier ID", () => {
    const value = transactionDetail();
    value.classifications = [
      ...value.classifications,
      value.classifications[0]!,
    ];

    expect(() => parseTransactionDetailResponse(value)).toThrow(
      "Invalid detailed classification at index 4",
    );
  });

  it("requires the matching classification revision", () => {
    const value = transactionDetail() as unknown as Record<string, unknown>;
    delete value.classification_revision;

    expect(() => parseTransactionDetailResponse(value)).toThrow(
      "Invalid transaction detail response",
    );
  });
});

describe("transactionDetailMatchesSnapshot", () => {
  it("accepts exact and newer revisions with the same compact assessment", () => {
    const currentSnapshot = snapshot();
    const transaction = currentSnapshot.transactions[0]!;
    const exact = transactionDetail();
    const newer = {
      ...transactionDetail(),
      classification_revision: exact.classification_revision + 1,
    };

    expect(
      transactionDetailMatchesSnapshot(currentSnapshot, transaction, exact),
    ).toBe(true);
    expect(
      transactionDetailMatchesSnapshot(currentSnapshot, transaction, newer),
    ).toBe(true);
  });

  it("rejects older revisions and newer changed assessments", () => {
    const currentSnapshot = snapshot();
    const transaction = currentSnapshot.transactions[0]!;
    const older = {
      ...transactionDetail(),
      classification_revision: currentSnapshot.classification_revision - 1,
    };
    const changed = transactionDetail();
    changed.classification_revision += 1;
    changed.assessment = {
      status: "indeterminate",
      primary_rule: null,
      violated_rules: [],
      unknown_rules: ["element_size"],
    };

    expect(
      transactionDetailMatchesSnapshot(currentSnapshot, transaction, older),
    ).toBe(false);
    expect(
      transactionDetailMatchesSnapshot(currentSnapshot, transaction, changed),
    ).toBe(false);
  });
});

describe("parseSourcesResponse", () => {
  it("parses source discovery", () => {
    const parsed = parseSourcesResponse({
      atlas_version: "1.0.0",
      sources: [source()],
    });

    expect(parsed.atlas_version).toBe("1.0.0");
    expect(parsed.sources[0]?.source_id).toBe("core");
  });

  it("rejects an invalid Atlas version", () => {
    expect(() =>
      parseSourcesResponse({
        atlas_version: "latest",
        sources: [source()],
      }),
    ).toThrow("Invalid sources response");
  });

  it("rejects a repeated source ID", () => {
    expect(() =>
      parseSourcesResponse({
        atlas_version: "1.0.0",
        sources: [source(), { ...source(), source_label: "Second reader" }],
      }),
    ).toThrow("Sources response repeats a source ID");
  });

  it("rejects source IDs that normalize as URL dot segments", () => {
    for (const sourceId of [".", ".."]) {
      expect(() =>
        parseSourcesResponse({
          atlas_version: "1.0.0",
          sources: [{ ...source(), source_id: sourceId }],
        }),
      ).toThrow("Invalid source summary");
    }
  });
});

describe("fetchSourceSnapshot", () => {
  it("requests the source-scoped current snapshot", async () => {
    const fetchMock = vi.fn().mockResolvedValue({
      ok: true,
      json: async () => ({ source: source(), snapshot: snapshot() }),
    });
    vi.stubGlobal("fetch", fetchMock);

    await fetchSourceSnapshot("core");

    expect(fetchMock).toHaveBeenCalledWith("/api/v1/sources/core/mempool", {
      headers: { Accept: "application/json" },
    });
  });

  it("passes a cancellation signal to a snapshot request", async () => {
    const fetchMock = vi.fn().mockResolvedValue({
      ok: true,
      json: async () => ({ source: source(), snapshot: snapshot() }),
    });
    vi.stubGlobal("fetch", fetchMock);
    const controller = new AbortController();

    await fetchSourceSnapshot("core", controller.signal);

    expect(fetchMock).toHaveBeenCalledWith("/api/v1/sources/core/mempool", {
      headers: { Accept: "application/json" },
      signal: controller.signal,
    });
  });

  it("requests and validates a source-scoped transaction detail", async () => {
    const fetchMock = vi.fn().mockResolvedValue({
      ok: true,
      json: async () => transactionDetail(),
    });
    vi.stubGlobal("fetch", fetchMock);

    await fetchTransactionDetail("core", TXID);

    expect(fetchMock).toHaveBeenCalledWith(
      `/api/v1/sources/core/transactions/${TXID}`,
      { headers: { Accept: "application/json" } },
    );
  });

  it("surfaces an API error body", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn().mockResolvedValue({
        ok: false,
        status: 404,
        json: async () => ({ error: 'unknown source "missing"' }),
      }),
    );

    await expect(fetchSourceSnapshot("missing")).rejects.toMatchObject({
      status: 404,
      message: 'Atlas request failed (404): unknown source "missing"',
    });
  });
});
