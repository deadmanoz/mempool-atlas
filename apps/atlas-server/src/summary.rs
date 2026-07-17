//! Server-side aggregate summary computation over one source's current
//! membership. One pass over the fact-bearing rows produces every histogram
//! and optional detail block, so cost is O(memberships) per request and the
//! response size is fixed by the canonical bin catalog.
//!
//! Rows carry optional derived shape facts (from observed raw transactions).
//! Shape-dependent dimensions bin only derived rows; the rest are reported in
//! each histogram's explicit `underived` bucket, never guessed into a bin.
//! Classification is the exception: a row without classifier evidence has the
//! honest verdict `unknown` and lands in that bin.

use atlas_model::{
    AGE_BIN_COUNT, AggregateBin, AwaitingRpcTotal, BinCatalog, Classification, DimensionHistogram,
    ECDF_FEE_BIN_COUNT, ECDF_FEE_MAX_SAT_PER_VB, ECDF_FEE_MIN_SAT_PER_VB, EcdfSeries,
    FEERATE_BIN_COUNT, FeeRateEcdf, INPUT_BIN_COUNT, INPUT_COUNT_UPPERS, JOINT_FEE_BIN_COUNT,
    JOINT_FEE_MAX_SAT_PER_VB, JOINT_FEE_MIN_SAT_PER_VB, JOINT_SIZE_BIN_COUNT, JOINT_SIZE_MAX_VB,
    JOINT_SIZE_MIN_VB, JointFeeSize, MempoolSummary, OUTPUT_BIN_COUNT, OUTPUT_COUNT_UPPERS,
    ScriptType, SourceHealth, SourceId, SummaryDetail, SummaryFilter, SummaryHistograms,
    SummaryTotals, UnavailableReason, VALUE_BIN_COUNT, age_bin, count_band_bin, feerate_bin,
    log_bin, log_spaced_edges, value_bin,
};

/// Derived shape facts and classifier verdict for one transaction, as read
/// from `transaction_shape`.
#[derive(Clone, Copy, Debug)]
pub struct ShapeRow {
    pub total_output_sats: u64,
    pub input_count: u64,
    pub output_count: u64,
    pub script_type: ScriptType,
    pub classification: Classification,
}

