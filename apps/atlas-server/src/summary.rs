//! Server-side aggregate summary computation over one source's current
//! membership. One pass over the fact-bearing rows produces every histogram
//! and optional detail block, so cost is O(memberships) per request and the
//! response size is fixed by the canonical bin catalog plus the registered
//! taxonomy vocabularies.
//!
//! Rows carry optional derived shape facts (from observed raw transactions)
//! plus one stored verdict per taxonomy that produced one. Shape-dependent
//! dimensions bin only derived rows; the rest are reported in each
//! histogram's explicit `underived` bucket, never guessed into a bin.
//! Taxonomies are the exception: a row without a stored verdict for a
//! taxonomy has the honest effective verdict `unknown` and lands in that
//! bin, so taxonomy histograms keep `underived` at zero.

use atlas_model::{
    AGE_BIN_COUNT, AggregateBin, AnomalyRegion, AwaitingRpcTotal, BinCatalog, DimensionHistogram,
    ECDF_FEE_BIN_COUNT, ECDF_FEE_MAX_SAT_PER_VB, ECDF_FEE_MIN_SAT_PER_VB, EcdfSeries,
    FEERATE_BIN_COUNT, FeeRateEcdf, INPUT_BIN_COUNT, INPUT_COUNT_UPPERS, JOINT_FEE_BIN_COUNT,
    JOINT_FEE_MAX_SAT_PER_VB, JOINT_FEE_MIN_SAT_PER_VB, JOINT_SIZE_BIN_COUNT, JOINT_SIZE_MAX_VB,
    JOINT_SIZE_MIN_VB, JointFeeSize, MempoolSummary, OUTPUT_BIN_COUNT, OUTPUT_COUNT_UPPERS,
    RegionAggregate, ScriptType, SourceHealth, SourceId, SummaryDetail, SummaryFilter,
    SummaryHistograms, SummaryTotals, TaxonomyDescriptor, TaxonomyHistogram, UnavailableReason,
    VALUE_BIN_COUNT, age_bin, count_band_bin, feerate_bin, log_bin, log_spaced_edges, value_bin,
};

/// The effective verdict of a row for a taxonomy without a stored verdict.
const UNKNOWN_VERDICT: &str = "unknown";

/// Derived shape facts for one transaction, as read from `transaction_shape`.
#[derive(Clone, Copy, Debug)]
pub struct ShapeRow {
    pub total_output_sats: u64,
    pub input_count: u64,
    pub output_count: u64,
    pub script_type: ScriptType,
}

/// One fact-bearing membership row, as read from the active SourceReplica.
/// `verdicts` holds `(taxonomy key, verdict key)` pairs from
/// `transaction_classification`; a taxonomy without a pair has the honest
/// effective verdict `unknown`.
#[derive(Clone, Debug)]
pub struct FactsRow {
    pub vsize: u64,
    pub fee_sats: u64,
    pub entered_at_ms: u64,
    pub shape: Option<ShapeRow>,
    pub verdicts: Vec<(String, String)>,
}

impl FactsRow {
    /// The row's effective verdict for one taxonomy: the stored verdict when
    /// present, otherwise the explicit `unknown`.
    fn effective_verdict(&self, taxonomy_key: &str) -> &str {
        self.verdicts
            .iter()
            .find(|(taxonomy, _)| taxonomy == taxonomy_key)
            .map_or(UNKNOWN_VERDICT, |(_, verdict)| verdict.as_str())
    }
}

/// Everything the summary needs from the store for one source.
#[derive(Clone, Debug)]
pub struct SourceMembershipFacts {
    pub health: SourceHealth,
    pub available: Vec<FactsRow>,
    pub awaiting_rpc_count: u64,
}

/// A dimension histogram under construction: canonical bins plus the
/// explicit bucket for matching rows whose evidence lacks the dimension.
struct HistogramAccumulator {
    bins: Vec<AggregateBin>,
    underived: AggregateBin,
}

