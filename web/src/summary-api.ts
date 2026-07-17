import { parseSourceHealth } from "./api";
import type {
  AggregateBin,
  BinCatalog,
  ClassificationKey,
  DimensionHistogram,
  EcdfSeries,
  FeeRateEcdf,
  JointFeeSize,
  MempoolSummary,
  ScriptTypeKey,
  SourceDescriptor,
  SummaryDimension,
  SummaryFilter,
  SummaryHistograms,
} from "./types";

export const CLASSIFICATION_KEYS: readonly ClassificationKey[] = [
  "payment",
  "consolidation",
  "batch",
  "coinjoin",
  "data",
  "lightning",
  "unknown",
];

export const SCRIPT_TYPE_KEYS: readonly ScriptTypeKey[] = [
  "p2tr",
  "p2wpkh",
  "p2wsh",
  "p2sh",
  "p2pkh",
  "op_return",
  "other",
];

const SUMMARY_DIMENSIONS: readonly SummaryDimension[] = [
  "classification",
  "script",
  "value",
  "inputs",
  "outputs",
  "age",
  "feerate",
];

const isRecord = (value: unknown): value is Record<string, unknown> =>
  typeof value === "object" && value !== null;

const isNonNegativeInteger = (value: unknown): value is number =>
  typeof value === "number" && Number.isSafeInteger(value) && value >= 0;

const isFiniteNumber = (value: unknown): value is number =>
  typeof value === "number" && Number.isFinite(value);

const isClassificationKey = (value: unknown): value is ClassificationKey =>
  typeof value === "string" &&
  (CLASSIFICATION_KEYS as readonly string[]).includes(value);

const isScriptTypeKey = (value: unknown): value is ScriptTypeKey =>
  typeof value === "string" &&
  (SCRIPT_TYPE_KEYS as readonly string[]).includes(value);

const parseNumberArray = (value: unknown, context: string): number[] => {
  if (!Array.isArray(value) || !value.every(isFiniteNumber)) {
    throw new TypeError(`Invalid ${context}`);
  }
  return value;
};

const parseAggregateBin = (value: unknown, context: string): AggregateBin => {
  if (
    !isRecord(value) ||
    !isNonNegativeInteger(value.count) ||
    !isNonNegativeInteger(value.vsize)
  ) {
    throw new TypeError(`Invalid ${context}`);
  }
  return { count: value.count, vsize: value.vsize };
};

const parseDimensionHistogram = (
  value: unknown,
  dimension: string,
): DimensionHistogram => {
  if (!isRecord(value)) {
    throw new TypeError(`Invalid ${dimension} histogram`);
  }
  if (value.status === "available" && Array.isArray(value.bins)) {
    return {
      status: "available",
      bins: value.bins.map((bin, index) =>
        parseAggregateBin(bin, `${dimension} histogram bin ${index}`),
      ),
      underived: parseAggregateBin(
        value.underived,
        `${dimension} histogram underived`,
      ),
    };
  }
  if (
    value.status === "unavailable" &&
    typeof value.reason === "string" &&
    value.reason.length > 0
  ) {
    return { status: "unavailable", reason: value.reason };
  }
  throw new TypeError(`Invalid ${dimension} histogram`);
};

const parseBinCatalog = (value: unknown): BinCatalog => {
  if (
    !isRecord(value) ||
    !Array.isArray(value.classification_keys) ||
    !value.classification_keys.every(isClassificationKey) ||
    !Array.isArray(value.script_keys) ||
    !value.script_keys.every(isScriptTypeKey)
  ) {
    throw new TypeError("Invalid bin catalog");
  }
  return {
    feerate_sat_per_vb_edges: parseNumberArray(
      value.feerate_sat_per_vb_edges,
      "fee-rate edges",
    ),
    age_ms_edges: parseNumberArray(value.age_ms_edges, "age edges"),
    value_sats_edges: parseNumberArray(value.value_sats_edges, "value edges"),
    input_count_uppers: parseNumberArray(
      value.input_count_uppers,
      "input-count uppers",
    ),
    output_count_uppers: parseNumberArray(
      value.output_count_uppers,
      "output-count uppers",
    ),
    classification_keys: value.classification_keys,
    script_keys: value.script_keys,
  };
};

const parseFilterEcho = (value: unknown): SummaryFilter => {
  if (!isRecord(value)) {
    throw new TypeError("Invalid filter echo");
  }
  const filter: SummaryFilter = {};
  if (value.classes !== undefined) {
    if (
      !Array.isArray(value.classes) ||
      !value.classes.every(isClassificationKey)
    ) {
      throw new TypeError("Invalid filter echo classes");
    }
    filter.classes = value.classes;
  }
  if (value.scripts !== undefined) {
    if (
      !Array.isArray(value.scripts) ||
      !value.scripts.every(isScriptTypeKey)
    ) {
      throw new TypeError("Invalid filter echo scripts");
    }
    filter.scripts = value.scripts;
  }
  if (value.feerate_min !== undefined) {
    if (!isFiniteNumber(value.feerate_min)) {
      throw new TypeError("Invalid filter echo feerate_min");
    }
    filter.feerate_min = value.feerate_min;
  }
  if (value.feerate_max !== undefined) {
    if (!isFiniteNumber(value.feerate_max)) {
      throw new TypeError("Invalid filter echo feerate_max");
    }
    filter.feerate_max = value.feerate_max;
  }
  return filter;
};

