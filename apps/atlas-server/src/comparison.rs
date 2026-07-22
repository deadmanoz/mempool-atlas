//! Server-side assembly of the read-time source comparison.
//!
//! Atlas never stores a combined mempool. The comparison is derived on demand
//! from the source-partitioned `current_membership` projections: the store
//! computes each membership-set region with set-algebra SQL and gathers only
//! the designated facts source's rows for that region, and this module runs the
//! summary aggregation over each region so only aggregates ever leave the
//! server.
//!
//! The caller supplies the source order. The server reports adjacent membership
//! differences in that order without inferring why a transaction is absent from
//! either source. `added` is `to \\ from` and `anomaly` is the reverse
//! difference. Shape and classification are intrinsic to a transaction, so
//! every region's histograms are well defined by txid; fee-rate and age come
//! from the region's one designated facts source.

use atlas_model::{
    AggregateBin, AwaitingRpcTotal, ComparisonSourceTotal, ComparisonStage, SourceComparison,
    SourceId, TaxonomyDescriptor,
};

use crate::summary::{RegionFacts, anomaly_region, region_aggregate};

/// One compared source's whole-membership totals: fact-bearing count and summed
/// virtual size, plus the count of members still awaiting RPC facts.
#[derive(Clone, Debug, Default)]
pub struct SourceTotal {
    pub present: AggregateBin,
    pub awaiting_rpc_count: u64,
}

/// The store inputs for one adjacent-pair stage `(from, to)` of the comparison.
/// `added` is aggregated from `to`; `anomaly` from `from`.
#[derive(Clone, Debug)]
pub struct StageInputs {
    pub from: SourceId,
    pub to: SourceId,
    pub added: RegionFacts,
    pub anomaly: RegionFacts,
}

/// Everything the comparison read model needs from the store: per-source
/// totals in request order, the shared-region facts from the last source, and
/// one stage per adjacent pair in request order.
#[derive(Clone, Debug)]
pub struct ComparisonInputs {
    pub source_totals: Vec<SourceTotal>,
    pub shared: RegionFacts,
    pub stages: Vec<StageInputs>,
}

/// The result of gathering comparison inputs: either every requested source
/// exists and its inputs were gathered, or one requested source is unknown and
/// is named so the handler can return a 404 identifying it.
#[derive(Clone, Debug)]
pub enum ComparisonOutcome {
    Computed(ComparisonInputs),
    UnknownSource(SourceId),
}

/// Assembles the comparison read model from the store inputs, the requested
/// source order, the registered taxonomy descriptors, and the request instant.
#[must_use]
pub fn compute_comparison(
    sources: Vec<SourceId>,
    inputs: &ComparisonInputs,
    as_of_ms: u64,
    taxonomies: &[TaxonomyDescriptor],
) -> SourceComparison {
    let source_totals = sources
        .iter()
        .zip(&inputs.source_totals)
        .map(|(source_id, total)| ComparisonSourceTotal {
            source_id: source_id.clone(),
            present: total.present,
            awaiting_rpc: AwaitingRpcTotal {
                count: total.awaiting_rpc_count,
            },
        })
        .collect();
    let shared = region_aggregate(&inputs.shared, as_of_ms, taxonomies);
    let stages = inputs
        .stages
        .iter()
        .map(|stage| ComparisonStage {
            from: stage.from.clone(),
            to: stage.to.clone(),
            added: region_aggregate(&stage.added, as_of_ms, taxonomies),
            anomaly: anomaly_region(&stage.anomaly),
        })
        .collect();
    SourceComparison {
        sources,
        as_of_ms,
        source_totals,
        shared,
        stages,
    }
}
