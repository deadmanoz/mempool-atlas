import { describe, expect, it } from "vitest";

import {
  classificationPresentation,
  transactionDetailFailurePresentation,
  unclassifiedLabel,
} from "./classification-progress";
import type { ClassificationProgress } from "./types";

describe("classificationPresentation", () => {
  const cases: Array<{
    name: string;
    progress: ClassificationProgress;
    total: number;
    label: string;
    compact: string;
    summary: string;
    detail: string;
  }> = [
    {
      name: "classifying with pending assessments",
      progress: {
        state: "classifying",
        revision: 3,
        classified_count: 8,
        unclassified_count: 2,
      },
      total: 10,
      label: "Classifying",
      compact: "Classifying · 8/10 assessed",
      summary:
        "8 of 10 assessed; 2 transactions have assessments pending while classification continues. Refresh to read newer progress.",
      detail:
        "Assessment is pending for this transaction while classification of this snapshot continues.",
    },
    {
      name: "classifying while every transaction has a partial or complete assessment",
      progress: {
        state: "classifying",
        revision: 4,
        classified_count: 10,
        unclassified_count: 0,
      },
      total: 10,
      label: "Classifying",
      compact: "Classifying · 10/10 assessed",
      summary:
        "All 10 transactions currently have an assessment. Atlas is still resolving incomplete checks. Refresh to read newer progress.",
      detail:
        "Assessment is pending for this transaction while classification of this snapshot continues.",
    },
    {
      name: "complete with terminal gaps",
      progress: {
        state: "complete",
        revision: 5,
        classified_count: 7,
        unclassified_count: 3,
      },
      total: 10,
      label: "Complete",
      compact: "Complete · 7/10 assessed",
      summary:
        "7 of 10 assessed. The pass is complete; assessments are unavailable for 3 transactions in this snapshot.",
      detail:
        "Assessment is unavailable because Atlas could not produce one during the completed classification pass for this snapshot.",
    },
    {
      name: "complete without gaps",
      progress: {
        state: "complete",
        revision: 5,
        classified_count: 10,
        unclassified_count: 0,
      },
      total: 10,
      label: "Complete",
      compact: "Complete · 10/10 assessed",
      summary: "All 10 transactions were assessed for this snapshot.",
      detail:
        "Assessment is unavailable because Atlas could not produce one during the completed classification pass for this snapshot.",
    },
    {
      name: "paused after a systemic failure",
      progress: {
        state: "paused",
        revision: 2,
        classified_count: 6,
        unclassified_count: 4,
      },
      total: 10,
      label: "Paused",
      compact: "Paused · 6/10 assessed",
      summary:
        "6 of 10 assessed. Classification paused after an operational failure with assessments unavailable for 4 transactions. It retries with the next snapshot.",
      detail:
        "Assessment is unavailable because classification paused after an operational failure. It will be retried with the next snapshot.",
    },
    {
      name: "empty complete snapshot",
      progress: {
        state: "complete",
        revision: 0,
        classified_count: 0,
        unclassified_count: 0,
      },
      total: 0,
      label: "Complete",
      compact: "Complete · 0/0 assessed",
      summary: "The snapshot is empty; classification is complete.",
      detail:
        "Assessment is unavailable for this transaction in the current snapshot.",
    },
  ];

  for (const testCase of cases) {
    it(testCase.name, () => {
      const presentation = classificationPresentation(
        testCase.progress,
        testCase.total,
      );

      expect(presentation).toEqual({
        label: testCase.label,
        compact: testCase.compact,
        summary: testCase.summary,
        unclassifiedDetail: testCase.detail,
      });
    });
  }

  it("uses the caller's count formatter", () => {
    const presentation = classificationPresentation(
      {
        state: "complete",
        revision: 1,
        classified_count: 1_234,
        unclassified_count: 1,
      },
      1_235,
      (value) => `[${value}]`,
    );

    expect(presentation.compact).toBe("Complete · [1234]/[1235] assessed");
    expect(presentation.summary).toContain("[1] transaction");
  });

  it("uses singular transaction grammar in every full-coverage state", () => {
    const summaries = (["classifying", "paused", "complete"] as const).map(
      (state) =>
        classificationPresentation(
          {
            state,
            revision: 1,
            classified_count: 1,
            unclassified_count: 0,
          },
          1,
        ).summary,
    );

    expect(summaries[0]).toContain("The transaction currently has");
    expect(summaries[1]).toContain("The transaction currently has");
    expect(summaries[2]).toContain("The transaction was assessed");
    expect(summaries.join(" ")).not.toContain("1 transactions");
  });
});

describe("unclassifiedLabel", () => {
  it.each([
    ["classifying", "Pending assessment"],
    ["complete", "Assessment unavailable"],
    ["paused", "Assessment unavailable"],
  ] as const)("labels %s lifecycle nulls", (state, expected) => {
    expect(
      unclassifiedLabel({
        state,
        revision: 1,
        classified_count: 0,
        unclassified_count: 1,
      }),
    ).toBe(expected);
  });
});

describe("transactionDetailFailurePresentation", () => {
  const progress: ClassificationProgress = {
    state: "complete",
    revision: 3,
    classified_count: 9,
    unclassified_count: 1,
  };

  it("uses lifecycle-aware copy for an unavailable assessment", () => {
    expect(transactionDetailFailurePresentation(progress, 10, false)).toEqual({
      status: "Assessment unavailable",
      detail:
        "Assessment is unavailable because Atlas could not produce one during the completed classification pass for this snapshot. Atlas has no rule evidence to show.",
    });
  });

  it("labels an unclassified transaction as pending while classification runs", () => {
    expect(
      transactionDetailFailurePresentation(
        {
          state: "classifying",
          revision: 2,
          classified_count: 6,
          unclassified_count: 4,
        },
        10,
        false,
      ),
    ).toEqual({
      status: "Pending assessment",
      detail:
        "Assessment is pending for this transaction while classification of this snapshot continues. Atlas has no rule evidence to show.",
    });
  });

  it("does not overwrite a displayed assessment with generic 503 copy", () => {
    expect(transactionDetailFailurePresentation(progress, 10, true)).toEqual({
      status: "Detail no longer current",
      detail:
        "Atlas no longer has rule evidence matching this displayed snapshot. Refresh to load the current snapshot before relying on transaction detail.",
    });
  });
});
