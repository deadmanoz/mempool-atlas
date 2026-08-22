import { describe, expect, it } from "vitest";

import {
  assertPinnedNodeRuntime,
  composeProjectionGates,
  maximumCandidate,
  projectWallClockMs,
} from "./project-stages-logic.mjs";

const gates = Object.freeze({
  node_primary_bytes: 100,
  node_complete_target_bytes: 200,
  node_complete_maximum_bytes: 250,
  comparison_primary_bytes: 300,
  comparison_complete_target_bytes: 400,
  comparison_complete_maximum_bytes: 500,
});

const measurements = (overrides = {}) => ({
  nodePrimaryBytes: 100,
  nodeCompleteBytes: 200,
  comparisonPrimaryBytes: 300,
  comparisonCompleteBytes: 400,
  ...overrides,
});

describe("staged projection release gates", () => {
  it("requires the pinned runtime with a diagnostic failure", () => {
    expect(() => assertPinnedNodeRuntime("22.23.2")).not.toThrow();
    expect(() => assertPinnedNodeRuntime("24.15.0")).toThrow(
      "staged projection requires Node.js 22.23.2 for reproducible gzip evidence; current runtime is 24.15.0",
    );
  });

  it("selects the maximum candidate and retains the first candidate on a tie", () => {
    const first = { id: "first", bytes: 20 };
    const candidates = [
      { id: "small", bytes: 10 },
      first,
      { id: "tied", bytes: 20 },
    ];

    expect(maximumCandidate(candidates, "bytes")).toBe(first);
    expect(() => maximumCandidate([], "bytes")).toThrow();
  });

  it("projects transfer, request, and processing time with ceiling rounding", () => {
    expect(projectWallClockMs(1_501, 1_000, 2, 0.5)).toBe(4_001);
    expect(projectWallClockMs(2_413_387, 159_461.45607954692, 2, 8)).toBe(
      25_135,
    );
  });

  it("passes measurements at every inclusive ceiling", () => {
    expect(composeProjectionGates(measurements(), gates)).toEqual({
      checks: {
        node_primary: true,
        node_complete_target: true,
        node_complete_maximum: true,
        comparison_primary: true,
        comparison_complete_target: true,
        comparison_complete_maximum: true,
      },
      release_gate_passed: true,
      requires_pre_authorized_rederivation: false,
    });
  });

  it("requires rederivation above either target without failing below the hard maximum", () => {
    for (const [overrides, targetCheck, maximumCheck] of [
      [
        { nodeCompleteBytes: 201 },
        "node_complete_target",
        "node_complete_maximum",
      ],
      [
        { comparisonCompleteBytes: 401 },
        "comparison_complete_target",
        "comparison_complete_maximum",
      ],
    ]) {
      const result = composeProjectionGates(measurements(overrides), gates);
      expect(result.checks[targetCheck]).toBe(false);
      expect(result.checks[maximumCheck]).toBe(true);
      expect(result.release_gate_passed).toBe(true);
      expect(result.requires_pre_authorized_rederivation).toBe(true);
    }
  });

  it("fails either primary ceiling or either complete hard maximum", () => {
    for (const [overrides, failedCheck] of [
      [{ nodePrimaryBytes: 101 }, "node_primary"],
      [{ nodeCompleteBytes: 251 }, "node_complete_maximum"],
      [{ comparisonPrimaryBytes: 301 }, "comparison_primary"],
      [{ comparisonCompleteBytes: 501 }, "comparison_complete_maximum"],
    ]) {
      const result = composeProjectionGates(measurements(overrides), gates);
      expect(result.checks[failedCheck]).toBe(false);
      expect(result.release_gate_passed).toBe(false);
    }
  });
});
