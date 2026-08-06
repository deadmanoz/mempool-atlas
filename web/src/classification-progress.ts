import type { ClassificationProgress } from "./types";

export interface ClassificationPresentation {
  label: "Classifying" | "Complete" | "Paused";
  compact: string;
  summary: string;
  unclassifiedDetail: string;
}

export interface TransactionDetailFailurePresentation {
  status:
    | "Pending assessment"
    | "Assessment unavailable"
    | "Detail no longer current";
  detail: string;
}

type FormatCount = (value: number) => string;

const defaultFormatCount: FormatCount = (value) => String(value);

const transactionLabel = (count: number): string =>
  count === 1 ? "transaction" : "transactions";

export const unclassifiedLabel = (
  progress: ClassificationProgress,
): "Pending assessment" | "Assessment unavailable" =>
  progress.state === "classifying"
    ? "Pending assessment"
    : "Assessment unavailable";

export const classificationPresentation = (
  progress: ClassificationProgress,
  transactionCount: number,
  formatCount: FormatCount = defaultFormatCount,
): ClassificationPresentation => {
  const classified = formatCount(progress.classified_count);
  const unclassified = formatCount(progress.unclassified_count);
  const total = formatCount(transactionCount);
  const fullCoverageSubject =
    transactionCount === 1 ? "The transaction" : `All ${total} transactions`;

  if (progress.state === "classifying") {
    return {
      label: "Classifying",
      compact: `Classifying · ${classified}/${total} assessed`,
      summary:
        progress.unclassified_count === 0
          ? `${fullCoverageSubject} currently ${transactionCount === 1 ? "has" : "have"} an assessment. Atlas is still resolving incomplete checks. Refresh to read newer progress.`
          : `${classified} of ${total} assessed; ${unclassified} ${transactionLabel(progress.unclassified_count)} ${progress.unclassified_count === 1 ? "has an assessment" : "have assessments"} pending while classification continues. Refresh to read newer progress.`,
      unclassifiedDetail:
        "Assessment is pending for this transaction while classification of this snapshot continues.",
    };
  }

  if (progress.state === "paused") {
    return {
      label: "Paused",
      compact: `Paused · ${classified}/${total} assessed`,
      summary:
        progress.unclassified_count === 0
          ? `${fullCoverageSubject} currently ${transactionCount === 1 ? "has" : "have"} an assessment. Classification paused after an operational failure while policy work remained. It retries with the next snapshot.`
          : `${classified} of ${total} assessed. Classification paused after an operational failure with assessments unavailable for ${unclassified} ${transactionLabel(progress.unclassified_count)}. It retries with the next snapshot.`,
      unclassifiedDetail:
        "Assessment is unavailable because classification paused after an operational failure. It will be retried with the next snapshot.",
    };
  }

  if (transactionCount === 0) {
    return {
      label: "Complete",
      compact: "Complete · 0/0 assessed",
      summary: "The snapshot is empty; classification is complete.",
      unclassifiedDetail:
        "Assessment is unavailable for this transaction in the current snapshot.",
    };
  }

  return {
    label: "Complete",
    compact: `Complete · ${classified}/${total} assessed`,
    summary:
      progress.unclassified_count === 0
        ? `${fullCoverageSubject} ${transactionCount === 1 ? "was" : "were"} assessed for this snapshot.`
        : `${classified} of ${total} assessed. The pass is complete; assessments are unavailable for ${unclassified} ${transactionLabel(progress.unclassified_count)} in this snapshot.`,
    unclassifiedDetail:
      "Assessment is unavailable because Atlas could not produce one during the completed classification pass for this snapshot.",
  };
};

export const transactionDetailFailurePresentation = (
  progress: ClassificationProgress,
  transactionCount: number,
  transactionHasAssessment: boolean,
  formatCount: FormatCount = defaultFormatCount,
): TransactionDetailFailurePresentation => {
  if (transactionHasAssessment) {
    return {
      status: "Detail no longer current",
      detail:
        "Atlas no longer has rule evidence matching this displayed snapshot. Refresh to load the current snapshot before relying on transaction detail.",
    };
  }

  return {
    status: unclassifiedLabel(progress),
    detail: `${classificationPresentation(progress, transactionCount, formatCount).unclassifiedDetail} Atlas has no rule evidence to show.`,
  };
};
