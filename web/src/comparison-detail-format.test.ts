import { describe, expect, it, vi } from "vitest";

vi.mock("./terrain", async (importOriginal) => {
  const actual = await importOriginal<typeof import("./terrain")>();
  return {
    ...actual,
    violationSignature: (
      assessment: Parameters<typeof actual.violationSignature>[0],
    ) =>
      assessment.status === "violating" &&
      assessment.violated_rules.length === 0
        ? null
        : actual.violationSignature(assessment),
  };
});

import {
  compactComparisonEvidence,
  comparisonAssessmentText,
} from "./comparison-detail-format";
import type { Bip110Assessment } from "./types";

describe("comparisonAssessmentText", () => {
  it.each<
    [description: string, assessment: Bip110Assessment, expected: string]
  >([
    [
      "compatible assessments",
      {
        status: "compatible",
        primary_rule: null,
        violated_rules: [],
        unknown_rules: [],
      },
      "Compatible with the deployed Knots mempool policy",
    ],
    [
      "indeterminate assessments",
      {
        status: "indeterminate",
        primary_rule: null,
        violated_rules: [],
        unknown_rules: ["output_size"],
      },
      "Indeterminate because one or more rule checks remain unresolved",
    ],
    [
      "violating assessments without a signature",
      {
        status: "violating",
        primary_rule: null,
        violated_rules: [],
        unknown_rules: [],
      },
      "Policy assessment unavailable",
    ],
    [
      "exact single-rule violations",
      {
        status: "violating",
        primary_rule: "output_size",
        violated_rules: ["output_size"],
        unknown_rules: [],
      },
      "Would violate exactly R1 only",
    ],
    [
      "exact multi-rule violations",
      {
        status: "violating",
        primary_rule: "output_size",
        violated_rules: ["output_size", "element_size"],
        unknown_rules: [],
      },
      "Would violate exactly R1 + R2",
    ],
    [
      "partial violations",
      {
        status: "violating",
        primary_rule: "output_size",
        violated_rules: ["output_size"],
        unknown_rules: ["element_size", "tapscript_op_if"],
      },
      "Proven R1; unresolved R2 + R7",
    ],
  ])("formats %s", (_description, assessment, expected) => {
    expect(comparisonAssessmentText(assessment)).toBe(expected);
  });
});

describe("compactComparisonEvidence", () => {
  it.each<[description: string, values: unknown[], expected: string]>([
    ["an empty collection", [], ""],
    ["one serializable exemplar", [{ input: 0 }], '{"input":0}'],
    ["one unencodable exemplar", [undefined], "unavailable exemplar"],
    [
      "multiple exemplars",
      [{ input: 0 }, { input: 1 }, { input: 2 }],
      '{"input":0} · exemplar of 3',
    ],
  ])("formats %s", (_description, values, expected) => {
    expect(compactComparisonEvidence(values)).toBe(expected);
  });
});
