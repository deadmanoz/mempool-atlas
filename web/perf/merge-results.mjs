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
import {
  PINNED_THROUGHPUT_BYTES_PER_SECOND,
  RELEASE_GATES,
} from "./release-gates.mjs";
import { validateStableReleaseResult } from "./release-result-validator.mjs";
import {
  validateQuorum,
  validateResponsivenessIntervals,
  validateResponsivenessSamples,
  validateWorkerTiming,
} from "./result-evidence-validator.mjs";

const WEB_ROOT = resolve(import.meta.dirname, "..");
const RESULT_ROOT = join(WEB_ROOT, ".perf-results");
const RAW_ROOT = join(RESULT_ROOT, "raw");
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
  const responsivenessIntervals = validateResponsivenessIntervals(
    result.responsiveness_intervals,
    expectedIntervalLabels,
    label,
  );
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
  validateResponsivenessSamples(result, responsivenessIntervals, label);
  const releaseValidation = validateStableReleaseResult(result);
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
    freshly_observed_throughput_bytes_per_second:
      releaseValidation.freshlyObservedThroughput,
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
    feasibility: releaseValidation.feasibility,
    freshly_derived_feasibility: releaseValidation.freshlyDerivedFeasibility,
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