impl HistogramAccumulator {
    fn new(bin_count: usize) -> Self {
        Self {
            bins: vec![AggregateBin::default(); bin_count],
            underived: AggregateBin::default(),
        }
    }

    fn record(&mut self, bin: Option<usize>, vsize: u64) {
        match bin {
            Some(index) => self.bins[index].add(vsize),
            None => self.underived.add(vsize),
        }
    }

    fn finish(self) -> DimensionHistogram {
        DimensionHistogram::Available {
            bins: self.bins,
            underived: self.underived,
        }
    }
}

/// The bin index of one verdict within a taxonomy's declared vocabulary.
/// Verdicts a pack no longer declares fall into the guaranteed `unknown`
/// bin instead of inventing a bin.
fn verdict_bin(taxonomy: &TaxonomyDescriptor, verdict: &str) -> usize {
    taxonomy
        .verdicts
        .iter()
        .position(|descriptor| descriptor.key == verdict)
        .unwrap_or_else(|| {
            taxonomy
                .verdicts
                .iter()
                .position(|descriptor| descriptor.key == UNKNOWN_VERDICT)
                .expect("every taxonomy declares the unknown verdict")
        })
}

fn matches(filter: &SummaryFilter, row: &FactsRow, feerate: f64) -> bool {
    for taxonomy_filter in &filter.taxonomies {
        // Known-to-match over the effective verdict: absence of a stored
        // verdict is the explicit verdict `unknown`, never a wildcard.
        let verdict = row.effective_verdict(&taxonomy_filter.key);
        if !taxonomy_filter
            .verdicts
            .iter()
            .any(|selected| selected == verdict)
        {
            return false;
        }
    }
    if let Some(scripts) = &filter.scripts {
        // Known-to-match semantics: a row without derived script evidence is
        // never selected by a script facet.
        let known_match = row
            .shape
            .is_some_and(|shape| scripts.contains(&shape.script_type));
        if !known_match {
            return false;
        }
    }
    if let Some(min) = filter.feerate_min
        && feerate < min
    {
        return false;
    }
    if let Some(max) = filter.feerate_max
        && feerate > max
    {
        return false;
    }
    true
}

/// The region facts a comparison region is aggregated over. Active
/// SourceReplica rows always carry RPC facts, so `awaiting_rpc_count` is a
/// legacy wire-compatibility counter and remains zero. Unlike
/// [`SourceMembershipFacts`] a region carries no source health, because a
/// region is a derived membership set, not a source.
#[derive(Clone, Debug, Default)]
pub struct RegionFacts {
    pub available: Vec<FactsRow>,
    pub awaiting_rpc_count: u64,
}

/// The histogram-bearing totals and bins produced by one aggregation pass,
/// shared by the summary and region entry points.
struct Aggregation {
    totals: SummaryTotals,
    bins: BinCatalog,
    histograms: SummaryHistograms,
    ecdf: Option<FeeRateEcdf>,
    joint_fee_size: Option<JointFeeSize>,
}

#[must_use]
pub fn compute_summary(
    source_id: SourceId,
    facts: &SourceMembershipFacts,
    filter: &SummaryFilter,
    detail: SummaryDetail,
    as_of_ms: u64,
    taxonomies: &[TaxonomyDescriptor],
) -> MempoolSummary {
    let Aggregation {
        totals,
        bins,
        histograms,
        ecdf,
        joint_fee_size,
    } = aggregate(
        &facts.available,
        facts.awaiting_rpc_count,
        filter,
        detail,
        as_of_ms,
        taxonomies,
    );
    MempoolSummary {
        source_id,
        as_of_ms,
        filter_echo: filter.clone(),
        totals,
        bins,
        histograms,
        ecdf,
        joint_fee_size,
        health: facts.health.clone(),
    }
}

