import { describe, expect, it } from "vitest";

import { validateInteractionEvidence } from "./interaction-evidence.mjs";

const interval = (label, start, end) => ({
  label,
  start_time_ms: start,
  end_time_ms: end,
});

const measurement = (label, start, end, handler, outcome) => ({
  label,
  handler_duration_ms: handler,
  settle_duration_ms: end - start,
  responsiveness_interval: interval(label, start, end),
  outcome,
});

const nodeEvidence = () => {
  const measuredInteractions = [
    measurement("node-fee-rate-by-age-activation", 10, 30, 5, {
      selected_lens: "fee-rate-by-age",
      rendered_transaction_count: 70_000,
    }),
    measurement("node-filter-input", 40, 80, 6, {
      minimum_fee_rate: 2,
      filtered_transaction_count: 60_000,
      source_transaction_count: 70_000,
      rendered_transaction_count: 60_000,
    }),
  ];
  return {
    result: {
      scenario: "node",
      measured_interactions: measuredInteractions,
      maximum_interaction_handler_ms: 6,
      maximum_interaction_settle_ms: 40,
    },
    intervals: measuredInteractions.map(
      ({ responsiveness_interval: recorded }) => recorded,
    ),
  };
};

describe("performance interaction evidence", () => {
  it("accepts the complete production-scale node interaction contract", () => {
    const { result, intervals } = nodeEvidence();
    expect(
      validateInteractionEvidence(result, intervals, "desktop.node"),
    ).toEqual({
      measurements: result.measured_interactions,
      maximumHandlerMs: 6,
      maximumSettleMs: 40,
    });
  });

  it("rejects a stable result with missing interaction evidence", () => {
    const { result, intervals } = nodeEvidence();
    expect(() =>
      validateInteractionEvidence(
        { ...result, measured_interactions: undefined },
        intervals,
        "desktop.node",
      ),
    ).toThrow("desktop.node.measured_interactions must be a non-empty array");
  });

  it("rejects evidence that is detached from its responsiveness interval", () => {
    const { result, intervals } = nodeEvidence();
    expect(() =>
      validateInteractionEvidence(
        result,
        intervals.map((recorded, index) =>
          index === 0 ? { ...recorded, end_time_ms: 31 } : recorded,
        ),
        "desktop.node",
      ),
    ).toThrow(
      "desktop.node.measured_interactions[0] does not match its responsiveness interval",
    );
  });

  it("requires the committed comparison scope outcome", () => {
    const measuredInteractions = [
      measurement("comparison-distribution-scope-switch", 10, 50, 4, {
        selected_scope: "common",
        left_model_committed: true,
        right_model_committed: false,
      }),
    ];
    expect(() =>
      validateInteractionEvidence(
        {
          scenario: "comparison",
          measured_interactions: measuredInteractions,
          maximum_interaction_handler_ms: 4,
          maximum_interaction_settle_ms: 40,
        },
        measuredInteractions.map(
          ({ responsiveness_interval: recorded }) => recorded,
        ),
        "mobile-slow-4g.comparison",
      ),
    ).toThrow(
      "mobile-slow-4g.comparison.measured_interactions outcomes are invalid",
    );
  });
});
