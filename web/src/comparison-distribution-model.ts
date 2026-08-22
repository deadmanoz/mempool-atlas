import {
  comparisonDistributionTransactions,
  type ComparisonDistributionScope,
} from "./comparison-distribution-population";
import type { ComparisonSide, CurrentComparison } from "./comparison-model";
import type { ComparisonDistributionSelection } from "./comparison-distribution-controls";
import {
  buildSnapshotDistributionModelCooperatively,
  type SnapshotDistributionModel,
} from "./snapshot-distributions";
import type { TerrainMode } from "./terrain";

const COMPARISON_PANEL_GROUP_LIMIT = 4;
const COMPARISON_DISTRIBUTION_BATCH_SIZE = 3_000;

export interface ComparisonSnapshotIdentity {
  sourceId: string;
  observedAtMs: number;
  classificationRevision: number;
}

export const PREPARED_COMPARISON_VIEW = Symbol("prepared-comparison-view");

export interface PreparedComparisonDistributions {
  readonly current: CurrentComparison;
  readonly scope: ComparisonDistributionScope;
  readonly selection: ComparisonDistributionSelection;
  readonly models: Readonly<Record<ComparisonSide, SnapshotDistributionModel>>;
  readonly prefetchedModels: Readonly<
    Partial<
      Record<
        ComparisonDistributionScope,
        Readonly<Record<ComparisonSide, SnapshotDistributionModel>>
      >
    >
  >;
  readonly variants: Readonly<Record<ComparisonSide, string>>;
  readonly leftIdentity: ComparisonSnapshotIdentity;
  readonly rightIdentity: ComparisonSnapshotIdentity;
  readonly [PREPARED_COMPARISON_VIEW]: object;
}

export const comparisonSnapshotIdentity = (
  current: CurrentComparison,
  side: ComparisonSide,
): ComparisonSnapshotIdentity => ({
  sourceId: current[side].snapshot.source_id,
  observedAtMs: current[side].snapshot.observed_at_ms,
  classificationRevision: current[side].snapshot.classification_revision,
});

export const sameComparisonSnapshotIdentity = (
  current: CurrentComparison,
  side: ComparisonSide,
  identity: ComparisonSnapshotIdentity,
): boolean => {
  const { snapshot } = current[side];
  return (
    snapshot.source_id === identity.sourceId &&
    snapshot.observed_at_ms === identity.observedAtMs &&
    snapshot.classification_revision === identity.classificationRevision
  );
};

export const comparisonDistributionVariant = (
  side: ComparisonSide,
  scope: ComparisonDistributionScope,
  classifierId: string | null,
  metric: TerrainMode,
): string =>
  `side=${side};scope=${scope};classifier=${classifierId ?? "none"};metric=${metric};groups=${COMPARISON_PANEL_GROUP_LIMIT};dataGroups=${COMPARISON_PANEL_GROUP_LIMIT}`;

export const buildComparisonDistributionSideModel = (
  current: CurrentComparison,
  side: ComparisonSide,
  scope: ComparisonDistributionScope,
  classifierId: string | null,
  metric: TerrainMode,
  signal: AbortSignal,
): Promise<SnapshotDistributionModel> => {
  const { snapshot } = current[side];
  const selectedDescriptor = snapshot.classifier_catalog.find(
    ({ id }) => id === classifierId,
  );
  return buildSnapshotDistributionModelCooperatively(
    {
      transactions: comparisonDistributionTransactions(current, side, scope),
      classifierCatalog: snapshot.classifier_catalog,
      selectedClassifier: selectedDescriptor ?? null,
      observedAtMs: snapshot.observed_at_ms,
      metric,
      groupLimit: COMPARISON_PANEL_GROUP_LIMIT,
      dataGroupLimit: COMPARISON_PANEL_GROUP_LIMIT,
    },
    {
      signal,
      batchSize: COMPARISON_DISTRIBUTION_BATCH_SIZE,
      aggregateOnlyClassifierBuckets: true,
    },
  );
};
