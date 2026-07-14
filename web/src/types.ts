export interface Membership {
  source_id: string;
  txid: string;
  present: boolean;
  updated_at_ms: number;
  evidence_event_id: string;
}

export interface MempoolResponse {
  source_id: string | null;
  memberships: Membership[];
}
