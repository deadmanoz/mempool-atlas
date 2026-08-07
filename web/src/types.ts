export type SourceAvailability = "waiting" | "ready" | "stale" | "error";
export type ClassificationState = "classifying" | "complete" | "paused";

export interface ClassificationProgress {
  state: ClassificationState;
  revision: number;
  classified_count: number;
  unclassified_count: number;
}

export interface ChainTip {
  height: number;
  hash: string;
}

export const RULE_IDS = [
  "output_size",
  "element_size",
  "undefined_version",
  "taproot_annex",
  "control_block_size",
  "op_success",
  "tapscript_op_if",
] as const;

export type RuleId = (typeof RULE_IDS)[number];
export type Bip110Status = "compatible" | "violating" | "indeterminate";
export type RuleVerdict = "pass" | "violate" | "unknown";

export interface Bip110Assessment {
  status: Bip110Status;
  primary_rule: RuleId | null;
  violated_rules: RuleId[];
  unknown_rules: RuleId[];
}

export interface Bip110Summary {
  evaluator_id: string;
  evaluator_version: string;
  scope: "knots_mempool_policy";
  compatible_count: number;
  violating_count: number;
  indeterminate_count: number;
  unclassified_count: number;
}

export type ClassifierMethodology =
  "exact" | "heuristic" | "fingerprint" | "policy";
export type ClassifierSemantics = "multi_label" | "rule_set";
export type ClassificationResultState = "complete" | "partial";

export interface ClassifierLabelDescriptor {
  key: string;
  label: string;
  description: string;
}

export interface ClassifierDescriptor {
  id: string;
  version: string;
  title: string;
  methodology: ClassifierMethodology;
  semantics: ClassifierSemantics;
  required_facts: string[];
  labels: ClassifierLabelDescriptor[];
}

export interface ClassificationResult {
  classifier_id: string;
  state: ClassificationResultState;
  primary_label: string | null;
  labels: string[];
  missing_facts: string[];
  evidence: unknown | null;
}

export interface ClassifierSummary {
  classifier_id: string;
  complete_count: number;
  partial_count: number;
  unclassified_count: number;
  label_counts: Record<string, number>;
}

export interface TransactionStructure {
  input_count: number;
  output_count: number;
  op_return_bytes: number;
  output_sats: number;
  witness_bytes: number;
}

export interface MempoolTransaction {
  txid: string;
  wtxid: string;
  vsize: number;
  weight: number;
  fee_sats: number;
  entered_at_ms: number;
  ancestor_count: number;
  ancestor_vsize: number;
  /**
   * Delta-adjusted ancestor fees in satoshis. `prioritisetransaction` deltas
   * are signed, so this field is signed and may be negative. The base
   * `fee_sats` above is never negative.
   */
  ancestor_fee_sats: number;
  descendant_count: number;
  descendant_vsize: number;
  replaceable: boolean;
  structure: TransactionStructure | null;
  classifications: ClassificationResult[];
  bip110: Bip110Assessment | null;
}

export interface MempoolSnapshot {
  source_id: string;
  source_label: string;
  collection_started_at_ms: number;
  collection_completed_at_ms: number;
  collection_duration_ms: number;
  observed_at_ms: number;
  classification_revision: number;
  chain_tip: ChainTip;
  transaction_count: number;
  total_vsize: number;
  classifier_catalog: ClassifierDescriptor[];
  classification_summaries: ClassifierSummary[];
  bip110_summary: Bip110Summary;
  transactions: MempoolTransaction[];
}

export interface RuleAssessment {
  rule: RuleId;
  number: number;
  verdict: RuleVerdict;
  evidence_count: number;
  evidence: unknown[];
  missing_count: number;
  missing: unknown[];
}

export interface TransactionDetailResponse {
  source_id: string;
  snapshot_observed_at_ms: number;
  classification_revision: number;
  txid: string;
  wtxid: string;
  classifications: ClassificationResult[];
  assessment: Bip110Assessment;
  rules: RuleAssessment[];
}

export interface SourceSummary {
  source_id: string;
  source_label: string;
  availability: SourceAvailability;
  poll_interval_seconds: number;
  last_poll_started_at_ms: number | null;
  snapshot_observed_at_ms: number | null;
  chain_tip: ChainTip | null;
  transaction_count: number | null;
  total_vsize: number | null;
  classification: ClassificationProgress | null;
  last_error: string | null;
}

export interface SourcesResponse {
  atlas_version: string;
  sources: SourceSummary[];
}

/**
 * One internally coherent v2 publication reconstructed from its manifest and
 * content-addressed stages. Pre-publication unavailability is an explicit v2
 * error and never crosses the transport boundary as a nullable payload.
 */
export interface LoadedSourcePublication {
  source: SourceSummary;
  publication: MempoolSnapshot;
}

export type StageKind =
  "population" | "membership" | "structure" | "classifier";

export interface StageDescriptor {
  kind: StageKind;
  classifier_id?: string;
  content_id: string;
  uncompressed_bytes: number;
  row_count: number;
  dependency_ids: string[];
}

export interface StagedSnapshotManifest {
  schema_version: 2;
  source: SourceSummary;
  source_id: string;
  source_label: string;
  collection_started_at_ms: number;
  collection_completed_at_ms: number;
  collection_duration_ms: number;
  observed_at_ms: number;
  classification_revision: number;
  chain_tip: ChainTip;
  transaction_count: number;
  total_vsize: number;
  classifier_catalog: ClassifierDescriptor[];
  classification_summaries: ClassifierSummary[];
  bip110_summary: Bip110Summary;
  row_count: number;
  population_id: string;
  classification_set_id: string;
  publication_id: string;
  stages: StageDescriptor[];
}
