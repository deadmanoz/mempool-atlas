//! Aggregate mempool summary wire contract.
//!
//! Every workbench view is a function of `{count, vsize}` aggregates over the
//! canonical bins defined here, so summary payload size is independent of
//! mempool depth. The bin edges are part of the wire contract and travel with
//! every summary in [`BinCatalog`] so clients never hardcode them.
//!
//! Aggregates only ever cover fact-bearing memberships. Entries still awaiting
//! RPC facts have no virtual size, fee, or entry time, so they are reported as
//! a separate count and never folded into any histogram or vsize sum.
//!
//! Classification is not one hardcoded dimension. Each classifier pack owns a
//! taxonomy — a keyed verdict vocabulary described by [`TaxonomyDescriptor`] —
//! and the catalog, histograms, and filters carry one entry per taxonomy.
//! Verdict vocabularies are per-pack data on the wire, so clients never
//! hardcode them.

use serde::{Deserialize, Serialize};

use crate::{ModelError, SourceHealth, SourceId};

/// Interior fee-rate bin edges in sat/vB. Bins are half-open `[lower, upper)`
/// with an implicit zero lower bound and unbounded top bin:
/// `<1, 1–2, 2–4, 4–8, 8–16, 16–32, 32–64, 64–128, 128+`.
pub const FEERATE_EDGES_SAT_PER_VB: [f64; 8] = [1.0, 2.0, 4.0, 8.0, 16.0, 32.0, 64.0, 128.0];
pub const FEERATE_BIN_COUNT: usize = FEERATE_EDGES_SAT_PER_VB.len() + 1;

/// Interior age bin edges in milliseconds since the node-reported entry time:
/// `<10m, 10–60m, 1–6h, 6–24h, 1–3d, 3d+`.
pub const AGE_EDGES_MS: [u64; 5] = [600_000, 3_600_000, 21_600_000, 86_400_000, 259_200_000];
pub const AGE_BIN_COUNT: usize = AGE_EDGES_MS.len() + 1;

/// Interior total-output-value bin edges in integer satoshis:
/// `<0.001, 0.001–0.01, 0.01–0.1, 0.1–1, 1–10, 10+` BTC.
pub const VALUE_EDGES_SATS: [u64; 5] = [100_000, 1_000_000, 10_000_000, 100_000_000, 1_000_000_000];
pub const VALUE_BIN_COUNT: usize = VALUE_EDGES_SATS.len() + 1;

/// Inclusive upper bounds of the input-count bands: `1, 2–5, 6–20, 21–100, 100+`.
pub const INPUT_COUNT_UPPERS: [u64; 4] = [1, 5, 20, 100];
pub const INPUT_BIN_COUNT: usize = INPUT_COUNT_UPPERS.len() + 1;

/// Inclusive upper bounds of the output-count bands: `1, 2, 3–10, 11–50, 50+`.
pub const OUTPUT_COUNT_UPPERS: [u64; 4] = [1, 2, 10, 50];
pub const OUTPUT_BIN_COUNT: usize = OUTPUT_COUNT_UPPERS.len() + 1;

/// Fine log-spaced fee-rate bins backing the per-verdict fee-rate ECDF.
pub const ECDF_FEE_BIN_COUNT: usize = 64;
pub const ECDF_FEE_MIN_SAT_PER_VB: f64 = 0.5;
pub const ECDF_FEE_MAX_SAT_PER_VB: f64 = 512.0;

/// Log-spaced grid backing the joint fee-rate × virtual-size heatmap.
pub const JOINT_FEE_BIN_COUNT: usize = 22;
pub const JOINT_FEE_MIN_SAT_PER_VB: f64 = 1.0;
pub const JOINT_FEE_MAX_SAT_PER_VB: f64 = 512.0;
pub const JOINT_SIZE_BIN_COUNT: usize = 14;
pub const JOINT_SIZE_MIN_VB: f64 = 100.0;
pub const JOINT_SIZE_MAX_VB: f64 = 100_000.0;

/// The behavior taxonomy's verdict vocabulary, shared by the baseline
/// heuristics pack and the seed tooling. `Unknown` is a first-class verdict:
/// it states that no classifier produced evidence, never that a classifier
/// ran and failed to match. Other taxonomies declare their own vocabularies
/// as [`TaxonomyDescriptor`] data instead of a shared enum.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Classification {
    Payment,
    Consolidation,
    Batch,
    Coinjoin,
    Data,
    Lightning,
    Unknown,
}

