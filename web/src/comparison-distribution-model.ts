import { TRANSACTION_PROPERTIES_CLASSIFIER_ID } from "./classifier-terrain";
import {
  comparisonDistributionTransactions,
  type ComparisonDistributionScope,
} from "./comparison-distribution-population";
import type { ComparisonSide, CurrentComparison } from "./comparison-model";
import {
  buildSnapshotDistributionModelCooperatively,
  type SnapshotDistributionModel,
} from "./snapshot-distributions";

const COMPARISON_PANEL_GROUP_LIMIT = 4;
const COMPARISON_DISTRIBUTION_BATCH_SIZE = 3_000;

export interface ComparisonSnapshotIdentity {
  sourceId: string;
  observedAtMs: number;
  classificationRevision: number;
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
): string =>
  `side=${side};scope=${scope};metric=vsize;groups=${COMPARISON_PANEL_GROUP_LIMIT};dataGroups=${COMPARISON_PANEL_GROUP_LIMIT}`;

export const buildComparisonDistributionSideModel = (
  current: CurrentComparison,
  side: ComparisonSide,
  scope: ComparisonDistributionScope,
  signal: AbortSignal,
): Promise<SnapshotDistributionModel> => {
  const { snapshot } = current[side];
  const propertyDescriptor = snapshot.classifier_catalog.find(
    ({ id }) => id === TRANSACTION_PROPERTIES_CLASSIFIER_ID,
  );
  return buildSnapshotDistributionModelCooperatively(
    {
      transactions: comparisonDistributionTransactions(current, side, scope),
      classifierCatalog: snapshot.classifier_catalog,
      selectedClassifier: propertyDescriptor ?? null,
      observedAtMs: snapshot.observed_at_ms,
      metric: "vsize",
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
