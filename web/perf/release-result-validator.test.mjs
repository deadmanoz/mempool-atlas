import { describe, expect, it } from "vitest";

import { RELEASE_GATES } from "./release-gates.mjs";
import { validateStableReleaseResult } from "./release-result-validator.mjs";

const releaseResult = () => {
  const gate = RELEASE_GATES.node;
  return {
    profile: "desktop",
    scenario: "node",
    metadata_usable_ms: RELEASE_GATES.metadata_usable_ms,
    primary_interaction_ms: gate.primary_interaction_ms,
    complete_feature_ready_ms: gate.complete_feature_ready_ms,
    quorum_totals: {
      primary: {
        encoded_body_bytes: gate.primary_encoded_body_bytes,
        transfer_window_ms: gate.primary_transfer_budget_ms / 2,
      },
      complete: {
        encoded_body_bytes: gate.complete_encoded_body_bytes,
        transfer_window_ms: gate.complete_transfer_budget_ms / 2,
      },
    },
    maximum_responsiveness_long_task_ms:
      RELEASE_GATES.maximum_responsiveness_long_task_ms,
    maximum_interaction_handler_ms: RELEASE_GATES.interaction_handler_ms,
    maximum_interaction_settle_ms: RELEASE_GATES.interaction_settle_ms,
    maximum_animation_frame_callback_ms:
      RELEASE_GATES.maximum_animation_frame_callback_ms.desktop,
    cls: RELEASE_GATES.cls,
    readiness_contract: {
      complete_models_committed: true,
      deferred_density_raster_excluded: true,
    },
    primary_memory_sample: {
      page_heap: { used_size_bytes: gate.page_heap_bytes },
      worker_inclusive_memory: { bytes: gate.cross_context_bytes },
    },
    page_heap: {
      complete: { used_size_bytes: gate.page_heap_bytes },
    },
    worker_inclusive_memory: {
      complete: { bytes: gate.cross_context_bytes },
      replacement_retained: { bytes: gate.replacement_retained_bytes },
      replacement_retained_gate_bytes: gate.replacement_retained_bytes,
    },
    request_outcomes: { status_counts: { 200: 8 } },
  };
};

describe("stable performance release result validation", () => {
  it("accepts every release boundary and derives fresh transfer capacity", () => {
    const validated = validateStableReleaseResult(releaseResult());

    expect(validated.feasibility.all).toBe(true);
    expect(Object.values(validated.feasibility.checks)).not.toContain(false);
    expect(validated.freshlyDerivedFeasibility.all).toBe(true);
  });

  it.each([
    ["metadata", (result) => (result.metadata_usable_ms += 0.001)],
    ["primary_timing", (result) => (result.primary_interaction_ms += 0.001)],
    [
      "complete_timing",
      (result) => (result.complete_feature_ready_ms += 0.001),
    ],
    [
      "primary_bytes",
      (result) => (result.quorum_totals.primary.encoded_body_bytes += 1),
    ],
    [
      "complete_bytes",
      (result) => (result.quorum_totals.complete.encoded_body_bytes += 1),
    ],
    [
      "long_task",
      (result) => (result.maximum_responsiveness_long_task_ms += 0.001),
    ],
    [
      "interaction_handler",
      (result) => (result.maximum_interaction_handler_ms += 0.001),
    ],
    [
      "interaction_settle",
      (result) => (result.maximum_interaction_settle_ms += 0.001),
    ],
    [
      "frame_callback",
      (result) => (result.maximum_animation_frame_callback_ms += 0.001),
    ],
    ["cls", (result) => (result.cls += 0.001)],
    [
      "primary_page_heap",
      (result) => (result.primary_memory_sample.page_heap.used_size_bytes += 1),
    ],
    [
      "complete_page_heap",
      (result) => (result.page_heap.complete.used_size_bytes += 1),
    ],
    [
      "primary_cross_context_memory",
      (result) =>
        (result.primary_memory_sample.worker_inclusive_memory.bytes += 1),
    ],
    [
      "complete_cross_context_memory",
      (result) => (result.worker_inclusive_memory.complete.bytes += 1),
    ],
    [
      "replacement_retained_memory",
      (result) =>
        (result.worker_inclusive_memory.replacement_retained.bytes += 1),
    ],
    [
      "replacement_retained_gate_declared",
      (result) =>
        (result.worker_inclusive_memory.replacement_retained_gate_bytes += 1),
    ],
    [
      "stable_statuses",
      (result) => {
        result.request_outcomes.status_counts[503] = 1;
      },
    ],
  ])("fails the %s gate immediately beyond its boundary", (check, mutate) => {
    const result = releaseResult();
    mutate(result);

    expect(validateStableReleaseResult(result).feasibility.checks[check]).toBe(
      false,
    );
  });

  it("requires the complete readiness contract", () => {
    const result = releaseResult();
    result.readiness_contract.complete_models_committed = false;

    expect(() => validateStableReleaseResult(result)).toThrow(
      "readiness_contract is missing or unsupported",
    );
  });

  it("fails freshly derived capacity when observed throughput is too low", () => {
    const result = releaseResult();
    result.quorum_totals.primary.transfer_window_ms =
      RELEASE_GATES.node.primary_transfer_budget_ms + 1;

    expect(
      validateStableReleaseResult(result).freshlyDerivedFeasibility.checks
        .primary,
    ).toBe(false);
  });

  it.each([
    {
      name: "missing interaction evidence maximum",
      mutate: (result) => {
        delete result.maximum_interaction_settle_ms;
      },
      message: "maximum_interaction_settle_ms is not a finite",
    },
    {
      name: "non-finite responsiveness metric",
      mutate: (result) => {
        result.cls = Number.NaN;
      },
      message: "cls is not a finite",
    },
    {
      name: "non-finite transfer metric",
      mutate: (result) => {
        result.quorum_totals.complete.transfer_window_ms =
          Number.POSITIVE_INFINITY;
      },
      message: "complete.transfer_window_ms is not a finite",
    },
    {
      name: "missing request status contract",
      mutate: (result) => {
        delete result.request_outcomes;
      },
      message: "request_outcomes.status_counts is missing",
    },
  ])("rejects $name", ({ mutate, message }) => {
    const result = releaseResult();
    mutate(result);

    expect(() => validateStableReleaseResult(result)).toThrow(message);
  });
});