impl Classification {
    pub const ALL: [Self; 7] = [
        Self::Payment,
        Self::Consolidation,
        Self::Batch,
        Self::Coinjoin,
        Self::Data,
        Self::Lightning,
        Self::Unknown,
    ];

    #[must_use]
    pub const fn key(self) -> &'static str {
        match self {
            Self::Payment => "payment",
            Self::Consolidation => "consolidation",
            Self::Batch => "batch",
            Self::Coinjoin => "coinjoin",
            Self::Data => "data",
            Self::Lightning => "lightning",
            Self::Unknown => "unknown",
        }
    }

    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Payment => "Payment",
            Self::Consolidation => "Consolidation",
            Self::Batch => "Batch payout",
            Self::Coinjoin => "CoinJoin",
            Self::Data => "Data / inscription",
            Self::Lightning => "Lightning",
            Self::Unknown => "Unknown",
        }
    }

    #[must_use]
    pub const fn index(self) -> usize {
        match self {
            Self::Payment => 0,
            Self::Consolidation => 1,
            Self::Batch => 2,
            Self::Coinjoin => 3,
            Self::Data => 4,
            Self::Lightning => 5,
            Self::Unknown => 6,
        }
    }

    #[must_use]
    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|value| value.key() == key)
    }
}

/// Canonical script-type facets. A transaction's script type is a single
/// derived label chosen by a documented dominance rule; `Other` covers
/// recognized-but-uncommon output types, not missing evidence.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ScriptType {
    P2tr,
    P2wpkh,
    P2wsh,
    P2sh,
    P2pkh,
    OpReturn,
    Other,
}

impl ScriptType {
    pub const ALL: [Self; 7] = [
        Self::P2tr,
        Self::P2wpkh,
        Self::P2wsh,
        Self::P2sh,
        Self::P2pkh,
        Self::OpReturn,
        Self::Other,
    ];

    #[must_use]
    pub const fn key(self) -> &'static str {
        match self {
            Self::P2tr => "p2tr",
            Self::P2wpkh => "p2wpkh",
            Self::P2wsh => "p2wsh",
            Self::P2sh => "p2sh",
            Self::P2pkh => "p2pkh",
            Self::OpReturn => "op_return",
            Self::Other => "other",
        }
    }

    #[must_use]
    pub const fn index(self) -> usize {
        match self {
            Self::P2tr => 0,
            Self::P2wpkh => 1,
            Self::P2wsh => 2,
            Self::P2sh => 3,
            Self::P2pkh => 4,
            Self::OpReturn => 5,
            Self::Other => 6,
        }
    }

    #[must_use]
    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|value| value.key() == key)
    }
}

/// One verdict a taxonomy can assign, with its human-readable label.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct VerdictDescriptor {
    pub key: String,
    pub label: String,
}

/// One classifier pack's verdict vocabulary. Verdict order defines bin order
/// on the wire: a [`TaxonomyHistogram`] for this taxonomy has exactly one bin
/// per descriptor, in this order. Every taxonomy must include an `unknown`
/// verdict as the honest no-evidence/no-match bucket. Keys are lowercase
/// `[a-z0-9_]+`.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct TaxonomyDescriptor {
    pub key: String,
    pub label: String,
    pub verdicts: Vec<VerdictDescriptor>,
}

/// One taxonomy's histogram over the matching set. The flattened histogram's
/// bins align with the verdict order the catalog declares for `key`.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct TaxonomyHistogram {
    pub key: String,
    #[serde(flatten)]
    pub histogram: DimensionHistogram,
}

/// A filter over one taxonomy: match transactions whose verdict for the
/// taxonomy `key` is one of `verdicts`.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct TaxonomyFilter {
    pub key: String,
    pub verdicts: Vec<String>,
}

