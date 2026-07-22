// Strict parser and fetch for the source-scoped rejection read model. This is
// "what one source refused": point-in-time policy evidence, deliberately kept
// separate from any comparison set difference. The window aggregate is bounded
// server-side; `recent` is a bounded, separately paginated slice.

import {
  isNonEmptyString,
  isNonNegativeInteger,
  isRecord,
} from "./summary-parsing";
import type {
  RejectionAttribution,
  RejectionAvailability,
  RejectionRecord,
  RejectionReasonCount,
  RejectionTaxonomyBreakdown,
  RejectionVerdictCount,
  RejectionWindow,
  SourceRejections,
} from "./types";

const isString = (value: unknown): value is string => typeof value === "string";
const isTxid = (value: unknown): value is string =>
  typeof value === "string" && /^[0-9a-f]{64}$/u.test(value);

const parseAvailability = (value: unknown): RejectionAvailability => {
  if (
    !isRecord(value) ||
    Object.keys(value).length !== 1 ||
    (value.status !== "available" && value.status !== "not_collected")
  ) {
    throw new TypeError("Invalid rejection availability");
  }
  return { status: value.status };
};

const parseWindow = (value: unknown): RejectionWindow => {
  if (!isRecord(value) || !isNonNegativeInteger(value.count)) {
    throw new TypeError("Invalid rejection window");
  }
  const window: RejectionWindow = { count: value.count };
  if (value.oldest_at_ms !== undefined) {
    if (!isNonNegativeInteger(value.oldest_at_ms)) {
      throw new TypeError("Invalid rejection window oldest_at_ms");
    }
    window.oldest_at_ms = value.oldest_at_ms;
  }
  if (value.newest_at_ms !== undefined) {
    if (!isNonNegativeInteger(value.newest_at_ms)) {
      throw new TypeError("Invalid rejection window newest_at_ms");
    }
    window.newest_at_ms = value.newest_at_ms;
  }
  return window;
};

const parseReasonCount = (
  value: unknown,
  index: number,
): RejectionReasonCount => {
  if (
    !isRecord(value) ||
    !isString(value.reason) ||
    !isNonNegativeInteger(value.count) ||
    value.count === 0 ||
    typeof value.is_rollup !== "boolean"
  ) {
    throw new TypeError(`Invalid rejection reason count at index ${index}`);
  }
  return {
    reason: value.reason,
    count: value.count,
    is_rollup: value.is_rollup,
  };
};

const parseVerdictCount = (
  value: unknown,
  context: string,
): RejectionVerdictCount => {
  if (
    !isRecord(value) ||
    !isNonEmptyString(value.verdict) ||
    !isNonNegativeInteger(value.count)
  ) {
    throw new TypeError(`Invalid ${context}`);
  }
  return { verdict: value.verdict, count: value.count };
};

const parseTaxonomyBreakdown = (
  value: unknown,
  index: number,
): RejectionTaxonomyBreakdown => {
  if (
    !isRecord(value) ||
    !isNonEmptyString(value.key) ||
    !isNonEmptyString(value.label) ||
    !Array.isArray(value.verdicts)
  ) {
    throw new TypeError(
      `Invalid rejection taxonomy breakdown at index ${index}`,
    );
  }
  return {
    key: value.key,
    label: value.label,
    verdicts: value.verdicts.map((verdict, verdictIndex) =>
      parseVerdictCount(
        verdict,
        `rejection taxonomy "${value.key}" verdict ${verdictIndex}`,
      ),
    ),
  };
};

const parseAttribution = (value: unknown): RejectionAttribution => {
  if (
    !isRecord(value) ||
    !isNonNegativeInteger(value.classified_count) ||
    !isNonNegativeInteger(value.unclassified_count) ||
    !Array.isArray(value.taxonomies)
  ) {
    throw new TypeError("Invalid rejection attribution");
  }
  return {
    classified_count: value.classified_count,
    unclassified_count: value.unclassified_count,
    taxonomies: value.taxonomies.map((taxonomy, index) =>
      parseTaxonomyBreakdown(taxonomy, index),
    ),
  };
};

const parseVerdictPair = (
  value: unknown,
  context: string,
): [string, string] => {
  if (
    !Array.isArray(value) ||
    value.length !== 2 ||
    !isNonEmptyString(value[0]) ||
    !isNonEmptyString(value[1])
  ) {
    throw new TypeError(`Invalid ${context}`);
  }
  return [value[0], value[1]];
};

const parseRecord = (value: unknown, index: number): RejectionRecord => {
  if (
    !isRecord(value) ||
    !isTxid(value.txid) ||
    !isString(value.reason) ||
    !isNonNegativeInteger(value.observed_at_ms) ||
    !isNonEmptyString(value.evidence_event_id) ||
    !Array.isArray(value.verdicts)
  ) {
    throw new TypeError(`Invalid rejection record at index ${index}`);
  }
  return {
    txid: value.txid,
    reason: value.reason,
    observed_at_ms: value.observed_at_ms,
    evidence_event_id: value.evidence_event_id,
    verdicts: value.verdicts.map((pair, pairIndex) =>
      parseVerdictPair(pair, `rejection record ${index} verdict ${pairIndex}`),
    ),
  };
};

