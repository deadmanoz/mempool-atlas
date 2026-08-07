import {
  DEFAULT_CLASSIFIER_ID,
  classifierDescriptor,
} from "./classification-view";
import {
  awaitAtlasCandidateRelease,
  type AtlasCandidateReadyDetail,
} from "./candidate-ready";
import {
  KNOTS_BIP110_CLASSIFIER_ID,
  classifierBucketForTransaction,
  precomputeClassifierBuckets,
  type ClassifierBucketKey,
} from "./classifier-terrain";
import { findSnapshotTransaction, snapshotIsComplete } from "./packed-store";
import type {
  PreparedSnapshotDistributions,
  SnapshotDistributionSelection,
  SnapshotDistributionsView,
} from "./snapshot-distributions-view";
import {
  TERRAIN_RULES,
  rulePopulation,
  terrainRegionKey,
  type TerrainMode,
  type TerrainSelection,
} from "./terrain";
import type {
  ClassificationProgress,
  LoadedSourcePublication,
  MempoolSnapshot,
  MempoolTransaction,
  RuleId,
  SourceSummary,
} from "./types";
import type { NodeViewState } from "./view-state";

export interface NodePublicationCandidate {
  source: SourceSummary;
  snapshot: MempoolSnapshot;
  classification: ClassificationProgress;
  complete: boolean;
  selectedClassifierId: string;
  selectedClassifierBucketKey: ClassifierBucketKey | null;
  selectedInspector: TerrainSelection;
  viewState: NodeViewState;
  requestedTransaction: MempoolTransaction | undefined;
}

export interface NodePublicationInput {
  viewState: NodeViewState;
  terrainMode: TerrainMode;
}

export interface PreparedNodePublicationCommit {
  candidate: NodePublicationCandidate;
  distributions: PreparedSnapshotDistributions | null;
  distributionSelection: SnapshotDistributionSelection;
  detail: AtlasCandidateReadyDetail;
}

export const chooseInitialNodeRule = (snapshot: MempoolSnapshot): RuleId => {
  let chosen: RuleId = "element_size";
  let maximum = -1;
  for (const rule of TERRAIN_RULES) {
    const count = rulePopulation(snapshot.transactions, rule.id).count;
    if (count > maximum) {
      chosen = rule.id;
      maximum = count;
    }
  }
  return chosen;
};

export const nodePublicationCandidateReadyDetail = (
  candidate: NodePublicationCandidate,
): AtlasCandidateReadyDetail => ({
  surface: "node",
  candidateKey: `${candidate.snapshot.source_id}:${candidate.snapshot.observed_at_ms}:${candidate.snapshot.classification_revision}`,
  sourceIds: [candidate.source.source_id],
});

export const prepareNodePublicationCandidate = async (
  response: LoadedSourcePublication,
  requestedState: NodeViewState,
  expectedComplete: boolean,
  signal?: AbortSignal,
): Promise<NodePublicationCandidate> => {
  const { source, publication: snapshot } = response;
  const complete = snapshotIsComplete(snapshot);
  if (complete !== expectedComplete) {
    throw new Error("Publication readiness does not match its renderer");
  }
  const classification = source.classification;
  if (classification === null) {
    throw new Error("Loaded publication is missing classification progress");
  }

  const requestedClassifierId =
    requestedState.classifier ??
    (requestedState.selection === null
      ? DEFAULT_CLASSIFIER_ID
      : KNOTS_BIP110_CLASSIFIER_ID);
  const selectedClassifierId =
    snapshot.classifier_catalog.find(({ id }) => id === requestedClassifierId)
      ?.id ??
    snapshot.classifier_catalog.find(({ id }) => id === DEFAULT_CLASSIFIER_ID)
      ?.id ??
    snapshot.classifier_catalog[0]?.id ??
    DEFAULT_CLASSIFIER_ID;

  await precomputeClassifierBuckets(
    snapshot.transactions,
    complete
      ? snapshot.classifier_catalog
      : snapshot.classifier_catalog.filter(
          ({ id }) => id === selectedClassifierId,
        ),
    signal === undefined ? {} : { signal },
  );
  signal?.throwIfAborted();

  const requestedTransaction =
    requestedState.txid === null
      ? undefined
      : findSnapshotTransaction(snapshot, requestedState.txid);
  let selectedClassifierBucketKey: ClassifierBucketKey | null = null;
  let selectedInspector: TerrainSelection;
  if (selectedClassifierId === KNOTS_BIP110_CLASSIFIER_ID) {
    selectedInspector =
      requestedTransaction === undefined
        ? (requestedState.selection ?? {
            kind: "rule",
            rule: chooseInitialNodeRule(snapshot),
          })
        : {
            kind: "region",
            regionKey: terrainRegionKey(requestedTransaction),
          };
  } else {
    selectedInspector = { kind: "rule", rule: "element_size" };
    const descriptor = classifierDescriptor(snapshot, selectedClassifierId);
    selectedClassifierBucketKey =
      requestedTransaction === undefined || descriptor === null
        ? null
        : classifierBucketForTransaction(requestedTransaction, descriptor).key;
  }

  return {
    source,
    snapshot,
    classification,
    complete,
    selectedClassifierId,
    selectedClassifierBucketKey,
    selectedInspector,
    viewState: {
      source: source.source_id,
      classifier: selectedClassifierId,
      selection:
        selectedClassifierId === KNOTS_BIP110_CLASSIFIER_ID
          ? selectedInspector
          : null,
      txid: requestedState.txid,
    },
    requestedTransaction,
  };
};

export const prepareNodePublicationCommit = async (
  response: LoadedSourcePublication,
  complete: boolean,
  replacement: boolean,
  signal: AbortSignal,
  isCurrent: () => boolean,
  currentInput: () => NodePublicationInput,
  distributionsView: SnapshotDistributionsView,
): Promise<PreparedNodePublicationCommit> => {
  while (true) {
    const input = currentInput();
    const candidate = await prepareNodePublicationCandidate(
      response,
      input.viewState,
      complete,
      signal,
    );
    const distributionSelection: SnapshotDistributionSelection = {
      classifierId: candidate.selectedClassifierId,
      bucketKey: candidate.selectedClassifierBucketKey,
      metric: input.terrainMode,
    };
    const distributions = complete
      ? await distributionsView.prepare(
          candidate.snapshot,
          distributionSelection,
          signal,
        )
      : null;
    const canCommit = (): boolean =>
      currentInput().viewState === input.viewState &&
      currentInput().terrainMode === input.terrainMode &&
      (distributions === null ||
        distributionsView.canCommit(
          distributions,
          candidate.snapshot,
          distributionSelection,
        ));
    signal.throwIfAborted();
    if (!isCurrent()) {
      throw new DOMException("Node request was superseded", "AbortError");
    }
    if (!canCommit()) continue;
    const detail = nodePublicationCandidateReadyDetail(candidate);
    if (replacement && complete) {
      await awaitAtlasCandidateRelease(detail, signal);
    }
    signal.throwIfAborted();
    if (!isCurrent()) {
      throw new DOMException("Node request was superseded", "AbortError");
    }
    if (!canCommit()) continue;
    return { candidate, distributions, distributionSelection, detail };
  }
};
