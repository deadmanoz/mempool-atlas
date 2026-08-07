import { classificationResult } from "./classification-view";
import {
  classifierBucketForTransaction,
  type ClassifierBucketKey,
} from "./classifier-terrain";
import {
  terrainRegionKey,
  type TerrainRegionKey,
  type TerrainSelection,
} from "./terrain";
import type { ClassifierDescriptor, MempoolTransaction, RuleId } from "./types";

export type NodeSampleSelection =
  | { kind: "bip110-region"; regionKey: TerrainRegionKey }
  | { kind: "bip110-rule"; rule: RuleId }
  | {
      kind: "classifier-bucket";
      descriptor: ClassifierDescriptor;
      bucketKey: ClassifierBucketKey;
    }
  | { kind: "classifier-label"; classifierId: string; label: string };

export interface NodeSampleSelectionState {
  terrain: boolean;
  bip110: boolean;
  inspector: TerrainSelection;
  descriptor: ClassifierDescriptor | null;
  bucketKey: ClassifierBucketKey | null;
  classifierId: string;
  label: string | null;
}

export const resolveNodeSampleSelection = ({
  terrain,
  bip110,
  inspector,
  descriptor,
  bucketKey,
  classifierId,
  label,
}: NodeSampleSelectionState): NodeSampleSelection | null => {
  if (terrain && bip110) {
    return inspector.kind === "region"
      ? { kind: "bip110-region", regionKey: inspector.regionKey }
      : { kind: "bip110-rule", rule: inspector.rule };
  }
  if (terrain && descriptor !== null && bucketKey !== null) {
    return { kind: "classifier-bucket", descriptor, bucketKey };
  }
  return label === null
    ? null
    : { kind: "classifier-label", classifierId, label };
};

export const nodeSampleSelectionContains = (
  transaction: MempoolTransaction,
  selection: NodeSampleSelection,
): boolean => {
  if (selection.kind === "bip110-region") {
    return terrainRegionKey(transaction) === selection.regionKey;
  }
  if (selection.kind === "bip110-rule") {
    return transaction.bip110?.violated_rules.includes(selection.rule) ?? false;
  }
  if (selection.kind === "classifier-bucket") {
    return (
      classifierBucketForTransaction(transaction, selection.descriptor).key ===
      selection.bucketKey
    );
  }
  return (
    classificationResult(transaction, selection.classifierId)?.labels.includes(
      selection.label,
    ) ?? false
  );
};
