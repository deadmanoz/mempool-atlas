import type {
  CaptureStatus,
  MempoolEntry,
  MempoolFacts,
  MempoolResponse,
  SourceHealth,
} from "./types";

const isRecord = (value: unknown): value is Record<string, unknown> =>
  typeof value === "object" && value !== null;

const isNonNegativeInteger = (value: unknown): value is number =>
  typeof value === "number" && Number.isSafeInteger(value) && value >= 0;

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

const parseMempoolFacts = (value: unknown, index: number): MempoolFacts => {
  if (!isRecord(value)) {
    throw new TypeError(`Invalid membership facts at index ${index}`);
  }

  if (value.status === "awaiting_rpc" && hasOnlyKeys(value, ["status"])) {
    return { status: "awaiting_rpc" };
  }

  if (
    value.status === "available" &&
    hasOnlyKeys(value, ["status", "vsize", "fee_sats", "entered_at_ms"]) &&
    isNonNegativeInteger(value.vsize) &&
    value.vsize > 0 &&
    isNonNegativeInteger(value.fee_sats) &&
    isNonNegativeInteger(value.entered_at_ms)
  ) {
    return {
      status: "available",
      vsize: value.vsize,
      fee_sats: value.fee_sats,
      entered_at_ms: value.entered_at_ms,
    };
  }

  throw new TypeError(`Invalid membership facts at index ${index}`);
};

const parseMembership = (value: unknown, index: number): MempoolEntry => {
  if (
    !isRecord(value) ||
    typeof value.txid !== "string" ||
    value.txid.length === 0 ||
    !isNonNegativeInteger(value.updated_at_ms) ||
    typeof value.evidence_event_id !== "string" ||
    value.evidence_event_id.length === 0
  ) {
    throw new TypeError(`Invalid membership at index ${index}`);
  }

  return {
    txid: value.txid,
    updated_at_ms: value.updated_at_ms,
    evidence_event_id: value.evidence_event_id,
    facts: parseMempoolFacts(value.facts, index),
  };
};

const parseCaptureStatus = (value: unknown): CaptureStatus => {
  if (!isRecord(value)) {
    throw new TypeError("Invalid capture status");
  }

  if (value.status === "no_reported_gaps") {
    return { status: "no_reported_gaps" };
  }

  if (
    value.status !== "contains_gaps" ||
    !isNonNegativeInteger(value.first_gap_at_ms) ||
    !isNonNegativeInteger(value.latest_gap_at_ms) ||
    value.latest_gap_at_ms < value.first_gap_at_ms ||
    !isNonNegativeInteger(value.marker_count) ||
    value.marker_count === 0 ||
    (value.strongest_certainty !== "possible_loss" &&
      value.strongest_certainty !== "known_loss") ||
    typeof value.latest_input !== "string" ||
    value.latest_input.trim().length === 0 ||
    typeof value.latest_reason !== "string" ||
    value.latest_reason.trim().length === 0
  ) {
    throw new TypeError("Invalid capture status");
  }

  return {
    status: "contains_gaps",
    first_gap_at_ms: value.first_gap_at_ms,
    latest_gap_at_ms: value.latest_gap_at_ms,
    marker_count: value.marker_count,
    strongest_certainty: value.strongest_certainty,
    latest_input: value.latest_input,
    latest_reason: value.latest_reason,
  };
};

const parseSourceHealth = (value: unknown): SourceHealth => {
  if (!isRecord(value) || !isNonNegativeInteger(value.last_seen_at_ms)) {
    throw new TypeError("Invalid source health");
  }

  return {
    last_seen_at_ms: value.last_seen_at_ms,
    capture: parseCaptureStatus(value.capture),
  };
};

export const parseMempoolResponse = (value: unknown): MempoolResponse => {
  if (
    !isRecord(value) ||
    typeof value.source_id !== "string" ||
    value.source_id.trim().length === 0 ||
    !Array.isArray(value.memberships)
  ) {
    throw new TypeError("Invalid mempool response");
  }

  return {
    source_id: value.source_id,
    health: parseSourceHealth(value.health),
    memberships: value.memberships.map(parseMembership),
  };
};

export const fetchMempool = async (
  sourceId: string,
): Promise<MempoolResponse> => {
  const response = await fetch(
    `/api/v1/sources/${encodeURIComponent(sourceId)}/mempool`,
    {
      headers: { Accept: "application/json" },
    },
  );

  if (!response.ok) {
    throw new Error(`Mempool request failed with HTTP ${response.status}`);
  }

  const snapshot = parseMempoolResponse(await response.json());
  if (snapshot.source_id !== sourceId) {
    throw new Error(
      `Mempool response source ${snapshot.source_id} does not match ${sourceId}`,
    );
  }
  return snapshot;
};
