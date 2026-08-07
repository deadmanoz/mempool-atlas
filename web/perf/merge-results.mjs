import {
  existsSync,
  mkdirSync,
  readFileSync,
  readdirSync,
  writeFileSync,
} from "node:fs";
import { join, resolve } from "node:path";

import {
  interactionLabelsForScenario,
  validateInteractionEvidence,
} from "./interaction-evidence.mjs";

const WEB_ROOT = resolve(import.meta.dirname, "..");
const RESULT_ROOT = join(WEB_ROOT, ".perf-results");
const RAW_ROOT = join(RESULT_ROOT, "raw");
const PINNED_THROUGHPUT_BYTES_PER_SECOND = 159_461.45607954692;

const RELEASE_GATES = Object.freeze({
  metadata_usable_ms: 5_000,
  maximum_responsiveness_long_task_ms: 200,
  interaction_handler_ms: 200,
  interaction_settle_ms: 5_000,
  bip110_rule_navigation_handler_ms: 200,
  maximum_animation_frame_callback_ms: Object.freeze({
    desktop: 8,
    "mobile-slow-4g": 16,
  }),
  cls: 0.1,
  node: Object.freeze({
    primary_interaction_ms: 30_000,
    complete_feature_ready_ms: 52_000,
    primary_transfer_budget_ms: 20_000,
    complete_transfer_budget_ms: 38_000,
    primary_encoded_body_bytes: 3_189_229,
    complete_encoded_body_bytes: 6_059_535,
    page_heap_bytes: 40_000_000,
    cross_context_bytes: 60 * 1024 * 1024,
    replacement_retained_bytes: 90 * 1024 * 1024,
    bip110_page_heap_bytes: 90_000_000,
    bip110_cross_context_bytes: 120 * 1024 * 1024,
  }),
  comparison: Object.freeze({
    primary_interaction_ms: 50_000,
    complete_feature_ready_ms: 94_000,
    primary_transfer_budget_ms: 36_000,
    complete_transfer_budget_ms: 76_000,
    primary_encoded_body_bytes: 5_740_612,
    complete_encoded_body_bytes: 12_119_070,
    page_heap_bytes: 60_000_000,
    cross_context_bytes: 100 * 1024 * 1024,
    replacement_retained_bytes: 150 * 1024 * 1024,
  }),
});

const files = (existsSync(RAW_ROOT) ? readdirSync(RAW_ROOT) : [])
  .filter((file) => file.endsWith(".json"))
  .sort();
if (files.length === 0) {
  throw new Error("performance run produced no result files");
}

const results = files.map((file) => {
  const result = JSON.parse(readFileSync(join(RAW_ROOT, file), "utf8"));
  if (result.schema_version !== 2) {
    throw new Error(`${file} is not a schema version 2 performance result`);
  }
  return result;
});

const stableLoads = results.filter(
  (result) => result.result_kind === "stable-success",
);
const memoryBaselines = results.filter(
  (result) => result.result_kind === "memory-baseline",
);
const recoveryFaultResults = results.filter(
  (result) => result.result_kind === "recovery-fault",
);
if (
  stableLoads.length + memoryBaselines.length + recoveryFaultResults.length !==
  results.length
) {
  throw new Error("performance run contains an unknown result kind");
}
if (stableLoads.length !== 4) {
  throw new Error("performance run requires four stable results");
}
if (memoryBaselines.length !== 2) {
  throw new Error("performance run requires two empty-worker memory results");
}
if (recoveryFaultResults.length !== 2) {
  throw new Error(
    "performance run requires two desktop recovery-fault results",
  );
}

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

const BIP110_RULE_IDS = Object.freeze([
  "output_size",
  "element_size",
  "undefined_version",
  "taproot_annex",
  "control_block_size",
  "op_success",
  "tapscript_op_if",
]);

