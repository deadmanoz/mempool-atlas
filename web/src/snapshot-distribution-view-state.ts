import type { ClassifierBucketKey } from "./classifier-terrain";
import type { TerrainMode } from "./terrain";
import type { MempoolSnapshot } from "./types";

export interface SnapshotDistributionSelection {
  classifierId: string;
  bucketKey: ClassifierBucketKey | null;
  metric: TerrainMode;
}

export interface SnapshotDistributionIdentity {
  sourceId: string;
  observedAtMs: number;
  classificationRevision: number;
}

export const snapshotDistributionIdentity = (
  snapshot: MempoolSnapshot,
): SnapshotDistributionIdentity => ({
  sourceId: snapshot.source_id,
  observedAtMs: snapshot.observed_at_ms,
  classificationRevision: snapshot.classification_revision,
});

export const sameSnapshotDistributionIdentity = (
  snapshot: MempoolSnapshot,
  identity: SnapshotDistributionIdentity,
): boolean =>
  snapshot.source_id === identity.sourceId &&
  snapshot.observed_at_ms === identity.observedAtMs &&
  snapshot.classification_revision === identity.classificationRevision;

export const sameSnapshotDistributionSelection = (
  left: SnapshotDistributionSelection,
  right: SnapshotDistributionSelection,
): boolean =>
  left.classifierId === right.classifierId &&
  left.bucketKey === right.bucketKey &&
  left.metric === right.metric;

export const snapshotDistributionVariant = (
  selection: SnapshotDistributionSelection,
  groupLimit: number,
  dataGroupLimit: number,
): string =>
  `classifier=${selection.classifierId};metric=${selection.metric};groups=${groupLimit};dataGroups=${dataGroupLimit}`;