/// The rich aggregate over one comparison region, shaped like a summary's
/// matching set. The region is aggregated with an empty filter and no detail
/// blocks, so `present` is the histogram-bearing total. The legacy
/// `awaiting_rpc` field remains zero for complete SourceReplica state.
#[must_use]
pub fn region_aggregate(
    facts: &RegionFacts,
    as_of_ms: u64,
    taxonomies: &[TaxonomyDescriptor],
) -> RegionAggregate {
    let Aggregation {
        totals,
        bins,
        histograms,
        ..
    } = aggregate(
        &facts.available,
        facts.awaiting_rpc_count,
        &SummaryFilter::default(),
        SummaryDetail::default(),
        as_of_ms,
        taxonomies,
    );
    RegionAggregate {
        present: totals.matching,
        awaiting_rpc: totals.awaiting_rpc,
        bins,
        histograms,
    }
}

/// The lightweight aggregate over one comparison region: fact-bearing count
/// and summed virtual size, with no histograms. The legacy `awaiting_rpc`
/// counter remains zero for active SourceReplica rows.
#[must_use]
pub fn anomaly_region(facts: &RegionFacts) -> AnomalyRegion {
    let mut present = AggregateBin::default();
    for row in &facts.available {
        present.add(row.vsize);
    }
    AnomalyRegion {
        present,
        awaiting_rpc: AwaitingRpcTotal {
            count: facts.awaiting_rpc_count,
        },
    }
}

