// Pure presentation logic for the comparison view: turning wire regions into
// headline text plus the composition rows the workbench already knows how to
// render, and aligning a rejection taxonomy breakdown to display metadata. No
// DOM access, so all of this is unit-testable.
//
// Every headline states the region's fact-bearing count and virtual size, which
// are internally consistent (those N transactions total V vB). The
// `awaitingCount` compatibility field remains visible to the renderer, although
// complete SourceReplica state always supplies zero.

import type {
  AnomalyRegion,
  ComparisonStage,
  RegionAggregate,
  RejectionAvailability,
  RejectionTaxonomyBreakdown,
  RejectionReasonCount,
  TaxonomyDescriptor,
} from "./types";
import type { CompositionRow, DimensionBinMeta } from "./workbench-model";
import {
  compositionRows,
  formatCompact,
  formatCount,
  verdictMeta,
} from "./workbench-model";

/** Neutral grey used when a rejection verdict has no catalog colour. Matches
 * the workbench's unknown-verdict colour. */
const FALLBACK_VERDICT_COLOR = "#6b7a8d";

/** Returns a panel message only when rejection evidence is unavailable. An
 * available zero-count window is rendered separately as an observed empty
 * window. */
export const rejectionAvailabilityMessage = (
  availability: RejectionAvailability,
): string | null =>
  availability.status === "not_collected"
    ? "Rejection evidence is not collected in state-only mode."
    : null;

export const rejectionReasonLabel = (
  reason: Pick<RejectionReasonCount, "reason" | "is_rollup">,
): string => (reason.is_rollup ? "Other reasons" : reason.reason);

export interface RegionMagnitude {
  /** Full membership, including any legacy factless count. */
  totalCount: number;
  /** Fact-bearing members, which carry the virtual size. */
  presentCount: number;
  /** Summed virtual size of the fact-bearing members, in vB. */
  vsize: number;
  /** Legacy compatibility count; zero for complete SourceReplica state. */
  awaitingCount: number;
}

/** Works structurally for both a full `RegionAggregate` and an
 * `AnomalyRegion`, which share the `present`/`awaiting_rpc` shape. */
export const regionMagnitude = (region: {
  present: { count: number; vsize: number };
  awaiting_rpc: { count: number };
}): RegionMagnitude => ({
  totalCount: region.present.count + region.awaiting_rpc.count,
  presentCount: region.present.count,
  vsize: region.present.vsize,
  awaitingCount: region.awaiting_rpc.count,
});

export interface RegionView {
  magnitude: RegionMagnitude;
  headline: string;
  rows: CompositionRow[];
}

/** The shared region: transactions present in every compared source. */
export const sharedView = (
  shared: RegionAggregate,
  sourceCount: number,
  metric: "count" | "vsize",
): RegionView => {
  const magnitude = regionMagnitude(shared);
  return {
    magnitude,
    headline: `${formatCount(magnitude.presentCount)} tx (${formatCompact(magnitude.vsize)} vB) shared by all ${sourceCount} sources`,
    rows: compositionRows(shared, metric),
  };
};

export interface AnomalyView {
  /** True when `from` carries transactions `to` does not, which is unexpected
   * under the asserted least-to-most-permissive ordering. */
  present: boolean;
  totalCount: number;
  presentCount: number;
  vsize: number;
  awaitingCount: number;
  note: string;
}

export const anomalyView = (stage: {
  from: string;
  to: string;
  anomaly: AnomalyRegion;
}): AnomalyView => {
  const magnitude = regionMagnitude(stage.anomaly);
  const present = magnitude.totalCount > 0;
  return {
    present,
    totalCount: magnitude.totalCount,
    presentCount: magnitude.presentCount,
    vsize: magnitude.vsize,
    awaitingCount: magnitude.awaitingCount,
    note: present
      ? `${formatCount(magnitude.totalCount)} tx present in ${stage.from} but not ${stage.to} (unexpected under this ordering)`
      : `No transactions are in ${stage.from} but absent from ${stage.to}; the expected nesting holds for this pair.`,
  };
};

export interface StageView {
  magnitude: RegionMagnitude;
  headline: string;
  rows: CompositionRow[];
  anomaly: AnomalyView;
}

/** One staged delta: the `added` membership difference rendered as composition
 * rows plus the reverse-difference anomaly. */
export const stageView = (
  stage: ComparisonStage,
  metric: "count" | "vsize",
): StageView => {
  const magnitude = regionMagnitude(stage.added);
  return {
    magnitude,
    headline: `${formatCount(magnitude.presentCount)} tx (${formatCompact(magnitude.vsize)} vB) are present in ${stage.to} but not ${stage.from}`,
    rows: compositionRows(stage.added, metric),
    anomaly: anomalyView(stage),
  };
};

export interface RejectionVerdictSegment extends DimensionBinMeta {
  count: number;
}

/** Align a rejection taxonomy breakdown to display metadata, keeping only the
 * verdicts with a nonzero count. When the taxonomy is known to the comparison
 * catalog its verdict labels and colours come from `verdictMeta`; otherwise the
 * breakdown's own verdict key is used as a label with a neutral colour. */
export const rejectionVerdictSegments = (
  breakdown: RejectionTaxonomyBreakdown,
  catalog: TaxonomyDescriptor[],
): RejectionVerdictSegment[] => {
  const descriptor = catalog.find((entry) => entry.key === breakdown.key);
  const meta = descriptor === undefined ? [] : verdictMeta(descriptor);
  const byKey = new Map(meta.map((entry) => [entry.key, entry]));
  return breakdown.verdicts
    .filter((verdict) => verdict.count > 0)
    .map((verdict) => {
      const found = byKey.get(verdict.verdict);
      return {
        key: verdict.verdict,
        label: found?.label ?? verdict.verdict,
        color: found?.color ?? FALLBACK_VERDICT_COLOR,
        count: verdict.count,
      };
    });
};
