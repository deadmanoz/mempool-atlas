import { afterEach, describe, expect, it, vi } from "vitest";

import {
  fetchSourceSnapshot,
  fetchTransactionDetail,
  parseSourceSnapshotResponse,
  parseSourcesResponse,
  parseTransactionDetailResponse,
  transactionDetailMatchesSnapshot,
} from "./api";
import type { MempoolSnapshot, TransactionDetailResponse } from "./types";

const TXID = "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f";
const BLOCK_HASH = "00".repeat(32);

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
  last_error: null,
});

const snapshot = (): MempoolSnapshot => ({
  source_id: "core",
  source_label: "Bitcoin Core",
  observed_at_ms: 1_700_000_001_000,
  classification_revision: 3,
  chain_tip: { height: 900_000, hash: BLOCK_HASH },
  transaction_count: 1,
  total_vsize: 141,
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
      fee_sats: 423,
      entered_at_ms: 1_699_999_000_000,
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

  it("requires a reader-visible classification revision", () => {
    const value = snapshot() as unknown as Record<string, unknown>;
    delete value.classification_revision;

    expect(() =>
      parseSourceSnapshotResponse({ source: source(), snapshot: value }),
    ).toThrow("Invalid mempool snapshot");
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

  it("accepts a definite violation without a deterministic primary rule", () => {
    const value = snapshot();
    value.transactions[0]!.bip110 = {
      status: "violating",
      primary_rule: null,
      violated_rules: ["element_size"],
      unknown_rules: ["element_size", "undefined_version"],
    };

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
    value.rules[1]!.missing_count = 2;
    value.rules[1]!.missing = [{ location: "witness[1]" }];

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
    expect(
      parseSourcesResponse({ sources: [source()] }).sources[0]?.source_id,
    ).toBe("core");
  });

  it("rejects source IDs that normalize as URL dot segments", () => {
    for (const sourceId of [".", ".."]) {
      expect(() =>
        parseSourcesResponse({
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