fn aggregate(
    available: &[FactsRow],
    awaiting_rpc_count: u64,
    filter: &SummaryFilter,
    detail: SummaryDetail,
    as_of_ms: u64,
    taxonomies: &[TaxonomyDescriptor],
) -> Aggregation {
    let mut all = AggregateBin::default();
    let mut matching = AggregateBin::default();
    let mut feerate_bins = HistogramAccumulator::new(FEERATE_BIN_COUNT);
    let mut age_bins = HistogramAccumulator::new(AGE_BIN_COUNT);
    let mut taxonomy_bins: Vec<HistogramAccumulator> = taxonomies
        .iter()
        .map(|taxonomy| HistogramAccumulator::new(taxonomy.verdicts.len()))
        .collect();
    let mut script_bins = HistogramAccumulator::new(ScriptType::ALL.len());
    let mut value_bins = HistogramAccumulator::new(VALUE_BIN_COUNT);
    let mut input_bins = HistogramAccumulator::new(INPUT_BIN_COUNT);
    let mut output_bins = HistogramAccumulator::new(OUTPUT_BIN_COUNT);
    // The ECDF splits by the first registered taxonomy's verdicts.
    let ecdf_taxonomy = taxonomies.first();
    let mut ecdf_bins = detail.ecdf.then(|| {
        vec![
            vec![0_u64; ECDF_FEE_BIN_COUNT];
            ecdf_taxonomy.map_or(0, |taxonomy| taxonomy.verdicts.len())
        ]
    });
    let mut joint_grid = detail
        .joint_fee_size
        .then(|| vec![vec![0_u64; JOINT_FEE_BIN_COUNT]; JOINT_SIZE_BIN_COUNT]);

    for row in available {
        all.add(row.vsize);
        let feerate = row.fee_sats as f64 / row.vsize as f64;
        if !matches(filter, row, feerate) {
            continue;
        }
        matching.add(row.vsize);
        feerate_bins.record(Some(feerate_bin(feerate)), row.vsize);
        let age_ms = as_of_ms.saturating_sub(row.entered_at_ms);
        age_bins.record(Some(age_bin(age_ms)), row.vsize);
        for (taxonomy, bins) in taxonomies.iter().zip(taxonomy_bins.iter_mut()) {
            let verdict = row.effective_verdict(&taxonomy.key);
            bins.record(Some(verdict_bin(taxonomy, verdict)), row.vsize);
        }
        script_bins.record(row.shape.map(|shape| shape.script_type.index()), row.vsize);
        value_bins.record(
            row.shape.map(|shape| value_bin(shape.total_output_sats)),
            row.vsize,
        );
        input_bins.record(
            row.shape
                .map(|shape| count_band_bin(shape.input_count, &INPUT_COUNT_UPPERS)),
            row.vsize,
        );
        output_bins.record(
            row.shape
                .map(|shape| count_band_bin(shape.output_count, &OUTPUT_COUNT_UPPERS)),
            row.vsize,
        );
        if let (Some(ecdf), Some(taxonomy)) = (ecdf_bins.as_mut(), ecdf_taxonomy) {
            let bin = log_bin(
                feerate,
                ECDF_FEE_MIN_SAT_PER_VB,
                ECDF_FEE_MAX_SAT_PER_VB,
                ECDF_FEE_BIN_COUNT,
            );
            let verdict = row.effective_verdict(&taxonomy.key);
            ecdf[verdict_bin(taxonomy, verdict)][bin] += row.vsize;
        }
        if let Some(grid) = joint_grid.as_mut() {
            let fee_bin = log_bin(
                feerate,
                JOINT_FEE_MIN_SAT_PER_VB,
                JOINT_FEE_MAX_SAT_PER_VB,
                JOINT_FEE_BIN_COUNT,
            );
            let size_bin = log_bin(
                row.vsize as f64,
                JOINT_SIZE_MIN_VB,
                JOINT_SIZE_MAX_VB,
                JOINT_SIZE_BIN_COUNT,
            );
            grid[size_bin][fee_bin] += row.vsize;
        }
    }

    Aggregation {
        totals: SummaryTotals {
            all,
            matching,
            awaiting_rpc: AwaitingRpcTotal {
                count: awaiting_rpc_count,
            },
        },
        bins: BinCatalog::for_taxonomies(taxonomies.to_vec()),
        histograms: SummaryHistograms {
            taxonomies: taxonomies
                .iter()
                .zip(taxonomy_bins)
                .map(|(taxonomy, bins)| TaxonomyHistogram {
                    key: taxonomy.key.clone(),
                    histogram: bins.finish(),
                })
                .collect(),
            script: script_bins.finish(),
            value: value_bins.finish(),
            inputs: input_bins.finish(),
            outputs: output_bins.finish(),
            age: age_bins.finish(),
            feerate: feerate_bins.finish(),
        },
        ecdf: ecdf_bins.map(|bins| fee_rate_ecdf(bins, ecdf_taxonomy)),
        joint_fee_size: joint_grid.map(|grid| JointFeeSize {
            fee_edges: log_spaced_edges(
                JOINT_FEE_MIN_SAT_PER_VB,
                JOINT_FEE_MAX_SAT_PER_VB,
                JOINT_FEE_BIN_COUNT,
            ),
            size_edges: log_spaced_edges(
                JOINT_SIZE_MIN_VB,
                JOINT_SIZE_MAX_VB,
                JOINT_SIZE_BIN_COUNT,
            ),
            grid,
        }),
    }
}

// UnavailableReason is retained on the wire for dimensions that a future
// deployment mode cannot derive at all; with server-side enrichment active
// every dimension is computed, so the server no longer emits it.
const _: fn() = || {
    let _ = UnavailableReason::RequiresRawTransaction;
};

fn fee_rate_ecdf(
    per_verdict_bins: Vec<Vec<u64>>,
    taxonomy: Option<&TaxonomyDescriptor>,
) -> FeeRateEcdf {
    let mut series = Vec::new();
    if let Some(taxonomy) = taxonomy {
        for (verdict, bins) in taxonomy.verdicts.iter().zip(&per_verdict_bins) {
            if bins.iter().all(|vsize| *vsize == 0) {
                continue;
            }
            let mut cumulative = 0;
            let cum_vsize = bins
                .iter()
                .map(|vsize| {
                    cumulative += vsize;
                    cumulative
                })
                .collect();
            series.push(EcdfSeries {
                key: verdict.key.clone(),
                cum_vsize,
            });
        }
    }
    FeeRateEcdf {
        taxonomy: taxonomy
            .map(|taxonomy| taxonomy.key.clone())
            .unwrap_or_default(),
        fee_edges: log_spaced_edges(
            ECDF_FEE_MIN_SAT_PER_VB,
            ECDF_FEE_MAX_SAT_PER_VB,
            ECDF_FEE_BIN_COUNT,
        ),
        series,
    }
}