/// Server-side filter facets. Facets use known-to-match semantics: a facet
/// selects only transactions whose evidence actually carries a matching value,
/// so filtering on a dimension without derived evidence matches nothing rather
/// than guessing. Fee-rate bounds are inclusive.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct SummaryFilter {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub taxonomies: Vec<TaxonomyFilter>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scripts: Option<Vec<ScriptType>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub feerate_min: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub feerate_max: Option<f64>,
}

impl SummaryFilter {
    pub fn validate(&self) -> Result<(), ModelError> {
        let mut seen_taxonomies = std::collections::HashSet::new();
        for taxonomy in &self.taxonomies {
            if taxonomy.verdicts.is_empty() {
                return Err(ModelError::EmptyTaxonomyFilter {
                    taxonomy: taxonomy.key.clone(),
                });
            }
            if !seen_taxonomies.insert(taxonomy.key.as_str()) {
                return Err(ModelError::DuplicateTaxonomyFilter {
                    taxonomy: taxonomy.key.clone(),
                });
            }
        }
        if self.scripts.as_ref().is_some_and(Vec::is_empty) {
            return Err(ModelError::EmptyFilterFacet { facet: "script" });
        }
        for (field, bound) in [
            ("feerate_min", self.feerate_min),
            ("feerate_max", self.feerate_max),
        ] {
            if bound.is_some_and(|value| !value.is_finite() || value < 0.0) {
                return Err(ModelError::InvalidFilterBound { field });
            }
        }
        if let (Some(min), Some(max)) = (self.feerate_min, self.feerate_max)
            && min > max
        {
            return Err(ModelError::InvertedFeerateBounds);
        }
        Ok(())
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

/// Optional detail blocks a summary request may ask for.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct SummaryDetail {
    pub ecdf: bool,
    pub joint_fee_size: bool,
}

/// Count of transactions plus their summed virtual size.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct AggregateBin {
    pub count: u64,
    pub vsize: u64,
}

impl AggregateBin {
    pub fn add(&mut self, vsize: u64) {
        self.count += 1;
        self.vsize += vsize;
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SummaryTotals {
    /// Every fact-bearing membership of the source.
    pub all: AggregateBin,
    /// The fact-bearing subset selected by the request filter.
    pub matching: AggregateBin,
    /// Memberships with no RPC facts yet; they carry no vsize and are never
    /// part of `all`, `matching`, or any histogram.
    pub awaiting_rpc: AwaitingRpcTotal,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct AwaitingRpcTotal {
    pub count: u64,
}

/// The canonical bins, emitted with every summary as the single source of
/// truth for axis labels and bin alignment. Static dimensions keep fixed
/// edges; taxonomy bins are whatever the registered classifier packs declare.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct BinCatalog {
    pub taxonomies: Vec<TaxonomyDescriptor>,
    pub feerate_sat_per_vb_edges: Vec<f64>,
    pub age_ms_edges: Vec<u64>,
    pub value_sats_edges: Vec<u64>,
    pub input_count_uppers: Vec<u64>,
    pub output_count_uppers: Vec<u64>,
    pub script_keys: Vec<ScriptType>,
}

impl BinCatalog {
    /// The canonical static edges plus the given taxonomy vocabularies.
    #[must_use]
    pub fn for_taxonomies(taxonomies: Vec<TaxonomyDescriptor>) -> Self {
        Self {
            taxonomies,
            feerate_sat_per_vb_edges: FEERATE_EDGES_SAT_PER_VB.to_vec(),
            age_ms_edges: AGE_EDGES_MS.to_vec(),
            value_sats_edges: VALUE_EDGES_SATS.to_vec(),
            input_count_uppers: INPUT_COUNT_UPPERS.to_vec(),
            output_count_uppers: OUTPUT_COUNT_UPPERS.to_vec(),
            script_keys: ScriptType::ALL.to_vec(),
        }
    }
}

/// One dimension's 1-D histogram over the matching set, or an explicit
/// statement that the dimension cannot be derived at all. Available bins
/// align with the canonical bin order of [`BinCatalog`] and include zero
/// bins. `underived` aggregates matching rows whose evidence does not carry
/// this dimension (no raw transaction was observed for them); those rows are
/// never guessed into a bin. Taxonomy histograms are the exception: a row
/// without classifier evidence has the explicit verdict `unknown`, so it
/// lands in the taxonomy's `unknown` bin and the taxonomy's `underived`
/// stays zero.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum DimensionHistogram {
    Available {
        bins: Vec<AggregateBin>,
        underived: AggregateBin,
    },
    Unavailable {
        reason: UnavailableReason,
    },
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum UnavailableReason {
    RequiresRawTransaction,
}

/// One [`TaxonomyHistogram`] per registered taxonomy, in catalog order,
/// followed by the static dimensions.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct SummaryHistograms {
    pub taxonomies: Vec<TaxonomyHistogram>,
    pub script: DimensionHistogram,
    pub value: DimensionHistogram,
    pub inputs: DimensionHistogram,
    pub outputs: DimensionHistogram,
    pub age: DimensionHistogram,
    pub feerate: DimensionHistogram,
}

/// Cumulative vsize per verdict of one taxonomy over fine log-spaced
/// fee-rate bins. `cum_vsize[i]` is the summed vsize of the verdict at fee
/// rates up to and including bin `i`; clients normalize against the final
/// element.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct FeeRateEcdf {
    /// The taxonomy whose verdict keys the series belong to.
    pub taxonomy: String,
    /// `ECDF_FEE_BIN_COUNT + 1` edges in sat/vB; values outside the range
    /// clamp into the first or last bin.
    pub fee_edges: Vec<f64>,
    /// One series per verdict with matching weight, in declared verdict order.
    pub series: Vec<EcdfSeries>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct EcdfSeries {
    pub key: String,
    pub cum_vsize: Vec<u64>,
}

/// Summed vsize over a log-spaced fee-rate × virtual-size grid.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct JointFeeSize {
    /// `JOINT_FEE_BIN_COUNT + 1` fee-rate edges in sat/vB.
    pub fee_edges: Vec<f64>,
    /// `JOINT_SIZE_BIN_COUNT + 1` virtual-size edges in vB.
    pub size_edges: Vec<f64>,
    /// Row-major summed vsize: `grid[size_bin][fee_bin]`, both ascending.
    pub grid: Vec<Vec<u64>>,
}

