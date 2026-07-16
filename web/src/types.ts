export interface MempoolEntry {
  txid: string;
  updated_at_ms: number;
  evidence_event_id: string;
  facts: MempoolFacts;
}

export type MempoolFacts =
  | {
      status: "awaiting_rpc";
    }
  | {
      status: "available";
      vsize: number;
      fee_sats: number;
      entered_at_ms: number;
    };

export type CaptureGapCertainty = "possible_loss" | "known_loss";

export type CaptureStatus =
  | {
      status: "no_reported_gaps";
    }
  | {
      status: "contains_gaps";
      first_gap_at_ms: number;
      latest_gap_at_ms: number;
      marker_count: number;
      strongest_certainty: CaptureGapCertainty;
      latest_input: string;
      latest_reason: string;
    };

export interface SourceHealth {
  last_seen_at_ms: number;
  capture: CaptureStatus;
}

export interface MempoolResponse {
  source_id: string;
  health: SourceHealth;
  memberships: MempoolEntry[];
}