#[cfg(test)]
mod tests {
    use atlas_model::{
        CaptureStatus, Classification, ReplicaCursor, SourceEpochId, TaxonomyFilter,
        VerdictDescriptor,
    };

    use super::*;

    fn source() -> SourceId {
        SourceId::new("source-a").expect("source")
    }

    fn health() -> SourceHealth {
        SourceHealth {
            state_cursor: ReplicaCursor::new(SourceEpochId::new("epoch-a").expect("epoch"), 1)
                .expect("cursor"),
            state_observed_at_ms: 1_000,
            capture: CaptureStatus::NotCollected,
        }
    }

    const AS_OF_MS: u64 = 1_752_710_400_000;

    /// The registry taxonomies used by every test: `behavior` first.
    fn taxonomies() -> Vec<TaxonomyDescriptor> {
        vec![behavior_taxonomy()]
    }

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

    fn behavior_filter(verdicts: &[&str]) -> SummaryFilter {
        SummaryFilter {
            taxonomies: vec![TaxonomyFilter {
                key: "behavior".to_owned(),
                verdicts: verdicts
                    .iter()
                    .map(|verdict| (*verdict).to_owned())
                    .collect(),
            }],
            ..SummaryFilter::default()
        }
    }

    fn row(vsize: u64, fee_sats: u64, age_ms: u64) -> FactsRow {
        FactsRow {
            vsize,
            fee_sats,
            entered_at_ms: AS_OF_MS - age_ms,
            shape: None,
            verdicts: Vec::new(),
        }
    }

    fn shaped(vsize: u64, fee_sats: u64, age_ms: u64, shape: ShapeRow, verdict: &str) -> FactsRow {
        FactsRow {
            shape: Some(shape),
            verdicts: vec![("behavior".to_owned(), verdict.to_owned())],
            ..row(vsize, fee_sats, age_ms)
        }
    }

    fn payment_shape() -> ShapeRow {
        ShapeRow {
            total_output_sats: 5_000_000,
            input_count: 2,
            output_count: 2,
            script_type: ScriptType::P2wpkh,
        }
    }

    fn payment_row(vsize: u64, fee_sats: u64, age_ms: u64) -> FactsRow {
        shaped(vsize, fee_sats, age_ms, payment_shape(), "payment")
    }

    fn facts(available: Vec<FactsRow>, awaiting_rpc_count: u64) -> SourceMembershipFacts {
        SourceMembershipFacts {
            health: health(),
            available,
            awaiting_rpc_count,
        }
    }

    fn region_facts(available: Vec<FactsRow>, awaiting_rpc_count: u64) -> RegionFacts {
        RegionFacts {
            available,
            awaiting_rpc_count,
        }
    }

    #[test]
    fn region_aggregate_mirrors_the_unfiltered_summary_of_the_same_rows() {
        let rows = vec![payment_row(200, 1_700, 0), row(400, 800, 0)];
        let summary = compute_summary(
            source(),
            &facts(rows.clone(), 2),
            &SummaryFilter::default(),
            SummaryDetail::default(),
            AS_OF_MS,
            &taxonomies(),
        );
        let aggregate = region_aggregate(&region_facts(rows, 2), AS_OF_MS, &taxonomies());

        // A region is the summary's matching set under an empty filter: present
        // is the histogram-bearing total, and the catalog and histograms match
        // byte for byte so a client renders both with the same code.
        assert_eq!(aggregate.present, summary.totals.all);
        assert_eq!(aggregate.present, summary.totals.matching);
        assert_eq!(aggregate.awaiting_rpc, summary.totals.awaiting_rpc);
        assert_eq!(aggregate.bins, summary.bins);
        assert_eq!(aggregate.histograms, summary.histograms);
    }