const validateWorkerTiming = (timing, label) => {
  if (typeof timing.source_id !== "string" || timing.source_id === "") {
    throw new Error(`${label}.source_id is missing`);
  }
  finite(timing.manifestFetchValidateMs, `${label}.manifestFetchValidateMs`);
  finite(timing.semanticValidationMs, `${label}.semanticValidationMs`);
  finite(timing.quorumMs, `${label}.quorumMs`, { positive: true });
  finite(timing.supersessionRestarts, `${label}.supersessionRestarts`);
  if (!Number.isInteger(timing.supersessionRestarts)) {
    throw new Error(`${label}.supersessionRestarts is not an integer`);
  }
  nonEmptyArray(timing.stages, `${label}.stages`).forEach((stage, index) => {
    if (
      !["population", "membership", "structure", "classifier"].includes(
        stage.kind,
      )
    ) {
      throw new Error(`${label}.stages[${index}].kind is invalid`);
    }
    if (typeof stage.reused !== "boolean") {
      throw new Error(`${label}.stages[${index}].reused is invalid`);
    }
    finite(
      stage.fetchDigestParseMs,
      `${label}.stages[${index}].fetchDigestParseMs`,
    );
    finite(
      stage.decodeValidatePackMs,
      `${label}.stages[${index}].decodeValidatePackMs`,
    );
  });
};

const validateQuorum = (quorum, label) => {
  if (typeof quorum !== "object" || quorum === null) {
    throw new Error(`${label} is missing`);
  }
  finite(quorum.encoded_body_bytes, `${label}.encoded_body_bytes`, {
    positive: true,
  });
  finite(quorum.decoded_body_bytes, `${label}.decoded_body_bytes`, {
    positive: true,
  });
  finite(quorum.cdp_encoded_data_length, `${label}.cdp_encoded_data_length`, {
    positive: true,
  });
  finite(quorum.request_count, `${label}.request_count`, { positive: true });
  finite(quorum.manifest_count, `${label}.manifest_count`, { positive: true });
  finite(quorum.stage_count, `${label}.stage_count`, { positive: true });
  nonEmptyArray(quorum.stage_ids, `${label}.stage_ids`);
  finite(quorum.request_span_ms, `${label}.request_span_ms`, {
    positive: true,
  });
  finite(quorum.transfer_window_ms, `${label}.transfer_window_ms`, {
    positive: true,
  });
  finite(quorum.summed_ttfb_ms, `${label}.summed_ttfb_ms`);
  finite(
    quorum.summed_server_processing_ms,
    `${label}.summed_server_processing_ms`,
  );
  if (typeof quorum.worker !== "object" || quorum.worker === null) {
    throw new Error(`${label}.worker is missing`);
  }
  [
    "source_count",
    "stage_count",
    "reused_stage_count",
    "fetched_stage_count",
    "manifest_fetch_validate_ms",
    "stage_fetch_digest_parse_ms",
    "stage_decode_validate_pack_ms",
    "semantic_validation_ms",
    "summed_quorum_ms",
    "maximum_source_quorum_ms",
    "supersession_restarts",
  ].forEach((field) =>
    finite(quorum.worker[field], `${label}.worker.${field}`),
  );
};

const expectedStableCases = [
  ["desktop", "node"],
  ["desktop", "comparison"],
  ["mobile-slow-4g", "node"],
  ["mobile-slow-4g", "comparison"],
];
for (const [profile, scenario] of expectedStableCases) {
  const matches = stableLoads.filter(
    (result) => result.profile === profile && result.scenario === scenario,
  );
  if (matches.length !== 1) {
    throw new Error(
      `performance run requires exactly one ${profile} ${scenario} stable result`,
    );
  }
}
for (const profile of ["desktop", "mobile-slow-4g"]) {
  const matches = memoryBaselines.filter(
    (result) => result.profile === profile,
  );
  if (matches.length !== 1) {
    throw new Error(
      `performance run requires exactly one ${profile} empty-worker memory result`,
    );
  }
}