/// The aggregate read model for one source, shaped so payload size depends on
/// the fixed bin catalog rather than mempool depth.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct MempoolSummary {
    pub source_id: SourceId,
    /// Server time the summary was computed at, in epoch milliseconds. Age
    /// bins are measured against this instant.
    pub as_of_ms: u64,
    pub filter_echo: SummaryFilter,
    pub totals: SummaryTotals,
    pub bins: BinCatalog,
    pub histograms: SummaryHistograms,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ecdf: Option<FeeRateEcdf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub joint_fee_size: Option<JointFeeSize>,
    pub health: SourceHealth,
}

/// One known source, for source discovery and selection.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SourceDescriptor {
    pub source_id: SourceId,
    pub last_seen_at_ms: u64,
    /// Current memberships including entries still awaiting RPC facts.
    pub membership_count: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SourcesResponse {
    pub sources: Vec<SourceDescriptor>,
}

/// Index of the half-open canonical fee-rate bin containing `feerate_sat_per_vb`.
#[must_use]
pub fn feerate_bin(feerate_sat_per_vb: f64) -> usize {
    FEERATE_EDGES_SAT_PER_VB
        .iter()
        .position(|edge| feerate_sat_per_vb < *edge)
        .unwrap_or(FEERATE_EDGES_SAT_PER_VB.len())
}

/// Index of the half-open canonical age bin containing `age_ms`.
#[must_use]
pub fn age_bin(age_ms: u64) -> usize {
    AGE_EDGES_MS
        .iter()
        .position(|edge| age_ms < *edge)
        .unwrap_or(AGE_EDGES_MS.len())
}

/// Index of the half-open canonical value bin containing `total_output_sats`.
#[must_use]
pub fn value_bin(total_output_sats: u64) -> usize {
    VALUE_EDGES_SATS
        .iter()
        .position(|edge| total_output_sats < *edge)
        .unwrap_or(VALUE_EDGES_SATS.len())
}

/// Index of the inclusive-upper count band containing `count`, for the
/// input/output-count bin definitions.
#[must_use]
pub fn count_band_bin(count: u64, uppers: &[u64]) -> usize {
    uppers
        .iter()
        .position(|upper| count <= *upper)
        .unwrap_or(uppers.len())
}