    #[test]
    fn anomaly_region_sums_present_vsize_and_counts_awaiting_only() {
        let anomaly = anomaly_region(&region_facts(
            vec![row(200, 400, 0), payment_row(300, 900, 0)],
            4,
        ));
        assert_eq!(
            anomaly.present,
            AggregateBin {
                count: 2,
                vsize: 500
            }
        );
        assert_eq!(anomaly.awaiting_rpc.count, 4);
    }

    fn parts(histogram: &DimensionHistogram) -> (&[AggregateBin], AggregateBin) {
        match histogram {
            DimensionHistogram::Available { bins, underived } => (bins, *underived),
            DimensionHistogram::Unavailable { .. } => panic!("expected available histogram"),
        }
    }

    fn behavior_parts(summary: &MempoolSummary) -> (&[AggregateBin], AggregateBin) {
        assert_eq!(summary.histograms.taxonomies.len(), 1);
        let taxonomy = &summary.histograms.taxonomies[0];
        assert_eq!(taxonomy.key, "behavior");
        parts(&taxonomy.histogram)
    }

    #[test]
    fn awaiting_rpc_stays_out_of_all_aggregates() {
        let summary = compute_summary(
            source(),
            &facts(vec![row(200, 400, 0)], 3),
            &SummaryFilter::default(),
            SummaryDetail::default(),
            AS_OF_MS,
            &taxonomies(),
        );

        assert_eq!(
            summary.totals.all,
            AggregateBin {
                count: 1,
                vsize: 200
            }
        );
        assert_eq!(summary.totals.matching, summary.totals.all);
        assert_eq!(summary.totals.awaiting_rpc.count, 3);
        let (feerate, _) = parts(&summary.histograms.feerate);
        assert_eq!(feerate.iter().map(|bin| bin.count).sum::<u64>(), 1);
    }

    #[test]
    fn catalog_and_histograms_carry_the_registered_taxonomies_in_order() {
        let summary = compute_summary(
            source(),
            &facts(vec![row(200, 400, 0)], 0),
            &SummaryFilter::default(),
            SummaryDetail::default(),
            AS_OF_MS,
            &taxonomies(),
        );

        assert_eq!(summary.bins.taxonomies, taxonomies());
        assert_eq!(summary.histograms.taxonomies.len(), 1);
        let (bins, _) = behavior_parts(&summary);
        assert_eq!(bins.len(), Classification::ALL.len());
    }

    #[test]
    fn verdictless_rows_fill_underived_buckets_and_the_unknown_bin() {
        let summary = compute_summary(
            source(),
            &facts(vec![row(200, 1_700, 0)], 0),
            &SummaryFilter::default(),
            SummaryDetail::default(),
            AS_OF_MS,
            &taxonomies(),
        );

        for histogram in [
            &summary.histograms.script,
            &summary.histograms.value,
            &summary.histograms.inputs,
            &summary.histograms.outputs,
        ] {
            let (bins, underived) = parts(histogram);
            assert!(bins.iter().all(|bin| bin.count == 0));
            assert_eq!(
                underived,
                AggregateBin {
                    count: 1,
                    vsize: 200
                }
            );
        }
        let (behavior, underived) = behavior_parts(&summary);
        assert_eq!(behavior[Classification::Unknown.index()].count, 1);
        assert_eq!(underived, AggregateBin::default());
        let (_, feerate_underived) = parts(&summary.histograms.feerate);
        assert_eq!(feerate_underived, AggregateBin::default());
    }

