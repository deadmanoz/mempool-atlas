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

export const validateWorkerTiming = (timing, label) => {
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

export const validateQuorum = (quorum, label) => {
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

export const validateResponsivenessIntervals = (
  responsivenessIntervals,
  expectedIntervalLabels,
  label,
) => {
  const intervals = nonEmptyArray(
    responsivenessIntervals,
    `${label}.responsiveness_intervals`,
  );
  if (
    intervals.length !== expectedIntervalLabels.length ||
    intervals.some(
      (interval, index) => interval.label !== expectedIntervalLabels[index],
    )
  ) {
    throw new Error(`${label}.responsiveness_intervals are incomplete`);
  }
  intervals.forEach((interval, index) => {
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
      interval.start_time_ms < intervals[index - 1].end_time_ms
    ) {
      throw new Error(`${label}.responsiveness_intervals overlap`);
    }
  });
  return intervals;
};

export const validateResponsivenessSamples = (
  result,
  responsivenessIntervals,
  label,
) => {
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
};
