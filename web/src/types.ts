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

export type ClassificationKey =
  | "payment"
  | "consolidation"
  | "batch"
  | "coinjoin"
  | "data"
  | "lightning"
  | "unknown";

export type ScriptTypeKey =
  "p2tr" | "p2wpkh" | "p2wsh" | "p2sh" | "p2pkh" | "op_return" | "other";

export interface SummaryFilter {
  classes?: ClassificationKey[];
  scripts?: ScriptTypeKey[];
  feerate_min?: number;
  feerate_max?: number;
}

export interface AggregateBin {
  count: number;
  vsize: number;
}

export interface SummaryTotals {
  all: AggregateBin;
  matching: AggregateBin;
  awaiting_rpc: { count: number };
}

export interface BinCatalog {
  feerate_sat_per_vb_edges: number[];
  age_ms_edges: number[];
  value_sats_edges: number[];
  input_count_uppers: number[];
  output_count_uppers: number[];
  classification_keys: ClassificationKey[];
  script_keys: ScriptTypeKey[];
}

export type DimensionHistogram =
  | { status: "available"; bins: AggregateBin[]; underived: AggregateBin }
  | { status: "unavailable"; reason: string };

export type SummaryDimension =
  | "classification"
  | "script"
  | "value"
  | "inputs"
  | "outputs"
  | "age"
  | "feerate";

export type SummaryHistograms = Record<SummaryDimension, DimensionHistogram>;

export interface EcdfSeries {
  key: ClassificationKey;
  cum_vsize: number[];
}

export interface FeeRateEcdf {
  fee_edges: number[];
  series: EcdfSeries[];
}

export interface JointFeeSize {
  fee_edges: number[];
  size_edges: number[];
  grid: number[][];
}

export interface MempoolSummary {
  source_id: string;
  as_of_ms: number;
  filter_echo: SummaryFilter;
  totals: SummaryTotals;
  bins: BinCatalog;
  histograms: SummaryHistograms;
  ecdf?: FeeRateEcdf;
  joint_fee_size?: JointFeeSize;
  health: SourceHealth;
}

export interface SourceDescriptor {
  source_id: string;
  last_seen_at_ms: number;
  membership_count: number;
}