for (const [scenario, fault] of [
  ["node", "refresh-manifest-503-once"],
  ["comparison", "refresh-membership-503-once"],
]) {
  const matches = recoveryFaultResults.filter(
    (result) =>
      result.profile === "desktop" &&
      result.scenario === scenario &&
      result.fault === fault,
  );
  if (matches.length !== 1) {
    throw new Error(`missing desktop ${scenario} ${fault} recovery result`);
  }
  const [result] = matches;
  if (
    result.injected !== true ||
    result.retained_active !== true ||
    (scenario === "comparison" && result.sibling_cancelled !== true) ||
    result.recovered !== true
  ) {
    throw new Error(`desktop ${scenario} ${fault} did not recover`);
  }
  finite(result.recovered_at_ms, `${scenario}.${fault}.recovered_at_ms`, {
    positive: true,
  });
}

const stableLoadSummaries = stableLoads.map((result) => {
  if (
    !RELEASE_GATES[result.scenario] ||
    !["desktop", "mobile-slow-4g"].includes(result.profile)
  ) {
    throw new Error(`unknown performance scenario ${result.scenario}`);
  }
  const label = `${result.profile}.${result.scenario}`;
  if (result.snapshot_transaction_count !== 70_000) {
    throw new Error(`${label} did not use the 70,000-row fixture`);
  }
  finite(result.metadata_usable_ms, `${label}.metadata_usable_ms`);
  finite(result.primary_interaction_ms, `${label}.primary_interaction_ms`, {
    positive: true,
  });
  finite(
    result.complete_feature_ready_ms,
    `${label}.complete_feature_ready_ms`,
    { positive: true },
  );
  const publicationDescriptors = nonEmptyArray(
    result.publication_descriptors,
    `${label}.publication_descriptors`,
  );
  const expectedSourceCount = result.scenario === "node" ? 1 : 2;
  if (
    publicationDescriptors.length !== expectedSourceCount ||
    publicationDescriptors.some(
      (descriptor) => descriptor.transaction_count !== 70_000,
    )
  ) {
    throw new Error(
      `${label} did not use ${expectedSourceCount} exact 70,000-row publication(s)`,
    );
  }
  nonEmptyArray(result.v2_requests, `${label}.v2_requests`);
  nonEmptyArray(result.stage_transfers, `${label}.stage_transfers`).forEach(
    (stage, index) => {
      const stageLabel = `${label}.stage_transfers[${index}]`;
      if (
        typeof stage.source_id !== "string" ||
        typeof stage.stage_id !== "string" ||
        stage.source_id === "" ||
        stage.stage_id === ""
      ) {
        throw new Error(`${stageLabel} has no descriptor identity`);
      }
      finite(stage.encoded_body_bytes, `${stageLabel}.encoded_body_bytes`, {
        positive: true,
      });
      finite(stage.decoded_body_bytes, `${stageLabel}.decoded_body_bytes`, {
        positive: true,
      });
      finite(
        stage.cdp_encoded_data_length,
        `${stageLabel}.cdp_encoded_data_length`,
        { positive: true },
      );
      finite(stage.ttfb_ms, `${stageLabel}.ttfb_ms`);
      finite(
        stage.transfer_excluding_ttfb_ms,
        `${stageLabel}.transfer_excluding_ttfb_ms`,
      );
      finite(stage.server_processing_ms, `${stageLabel}.server_processing_ms`);
    },
  );
  for (const quorumName of ["primary", "complete"]) {
    validateQuorum(
      result.quorum_totals?.[quorumName],
      `${label}.quorum_totals.${quorumName}`,
    );
    nonEmptyArray(
      result.worker_timings?.[quorumName],
      `${label}.worker_timings.${quorumName}`,
    ).forEach((timing, index) =>
      validateWorkerTiming(
        timing,
        `${label}.worker_timings.${quorumName}[${index}]`,
      ),
    );
  }
  finite(
    result.maximum_responsiveness_long_task_ms,
    `${label}.maximum_responsiveness_long_task_ms`,
  );
  const responsivenessIntervals = nonEmptyArray(
    result.responsiveness_intervals,
    `${label}.responsiveness_intervals`,
  );
  const expectedInteractionLabels = interactionLabelsForScenario(
    result.scenario,
  );
  const expectedIntervalLabels = [
    "post-metadata-pre-instrumentation",
    "replacement-prepare",
    "replacement-commit",
    ...expectedInteractionLabels,
    ...(result.scenario === "node" ? ["bip110-rule-navigation"] : []),
  ];
  if (
    responsivenessIntervals.length !== expectedIntervalLabels.length ||
    responsivenessIntervals.some(
      (interval, index) => interval.label !== expectedIntervalLabels[index],
    )
  ) {
    throw new Error(`${label}.responsiveness_intervals are incomplete`);
  }
  responsivenessIntervals.forEach((interval, index) => {
    finite(
      interval.start_time_ms,
      `${label}.responsiveness_intervals[${index}].start_time_ms`,
    );
    finite(
      interval.end_time_ms,
      `${label}.responsiveness_intervals[${index}].end_time_ms`,
    );
    if (interval.end_time_ms < interval.start_time_ms) {
      throw new Error(
        `${label}.responsiveness_intervals[${index}] ends before it starts`,
      );
    }
    if (
      index > 0 &&
      interval.start_time_ms < responsivenessIntervals[index - 1].end_time_ms
    ) {
      throw new Error(`${label}.responsiveness_intervals overlap`);
    }
  });
  validateInteractionEvidence(result, responsivenessIntervals, label);
  if (result.scenario === "node") {
    const measurement = result.bip110_rule_navigation;
    if (typeof measurement !== "object" || measurement === null) {
      throw new Error(`${label}.bip110_rule_navigation is missing`);
    }
    const ruleIds = nonEmptyArray(
      measurement.ruleIds,
      `${label}.bip110_rule_navigation.ruleIds`,
    );
    if (
      ruleIds.length !== BIP110_RULE_IDS.length ||
      ruleIds.some(
        (ruleId, index) =>
          typeof ruleId !== "string" || ruleId !== BIP110_RULE_IDS[index],
      ) ||
      new Set(ruleIds).size !== BIP110_RULE_IDS.length
    ) {
      throw new Error(
        `${label}.bip110_rule_navigation.ruleIds are incomplete or duplicated`,
      );
    }
    if (measurement.selectedRule !== ruleIds.at(-1)) {
      throw new Error(
        `${label}.bip110_rule_navigation did not select the final rule`,
      );
    }
    const handlerDurationMs = finite(
      measurement.handlerDurationMs,
      `${label}.bip110_rule_navigation.handlerDurationMs`,
    );
    const handlerDurationsMs = nonEmptyArray(
      measurement.handlerDurationsMs,
      `${label}.bip110_rule_navigation.handlerDurationsMs`,
    );
    if (
      handlerDurationsMs.length !== BIP110_RULE_IDS.length ||
      handlerDurationsMs.some(
        (duration, index) =>
          finite(
            duration,
            `${label}.bip110_rule_navigation.handlerDurationsMs[${index}]`,
          ) > RELEASE_GATES.bip110_rule_navigation_handler_ms,
      ) ||
      Math.max(...handlerDurationsMs) !== handlerDurationMs
    ) {
      throw new Error(
        `${label}.bip110_rule_navigation handler durations are invalid`,
      );
    }
    if (handlerDurationMs > RELEASE_GATES.bip110_rule_navigation_handler_ms) {
      throw new Error(
        `${label}.bip110_rule_navigation.handlerDurationMs exceeds its release gate`,
      );
    }
    const measuredInterval = measurement.responsivenessInterval;
    const recordedInterval = responsivenessIntervals.find(
      (interval) => interval.label === "bip110-rule-navigation",
    );
    if (
      typeof measuredInterval !== "object" ||
      measuredInterval === null ||
      measuredInterval.label !== recordedInterval?.label ||
      measuredInterval.start_time_ms !== recordedInterval.start_time_ms ||
      measuredInterval.end_time_ms !== recordedInterval.end_time_ms
    ) {
      throw new Error(
        `${label}.bip110_rule_navigation interval does not match its responsiveness interval`,
      );
    }
    const memory = result.bip110_memory_sample;
    if (typeof memory !== "object" || memory === null) {
      throw new Error(`${label}.bip110_memory_sample is missing`);
    }
    if (
      finite(
        memory.page_heap?.used_size_bytes,
        `${label}.bip110_memory_sample.page_heap.used_size_bytes`,
        { positive: true },
      ) > RELEASE_GATES.node.bip110_page_heap_bytes ||
      finite(
        memory.worker_inclusive_memory?.bytes,
        `${label}.bip110_memory_sample.worker_inclusive_memory.bytes`,
        { positive: true },
      ) > RELEASE_GATES.node.bip110_cross_context_bytes ||
      memory.worker_inclusive_memory?.supported !== true ||
      memory.worker_inclusive_memory?.error !== null
    ) {
      throw new Error(`${label}.bip110_memory_sample exceeds its release gate`);
    }
  } else if (result.bip110_rule_navigation !== null) {
    throw new Error(
      `${label}.bip110_rule_navigation must be null for comparison`,
    );
  } else if (result.bip110_memory_sample !== null) {
    throw new Error(
      `${label}.bip110_memory_sample must be null for comparison`,
    );
  }
  const responsivenessLongTasks = result.responsiveness_long_tasks;
  const responsivenessFrames = result.responsiveness_animation_frame_callbacks;
  if (
    !Array.isArray(responsivenessLongTasks) ||
    !Array.isArray(responsivenessFrames)
  ) {
    throw new Error(`${label}.responsiveness samples are missing`);
  }
  const startsInsideResponsivenessInterval = (startTime) =>
    responsivenessIntervals.some(
      (interval) =>
        startTime >= interval.start_time_ms &&
        startTime <= interval.end_time_ms,
    );
  responsivenessLongTasks.forEach((task, index) => {
    finite(
      task.start_time_ms,
      `${label}.responsiveness_long_tasks[${index}].start_time_ms`,
    );
    finite(
      task.duration_ms,
      `${label}.responsiveness_long_tasks[${index}].duration_ms`,
    );
    if (!startsInsideResponsivenessInterval(task.start_time_ms)) {
      throw new Error(`${label}.responsiveness_long_tasks escaped its windows`);
    }
  });
  responsivenessFrames.forEach((callback, index) => {
    finite(
      callback.start_time_ms,
      `${label}.responsiveness_animation_frame_callbacks[${index}].start_time_ms`,
    );
    finite(
      callback.duration_ms,
      `${label}.responsiveness_animation_frame_callbacks[${index}].duration_ms`,
    );
    if (!startsInsideResponsivenessInterval(callback.start_time_ms)) {
      throw new Error(
        `${label}.responsiveness_animation_frame_callbacks escaped its windows`,
      );
    }
  });
  const observedMaximumLongTask = Math.max(
    0,
    ...responsivenessLongTasks.map((task) => task.duration_ms),
  );
  const observedMaximumFrameCallback = Math.max(
    0,
    ...responsivenessFrames.map((callback) => callback.duration_ms),
  );
  if (
    result.maximum_responsiveness_long_task_ms !== observedMaximumLongTask ||
    result.maximum_animation_frame_callback_ms !== observedMaximumFrameCallback
  ) {
    throw new Error(`${label}.responsiveness maxima do not match the samples`);
  }
  finite(
    result.maximum_animation_frame_callback_ms,
    `${label}.maximum_animation_frame_callback_ms`,
  );
  finite(result.cls, `${label}.cls`);
  if (
    result.readiness_contract?.complete_models_committed !== true ||
    result.readiness_contract?.deferred_density_raster_excluded !== true
  ) {
    throw new Error(`${label}.readiness_contract is missing or unsupported`);
  }
  finite(
    result.primary_memory_sample?.page_heap?.used_size_bytes,
    `${label}.primary_memory_sample.page_heap.used_size_bytes`,
    { positive: true },
  );
  finite(
    result.page_heap?.complete?.used_size_bytes,
    `${label}.page_heap.complete.used_size_bytes`,
    { positive: true },
  );
  finite(
    result.primary_memory_sample?.worker_inclusive_memory?.bytes,
    `${label}.primary_memory_sample.worker_inclusive_memory.bytes`,
    { positive: true },
  );
  finite(
    result.worker_inclusive_memory?.complete?.bytes,
    `${label}.worker_inclusive_memory.complete.bytes`,
    { positive: true },
  );
  finite(
    result.worker_inclusive_memory?.replacement_retained?.bytes,
    `${label}.worker_inclusive_memory.replacement_retained.bytes`,
    { positive: true },
  );
  const gate = RELEASE_GATES[result.scenario];
  if (
    result.primary_memory_sample?.isolated_context !== true ||
    result.primary_memory_sample?.cross_origin_isolated !== true
  ) {
    throw new Error(`${label}.primary_memory_sample lacks isolated provenance`);
  }
  const heldSecondManifestSourceIds = nonEmptyArray(
    result.primary_memory_sample?.completion_gate
      ?.held_second_manifest_source_ids,
    `${label}.primary_memory_sample.completion_gate.held_second_manifest_source_ids`,
  );
  const expectedHeldSourceIds = publicationDescriptors
    .map((descriptor) => descriptor.source_id)
    .sort();
  if (
    heldSecondManifestSourceIds.length !== expectedSourceCount ||
    JSON.stringify([...heldSecondManifestSourceIds].sort()) !==
      JSON.stringify(expectedHeldSourceIds)
  ) {
    throw new Error(
      `${label}.primary_memory_sample completion gate did not hold every source's second manifest`,
    );
  }
  const memoryMilestones = {
    primary: result.primary_memory_sample.worker_inclusive_memory,
    complete: result.worker_inclusive_memory.complete,
    replacement_retained: result.worker_inclusive_memory.replacement_retained,
  };
  for (const [milestone, memory] of Object.entries(memoryMilestones)) {
    if (memory.supported !== true || memory.error !== null) {
      throw new Error(
        `${label}.${milestone} worker-inclusive measurement failed`,
      );
    }
  }
  const checks = {
    metadata: result.metadata_usable_ms <= RELEASE_GATES.metadata_usable_ms,
    primary_timing:
      result.primary_interaction_ms <= gate.primary_interaction_ms,
    complete_timing:
      result.complete_feature_ready_ms <= gate.complete_feature_ready_ms,
    primary_bytes:
      result.quorum_totals.primary.encoded_body_bytes <=
      gate.primary_encoded_body_bytes,
    complete_bytes:
      result.quorum_totals.complete.encoded_body_bytes <=
      gate.complete_encoded_body_bytes,
    long_task:
      result.maximum_responsiveness_long_task_ms <=
      RELEASE_GATES.maximum_responsiveness_long_task_ms,
    interaction_handler:
      result.maximum_interaction_handler_ms <=
      RELEASE_GATES.interaction_handler_ms,
    interaction_settle:
      result.maximum_interaction_settle_ms <=
      RELEASE_GATES.interaction_settle_ms,
    frame_callback:
      result.maximum_animation_frame_callback_ms <=
      RELEASE_GATES.maximum_animation_frame_callback_ms[result.profile],
    cls: result.cls <= RELEASE_GATES.cls,
    primary_page_heap:
      result.primary_memory_sample.page_heap.used_size_bytes <=
      gate.page_heap_bytes,
    complete_page_heap:
      result.page_heap.complete.used_size_bytes <= gate.page_heap_bytes,
    primary_cross_context_memory:
      result.primary_memory_sample.worker_inclusive_memory.bytes <=
      gate.cross_context_bytes,
    complete_cross_context_memory:
      result.worker_inclusive_memory.complete.bytes <= gate.cross_context_bytes,
    replacement_retained_memory:
      result.worker_inclusive_memory.replacement_retained.bytes <=
      gate.replacement_retained_bytes,
    replacement_retained_gate_declared:
      result.worker_inclusive_memory.replacement_retained_gate_bytes ===
      gate.replacement_retained_bytes,
    stable_statuses: Object.keys(
      result.request_outcomes?.status_counts ?? {},
    ).every((status) => status === "200"),
  };
  const observedThroughput = {
    primary:
      result.quorum_totals.primary.encoded_body_bytes /
      (result.quorum_totals.primary.transfer_window_ms / 1_000),
    complete:
      result.quorum_totals.complete.encoded_body_bytes /
      (result.quorum_totals.complete.transfer_window_ms / 1_000),
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
    primary:
      result.quorum_totals.primary.encoded_body_bytes <= freshCapacity.primary,
    complete:
      result.quorum_totals.complete.encoded_body_bytes <=
      freshCapacity.complete,
  };
  return {
    profile: result.profile,
    scenario: result.scenario,
    web_build_id: result.web_build_id,
    milestones: {
      metadata_usable_ms: result.metadata_usable_ms,
      primary_interaction_ms: result.primary_interaction_ms,
      complete_feature_ready_ms: result.complete_feature_ready_ms,
    },
    publication_descriptors: result.publication_descriptors,
    stage_transfers: result.stage_transfers,
    worker_timings: result.worker_timings,
    quorum_totals: result.quorum_totals,
    freshly_observed_throughput_bytes_per_second: observedThroughput,
    responsiveness: {
      maximum_responsiveness_long_task_ms:
        result.maximum_responsiveness_long_task_ms,
      maximum_animation_frame_callback_ms:
        result.maximum_animation_frame_callback_ms,
      measured_interactions: result.measured_interactions,
      maximum_interaction_handler_ms: result.maximum_interaction_handler_ms,
      maximum_interaction_settle_ms: result.maximum_interaction_settle_ms,
      cls: result.cls,
    },
    readiness_contract: result.readiness_contract,
    bip110_rule_navigation: result.bip110_rule_navigation,
    bip110_memory_sample: result.bip110_memory_sample,
    memory: {
      primary_sample: result.primary_memory_sample,
      page_heap: result.page_heap,
      worker_inclusive: result.worker_inclusive_memory,
    },
    request_outcomes: result.request_outcomes,
    feasibility: {
      checks,
      all: Object.values(checks).every(Boolean),
    },
    freshly_derived_feasibility: {
      compressed_capacity_bytes: freshCapacity,
      checks: freshChecks,
      all: Object.values(freshChecks).every(Boolean),
    },
  };
});

