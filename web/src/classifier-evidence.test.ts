import { describe, expect, it } from "vitest";

import {
  classifierDetectionPresentations,
  missingClassifierFactsText,
} from "./classifier-evidence";
import type { ClassificationResult, ClassifierDescriptor } from "./types";

const descriptor: ClassifierDescriptor = {
  id: "data_carriage_shape",
  version: "5",
  title: "Data carriage shapes",
  methodology: "heuristic",
  semantics: "multi_label",
  required_facts: [],
  labels: [
    {
      key: "push_drop_witness",
      label: "Push/drop witness",
      description: "A balanced push/drop carrier.",
    },
  ],
};

const result = (evidence: unknown): ClassificationResult => ({
  classifier_id: "data_carriage_shape",
  state: "complete",
  primary_label: "push_drop_witness",
  labels: ["push_drop_witness"],
  missing_facts: [],
  evidence,
});

describe("classifier evidence presentation", () => {
  it("formats carrier evidence without exposing raw JSON", () => {
    expect(
      classifierDetectionPresentations(
        result({
          detections: [
            {
              label: "push_drop_witness",
              carrier: "tapscript",
              input: 2,
              element: 3,
              framing: "push_drop",
              pushed_elements: 6,
              pushed_bytes: 1_530,
            },
          ],
        }),
        descriptor,
      ),
    ).toEqual([
      {
        label: "Detection 1 · Push/drop witness",
        summary:
          "Tapscript · input 2 · witness element 3 · push/drop envelope · 6 pushed elements · 1,530 pushed bytes",
      },
    ]);
  });

  it("ignores malformed or irrelevant evidence instead of stringifying it", () => {
    expect(
      classifierDetectionPresentations(result({ fixture: true }), descriptor),
    ).toEqual([]);
    expect(
      classifierDetectionPresentations(
        {
          ...result({ detections: [{ fixture: true }] }),
          classifier_id: "transaction_shape",
        },
        descriptor,
      ),
    ).toEqual([]);
  });

  it("uses readable missing-fact copy", () => {
    expect(
      missingClassifierFactsText({
        ...result({ detections: [] }),
        state: "partial",
        missing_facts: ["input_script_pubkeys"],
      }),
    ).toBe("spent-output scripts for one or more inputs");
  });
});
