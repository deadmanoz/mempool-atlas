// Strict parser and fetch for the read-time source comparison. The comparison
// is derived on demand from independent source projections and ships only
// aggregates, so every region reuses the summary `bins`/`histograms` shapes and
// their shared parsers. Validation matches `parseMempoolSummary`: the asserted
// source order is bounded to 2..4, the stage list is checked to be exactly the
// adjacent pairs of that order, and each region's histograms are validated
// against their own bin catalog.

import {
  isNonEmptyString,
  isNonNegativeInteger,
  isRecord,
  parseAggregateBin,
  parseBinCatalog,
  parseHistograms,
} from "./summary-parsing";
import type {
  AnomalyRegion,
  ComparisonSourceTotal,
  ComparisonStage,
  RegionAggregate,
  SourceComparison,
} from "./types";

const MIN_SOURCES = 2;
const MAX_SOURCES = 4;

const parseAwaitingRpc = (
  value: unknown,
  context: string,
): { count: number } => {
  if (!isRecord(value) || !isNonNegativeInteger(value.count)) {
    throw new TypeError(`Invalid ${context}`);
  }
  return { count: value.count };
};

const parseRegionAggregate = (
  value: unknown,
  context: string,
): RegionAggregate => {
  if (!isRecord(value)) {
    throw new TypeError(`Invalid ${context}`);
  }
  const bins = parseBinCatalog(value.bins);
  return {
    present: parseAggregateBin(value.present, `${context} present`),
    awaiting_rpc: parseAwaitingRpc(
      value.awaiting_rpc,
      `${context} awaiting_rpc`,
    ),
    bins,
    histograms: parseHistograms(value.histograms, bins),
  };
};

const parseAnomalyRegion = (value: unknown, context: string): AnomalyRegion => {
  if (!isRecord(value)) {
    throw new TypeError(`Invalid ${context}`);
  }
  return {
    present: parseAggregateBin(value.present, `${context} present`),
    awaiting_rpc: parseAwaitingRpc(
      value.awaiting_rpc,
      `${context} awaiting_rpc`,
    ),
  };
};

const parseStage = (
  value: unknown,
  sources: string[],
  index: number,
): ComparisonStage => {
  if (
    !isRecord(value) ||
    !isNonEmptyString(value.from) ||
    !isNonEmptyString(value.to)
  ) {
    throw new TypeError(`Invalid comparison stage at index ${index}`);
  }
  const expectedFrom = sources[index];
  const expectedTo = sources[index + 1];
  if (value.from !== expectedFrom || value.to !== expectedTo) {
    throw new TypeError(
      `Comparison stage ${index} is not the adjacent pair "${expectedFrom}" -> "${expectedTo}"`,
    );
  }
  return {
    from: value.from,
    to: value.to,
    added: parseRegionAggregate(value.added, `stage ${index} added region`),
    anomaly: parseAnomalyRegion(value.anomaly, `stage ${index} anomaly region`),
  };
};

const parseSourceTotals = (
  value: unknown,
  sources: string[],
): ComparisonSourceTotal[] => {
  if (!Array.isArray(value) || value.length !== sources.length) {
    throw new TypeError("Comparison source totals do not cover every source");
  }
  return value.map((entry, index) => {
    if (!isRecord(entry) || !isNonEmptyString(entry.source_id)) {
      throw new TypeError(`Invalid comparison source total at index ${index}`);
    }
    if (entry.source_id !== sources[index]) {
      throw new TypeError(
        `Comparison source total ${index} does not match requested source "${sources[index]}"`,
      );
    }
    return {
      source_id: entry.source_id,
      present: parseAggregateBin(
        entry.present,
        `source total ${index} present`,
      ),
      awaiting_rpc: parseAwaitingRpc(
        entry.awaiting_rpc,
        `source total ${index} awaiting_rpc`,
      ),
    };
  });
};

export const parseSourceComparison = (value: unknown): SourceComparison => {
  if (
    !isRecord(value) ||
    !isNonNegativeInteger(value.as_of_ms) ||
    !Array.isArray(value.sources) ||
    !value.sources.every(isNonEmptyString)
  ) {
    throw new TypeError("Invalid source comparison");
  }
  const sources = value.sources;
  if (sources.length < MIN_SOURCES || sources.length > MAX_SOURCES) {
    throw new TypeError(
      `A comparison needs ${MIN_SOURCES}..${MAX_SOURCES} sources, got ${sources.length}`,
    );
  }
  if (new Set(sources).size !== sources.length) {
    throw new TypeError("Comparison sources must be distinct");
  }
  if (
    !Array.isArray(value.stages) ||
    value.stages.length !== sources.length - 1
  ) {
    throw new TypeError(
      "A comparison must carry exactly one stage per adjacent source pair",
    );
  }
  return {
    sources,
    as_of_ms: value.as_of_ms,
    source_totals: parseSourceTotals(value.source_totals, sources),
    shared: parseRegionAggregate(value.shared, "shared region"),
    stages: value.stages.map((stage, index) =>
      parseStage(stage, sources, index),
    ),
  };
};

export const comparisonQuery = (sources: string[]): string => {
  const parameters = new URLSearchParams();
  parameters.set("sources", sources.join(","));
  return parameters.toString();
};

export const fetchComparison = async (
  sources: string[],
  signal?: AbortSignal,
): Promise<SourceComparison> => {
  const response = await fetch(
    `/api/v1/sources/compare?${comparisonQuery(sources)}`,
    { headers: { Accept: "application/json" }, signal: signal ?? null },
  );
  if (!response.ok) {
    throw new Error(`Comparison request failed with HTTP ${response.status}`);
  }
  const comparison = parseSourceComparison(await response.json());
  if (comparison.sources.join(",") !== sources.join(",")) {
    throw new Error(
      "Comparison response did not echo the requested source order",
    );
  }
  return comparison;
};
