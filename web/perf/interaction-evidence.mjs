const finite = (value, label, { positive = false } = {}) => {
  if (
    typeof value !== "number" ||
    !Number.isFinite(value) ||
    (positive ? value <= 0 : value < 0)
  ) {
    throw new Error(
      `${label} is not a finite ${positive ? "positive" : "non-negative"} number`,
    );
  }
  return value;
};

const nonEmptyArray = (value, label) => {
  if (!Array.isArray(value) || value.length === 0) {
    throw new Error(`${label} must be a non-empty array`);
  }
  return value;
};

export const interactionLabelsForScenario = (scenario) => {
  if (scenario === "node") {
    return [
      "node-buckets-activation",
      "node-terrain-metric-toggle",
      "node-classifier-lens-switch",
      "node-marginal-label-selection",
      "node-fee-rate-by-age-activation",
      "node-filter-input",
    ];
  }
  if (scenario === "comparison") {
    return ["comparison-distribution-scope-switch"];
  }
  throw new Error(`unknown interaction scenario ${scenario}`);
};

export const validateInteractionEvidence = (
  result,
  responsivenessIntervals,
  label,
) => {
  const expectedLabels = interactionLabelsForScenario(result.scenario);
  const measurements = nonEmptyArray(
    result.measured_interactions,
    `${label}.measured_interactions`,
  );
  if (
    measurements.length !== expectedLabels.length ||
    measurements.some(
      (measurement, index) => measurement.label !== expectedLabels[index],
    )
  ) {
    throw new Error(`${label}.measured_interactions are incomplete`);
  }

  measurements.forEach((measurement, index) => {
    const measurementLabel = `${label}.measured_interactions[${index}]`;
    const handlerDurationMs = finite(
      measurement.handler_duration_ms,
      `${measurementLabel}.handler_duration_ms`,
    );
    const settleDurationMs = finite(
      measurement.settle_duration_ms,
      `${measurementLabel}.settle_duration_ms`,
    );
    const measuredInterval = measurement.responsiveness_interval;
    const recordedInterval = responsivenessIntervals.find(
      (interval) => interval.label === measurement.label,
    );
    if (
      typeof measuredInterval !== "object" ||
      measuredInterval === null ||
      recordedInterval === undefined ||
      measuredInterval.label !== recordedInterval.label ||
      measuredInterval.start_time_ms !== recordedInterval.start_time_ms ||
      measuredInterval.end_time_ms !== recordedInterval.end_time_ms ||
      Math.abs(
        settleDurationMs -
          (recordedInterval.end_time_ms - recordedInterval.start_time_ms),
      ) > 0.001 ||
      handlerDurationMs > settleDurationMs
    ) {
      throw new Error(
        `${measurementLabel} does not match its responsiveness interval`,
      );
    }
    if (
      typeof measurement.outcome !== "object" ||
      measurement.outcome === null
    ) {
      throw new Error(`${measurementLabel}.outcome is missing`);
    }
  });

  const [firstInteraction] = measurements;
  if (result.scenario === "node") {
    const [
      terrainInteraction,
      metricInteraction,
      classifierInteraction,
      labelInteraction,
      feeAgeInteraction,
      filterInteraction,
    ] = measurements;
    const sourceTransactionCount = finite(
      result.snapshot_transaction_count,
      `${label}.snapshot_transaction_count`,
      { positive: true },
    );
    const filteredCount = finite(
      filterInteraction.outcome.filtered_transaction_count,
      `${label}.node-filter-input.filtered_transaction_count`,
      { positive: true },
    );
    if (
      terrainInteraction.outcome.selected_lens !== "buckets" ||
      terrainInteraction.outcome.rendered_transaction_count !==
        sourceTransactionCount ||
      metricInteraction.outcome.selected_metric !== "vsize" ||
      metricInteraction.outcome.rendered_transaction_count !==
        sourceTransactionCount ||
      classifierInteraction.outcome.selected_classifier !==
        "transaction_shape" ||
      classifierInteraction.outcome.rendered_transaction_count !==
        sourceTransactionCount ||
      labelInteraction.outcome.selected_classifier !== "transaction_shape" ||
      labelInteraction.outcome.selected_label !== "other_shape" ||
      labelInteraction.outcome.label_selected !== true ||
      feeAgeInteraction.outcome.selected_lens !== "fee-rate-by-age" ||
      feeAgeInteraction.outcome.rendered_transaction_count !==
        sourceTransactionCount ||
      filterInteraction.outcome.minimum_fee_rate !== 2 ||
      filterInteraction.outcome.source_transaction_count !==
        sourceTransactionCount ||
      filteredCount >= sourceTransactionCount ||
      filterInteraction.outcome.rendered_transaction_count !== filteredCount
    ) {
      throw new Error(`${label}.measured_interactions outcomes are invalid`);
    }
  } else if (
    firstInteraction.outcome.selected_scope !== "common" ||
    firstInteraction.outcome.left_model_committed !== true ||
    firstInteraction.outcome.right_model_committed !== true
  ) {
    throw new Error(`${label}.measured_interactions outcomes are invalid`);
  }

  const maximumHandlerMs = Math.max(
    ...measurements.map((measurement) => measurement.handler_duration_ms),
  );
  const maximumSettleMs = Math.max(
    ...measurements.map((measurement) => measurement.settle_duration_ms),
  );
  if (
    finite(
      result.maximum_interaction_handler_ms,
      `${label}.maximum_interaction_handler_ms`,
    ) !== maximumHandlerMs ||
    finite(
      result.maximum_interaction_settle_ms,
      `${label}.maximum_interaction_settle_ms`,
    ) !== maximumSettleMs
  ) {
    throw new Error(`${label}.measured_interactions maxima do not match`);
  }

  return { measurements, maximumHandlerMs, maximumSettleMs };
};
