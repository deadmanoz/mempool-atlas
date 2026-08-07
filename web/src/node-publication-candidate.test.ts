import { describe, expect, it, vi } from "vitest";

import {
  nodePublicationCandidateReadyDetail,
  prepareNodePublicationCommit,
  prepareNodePublicationCandidate,
  retainNodePublicationSelection,
  type NodePublicationInput,
} from "./node-publication-candidate";
import { DEFAULT_FILTERS } from "./filters";
import type { SnapshotDistributionsView } from "./snapshot-distributions-view";
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

const publication = (
  snapshotIdentity = "snapshot-a",
  publicationId = "12".repeat(32),
): LoadedSourcePublication => ({
  source: source(),
  publication_id: publicationId,
  snapshot_identity: snapshotIdentity,
  publication: snapshot(),
});

const publicationInput = (
  classifier: string | null = null,
): NodePublicationInput => ({
  viewState: {
    source: "core",
    classifier,
    selection: null,
    txid: null,
  },
  terrainMode: "count",
  filters: DEFAULT_FILTERS,
  currentSnapshotIdentity: null,
  selectedClassifierLabel: null,
  selectedClassifierBucketKey: null,
  selectedInspector: { kind: "rule", rule: "element_size" },
});

const unusedDistributionsView = {} as SnapshotDistributionsView;

describe("node publication candidate", () => {
  it("resolves the classifier and view state before publication commit", async () => {
    const candidate = await prepareNodePublicationCandidate(
      publication(),
      { source: "core", classifier: null, selection: null, txid: null },
      false,
    );

    expect(candidate.selectedClassifierId).toBe("transaction_properties");
    expect(candidate.selectedClassifierLabel).toBeNull();
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
      candidateKey: "snapshot-a",
      sourceIds: ["core"],
    });
  });

  it("retains primary-stage interaction state when snapshot content is unchanged", async () => {
    const candidate = await prepareNodePublicationCandidate(
      publication("snapshot-a", "34".repeat(32)),
      {
        source: "core",
        classifier: "transaction_properties",
        selection: null,
        txid: null,
      },
      false,
    );
    const bucketKey = "complete:version_2" as const;

    const retained = retainNodePublicationSelection(candidate, {
      viewState: candidate.viewState,
      terrainMode: "count",
      filters: DEFAULT_FILTERS,
      currentSnapshotIdentity: "snapshot-a",
      selectedClassifierLabel: "version_2",
      selectedClassifierBucketKey: bucketKey,
      selectedInspector: { kind: "rule", rule: "taproot_annex" },
    });

    expect(retained.selectedClassifierLabel).toBe("version_2");
    expect(retained.selectedClassifierBucketKey).toBe(bucketKey);
  });

  it("does not carry interaction state into different snapshot content", async () => {
    const candidate = await prepareNodePublicationCandidate(
      publication(),
      {
        source: "core",
        classifier: "transaction_properties",
        selection: null,
        txid: null,
      },
      false,
    );

    const retained = retainNodePublicationSelection(candidate, {
      viewState: candidate.viewState,
      terrainMode: "count",
      filters: DEFAULT_FILTERS,
      currentSnapshotIdentity: "snapshot-b",
      selectedClassifierLabel: "version_2",
      selectedClassifierBucketKey: "complete:version_2",
      selectedInspector: { kind: "rule", rule: "taproot_annex" },
    });

    expect(retained).toBe(candidate);
    expect(retained.selectedClassifierLabel).toBeNull();
    expect(retained.selectedClassifierBucketKey).toBeNull();
  });

  it("retains the policy inspector for the same publication", async () => {
    const candidate = await prepareNodePublicationCandidate(
      publication(),
      {
        source: "core",
        classifier: "knots_bip110",
        selection: null,
        txid: null,
      },
      false,
    );
    const selectedInspector = {
      kind: "rule",
      rule: "taproot_annex",
    } as const;
    const viewState = {
      ...candidate.viewState,
      selection: selectedInspector,
    };

    const retained = retainNodePublicationSelection(candidate, {
      viewState,
      terrainMode: "count",
      filters: DEFAULT_FILTERS,
      currentSnapshotIdentity: candidate.snapshotIdentity,
      selectedClassifierLabel: null,
      selectedClassifierBucketKey: null,
      selectedInspector,
    });

    expect(retained.selectedInspector).toEqual(selectedInspector);
    expect(retained.viewState.selection).toEqual(selectedInspector);
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

  it("chooses the largest indexed BIP-110 rule population", async () => {
    const candidate = await prepareNodePublicationCandidate(
      publication(),
      {
        source: "core",
        classifier: "knots_bip110",
        selection: null,
        txid: null,
      },
      false,
    );

    expect(candidate.selectedInspector).toEqual({
      kind: "rule",
      rule: "output_size",
    });
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

  it("reprepares from the latest node state after one commit invalidation", async () => {
    const initial = publicationInput();
    const changed = publicationInput("knots_bip110");
    let reads = 0;

    const prepared = await prepareNodePublicationCommit(
      publication(),
      false,
      false,
      new AbortController().signal,
      () => true,
      () => (reads++ === 0 ? initial : changed),
      unusedDistributionsView,
    );

    expect(prepared.candidate.selectedClassifierId).toBe("knots_bip110");
    expect(reads).toBe(5);
  });

  it("reprepares when membership filters change before commit", async () => {
    const initial = publicationInput();
    const changed: NodePublicationInput = {
      ...initial,
      filters: { ...DEFAULT_FILTERS, minimumFeeRate: 5 },
    };
    let reads = 0;

    await prepareNodePublicationCommit(
      publication(),
      false,
      false,
      new AbortController().signal,
      () => true,
      () => (reads++ === 0 ? initial : changed),
      unusedDistributionsView,
    );

    expect(reads).toBe(5);
  });

  it("exits after four consecutive node-state invalidations", async () => {
    const isCurrent = vi.fn(() => true);
    let reads = 0;
    const changingInput = (): NodePublicationInput => {
      reads += 1;
      return publicationInput(reads % 2 === 0 ? "knots_bip110" : null);
    };

    await expect(
      prepareNodePublicationCommit(
        publication(),
        false,
        false,
        new AbortController().signal,
        isCurrent,
        changingInput,
        unusedDistributionsView,
      ),
    ).rejects.toThrow(
      "Node publication candidate changed during 4 consecutive commit attempts",
    );

    expect(isCurrent).toHaveBeenCalledTimes(4);
    expect(reads).toBe(8);
  });
});