const parseEcdf = (value: unknown): FeeRateEcdf => {
  if (!isRecord(value) || !Array.isArray(value.series)) {
    throw new TypeError("Invalid ECDF block");
  }
  const series: EcdfSeries[] = value.series.map((entry, index) => {
    if (
      !isRecord(entry) ||
      !isClassificationKey(entry.key) ||
      !Array.isArray(entry.cum_vsize) ||
      !entry.cum_vsize.every(isNonNegativeInteger)
    ) {
      throw new TypeError(`Invalid ECDF series at index ${index}`);
    }
    return { key: entry.key, cum_vsize: entry.cum_vsize };
  });
  return {
    fee_edges: parseNumberArray(value.fee_edges, "ECDF fee edges"),
    series,
  };
};

const parseJoint = (value: unknown): JointFeeSize => {
  if (!isRecord(value) || !Array.isArray(value.grid)) {
    throw new TypeError("Invalid joint fee-size block");
  }
  const grid = value.grid.map((row, index) => {
    if (!Array.isArray(row) || !row.every(isNonNegativeInteger)) {
      throw new TypeError(`Invalid joint grid row ${index}`);
    }
    return row;
  });
  return {
    fee_edges: parseNumberArray(value.fee_edges, "joint fee edges"),
    size_edges: parseNumberArray(value.size_edges, "joint size edges"),
    grid,
  };
};

export const parseMempoolSummary = (value: unknown): MempoolSummary => {
  if (
    !isRecord(value) ||
    typeof value.source_id !== "string" ||
    value.source_id.trim().length === 0 ||
    !isNonNegativeInteger(value.as_of_ms) ||
    !isRecord(value.totals) ||
    !isRecord(value.histograms)
  ) {
    throw new TypeError("Invalid mempool summary");
  }
  const awaiting = value.totals.awaiting_rpc;
  if (!isRecord(awaiting) || !isNonNegativeInteger(awaiting.count)) {
    throw new TypeError("Invalid awaiting-RPC total");
  }
  const histogramEntries = value.histograms;
  const histograms = Object.fromEntries(
    SUMMARY_DIMENSIONS.map((dimension) => [
      dimension,
      parseDimensionHistogram(histogramEntries[dimension], dimension),
    ]),
  ) as SummaryHistograms;

  const summary: MempoolSummary = {
    source_id: value.source_id,
    as_of_ms: value.as_of_ms,
    filter_echo: parseFilterEcho(value.filter_echo),
    totals: {
      all: parseAggregateBin(value.totals.all, "totals.all"),
      matching: parseAggregateBin(value.totals.matching, "totals.matching"),
      awaiting_rpc: { count: awaiting.count },
    },
    bins: parseBinCatalog(value.bins),
    histograms,
    health: parseSourceHealth(value.health),
  };
  if (value.ecdf !== undefined) {
    summary.ecdf = parseEcdf(value.ecdf);
  }
  if (value.joint_fee_size !== undefined) {
    summary.joint_fee_size = parseJoint(value.joint_fee_size);
  }
  return summary;
};

export const parseSourcesResponse = (value: unknown): SourceDescriptor[] => {
  if (!isRecord(value) || !Array.isArray(value.sources)) {
    throw new TypeError("Invalid sources response");
  }
  return value.sources.map((entry, index) => {
    if (
      !isRecord(entry) ||
      typeof entry.source_id !== "string" ||
      entry.source_id.trim().length === 0 ||
      !isNonNegativeInteger(entry.last_seen_at_ms) ||
      !isNonNegativeInteger(entry.membership_count)
    ) {
      throw new TypeError(`Invalid source descriptor at index ${index}`);
    }
    return {
      source_id: entry.source_id,
      last_seen_at_ms: entry.last_seen_at_ms,
      membership_count: entry.membership_count,
    };
  });
};

export const summaryQuery = (filter: SummaryFilter): string => {
  const parameters = new URLSearchParams();
  if (filter.classes !== undefined && filter.classes.length > 0) {
    parameters.set("class", filter.classes.join(","));
  }
  if (filter.scripts !== undefined && filter.scripts.length > 0) {
    parameters.set("script", filter.scripts.join(","));
  }
  if (filter.feerate_min !== undefined) {
    parameters.set("feerate_min", String(filter.feerate_min));
  }
  if (filter.feerate_max !== undefined) {
    parameters.set("feerate_max", String(filter.feerate_max));
  }
  parameters.set("detail", "ecdf,joint");
  return parameters.toString();
};

export const fetchSummary = async (
  sourceId: string,
  filter: SummaryFilter,
): Promise<MempoolSummary> => {
  const response = await fetch(
    `/api/v1/sources/${encodeURIComponent(sourceId)}/mempool/summary?${summaryQuery(filter)}`,
    { headers: { Accept: "application/json" } },
  );
  if (!response.ok) {
    throw new Error(`Summary request failed with HTTP ${response.status}`);
  }
  const summary = parseMempoolSummary(await response.json());
  if (summary.source_id !== sourceId) {
    throw new Error(
      `Summary response source ${summary.source_id} does not match ${sourceId}`,
    );
  }
  return summary;
};

export const fetchSources = async (): Promise<SourceDescriptor[]> => {
  const response = await fetch("/api/v1/sources", {
    headers: { Accept: "application/json" },
  });
  if (!response.ok) {
    throw new Error(`Sources request failed with HTTP ${response.status}`);
  }
  return parseSourcesResponse(await response.json());
};
