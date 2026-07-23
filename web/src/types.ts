export type SourceAvailability = "waiting" | "ready" | "stale" | "error";

export interface ChainTip {
  height: number;
  hash: string;
}

export interface MempoolTransaction {
  txid: string;
  vsize: number;
  fee_sats: number;
  entered_at_ms: number;
}

export interface MempoolSnapshot {
  source_id: string;
  source_label: string;
  observed_at_ms: number;
  chain_tip: ChainTip;
  transaction_count: number;
  total_vsize: number;
  transactions: MempoolTransaction[];
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
