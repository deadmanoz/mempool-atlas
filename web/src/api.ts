import type { Membership, MempoolResponse } from "./types";

const isRecord = (value: unknown): value is Record<string, unknown> =>
  typeof value === "object" && value !== null;

const parseMembership = (value: unknown, index: number): Membership => {
  if (
    !isRecord(value) ||
    typeof value.source_id !== "string" ||
    typeof value.txid !== "string" ||
    typeof value.present !== "boolean" ||
    typeof value.updated_at_ms !== "number" ||
    !Number.isFinite(value.updated_at_ms) ||
    typeof value.evidence_event_id !== "string"
  ) {
    throw new TypeError(`Invalid membership at index ${index}`);
  }

  return {
    source_id: value.source_id,
    txid: value.txid,
    present: value.present,
    updated_at_ms: value.updated_at_ms,
    evidence_event_id: value.evidence_event_id,
  };
};

export const parseMempoolResponse = (value: unknown): MempoolResponse => {
  if (
    !isRecord(value) ||
    (value.source_id !== null && typeof value.source_id !== "string") ||
    !Array.isArray(value.memberships)
  ) {
    throw new TypeError("Invalid mempool response");
  }

  return {
    source_id: value.source_id,
    memberships: value.memberships.map(parseMembership),
  };
};

export const fetchMempool = async (): Promise<MempoolResponse> => {
  const response = await fetch("/api/v1/mempool", {
    headers: { Accept: "application/json" },
  });

  if (!response.ok) {
    throw new Error(`Mempool request failed with HTTP ${response.status}`);
  }

  return parseMempoolResponse(await response.json());
};
