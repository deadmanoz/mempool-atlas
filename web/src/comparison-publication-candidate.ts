import {
  buildComparisonPolicyViewCooperatively,
  type ComparisonPolicyView,
} from "./comparison-policy-view";
import {
  awaitAtlasCandidateRelease,
  awaitAtlasPublicationAttemptRelease,
  type AtlasCandidateReadyDetail,
} from "./candidate-ready";
import type {
  ComparisonCanvasView,
  PreparedComparisonCanvas,
} from "./comparison-canvas-view";
import type {
  ComparisonDistributionsView,
  PreparedComparisonDistributions,
} from "./comparison-distributions-view";
import {
  compareCurrentSnapshots,
  requireLoadedSnapshot,
  type CurrentComparison,
} from "./comparison-model";
import { requirePublicationCommitRetry } from "./publication-commit-budget";
import type { LoadedSourcePublication } from "./types";

export interface PreparedComparisonPublication {
  comparison: CurrentComparison;
  policyView: ComparisonPolicyView;
  candidateKey: string;
  sourceIds: string[];
}

export interface PreparedComparisonCommitCandidate {
  publication: PreparedComparisonPublication;
  distributions: PreparedComparisonDistributions | null;
  canvas: PreparedComparisonCanvas;
  detail: AtlasCandidateReadyDetail;
}

export const commitPreparedComparisonDistributions = (
  complete: boolean,
  prepared: PreparedComparisonDistributions | null,
  current: CurrentComparison,
  distributionsView: ComparisonDistributionsView,
): void => {
  if (!complete) {
    distributionsView.reset();
    return;
  }
  if (prepared === null || !distributionsView.commit(prepared, current)) {
    throw new DOMException(
      "Comparison distribution candidate was superseded",
      "AbortError",
    );
  }
};

const publicationKey = ({
  snapshot_identity,
}: LoadedSourcePublication): string => snapshot_identity;

export const prepareComparisonPublication = async (
  left: LoadedSourcePublication,
  right: LoadedSourcePublication,
  complete: boolean,
  signal: AbortSignal,
): Promise<PreparedComparisonPublication> => {
  signal.throwIfAborted();
  const comparison = compareCurrentSnapshots(
    requireLoadedSnapshot(left),
    requireLoadedSnapshot(right),
    complete,
  );
  const policyView = await buildComparisonPolicyViewCooperatively(comparison, {
    signal,
  });
  signal.throwIfAborted();
  return {
    comparison,
    policyView,
    candidateKey: `${publicationKey(left)}|${publicationKey(right)}`,
    sourceIds: [left.publication.source_id, right.publication.source_id],
  };
};

export const prepareComparisonCommitCandidate = async (
  left: LoadedSourcePublication,
  right: LoadedSourcePublication,
  complete: boolean,
  replacement: boolean,
  signal: AbortSignal,
  isCurrent: () => boolean,
  distributionsView: ComparisonDistributionsView,
  canvasView: ComparisonCanvasView,
): Promise<PreparedComparisonCommitCandidate> => {
  for (let attempt = 1; ; attempt += 1) {
    const publication = await prepareComparisonPublication(
      left,
      right,
      complete,
      signal,
    );
    const distributions = complete
      ? await distributionsView.prepare(publication.comparison, signal)
      : null;
    const canvas = canvasView.prepareCandidate(publication.comparison);
    const canCommit = (): boolean =>
      canvasView.canCommitCandidate(canvas) &&
      (distributions === null ||
        distributionsView.canCommit(distributions, publication.comparison));
    await awaitAtlasPublicationAttemptRelease(
      { surface: "comparison", attempt, complete },
      signal,
    );
    signal.throwIfAborted();
    if (!isCurrent()) {
      throw new DOMException("Comparison request was superseded", "AbortError");
    }
    if (!canCommit()) {
      requirePublicationCommitRetry("Comparison", attempt);
      continue;
    }

    const detail: AtlasCandidateReadyDetail = {
      surface: "comparison",
      candidateKey: publication.candidateKey,
      sourceIds: publication.sourceIds,
    };
    if (replacement && complete) {
      await awaitAtlasCandidateRelease(detail, signal);
    }
    signal.throwIfAborted();
    if (!isCurrent()) {
      throw new DOMException("Comparison request was superseded", "AbortError");
    }
    if (!canCommit()) {
      requirePublicationCommitRetry("Comparison", attempt);
      continue;
    }
    return { publication, distributions, canvas, detail };
  }
};
