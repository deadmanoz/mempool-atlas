import { describe, expect, it } from "vitest";

import {
  nodePublicationCandidateReadyDetail,
  prepareNodePublicationCandidate,
} from "./node-publication-candidate";
import { mempoolTransaction } from "./test-fixtures";
import type {
  ClassifierDescriptor,
  LoadedSourcePublication,
  MempoolSnapshot,
  SourceSummary,
} from "./types";

const properties: ClassifierDescriptor = {
  id: "transaction_properties",
  version: "1",
  title: "Transaction properties",
  methodology: "exact",
  semantics: "multi_label",
  required_facts: ["raw_transaction"],
  labels: [],
};

const bip110: ClassifierDescriptor = {
  id: "knots_bip110",
  version: "1",
  title: "BIP-110",
  methodology: "policy",
  semantics: "rule_set",
  required_facts: ["raw_transaction"],
  labels: [],
};

const snapshot = (): MempoolSnapshot => ({
  source_id: "core",
  source_label: "Core",
  collection_started_at_ms: 1_700_000_000_000,
  collection_completed_at_ms: 1_700_000_001_000,
  collection_duration_ms: 1_000,
  observed_at_ms: 1_700_000_001_000,
  classification_revision: 7,
  chain_tip: { height: 900_000, hash: "00".repeat(32) },
  transaction_count: 2,
  total_vsize: 400,
  classifier_catalog: [properties, bip110],
  classification_summaries: [],
  bip110_summary: {
    evaluator_id: "rdts-rules",
    evaluator_version: "1",
    scope: "knots_mempool_policy",
    compatible_count: 0,
    violating_count: 2,
    indeterminate_count: 0,
    unclassified_count: 0,
  },
  transactions: [
    mempoolTransaction(1, {
      bip110: {
        status: "violating",
        primary_rule: "output_size",
        violated_rules: ["output_size"],
        unknown_rules: [],
      },
    }),
    mempoolTransaction(2, {
      bip110: {
        status: "violating",
        primary_rule: "output_size",
        violated_rules: ["output_size"],
        unknown_rules: [],
      },
    }),
  ],
});

const source = (): SourceSummary => ({
  source_id: "core",
  source_label: "Core",
  availability: "ready",
  poll_interval_seconds: 30,
  last_poll_started_at_ms: 1_700_000_000_000,
  snapshot_observed_at_ms: 1_700_000_001_000,
  chain_tip: { height: 900_000, hash: "00".repeat(32) },
  transaction_count: 2,
  total_vsize: 400,
  classification: {
    state: "complete",
    revision: 7,
    classified_count: 2,
    unclassified_count: 0,
  },
  last_error: null,
});

const publication = (): LoadedSourcePublication => ({
  source: source(),
  publication: snapshot(),
});

describe("prepareNodePublicationCandidate", () => {
  it("resolves the classifier and view state before publication commit", async () => {
    const candidate = await prepareNodePublicationCandidate(
      publication(),
      { source: "core", classifier: null, selection: null, txid: null },
      false,
    );

    expect(candidate.selectedClassifierId).toBe("transaction_properties");
    expect(candidate.selectedClassifierBucketKey).toBeNull();
    expect(candidate.selectedInspector).toEqual({
      kind: "rule",
      rule: "element_size",
    });
    expect(candidate.viewState).toEqual({
      source: "core",
      classifier: "transaction_properties",
      selection: null,
      txid: null,
    });
    expect(nodePublicationCandidateReadyDetail(candidate)).toEqual({
      surface: "node",
      candidateKey: "core:1700000001000:7",
      sourceIds: ["core"],
    });
  });

  it("preserves an explicit BIP-110 selection", async () => {
    const selection = { kind: "rule", rule: "taproot_annex" } as const;
    const candidate = await prepareNodePublicationCandidate(
      publication(),
      {
        source: "core",
        classifier: "knots_bip110",
        selection,
        txid: null,
      },
      false,
    );

    expect(candidate.selectedClassifierId).toBe("knots_bip110");
    expect(candidate.selectedInspector).toEqual(selection);
    expect(candidate.viewState.selection).toEqual(selection);
  });

  it("rejects an incoherent renderer stage before exposing a candidate", async () => {
    await expect(
      prepareNodePublicationCandidate(
        publication(),
        { source: "core", classifier: null, selection: null, txid: null },
        true,
      ),
    ).rejects.toThrow("Publication readiness does not match its renderer");
  });

  it("rejects missing classification progress before commit", async () => {
    const response = publication();
    response.source.classification = null;

    await expect(
      prepareNodePublicationCandidate(
        response,
        { source: "core", classifier: null, selection: null, txid: null },
        false,
      ),
    ).rejects.toThrow("Loaded publication is missing classification progress");
  });
});