    #[test]
    fn rows_land_in_their_taxonomy_verdict_and_shape_bins() {
        let coinjoin_shape = ShapeRow {
            total_output_sats: 200_000_000,
            input_count: 40,
            output_count: 40,
            script_type: ScriptType::P2tr,
        };
        let summary = compute_summary(
            source(),
            &facts(
                vec![
                    payment_row(200, 1_700, 0),
                    shaped(11_000, 77_000, 0, coinjoin_shape, "coinjoin"),
                    row(400, 800, 0),
                ],
                0,
            ),
            &SummaryFilter::default(),
            SummaryDetail::default(),
            AS_OF_MS,
            &taxonomies(),
        );

        let (behavior, _) = behavior_parts(&summary);
        assert_eq!(behavior[Classification::Payment.index()].count, 1);
        assert_eq!(behavior[Classification::Coinjoin.index()].count, 1);
        assert_eq!(behavior[Classification::Unknown.index()].count, 1);

        let (script, script_underived) = parts(&summary.histograms.script);
        assert_eq!(script[ScriptType::P2wpkh.index()].count, 1);
        assert_eq!(script[ScriptType::P2tr.index()].count, 1);
        assert_eq!(script_underived.count, 1);

        // 0.05 BTC -> 0.01–0.1 bin; 2 BTC -> 1–10 bin.
        let (value, _) = parts(&summary.histograms.value);
        assert_eq!(value[2].count, 1);
        assert_eq!(value[4].count, 1);

        // 2 inputs -> band 2–5; 40 inputs -> band 21–100.
        let (inputs, _) = parts(&summary.histograms.inputs);
        assert_eq!(inputs[1].count, 1);
        assert_eq!(inputs[3].count, 1);

        // 2 outputs -> band 2; 40 outputs -> band 11–50.
        let (outputs, _) = parts(&summary.histograms.outputs);
        assert_eq!(outputs[1].count, 1);
        assert_eq!(outputs[3].count, 1);
    }

    #[test]
    fn script_facets_match_only_derived_evidence() {
        let rows = vec![payment_row(200, 1_700, 0), row(400, 800, 0)];
        let p2wpkh_only = SummaryFilter {
            scripts: Some(vec![ScriptType::P2wpkh]),
            ..SummaryFilter::default()
        };
        let summary = compute_summary(
            source(),
            &facts(rows.clone(), 0),
            &p2wpkh_only,
            SummaryDetail::default(),
            AS_OF_MS,
            &taxonomies(),
        );
        assert_eq!(
            summary.totals.matching,
            AggregateBin {
                count: 1,
                vsize: 200
            }
        );

        let p2tr_only = SummaryFilter {
            scripts: Some(vec![ScriptType::P2tr]),
            ..SummaryFilter::default()
        };
        let summary = compute_summary(
            source(),
            &facts(rows, 0),
            &p2tr_only,
            SummaryDetail::default(),
            AS_OF_MS,
            &taxonomies(),
        );
        assert_eq!(summary.totals.matching.count, 0);
    }

    #[test]
    fn taxonomy_facets_select_effective_verdicts_including_honest_unknown() {
        let rows = vec![payment_row(200, 1_700, 0), row(400, 800, 0)];
        let summary = compute_summary(
            source(),
            &facts(rows.clone(), 0),
            &behavior_filter(&["payment"]),
            SummaryDetail::default(),
            AS_OF_MS,
            &taxonomies(),
        );
        assert_eq!(summary.totals.matching.count, 1);

        let summary = compute_summary(
            source(),
            &facts(rows, 0),
            &behavior_filter(&["unknown"]),
            SummaryDetail::default(),
            AS_OF_MS,
            &taxonomies(),
        );
        assert_eq!(
            summary.totals.matching,
            AggregateBin {
                count: 1,
                vsize: 400
            }
        );
    }

