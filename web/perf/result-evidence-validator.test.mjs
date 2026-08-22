import { describe, expect, it } from "vitest";

import {
  validateQuorum,
  validateResponsivenessIntervals,
  validateResponsivenessSamples,
  validateWorkerTiming,
} from "./result-evidence-validator.mjs";

const workerTiming = () => ({
  source_id: "node-a",
  manifestFetchValidateMs: 1,
  semanticValidationMs: 2,
  quorumMs: 3,
  supersessionRestarts: 0,
  stages: [
    {
      kind: "population",
      reused: false,
      fetchDigestParseMs: 4,
      decodeValidatePackMs: 5,
    },
  ],
});

const quorum = () => ({
  encoded_body_bytes: 1,
  decoded_body_bytes: 2,
  cdp_encoded_data_length: 3,
  request_count: 4,
  manifest_count: 1,
  stage_count: 3,
  stage_ids: ["population:sha256"],
  request_span_ms: 5,
  transfer_window_ms: 6,
  summed_ttfb_ms: 0,
  summed_server_processing_ms: 0,
  worker: {
    source_count: 1,
    stage_count: 3,
    reused_stage_count: 0,
    fetched_stage_count: 3,
    manifest_fetch_validate_ms: 1,
    stage_fetch_digest_parse_ms: 2,
    stage_decode_validate_pack_ms: 3,
    semantic_validation_ms: 4,
    summed_quorum_ms: 5,
    maximum_source_quorum_ms: 5,
    supersession_restarts: 0,
  },
});

const intervals = () => [
  { label: "prepare", start_time_ms: 10, end_time_ms: 20 },
  { label: "commit", start_time_ms: 20, end_time_ms: 30 },
];

const responsivenessResult = () => ({
  responsiveness_long_tasks: [
    { start_time_ms: 10, duration_ms: 4 },
    { start_time_ms: 30, duration_ms: 7 },
  ],
  responsiveness_animation_frame_callbacks: [
    { start_time_ms: 20, duration_ms: 3 },
  ],
  maximum_responsiveness_long_task_ms: 7,
  maximum_animation_frame_callback_ms: 3,
});

describe("performance result evidence validation", () => {
  it("accepts complete worker timing and quorum evidence", () => {
    expect(validateWorkerTiming(workerTiming(), "desktop.node.worker")).toBe(
      undefined,
    );
    expect(validateQuorum(quorum(), "desktop.node.quorum")).toBe(undefined);
  });

  it.each([
    {
      name: "missing source identity",
      mutate: (timing) => {
        timing.source_id = "";
      },
      message: "source_id is missing",
    },
    {
      name: "fractional supersession count",
      mutate: (timing) => {
        timing.supersessionRestarts = 0.5;
      },
      message: "supersessionRestarts is not an integer",
    },
    {
      name: "unknown stage kind",
      mutate: (timing) => {
        timing.stages[0].kind = "manifest";
      },
      message: "stages[0].kind is invalid",
    },
    {
      name: "non-boolean reuse marker",
      mutate: (timing) => {
        timing.stages[0].reused = 0;
      },
      message: "stages[0].reused is invalid",
    },
    {
      name: "non-finite stage duration",
      mutate: (timing) => {
        timing.stages[0].decodeValidatePackMs = Number.NaN;
      },
      message: "decodeValidatePackMs is not a finite",
    },
  ])("rejects worker timing with $name", ({ mutate, message }) => {
    const timing = workerTiming();
    mutate(timing);
    expect(() => validateWorkerTiming(timing, "desktop.node.worker")).toThrow(
      message,
    );
  });

  it.each([
    {
      name: "a missing quorum",
      mutate: () => null,
      message: "desktop.node.quorum is missing",
    },
    {
      name: "a non-positive encoded body",
      mutate: (value) => {
        value.encoded_body_bytes = 0;
        return value;
      },
      message: "encoded_body_bytes is not a finite positive number",
    },
    {
      name: "a missing worker summary",
      mutate: (value) => {
        delete value.worker;
        return value;
      },
      message: "worker is missing",
    },
    {
      name: "a non-finite worker metric",
      mutate: (value) => {
        value.worker.semantic_validation_ms = Number.POSITIVE_INFINITY;
        return value;
      },
      message: "worker.semantic_validation_ms is not a finite",
    },
  ])("rejects quorum evidence with $name", ({ mutate, message }) => {
    expect(() =>
      validateQuorum(mutate(quorum()), "desktop.node.quorum"),
    ).toThrow(message);
  });

  it("accepts ordered intervals that touch at their boundaries", () => {
    const value = intervals();
    expect(
      validateResponsivenessIntervals(
        value,
        ["prepare", "commit"],
        "desktop.node",
      ),
    ).toBe(value);
  });

  it.each([
    {
      name: "an incomplete label sequence",
      mutate: (value) => value.reverse(),
      message: "responsiveness_intervals are incomplete",
    },
    {
      name: "an interval ending before it starts",
      mutate: (value) => {
        value[0].end_time_ms = 9;
        return value;
      },
      message: "responsiveness_intervals[0] ends before it starts",
    },
    {
      name: "overlapping intervals",
      mutate: (value) => {
        value[1].start_time_ms = 19;
        return value;
      },
      message: "responsiveness_intervals overlap",
    },
  ])("rejects $name", ({ mutate, message }) => {
    expect(() =>
      validateResponsivenessIntervals(
        mutate(intervals()),
        ["prepare", "commit"],
        "desktop.node",
      ),
    ).toThrow(message);
  });

  it("accepts samples starting on either interval boundary", () => {
    expect(
      validateResponsivenessSamples(
        responsivenessResult(),
        intervals(),
        "desktop.node",
      ),
    ).toBe(undefined);
  });

  it.each([
    {
      name: "missing sample arrays",
      mutate: (result) => {
        delete result.responsiveness_long_tasks;
      },
      message: "responsiveness samples are missing",
    },
    {
      name: "a long task outside every interval",
      mutate: (result) => {
        result.responsiveness_long_tasks[0].start_time_ms = 9;
      },
      message: "responsiveness_long_tasks escaped its windows",
    },
    {
      name: "a frame callback outside every interval",
      mutate: (result) => {
        result.responsiveness_animation_frame_callbacks[0].start_time_ms = 31;
      },
      message: "responsiveness_animation_frame_callbacks escaped its windows",
    },
    {
      name: "a maximum detached from the samples",
      mutate: (result) => {
        result.maximum_responsiveness_long_task_ms = 8;
      },
      message: "responsiveness maxima do not match the samples",
    },
  ])("rejects $name", ({ mutate, message }) => {
    const result = responsivenessResult();
    mutate(result);
    expect(() =>
      validateResponsivenessSamples(result, intervals(), "desktop.node"),
    ).toThrow(message);
  });
});
