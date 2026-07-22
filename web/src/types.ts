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

export type ScriptTypeKey =
  "p2tr" | "p2wpkh" | "p2wsh" | "p2sh" | "p2pkh" | "op_return" | "other";

/** One verdict a taxonomy can assign, e.g. `{key: "payment", label: "Payment"}`
 * within the "behavior" taxonomy. Keys are server-defined and opaque to the
 * client beyond the reserved "unknown" sentinel every taxonomy must carry. */
export interface VerdictDescriptor {
  key: string;
  label: string;
}

/** One independent classification axis the server derives from raw
 * transaction bytes, e.g. "behavior" today and, later, others such as
 * "bip110". The client treats the set of taxonomies as open-ended. */
export interface TaxonomyDescriptor {
  key: string;
  label: string;
  verdicts: VerdictDescriptor[];
}

/** A filter selection for one taxonomy: the verdict keys to match within it. */
export interface TaxonomyFilterSelection {
  key: string;
  verdicts: string[];
}

export interface SummaryFilter {
  taxonomies?: TaxonomyFilterSelection[];
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
  taxonomies: TaxonomyDescriptor[];
  feerate_sat_per_vb_edges: number[];
  age_ms_edges: number[];
  value_sats_edges: number[];
  input_count_uppers: number[];
  output_count_uppers: number[];
  script_keys: ScriptTypeKey[];
}

export type DimensionHistogram =
  | { status: "available"; bins: AggregateBin[]; underived: AggregateBin }
  | { status: "unavailable"; reason: string };

/** A dimension histogram for one taxonomy, tagged with the taxonomy key it
 * belongs to so the client can align it against the bin catalog without
 * relying on array position alone. */
export type TaxonomyHistogram = { key: string } & DimensionHistogram;

/** The fixed transaction-shape facets, independent of any taxonomy. */
export type ShapeDimension =
  "script" | "value" | "inputs" | "outputs" | "age" | "feerate";

export type SummaryHistograms = {
  taxonomies: TaxonomyHistogram[];
} & Record<ShapeDimension, DimensionHistogram>;

export interface EcdfSeries {
  key: string;
  cum_vsize: number[];
}

export interface FeeRateEcdf {
  /** Which taxonomy's verdict keys the series `key` values belong to. */
  taxonomy: string;
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

/** The rich aggregate over one membership-set region of a comparison, shaped
 * exactly like a summary's matching set so the same histogram code renders it.
 * `bins` and `histograms` are the summary wire shapes. */
export interface RegionAggregate {
  present: AggregateBin;
  awaiting_rpc: { count: number };
  bins: BinCatalog;
  histograms: SummaryHistograms;
}

/** A region reported by count and virtual size only. Used for the anomaly
 * region of a stage, which is expected to be empty under a clean permissiveness
 * ordering and does not warrant full histograms. */
export interface AnomalyRegion {
  present: AggregateBin;
  awaiting_rpc: { count: number };
}

/** One adjacent step in the asserted source order. `added` is present in `to`
 * but absent from `from`; `anomaly` is the reverse membership difference. A
 * set difference alone is not evidence that either source rejected a tx. */
export interface ComparisonStage {
  from: string;
  to: string;
  added: RegionAggregate;
  anomaly: AnomalyRegion;
}

/** One compared source's total current membership, for context alongside the
 * derived regions. */
export interface ComparisonSourceTotal {
  source_id: string;
  present: AggregateBin;
  awaiting_rpc: { count: number };
}

/** A read-time derived comparison across 2..4 independent source snapshots.
 * The caller supplies the source order (least to most permissive); the server
 * reports the staged deltas along it and flags where the nesting does not
 * hold, never mutating or combining the underlying sources. */
export interface SourceComparison {
  sources: string[];
  as_of_ms: number;
  source_totals: ComparisonSourceTotal[];
  /** Transactions present in every compared source. */
  shared: RegionAggregate;
  /** One entry per adjacent source pair, in request order. */
  stages: ComparisonStage[];
}

/** The bounded recent rejection window. `oldest_at_ms`/`newest_at_ms` are
 * absent only when the window is empty. */
export interface RejectionWindow {
  count: number;
  oldest_at_ms?: number;
  newest_at_ms?: number;
}

/** Count of rejections carrying one node-provided reason string. Reasons are
 * the source's own free-form text. */
export interface RejectionReasonCount {
  reason: string;
  count: number;
  /** True only for the server's synthetic long-tail reason bucket. */
  is_rollup: boolean;
}

export interface RejectionVerdictCount {
  verdict: string;
  count: number;
}

/** Per-taxonomy verdict counts over the classified rejections in the window,
 * in the taxonomy's declared verdict order. */
export interface RejectionTaxonomyBreakdown {
  key: string;
  label: string;
  verdicts: RejectionVerdictCount[];
}

/** Best-effort classification attribution over the window. Rejections without
 * stored derived verdicts are reported honestly as `unclassified_count`, never
 * guessed into a verdict. */
export interface RejectionAttribution {
  classified_count: number;
  unclassified_count: number;
  taxonomies: RejectionTaxonomyBreakdown[];
}

/** One rejection in the recent list. `verdicts` carries stored derived
 * `[taxonomy, verdict]` pairs when available and is empty otherwise. */
export interface RejectionRecord {
  txid: string;
  reason: string;
  observed_at_ms: number;
  evidence_event_id: string;
  verdicts: [string, string][];
}

/** The source-scoped rejection read model: what a single source refused,
 * distinct from any set difference in a comparison. */
export interface SourceRejections {
  source_id: string;
  as_of_ms: number;
  window: RejectionWindow;
  by_reason: RejectionReasonCount[];
  attribution: RejectionAttribution;
  recent: RejectionRecord[];
  next_cursor?: string;
}