    #[test]
    fn feerate_filter_bounds_are_inclusive_and_shrink_matching_only() {
        let rows = vec![row(800, 400, 0), row(200, 1_700, 0), row(100, 40_000, 0)];
        let filter = SummaryFilter {
            feerate_min: Some(0.5),
            feerate_max: Some(8.5),
            ..SummaryFilter::default()
        };
        let summary = compute_summary(
            source(),
            &facts(rows, 0),
            &filter,
            SummaryDetail::default(),
            AS_OF_MS,
            &taxonomies(),
        );

        assert_eq!(
            summary.totals.all,
            AggregateBin {
                count: 3,
                vsize: 1_100
            }
        );
        assert_eq!(
            summary.totals.matching,
            AggregateBin {
                count: 2,
                vsize: 1_000
            }
        );
        assert_eq!(summary.filter_echo, filter);
    }

    #[test]
    fn detail_blocks_are_present_only_when_requested() {
        let rows = vec![payment_row(200, 1_700, 0), row(300, 600, 0)];
        let bare = compute_summary(
            source(),
            &facts(rows.clone(), 0),
            &SummaryFilter::default(),
            SummaryDetail::default(),
            AS_OF_MS,
            &taxonomies(),
        );
        assert!(bare.ecdf.is_none());
        assert!(bare.joint_fee_size.is_none());

        let detailed = compute_summary(
            source(),
            &facts(rows, 0),
            &SummaryFilter::default(),
            SummaryDetail {
                ecdf: true,
                joint_fee_size: true,
            },
            AS_OF_MS,
            &taxonomies(),
        );
        let ecdf = detailed.ecdf.expect("ecdf requested");
        assert_eq!(ecdf.taxonomy, "behavior");
        assert_eq!(ecdf.fee_edges.len(), ECDF_FEE_BIN_COUNT + 1);
        let keys: Vec<&str> = ecdf
            .series
            .iter()
            .map(|series| series.key.as_str())
            .collect();
        assert_eq!(keys, vec!["payment", "unknown"]);
        for series in &ecdf.series {
            assert!(series.cum_vsize.windows(2).all(|pair| pair[0] <= pair[1]));
        }
        let total: u64 = ecdf
            .series
            .iter()
            .map(|series| series.cum_vsize.last().copied().unwrap_or(0))
            .sum();
        assert_eq!(total, detailed.totals.matching.vsize);

        let joint = detailed.joint_fee_size.expect("joint requested");
        assert_eq!(joint.grid.len(), JOINT_SIZE_BIN_COUNT);
        let grid_total: u64 = joint.grid.iter().flatten().sum();
        assert_eq!(grid_total, detailed.totals.matching.vsize);
    }

    #[test]
    fn future_entry_times_clamp_into_the_youngest_age_bin() {
        let summary = compute_summary(
            source(),
            &facts(
                vec![FactsRow {
                    vsize: 100,
                    fee_sats: 100,
                    entered_at_ms: AS_OF_MS + 5_000,
                    shape: None,
                    verdicts: Vec::new(),
                }],
                0,
            ),
            &SummaryFilter::default(),
            SummaryDetail::default(),
            AS_OF_MS,
            &taxonomies(),
        );
        let (age, _) = parts(&summary.histograms.age);
        assert_eq!(age[0].count, 1);
    }

    #[test]
    fn undeclared_stored_verdicts_fall_back_to_the_unknown_bin() {
        let stale = FactsRow {
            verdicts: vec![("behavior".to_owned(), "retired_verdict".to_owned())],
            ..row(200, 400, 0)
        };
        let summary = compute_summary(
            source(),
            &facts(vec![stale], 0),
            &SummaryFilter::default(),
            SummaryDetail::default(),
            AS_OF_MS,
            &taxonomies(),
        );
        let (behavior, _) = behavior_parts(&summary);
        assert_eq!(behavior[Classification::Unknown.index()].count, 1);
    }
}