/// `bin_count + 1` log-spaced edges from `min` to `max`, with exact endpoints.
#[must_use]
pub fn log_spaced_edges(min: f64, max: f64, bin_count: usize) -> Vec<f64> {
    let (log_min, log_max) = (min.ln(), max.ln());
    let mut edges = Vec::with_capacity(bin_count + 1);
    edges.push(min);
    for index in 1..bin_count {
        let fraction = index as f64 / bin_count as f64;
        edges.push((log_min + fraction * (log_max - log_min)).exp());
    }
    edges.push(max);
    edges
}

/// Index of the log-spaced bin containing `value`; out-of-range values clamp
/// into the first or last bin.
#[must_use]
pub fn log_bin(value: f64, min: f64, max: f64, bin_count: usize) -> usize {
    if value <= min {
        return 0;
    }
    if value >= max {
        return bin_count - 1;
    }
    let fraction = (value.ln() - min.ln()) / (max.ln() - min.ln());
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let index = (fraction * bin_count as f64) as usize;
    index.min(bin_count - 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn behavior_taxonomy() -> TaxonomyDescriptor {
        TaxonomyDescriptor {
            key: "behavior".to_owned(),
            label: "Behavior".to_owned(),
            verdicts: Classification::ALL
                .into_iter()
                .map(|classification| VerdictDescriptor {
                    key: classification.key().to_owned(),
                    label: classification.label().to_owned(),
                })
                .collect(),
        }
    }

    fn taxonomy_filter(key: &str, verdicts: &[&str]) -> TaxonomyFilter {
        TaxonomyFilter {
            key: key.to_owned(),
            verdicts: verdicts
                .iter()
                .map(|verdict| (*verdict).to_owned())
                .collect(),
        }
    }

    #[test]
    fn facet_keys_round_trip_between_wire_and_lookup() {
        for classification in Classification::ALL {
            assert_eq!(
                serde_json::to_value(classification).expect("serialize"),
                serde_json::json!(classification.key())
            );
            assert_eq!(
                Classification::from_key(classification.key()),
                Some(classification)
            );
            assert_eq!(Classification::ALL[classification.index()], classification);
        }
        for script in ScriptType::ALL {
            assert_eq!(
                serde_json::to_value(script).expect("serialize"),
                serde_json::json!(script.key())
            );
            assert_eq!(ScriptType::from_key(script.key()), Some(script));
            assert_eq!(ScriptType::ALL[script.index()], script);
        }
        assert_eq!(Classification::from_key("snazzy"), None);
        assert_eq!(ScriptType::from_key("opreturn"), None);
    }

    #[test]
    fn classification_labels_are_human_readable() {
        let labels: Vec<&str> = Classification::ALL
            .into_iter()
            .map(Classification::label)
            .collect();
        assert_eq!(
            labels,
            vec![
                "Payment",
                "Consolidation",
                "Batch payout",
                "CoinJoin",
                "Data / inscription",
                "Lightning",
                "Unknown",
            ]
        );
    }

    #[test]
    fn catalog_for_taxonomies_carries_them_with_the_static_edges() {
        let catalog = BinCatalog::for_taxonomies(vec![behavior_taxonomy()]);
        assert_eq!(catalog.taxonomies, vec![behavior_taxonomy()]);
        assert_eq!(
            catalog.taxonomies[0].verdicts.len(),
            Classification::ALL.len()
        );
        assert_eq!(
            catalog.feerate_sat_per_vb_edges.len() + 1,
            FEERATE_BIN_COUNT
        );
        assert_eq!(catalog.age_ms_edges.len() + 1, AGE_BIN_COUNT);
        assert_eq!(catalog.value_sats_edges.len() + 1, VALUE_BIN_COUNT);
        assert_eq!(catalog.input_count_uppers.len() + 1, INPUT_BIN_COUNT);
        assert_eq!(catalog.output_count_uppers.len() + 1, OUTPUT_BIN_COUNT);
        assert_eq!(catalog.script_keys.len(), ScriptType::ALL.len());

        let empty = BinCatalog::for_taxonomies(Vec::new());
        assert!(empty.taxonomies.is_empty());
        assert_eq!(
            empty.feerate_sat_per_vb_edges,
            catalog.feerate_sat_per_vb_edges
        );
    }

    #[test]
    fn feerate_bins_are_half_open_on_their_upper_edge() {
        assert_eq!(feerate_bin(0.0), 0);
        assert_eq!(feerate_bin(0.99), 0);
        assert_eq!(feerate_bin(1.0), 1);
        assert_eq!(feerate_bin(7.99), 3);
        assert_eq!(feerate_bin(8.0), 4);
        assert_eq!(feerate_bin(128.0), 8);
        assert_eq!(feerate_bin(100_000.0), 8);
    }

    #[test]
    fn age_bins_are_half_open_on_their_upper_edge() {
        assert_eq!(age_bin(0), 0);
        assert_eq!(age_bin(599_999), 0);
        assert_eq!(age_bin(600_000), 1);
        assert_eq!(age_bin(86_400_000), 4);
        assert_eq!(age_bin(u64::MAX), 5);
    }

    #[test]
    fn value_and_count_bins_respect_their_edge_semantics() {
        assert_eq!(value_bin(0), 0);
        assert_eq!(value_bin(99_999), 0);
        assert_eq!(value_bin(100_000), 1);
        assert_eq!(value_bin(1_000_000_000), 5);
        assert_eq!(value_bin(u64::MAX), 5);
        assert_eq!(count_band_bin(1, &INPUT_COUNT_UPPERS), 0);
        assert_eq!(count_band_bin(2, &INPUT_COUNT_UPPERS), 1);
        assert_eq!(count_band_bin(5, &INPUT_COUNT_UPPERS), 1);
        assert_eq!(count_band_bin(100, &INPUT_COUNT_UPPERS), 3);
        assert_eq!(count_band_bin(101, &INPUT_COUNT_UPPERS), 4);
        assert_eq!(count_band_bin(2, &OUTPUT_COUNT_UPPERS), 1);
        assert_eq!(count_band_bin(3, &OUTPUT_COUNT_UPPERS), 2);
    }

    #[test]
    fn log_bins_clamp_out_of_range_values() {
        assert_eq!(log_bin(0.1, 0.5, 512.0, 64), 0);
        assert_eq!(log_bin(0.5, 0.5, 512.0, 64), 0);
        assert_eq!(log_bin(512.0, 0.5, 512.0, 64), 63);
        assert_eq!(log_bin(1_000_000.0, 0.5, 512.0, 64), 63);
        let edges = log_spaced_edges(0.5, 512.0, 64);
        assert_eq!(edges.len(), 65);
        assert_eq!(edges[0], 0.5);
        assert_eq!(edges[64], 512.0);
        for pair in edges.windows(2) {
            assert!(pair[0] < pair[1]);
        }
    }

    #[test]
    fn log_bin_indices_agree_with_emitted_edges() {
        let edges = log_spaced_edges(1.0, 512.0, 22);
        for (index, window) in edges.windows(2).enumerate() {
            let midpoint = (window[0] * window[1]).sqrt();
            assert_eq!(log_bin(midpoint, 1.0, 512.0, 22), index);
        }
    }

    #[test]
    fn filter_validation_rejects_degenerate_requests() {
        let empty_verdicts = SummaryFilter {
            taxonomies: vec![taxonomy_filter("behavior", &[])],
            ..SummaryFilter::default()
        };
        assert_eq!(
            empty_verdicts.validate(),
            Err(ModelError::EmptyTaxonomyFilter {
                taxonomy: "behavior".to_owned(),
            })
        );

        let duplicate_taxonomies = SummaryFilter {
            taxonomies: vec![
                taxonomy_filter("behavior", &["payment"]),
                taxonomy_filter("behavior", &["unknown"]),
            ],
            ..SummaryFilter::default()
        };
        assert_eq!(
            duplicate_taxonomies.validate(),
            Err(ModelError::DuplicateTaxonomyFilter {
                taxonomy: "behavior".to_owned(),
            })
        );

        let empty_scripts = SummaryFilter {
            scripts: Some(Vec::new()),
            ..SummaryFilter::default()
        };
        assert_eq!(
            empty_scripts.validate(),
            Err(ModelError::EmptyFilterFacet { facet: "script" })
        );

        let negative = SummaryFilter {
            feerate_min: Some(-1.0),
            ..SummaryFilter::default()
        };
        assert_eq!(
            negative.validate(),
            Err(ModelError::InvalidFilterBound {
                field: "feerate_min"
            })
        );

        let not_finite = SummaryFilter {
            feerate_max: Some(f64::NAN),
            ..SummaryFilter::default()
        };
        assert_eq!(
            not_finite.validate(),
            Err(ModelError::InvalidFilterBound {
                field: "feerate_max"
            })
        );

        let inverted = SummaryFilter {
            feerate_min: Some(8.0),
            feerate_max: Some(4.0),
            ..SummaryFilter::default()
        };
        assert_eq!(inverted.validate(), Err(ModelError::InvertedFeerateBounds));

        let distinct_taxonomies = SummaryFilter {
            taxonomies: vec![
                taxonomy_filter("behavior", &["payment", "unknown"]),
                taxonomy_filter("data_protocol", &["unknown"]),
            ],
            ..SummaryFilter::default()
        };
        assert_eq!(distinct_taxonomies.validate(), Ok(()));
        assert!(!distinct_taxonomies.is_empty());

        assert_eq!(SummaryFilter::default().validate(), Ok(()));
        assert!(SummaryFilter::default().is_empty());
    }

    #[test]
    fn dimension_histograms_have_explicit_wire_shapes() {
        assert_eq!(
            serde_json::to_value(DimensionHistogram::Available {
                bins: vec![AggregateBin {
                    count: 2,
                    vsize: 300
                }],
                underived: AggregateBin {
                    count: 1,
                    vsize: 90
                },
            })
            .expect("available"),
            serde_json::json!({
                "status": "available",
                "bins": [{ "count": 2, "vsize": 300 }],
                "underived": { "count": 1, "vsize": 90 },
            })
        );
        assert_eq!(
            serde_json::to_value(DimensionHistogram::Unavailable {
                reason: UnavailableReason::RequiresRawTransaction,
            })
            .expect("unavailable"),
            serde_json::json!({
                "status": "unavailable",
                "reason": "requires_raw_transaction",
            })
        );
    }

    #[test]
    fn taxonomy_histograms_flatten_their_histogram_onto_the_key() {
        let histogram = TaxonomyHistogram {
            key: "behavior".to_owned(),
            histogram: DimensionHistogram::Available {
                bins: vec![AggregateBin {
                    count: 2,
                    vsize: 300,
                }],
                underived: AggregateBin { count: 0, vsize: 0 },
            },
        };
        let wire = serde_json::json!({
            "key": "behavior",
            "status": "available",
            "bins": [{ "count": 2, "vsize": 300 }],
            "underived": { "count": 0, "vsize": 0 },
        });
        assert_eq!(serde_json::to_value(&histogram).expect("serialize"), wire);
        assert_eq!(
            serde_json::from_value::<TaxonomyHistogram>(wire).expect("deserialize"),
            histogram
        );
    }

    #[test]
    fn taxonomy_descriptors_have_explicit_wire_shapes() {
        assert_eq!(
            serde_json::to_value(TaxonomyDescriptor {
                key: "behavior".to_owned(),
                label: "Behavior".to_owned(),
                verdicts: vec![VerdictDescriptor {
                    key: "unknown".to_owned(),
                    label: "Unknown".to_owned(),
                }],
            })
            .expect("serialize"),
            serde_json::json!({
                "key": "behavior",
                "label": "Behavior",
                "verdicts": [{ "key": "unknown", "label": "Unknown" }],
            })
        );
    }

    #[test]
    fn empty_filter_echo_serializes_without_facets() {
        assert_eq!(
            serde_json::to_value(SummaryFilter::default()).expect("filter"),
            serde_json::json!({})
        );
        let filter = SummaryFilter {
            taxonomies: vec![taxonomy_filter("behavior", &["payment", "unknown"])],
            feerate_min: Some(4.0),
            ..SummaryFilter::default()
        };
        assert_eq!(
            serde_json::to_value(filter).expect("filter"),
            serde_json::json!({
                "taxonomies": [
                    { "key": "behavior", "verdicts": ["payment", "unknown"] },
                ],
                "feerate_min": 4.0,
            })
        );
    }
}
