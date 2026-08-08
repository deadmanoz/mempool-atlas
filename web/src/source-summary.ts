import type {
  ChainTip,
  ClassificationProgress,
  SourceAvailability,
  SourceSummary,
} from "./types";

const isRecord = (value: unknown): value is Record<string, unknown> =>
  typeof value === "object" && value !== null;

const hasOnlyKeys = (
  value: Record<string, unknown>,
  expectedKeys: readonly string[],
): boolean => {
  const keys = Object.keys(value);
  return (
    keys.length === expectedKeys.length &&
    keys.every((key) => expectedKeys.includes(key))
  );
};

const isNonNegativeInteger = (value: unknown): value is number =>
  typeof value === "number" && Number.isSafeInteger(value) && value >= 0;

export const isSourceId = (value: unknown): value is string =>
  typeof value === "string" &&
  value !== "." &&
  value !== ".." &&
  /^[A-Za-z0-9._-]{1,64}$/.test(value);

const parseChainTip = (value: unknown): ChainTip => {
  if (
    !isRecord(value) ||
    !hasOnlyKeys(value, ["height", "hash"]) ||
    !isNonNegativeInteger(value.height) ||
    typeof value.hash !== "string" ||
    !/^[0-9a-f]{64}$/.test(value.hash)
  ) {
    throw new TypeError("Invalid chain tip");
  }
  return value as unknown as ChainTip;
};

const parseNullableInteger = (value: unknown, field: string): number | null => {
  if (value === null || isNonNegativeInteger(value)) {
    return value;
  }
  throw new TypeError(`Invalid ${field}`);
};

const parseAvailability = (value: unknown): SourceAvailability => {
  if (
    value === "waiting" ||
    value === "ready" ||
    value === "stale" ||
    value === "error"
  ) {
    return value;
  }
  throw new TypeError("Invalid source availability");
};

const parseClassificationProgress = (
  value: unknown,
): ClassificationProgress | null => {
  if (value === null) {
    return null;
  }
  if (
    !isRecord(value) ||
    !hasOnlyKeys(value, [
      "state",
      "revision",
      "classified_count",
      "unclassified_count",
    ]) ||
    (value.state !== "classifying" &&
      value.state !== "complete" &&
      value.state !== "paused") ||
    !isNonNegativeInteger(value.revision) ||
    !isNonNegativeInteger(value.classified_count) ||
    !isNonNegativeInteger(value.unclassified_count)
  ) {
    throw new TypeError("Invalid classification progress");
  }
  return value as unknown as ClassificationProgress;
};

export const parseSourceSummary = (value: unknown): SourceSummary => {
  if (
    !isRecord(value) ||
    !hasOnlyKeys(value, [
      "source_id",
      "source_label",
      "availability",
      "poll_interval_seconds",
      "last_poll_started_at_ms",
      "snapshot_observed_at_ms",
      "chain_tip",
      "transaction_count",
      "total_vsize",
      "classification",
      "last_error",
    ]) ||
    !isSourceId(value.source_id) ||
    typeof value.source_label !== "string" ||
    value.source_label.trim().length === 0 ||
    !isNonNegativeInteger(value.poll_interval_seconds) ||
    value.poll_interval_seconds === 0 ||
    (value.last_error !== null && typeof value.last_error !== "string")
  ) {
    throw new TypeError("Invalid source summary");
  }

  const availability = parseAvailability(value.availability);
  parseNullableInteger(value.last_poll_started_at_ms, "last poll time");
  const snapshotObservedAtMs = parseNullableInteger(
    value.snapshot_observed_at_ms,
    "snapshot observation time",
  );
  const transactionCount = parseNullableInteger(
    value.transaction_count,
    "transaction count",
  );
  const totalVsize = parseNullableInteger(value.total_vsize, "total vsize");
  const chainTip =
    value.chain_tip === null ? null : parseChainTip(value.chain_tip);
  const classification = parseClassificationProgress(value.classification);

  const hasSnapshot =
    snapshotObservedAtMs !== null &&
    transactionCount !== null &&
    totalVsize !== null &&
    chainTip !== null;
  const hasNoSnapshot =
    snapshotObservedAtMs === null &&
    transactionCount === null &&
    totalVsize === null &&
    chainTip === null;
  if (!hasSnapshot && !hasNoSnapshot) {
    throw new TypeError("Source summary contains a partial snapshot");
  }
  if (hasSnapshot) {
    if (
      classification === null ||
      classification.classified_count + classification.unclassified_count !==
        transactionCount
    ) {
      throw new TypeError(
        "Source summary classification does not match its snapshot metadata",
      );
    }
  } else if (classification !== null) {
    throw new TypeError(
      "Source without a snapshot unexpectedly contains classification progress",
    );
  }
  if (
    (availability === "waiting" || availability === "error") &&
    !hasNoSnapshot
  ) {
    throw new TypeError("Unavailable source unexpectedly contains a snapshot");
  }
  if ((availability === "ready" || availability === "stale") && !hasSnapshot) {
    throw new TypeError("Available source is missing its snapshot");
  }
  if (
    (availability === "waiting" || availability === "ready") &&
    value.last_error !== null
  ) {
    throw new TypeError("Healthy source unexpectedly contains an error");
  }
  if (
    (availability === "stale" || availability === "error") &&
    (typeof value.last_error !== "string" ||
      value.last_error.trim().length === 0)
  ) {
    throw new TypeError("Failed source is missing its error");
  }

  return value as unknown as SourceSummary;
};