/// One fact-bearing membership row, as read from `current_membership`.
#[derive(Clone, Copy, Debug)]
pub struct FactsRow {
    pub vsize: u64,
    pub fee_sats: u64,
    pub entered_at_ms: u64,
    pub shape: Option<ShapeRow>,
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

fn matches(
    filter: &SummaryFilter,
    classification: Classification,
    script: Option<ScriptType>,
    feerate: f64,
) -> bool {
    if let Some(classes) = &filter.classes
        && !classes.contains(&classification)
    {
        return false;
    }
    if let Some(scripts) = &filter.scripts {
        // Known-to-match semantics: a row without derived script evidence is
        // never selected by a script facet.
        let known_match = script.is_some_and(|value| scripts.contains(&value));
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

#[must_use]
pub fn compute_summary(
    source_id: SourceId,
    facts: &SourceMembershipFacts,
    filter: &SummaryFilter,
    detail: SummaryDetail,
    as_of_ms: u64,
) -> MempoolSummary {
    let mut all = AggregateBin::default();
    let mut matching = AggregateBin::default();
    let mut feerate_bins = HistogramAccumulator::new(FEERATE_BIN_COUNT);
    let mut age_bins = HistogramAccumulator::new(AGE_BIN_COUNT);
    let mut classification_bins = HistogramAccumulator::new(Classification::ALL.len());
    let mut script_bins = HistogramAccumulator::new(ScriptType::ALL.len());
    let mut value_bins = HistogramAccumulator::new(VALUE_BIN_COUNT);
    let mut input_bins = HistogramAccumulator::new(INPUT_BIN_COUNT);
    let mut output_bins = HistogramAccumulator::new(OUTPUT_BIN_COUNT);
    let mut ecdf_bins = detail
        .ecdf
        .then(|| vec![vec![0_u64; ECDF_FEE_BIN_COUNT]; Classification::ALL.len()]);
    let mut joint_grid = detail
        .joint_fee_size
        .then(|| vec![vec![0_u64; JOINT_FEE_BIN_COUNT]; JOINT_SIZE_BIN_COUNT]);

    for row in &facts.available {
        all.add(row.vsize);
        let feerate = row.fee_sats as f64 / row.vsize as f64;
        // A row without classifier evidence has the explicit honest verdict
        // `unknown`; shape-only dimensions stay underived instead.
        let classification = row
            .shape
            .map_or(Classification::Unknown, |shape| shape.classification);
        if !matches(
            filter,
            classification,
            row.shape.map(|shape| shape.script_type),
            feerate,
        ) {
            continue;
        }
        matching.add(row.vsize);
        feerate_bins.record(Some(feerate_bin(feerate)), row.vsize);
        let age_ms = as_of_ms.saturating_sub(row.entered_at_ms);
        age_bins.record(Some(age_bin(age_ms)), row.vsize);
        classification_bins.record(Some(classification.index()), row.vsize);
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
        if let Some(ecdf) = ecdf_bins.as_mut() {
            let bin = log_bin(
                feerate,
                ECDF_FEE_MIN_SAT_PER_VB,
                ECDF_FEE_MAX_SAT_PER_VB,
                ECDF_FEE_BIN_COUNT,
            );
            ecdf[classification.index()][bin] += row.vsize;
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

    MempoolSummary {
        source_id,
        as_of_ms,
        filter_echo: filter.clone(),
        totals: SummaryTotals {
            all,
            matching,
            awaiting_rpc: AwaitingRpcTotal {
                count: facts.awaiting_rpc_count,
            },
        },
        bins: BinCatalog::canonical(),
        histograms: SummaryHistograms {
            classification: classification_bins.finish(),
            script: script_bins.finish(),
            value: value_bins.finish(),
            inputs: input_bins.finish(),
            outputs: output_bins.finish(),
            age: age_bins.finish(),
            feerate: feerate_bins.finish(),
        },
        ecdf: ecdf_bins.map(fee_rate_ecdf),
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
        health: facts.health.clone(),
    }
}

// UnavailableReason is retained on the wire for dimensions that a future
// deployment mode cannot derive at all; with server-side enrichment active
// every dimension is computed, so the server no longer emits it.
const _: fn() = || {
    let _ = UnavailableReason::RequiresRawTransaction;
};

fn fee_rate_ecdf(per_class_bins: Vec<Vec<u64>>) -> FeeRateEcdf {
    let mut series = Vec::new();
    for classification in Classification::ALL {
        let bins = &per_class_bins[classification.index()];
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
            key: classification,
            cum_vsize,
        });
    }
    FeeRateEcdf {
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
    use atlas_model::CaptureStatus;

    use super::*;

    fn source() -> SourceId {
        SourceId::new("source-a").expect("source")
    }

    fn health() -> SourceHealth {
        SourceHealth {
            last_seen_at_ms: 1_000,
            capture: CaptureStatus::NoReportedGaps,
        }
    }

    const AS_OF_MS: u64 = 1_752_710_400_000;

    fn row(vsize: u64, fee_sats: u64, age_ms: u64) -> FactsRow {
        FactsRow {
            vsize,
            fee_sats,
            entered_at_ms: AS_OF_MS - age_ms,
            shape: None,
        }
    }

    fn shaped(vsize: u64, fee_sats: u64, age_ms: u64, shape: ShapeRow) -> FactsRow {
        FactsRow {
            shape: Some(shape),
            ..row(vsize, fee_sats, age_ms)
        }
    }

    fn payment_shape() -> ShapeRow {
        ShapeRow {
            total_output_sats: 5_000_000,
            input_count: 2,
            output_count: 2,
            script_type: ScriptType::P2wpkh,
            classification: Classification::Payment,
        }
    }

    fn facts(available: Vec<FactsRow>, awaiting_rpc_count: u64) -> SourceMembershipFacts {
        SourceMembershipFacts {
            health: health(),
            available,
            awaiting_rpc_count,
        }
    }

    fn parts(histogram: &DimensionHistogram) -> (&[AggregateBin], AggregateBin) {
        match histogram {
            DimensionHistogram::Available { bins, underived } => (bins, *underived),
            DimensionHistogram::Unavailable { .. } => panic!("expected available histogram"),
        }
    }

    #[test]
    fn awaiting_rpc_stays_out_of_all_aggregates() {
        let summary = compute_summary(
            source(),
            &facts(vec![row(200, 400, 0)], 3),
            &SummaryFilter::default(),
            SummaryDetail::default(),
            AS_OF_MS,
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
    fn shapeless_rows_fill_underived_buckets_and_the_unknown_class() {
        let summary = compute_summary(
            source(),
            &facts(vec![row(200, 1_700, 0)], 0),
            &SummaryFilter::default(),
            SummaryDetail::default(),
            AS_OF_MS,
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
        let (classification, underived) = parts(&summary.histograms.classification);
        assert_eq!(classification[Classification::Unknown.index()].count, 1);
        assert_eq!(underived, AggregateBin::default());
        let (_, feerate_underived) = parts(&summary.histograms.feerate);
        assert_eq!(feerate_underived, AggregateBin::default());
    }

    #[test]
    fn shaped_rows_land_in_derived_dimension_bins() {
        let coinjoin = ShapeRow {
            total_output_sats: 200_000_000,
            input_count: 40,
            output_count: 40,
            script_type: ScriptType::P2tr,
            classification: Classification::Coinjoin,
        };
        let summary = compute_summary(
            source(),
            &facts(
                vec![
                    shaped(200, 1_700, 0, payment_shape()),
                    shaped(11_000, 77_000, 0, coinjoin),
                    row(400, 800, 0),
                ],
                0,
            ),
            &SummaryFilter::default(),
            SummaryDetail::default(),
            AS_OF_MS,
        );

        let (classification, _) = parts(&summary.histograms.classification);
        assert_eq!(classification[Classification::Payment.index()].count, 1);
        assert_eq!(classification[Classification::Coinjoin.index()].count, 1);
        assert_eq!(classification[Classification::Unknown.index()].count, 1);

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
        let rows = vec![shaped(200, 1_700, 0, payment_shape()), row(400, 800, 0)];
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
        );
        assert_eq!(summary.totals.matching.count, 0);
    }

    #[test]
    fn class_facets_select_verdicts_including_honest_unknown() {
        let rows = vec![shaped(200, 1_700, 0, payment_shape()), row(400, 800, 0)];
        let payment_only = SummaryFilter {
            classes: Some(vec![Classification::Payment]),
            ..SummaryFilter::default()
        };
        let summary = compute_summary(
            source(),
            &facts(rows.clone(), 0),
            &payment_only,
            SummaryDetail::default(),
            AS_OF_MS,
        );
        assert_eq!(summary.totals.matching.count, 1);

        let unknown_only = SummaryFilter {
            classes: Some(vec![Classification::Unknown]),
            ..SummaryFilter::default()
        };
        let summary = compute_summary(
            source(),
            &facts(rows, 0),
            &unknown_only,
            SummaryDetail::default(),
            AS_OF_MS,
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
        let rows = vec![shaped(200, 1_700, 0, payment_shape()), row(300, 600, 0)];
        let bare = compute_summary(
            source(),
            &facts(rows.clone(), 0),
            &SummaryFilter::default(),
            SummaryDetail::default(),
            AS_OF_MS,
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
        );
        let ecdf = detailed.ecdf.expect("ecdf requested");
        assert_eq!(ecdf.fee_edges.len(), ECDF_FEE_BIN_COUNT + 1);
        let keys: Vec<Classification> = ecdf.series.iter().map(|series| series.key).collect();
        assert_eq!(keys, vec![Classification::Payment, Classification::Unknown]);
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
                }],
                0,
            ),
            &SummaryFilter::default(),
            SummaryDetail::default(),
            AS_OF_MS,
        );
        let (age, _) = parts(&summary.histograms.age);
        assert_eq!(age[0].count, 1);
    }
}
