import { RELEASE_GATES } from "./release-gates.mjs";

const record = (value, label) => {
  if (typeof value !== "object" || value === null || Array.isArray(value)) {
    throw new Error(`${label} is missing`);
  }
  return value;
};

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

const transfer = (result, milestone, label) => {
  const quorum = record(
    result.quorum_totals?.[milestone],
    `${label}.quorum_totals.${milestone}`,
  );
  return {
    bytes: finite(
      quorum.encoded_body_bytes,
      `${label}.quorum_totals.${milestone}.encoded_body_bytes`,
      { positive: true },
    ),
    windowMs: finite(
      quorum.transfer_window_ms,
      `${label}.quorum_totals.${milestone}.transfer_window_ms`,
      { positive: true },
    ),
  };
};

const memoryBytes = (value, label) => finite(value, label, { positive: true });

const validateStableStatuses = (result, label) => {
  const statusCounts = record(
    result.request_outcomes?.status_counts,
    `${label}.request_outcomes.status_counts`,
  );
  const entries = Object.entries(statusCounts);
  if (entries.length === 0) {
    throw new Error(`${label}.request_outcomes.status_counts is empty`);
  }
  for (const [status, count] of entries) {
    finite(count, `${label}.request_outcomes.status_counts.${status}`, {
      positive: true,
    });
    if (!Number.isInteger(count)) {
      throw new Error(
        `${label}.request_outcomes.status_counts.${status} is not an integer`,
      );
    }
  }
  return entries.every(([status]) => status === "200");
};

export const validateStableReleaseResult = (result) => {
  record(result, "stable result");
  const gate = RELEASE_GATES[result.scenario];
  if (gate === undefined) {
    throw new Error(`unknown performance scenario ${result.scenario}`);
  }
  const frameGate =
    RELEASE_GATES.maximum_animation_frame_callback_ms[result.profile];
  if (frameGate === undefined) {
    throw new Error(`unknown performance profile ${result.profile}`);
  }
  const label = `${result.profile}.${result.scenario}`;
  const timings = {
    metadata: finite(result.metadata_usable_ms, `${label}.metadata_usable_ms`),
    primary: finite(
      result.primary_interaction_ms,
      `${label}.primary_interaction_ms`,
      { positive: true },
    ),
    complete: finite(
      result.complete_feature_ready_ms,
      `${label}.complete_feature_ready_ms`,
      { positive: true },
    ),
  };
  const transfers = {
    primary: transfer(result, "primary", label),
    complete: transfer(result, "complete", label),
  };
  const responsiveness = {
    longTask: finite(
      result.maximum_responsiveness_long_task_ms,
      `${label}.maximum_responsiveness_long_task_ms`,
    ),
    interactionHandler: finite(
      result.maximum_interaction_handler_ms,
      `${label}.maximum_interaction_handler_ms`,
    ),
    interactionSettle: finite(
      result.maximum_interaction_settle_ms,
      `${label}.maximum_interaction_settle_ms`,
    ),
    frameCallback: finite(
      result.maximum_animation_frame_callback_ms,
      `${label}.maximum_animation_frame_callback_ms`,
    ),
    cls: finite(result.cls, `${label}.cls`),
  };
  if (
    result.readiness_contract?.complete_models_committed !== true ||
    result.readiness_contract?.deferred_density_raster_excluded !== true
  ) {
    throw new Error(`${label}.readiness_contract is missing or unsupported`);
  }
  const memory = {
    primaryPageHeap: memoryBytes(
      result.primary_memory_sample?.page_heap?.used_size_bytes,
      `${label}.primary_memory_sample.page_heap.used_size_bytes`,
    ),
    completePageHeap: memoryBytes(
      result.page_heap?.complete?.used_size_bytes,
      `${label}.page_heap.complete.used_size_bytes`,
    ),
    primaryCrossContext: memoryBytes(
      result.primary_memory_sample?.worker_inclusive_memory?.bytes,
      `${label}.primary_memory_sample.worker_inclusive_memory.bytes`,
    ),
    completeCrossContext: memoryBytes(
      result.worker_inclusive_memory?.complete?.bytes,
      `${label}.worker_inclusive_memory.complete.bytes`,
    ),
    replacementRetained: memoryBytes(
      result.worker_inclusive_memory?.replacement_retained?.bytes,
      `${label}.worker_inclusive_memory.replacement_retained.bytes`,
    ),
    replacementGate: memoryBytes(
      result.worker_inclusive_memory?.replacement_retained_gate_bytes,
      `${label}.worker_inclusive_memory.replacement_retained_gate_bytes`,
    ),
  };
  const checks = {
    metadata: timings.metadata <= RELEASE_GATES.metadata_usable_ms,
    primary_timing: timings.primary <= gate.primary_interaction_ms,
    complete_timing: timings.complete <= gate.complete_feature_ready_ms,
    primary_bytes: transfers.primary.bytes <= gate.primary_encoded_body_bytes,
    complete_bytes:
      transfers.complete.bytes <= gate.complete_encoded_body_bytes,
    long_task:
      responsiveness.longTask <=
      RELEASE_GATES.maximum_responsiveness_long_task_ms,
    interaction_handler:
      responsiveness.interactionHandler <= RELEASE_GATES.interaction_handler_ms,
    interaction_settle:
      responsiveness.interactionSettle <= RELEASE_GATES.interaction_settle_ms,
    frame_callback: responsiveness.frameCallback <= frameGate,
    cls: responsiveness.cls <= RELEASE_GATES.cls,
    primary_page_heap: memory.primaryPageHeap <= gate.page_heap_bytes,
    complete_page_heap: memory.completePageHeap <= gate.page_heap_bytes,
    primary_cross_context_memory:
      memory.primaryCrossContext <= gate.cross_context_bytes,
    complete_cross_context_memory:
      memory.completeCrossContext <= gate.cross_context_bytes,
    replacement_retained_memory:
      memory.replacementRetained <= gate.replacement_retained_bytes,
    replacement_retained_gate_declared:
      memory.replacementGate === gate.replacement_retained_bytes,
    stable_statuses: validateStableStatuses(result, label),
  };
  const observedThroughput = {
    primary: transfers.primary.bytes / (transfers.primary.windowMs / 1_000),
    complete: transfers.complete.bytes / (transfers.complete.windowMs / 1_000),
  };
  const freshCapacity = {
    primary: Math.floor(
      observedThroughput.primary * (gate.primary_transfer_budget_ms / 1_000),
    ),
    complete: Math.floor(
      observedThroughput.complete * (gate.complete_transfer_budget_ms / 1_000),
    ),
  };
  const freshChecks = {
    primary: transfers.primary.bytes <= freshCapacity.primary,
    complete: transfers.complete.bytes <= freshCapacity.complete,
  };
  return {
    feasibility: {
      checks,
      all: Object.values(checks).every(Boolean),
    },
    freshlyObservedThroughput: observedThroughput,
    freshlyDerivedFeasibility: {
      compressed_capacity_bytes: freshCapacity,
      checks: freshChecks,
      all: Object.values(freshChecks).every(Boolean),
    },
  };
};