export const parseSourceRejections = (value: unknown): SourceRejections => {
  if (
    !isRecord(value) ||
    !isNonEmptyString(value.source_id) ||
    !isNonNegativeInteger(value.as_of_ms) ||
    !Array.isArray(value.by_reason) ||
    !Array.isArray(value.recent)
  ) {
    throw new TypeError("Invalid source rejections");
  }
  const availability = parseAvailability(value.availability);
  const window = parseWindow(value.window);
  const byReason = value.by_reason.map((entry, index) =>
    parseReasonCount(entry, index),
  );
  const attribution = parseAttribution(value.attribution);
  const recent = value.recent.map((entry, index) => parseRecord(entry, index));

  const hasOldest = window.oldest_at_ms !== undefined;
  const hasNewest = window.newest_at_ms !== undefined;
  if (
    (window.count === 0 && (hasOldest || hasNewest)) ||
    (window.count > 0 && (!hasOldest || !hasNewest)) ||
    (hasOldest && hasNewest && window.oldest_at_ms! > window.newest_at_ms!) ||
    (hasNewest && window.newest_at_ms! > value.as_of_ms)
  ) {
    throw new TypeError("Rejection window timestamps do not match its count");
  }

  const reasonTotal = byReason.reduce((sum, entry) => sum + entry.count, 0);
  if (reasonTotal !== window.count) {
    throw new TypeError("Rejection reason counts do not match the window");
  }
  const reasonKeys = byReason.map(
    (entry) => `${entry.is_rollup ? "rollup" : "literal"}:${entry.reason}`,
  );
  if (new Set(reasonKeys).size !== reasonKeys.length) {
    throw new TypeError("Rejection reason entries must be distinct");
  }
  if (byReason.filter((entry) => entry.is_rollup).length > 1) {
    throw new TypeError("Rejection reasons may contain at most one rollup");
  }

  if (
    attribution.classified_count + attribution.unclassified_count !==
    window.count
  ) {
    throw new TypeError("Rejection attribution does not match the window");
  }
  const taxonomyKeys = attribution.taxonomies.map((taxonomy) => taxonomy.key);
  if (new Set(taxonomyKeys).size !== taxonomyKeys.length) {
    throw new TypeError("Rejection attribution taxonomies must be distinct");
  }
  for (const taxonomy of attribution.taxonomies) {
    const verdictKeys = taxonomy.verdicts.map((verdict) => verdict.verdict);
    const verdictTotal = taxonomy.verdicts.reduce(
      (sum, verdict) => sum + verdict.count,
      0,
    );
    if (
      new Set(verdictKeys).size !== verdictKeys.length ||
      verdictTotal !== attribution.classified_count
    ) {
      throw new TypeError(
        `Rejection taxonomy "${taxonomy.key}" does not match classified_count`,
      );
    }
  }

  const rejections: SourceRejections = {
    source_id: value.source_id,
    as_of_ms: value.as_of_ms,
    availability,
    window,
    by_reason: byReason,
    attribution,
    recent,
  };
  if (value.next_cursor !== undefined) {
    if (!isNonEmptyString(value.next_cursor)) {
      throw new TypeError("Invalid rejection next_cursor");
    }
    rejections.next_cursor = value.next_cursor;
  }
  if (
    availability.status === "not_collected" &&
    (window.count !== 0 ||
      byReason.length !== 0 ||
      attribution.classified_count !== 0 ||
      attribution.unclassified_count !== 0 ||
      attribution.taxonomies.length !== 0 ||
      recent.length !== 0 ||
      rejections.next_cursor !== undefined)
  ) {
    throw new TypeError(
      "Rejection evidence marked not_collected must not contain observations",
    );
  }
  return rejections;
};

export const fetchRejections = async (
  sourceId: string,
  limit: number,
  signal?: AbortSignal,
): Promise<SourceRejections> => {
  const parameters = new URLSearchParams();
  parameters.set("limit", String(limit));
  const response = await fetch(
    `/api/v1/sources/${encodeURIComponent(sourceId)}/rejections?${parameters.toString()}`,
    { headers: { Accept: "application/json" }, signal: signal ?? null },
  );
  if (!response.ok) {
    throw new Error(`Rejections request failed with HTTP ${response.status}`);
  }
  const rejections = parseSourceRejections(await response.json());
  if (rejections.source_id !== sourceId) {
    throw new Error(
      `Rejections response source ${rejections.source_id} does not match ${sourceId}`,
    );
  }
  return rejections;
};
