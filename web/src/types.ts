export type SourceAvailability = "waiting" | "ready" | "stale" | "error";

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

export interface MempoolTransaction {
  txid: string;
  wtxid: string;
  vsize: number;
  fee_sats: number;
  entered_at_ms: number;
  bip110: Bip110Assessment | null;
}

export interface MempoolSnapshot {
  source_id: string;
  source_label: string;
  observed_at_ms: number;
  chain_tip: ChainTip;
  transaction_count: number;
  total_vsize: number;
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
  txid: string;
  wtxid: string;
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
  last_error: string | null;
}

export interface SourcesResponse {
  sources: SourceSummary[];
}

export interface SourceSnapshotResponse {
  source: SourceSummary;
  snapshot: MempoolSnapshot | null;
}
