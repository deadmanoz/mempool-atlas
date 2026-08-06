import type {
  Bip110Assessment,
  Bip110Summary,
  MempoolSnapshot,
  MempoolTransaction,
  RuleId,
} from "./types";
import type { LoadedSourceSnapshot } from "./comparison-model";

export const violating = (
  violatedRules: RuleId[],
  unknownRules: RuleId[] = [],
): Bip110Assessment => ({
  status: "violating",
  primary_rule: violatedRules[0] ?? null,
  violated_rules: violatedRules,
  unknown_rules: unknownRules,
});

const summary = (
  transactions: readonly MempoolTransaction[],
): Bip110Summary => ({
  evaluator_id: "rdts-rules",
  evaluator_version: "0.1.0",
  scope: "knots_mempool_policy",
  compatible_count: transactions.filter(
    ({ bip110 }) => bip110?.status === "compatible",
  ).length,
  violating_count: transactions.filter(
    ({ bip110 }) => bip110?.status === "violating",
  ).length,
  indeterminate_count: transactions.filter(
    ({ bip110 }) => bip110?.status === "indeterminate",
  ).length,
  unclassified_count: transactions.filter(({ bip110 }) => bip110 === null)
    .length,
});

export const loadedSource = (
  sourceId: string,
  transactions: MempoolTransaction[],
  observedAtMs?: number,
): LoadedSourceSnapshot => {
  const observationTime = observedAtMs ?? 1_700_000_001_000;
  const snapshot: MempoolSnapshot = {
    source_id: sourceId,
    source_label:
      observedAtMs === undefined
        ? `${sourceId.toUpperCase()} node`
        : sourceId.toUpperCase(),
    collection_started_at_ms: observationTime - 1_000,
    collection_completed_at_ms: observationTime,
    collection_duration_ms: 1_000,
    observed_at_ms: observationTime,
    classification_revision: 1,
    chain_tip: { height: 900_000, hash: "00".repeat(32) },
    transaction_count: transactions.length,
    total_vsize: transactions.reduce((total, entry) => total + entry.vsize, 0),
    classifier_catalog: [],
    classification_summaries: [],
    bip110_summary: summary(transactions),
    transactions,
  };
  return {
    source: {
      source_id: sourceId,
      source_label: snapshot.source_label,
      availability: "ready",
      poll_interval_seconds: 300,
      last_poll_started_at_ms: snapshot.collection_started_at_ms,
      snapshot_observed_at_ms: snapshot.observed_at_ms,
      chain_tip: snapshot.chain_tip,
      transaction_count: snapshot.transaction_count,
      total_vsize: snapshot.total_vsize,
      classification: {
        state: "complete",
        revision: snapshot.classification_revision,
        classified_count:
          snapshot.transaction_count -
          snapshot.bip110_summary.unclassified_count,
        unclassified_count: snapshot.bip110_summary.unclassified_count,
      },
      last_error: null,
    },
    snapshot,
  };
};