memoryBaselines.forEach((result) => {
  const label = `${result.profile}.memory`;
  finite(
    result.page_heap?.used_size_bytes,
    `${label}.page_heap.used_size_bytes`,
    {
      positive: true,
    },
  );
  finite(
    result.worker_inclusive_memory?.bytes,
    `${label}.worker_inclusive.bytes`,
    {
      positive: true,
    },
  );
  if (
    result.worker_inclusive_memory.supported !== true ||
    result.worker_inclusive_memory.error !== null
  ) {
    throw new Error(`${label} worker-inclusive measurement failed`);
  }
});

const output = {
  schema_version: 2,
  generated_at: new Date().toISOString(),
  pinned_throughput_bytes_per_second: PINNED_THROUGHPUT_BYTES_PER_SECOND,
  release_gates: RELEASE_GATES,
  web_build_ids: [...new Set(results.map((result) => result.web_build_id))],
  stable_loads: stableLoadSummaries,
  recovery_fault_results: recoveryFaultResults,
  memory_baselines: memoryBaselines,
  results,
};

mkdirSync(RESULT_ROOT, { recursive: true });
const outputPath = join(RESULT_ROOT, "latest.json");
writeFileSync(outputPath, `${JSON.stringify(output, null, 2)}\n`);

const failed = stableLoadSummaries.filter((result) => !result.feasibility.all);
if (failed.length > 0) {
  throw new Error(
    `performance release gates failed for ${failed
      .map((result) => `${result.profile}/${result.scenario}`)
      .join(", ")}; results: ${outputPath}`,
  );
}

console.log(`performance results: ${outputPath} (all stable gates passed)`);
