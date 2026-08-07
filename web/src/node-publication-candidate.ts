import {
  DEFAULT_CLASSIFIER_ID,
  classifierDescriptor,
} from "./classification-view";
import {
  awaitAtlasCandidateRelease,
  awaitAtlasPublicationAttemptRelease,
  type AtlasCandidateReadyDetail,
} from "./candidate-ready";
import { bip110RulePopulationSummary } from "./bip110-rule-index";
import {
  KNOTS_BIP110_CLASSIFIER_ID,
  classifierBucketForTransaction,
  precomputeClassifierBuckets,
  type ClassifierBucketKey,
} from "./classifier-terrain";
import {
  filterTransactionsCooperatively,
  type MempoolFilters,
} from "./filters";
import { findSnapshotTransaction, snapshotIsComplete } from "./packed-store";
import { requirePublicationCommitRetry } from "./publication-commit-budget";
import type {
  PreparedSnapshotDistributions,
  SnapshotDistributionSelection,
  SnapshotDistributionsView,
} from "./snapshot-distributions-view";
import {
  TERRAIN_RULES,
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
  snapshotIdentity: string;
  snapshot: MempoolSnapshot;
  classification: ClassificationProgress;
  complete: boolean;
  selectedClassifierId: string;
  selectedClassifierLabel: string | null;
  selectedClassifierBucketKey: ClassifierBucketKey | null;
  selectedInspector: TerrainSelection;
  viewState: NodeViewState;
  requestedTransaction: MempoolTransaction | undefined;
}

export interface NodePublicationInput {
  viewState: NodeViewState;
  terrainMode: TerrainMode;
  filters: Readonly<MempoolFilters>;
  currentSnapshotIdentity: string | null;
  selectedClassifierLabel: string | null;
  selectedClassifierBucketKey: ClassifierBucketKey | null;
  selectedInspector: TerrainSelection;
}

export interface PreparedNodePublicationCommit {
  candidate: NodePublicationCandidate;
  distributions: PreparedSnapshotDistributions | null;
  distributionSelection: SnapshotDistributionSelection;
  filteredTransactions: MempoolTransaction[] | null;
  detail: AtlasCandidateReadyDetail;
}

const sameFilters = (
  left: Readonly<MempoolFilters>,
  right: Readonly<MempoolFilters>,
): boolean =>
  left.minimumFeeRate === right.minimumFeeRate &&
  left.maximumAgeMs === right.maximumAgeMs &&
  left.minimumVsize === right.minimumVsize;

export const chooseInitialNodeRule = (snapshot: MempoolSnapshot): RuleId => {
  let chosen: RuleId = "element_size";
  let maximum = -1;
  for (const rule of TERRAIN_RULES) {
    const count = bip110RulePopulationSummary(
      snapshot.transactions,
      rule.id,
    ).count;
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
  candidateKey: candidate.snapshotIdentity,
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
    snapshotIdentity: response.snapshot_identity,
    snapshot,
    classification,
    complete,
    selectedClassifierId,
    selectedClassifierLabel: null,
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

export const retainNodePublicationSelection = (
  candidate: NodePublicationCandidate,
  input: NodePublicationInput,
): NodePublicationCandidate => {
  if (
    input.currentSnapshotIdentity !== candidate.snapshotIdentity ||
    input.viewState.classifier !== candidate.selectedClassifierId
  ) {
    return candidate;
  }

  const isBip110 =
    candidate.selectedClassifierId === KNOTS_BIP110_CLASSIFIER_ID;
  return {
    ...candidate,
    selectedClassifierLabel: isBip110 ? null : input.selectedClassifierLabel,
    selectedClassifierBucketKey: isBip110
      ? null
      : input.selectedClassifierBucketKey,
    selectedInspector: isBip110
      ? input.selectedInspector
      : candidate.selectedInspector,
    viewState: {
      ...candidate.viewState,
      selection: isBip110 ? input.selectedInspector : null,
    },
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
  for (let attempt = 1; ; attempt += 1) {
    const input = currentInput();
    const candidate = retainNodePublicationSelection(
      await prepareNodePublicationCandidate(
        response,
        input.viewState,
        complete,
        signal,
      ),
      input,
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
    const filteredTransactions = complete
      ? await filterTransactionsCooperatively(
          candidate.snapshot.transactions,
          input.filters,
          candidate.snapshot.observed_at_ms,
          { signal },
        )
      : null;
    const canCommit = (): boolean => {
      const current = currentInput();
      return (
        current.viewState === input.viewState &&
        current.terrainMode === input.terrainMode &&
        sameFilters(current.filters, input.filters) &&
        current.currentSnapshotIdentity === input.currentSnapshotIdentity &&
        current.selectedClassifierLabel === input.selectedClassifierLabel &&
        current.selectedClassifierBucketKey ===
          input.selectedClassifierBucketKey &&
        current.selectedInspector === input.selectedInspector &&
        (distributions === null ||
          distributionsView.canCommit(
            distributions,
            candidate.snapshot,
            distributionSelection,
          ))
      );
    };
    await awaitAtlasPublicationAttemptRelease(
      { surface: "node", attempt, complete },
      signal,
    );
    signal.throwIfAborted();
    if (!isCurrent()) {
      throw new DOMException("Node request was superseded", "AbortError");
    }
    if (!canCommit()) {
      requirePublicationCommitRetry("Node", attempt);
      continue;
    }
    const detail = nodePublicationCandidateReadyDetail(candidate);
    if (replacement && complete) {
      await awaitAtlasCandidateRelease(detail, signal);
    }
    signal.throwIfAborted();
    if (!isCurrent()) {
      throw new DOMException("Node request was superseded", "AbortError");
    }
    if (!canCommit()) {
      requirePublicationCommitRetry("Node", attempt);
      continue;
    }
    return {
      candidate,
      distributions,
      distributionSelection,
      filteredTransactions,
      detail,
    };
  }
};
