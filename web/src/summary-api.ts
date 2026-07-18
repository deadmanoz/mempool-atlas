import { parseSourceHealth } from "./api";
import type {
  AggregateBin,
  BinCatalog,
  DimensionHistogram,
  EcdfSeries,
  FeeRateEcdf,
  JointFeeSize,
  MempoolSummary,
  ScriptTypeKey,
  ShapeDimension,
  SourceDescriptor,
  SummaryFilter,
  SummaryHistograms,
  TaxonomyDescriptor,
  TaxonomyFilterSelection,
  TaxonomyHistogram,
  VerdictDescriptor,
} from "./types";

export const SCRIPT_TYPE_KEYS: readonly ScriptTypeKey[] = [
  "p2tr",
  "p2wpkh",
  "p2wsh",
  "p2sh",
  "p2pkh",
  "op_return",
  "other",
];

const SHAPE_DIMENSIONS: readonly ShapeDimension[] = [
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

const isNonEmptyString = (value: unknown): value is string =>
  typeof value === "string" && value.length > 0;

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

const parseVerdictDescriptor = (
  value: unknown,
  context: string,
): VerdictDescriptor => {
  if (
    !isRecord(value) ||
    !isNonEmptyString(value.key) ||
    !isNonEmptyString(value.label)
  ) {
    throw new TypeError(`Invalid ${context}`);
  }
  return { key: value.key, label: value.label };
};

const parseTaxonomyDescriptor = (
  value: unknown,
  context: string,
): TaxonomyDescriptor => {
  if (
    !isRecord(value) ||
    !isNonEmptyString(value.key) ||
    !isNonEmptyString(value.label) ||
    !Array.isArray(value.verdicts) ||
    value.verdicts.length === 0
  ) {
    throw new TypeError(`Invalid ${context}`);
  }
  const verdicts = value.verdicts.map((verdict, index) =>
    parseVerdictDescriptor(verdict, `${context} verdict ${index}`),
  );
  if (!verdicts.some((verdict) => verdict.key === "unknown")) {
    throw new TypeError(`${context} is missing the "unknown" verdict`);
  }
  return { key: value.key, label: value.label, verdicts };
};

const parseDimensionHistogram = (
  value: unknown,
  context: string,
  verdictCount?: number,
): DimensionHistogram => {
  if (!isRecord(value)) {
    throw new TypeError(`Invalid ${context}`);
  }
  if (value.status === "available" && Array.isArray(value.bins)) {
    if (verdictCount !== undefined && value.bins.length !== verdictCount) {
      throw new TypeError(`Invalid ${context} bin count`);
    }
    return {
      status: "available",
      bins: value.bins.map((bin, index) =>
        parseAggregateBin(bin, `${context} bin ${index}`),
      ),
      underived: parseAggregateBin(value.underived, `${context} underived`),
    };
  }
  if (
    value.status === "unavailable" &&
    typeof value.reason === "string" &&
    value.reason.length > 0
  ) {
    return { status: "unavailable", reason: value.reason };
  }
  throw new TypeError(`Invalid ${context}`);
};

const parseTaxonomyHistogram = (
  value: unknown,
  taxonomy: TaxonomyDescriptor,
  index: number,
): TaxonomyHistogram => {
  if (!isRecord(value) || value.key !== taxonomy.key) {
    throw new TypeError(
      `Taxonomy histogram at index ${index} does not match catalog key "${taxonomy.key}"`,
    );
  }
  const histogram = parseDimensionHistogram(
    value,
    `taxonomy "${taxonomy.key}" histogram`,
    taxonomy.verdicts.length,
  );
  return { key: taxonomy.key, ...histogram };
};

const parseBinCatalog = (value: unknown): BinCatalog => {
  if (
    !isRecord(value) ||
    !Array.isArray(value.taxonomies) ||
    value.taxonomies.length === 0 ||
    !Array.isArray(value.script_keys) ||
    !value.script_keys.every(isScriptTypeKey)
  ) {
    throw new TypeError("Invalid bin catalog");
  }
  const taxonomies = value.taxonomies.map((taxonomy, index) =>
    parseTaxonomyDescriptor(taxonomy, `bin catalog taxonomy ${index}`),
  );
  return {
    taxonomies,
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
    script_keys: value.script_keys,
  };
};

const parseHistograms = (
  value: unknown,
  bins: BinCatalog,
): SummaryHistograms => {
  if (!isRecord(value) || !Array.isArray(value.taxonomies)) {
    throw new TypeError("Invalid histograms");
  }
  const rawTaxonomies = value.taxonomies;
  if (rawTaxonomies.length !== bins.taxonomies.length) {
    throw new TypeError(
      "Histogram taxonomies do not match the bin catalog taxonomies",
    );
  }
  const taxonomies = bins.taxonomies.map((taxonomy, index) =>
    parseTaxonomyHistogram(rawTaxonomies[index], taxonomy, index),
  );
  const shapeEntries = Object.fromEntries(
    SHAPE_DIMENSIONS.map((dimension) => [
      dimension,
      parseDimensionHistogram(value[dimension], `${dimension} histogram`),
    ]),
  ) as Record<ShapeDimension, DimensionHistogram>;
  return { taxonomies, ...shapeEntries };
};

const parseTaxonomyFilterSelection = (
  value: unknown,
  index: number,
): TaxonomyFilterSelection => {
  if (
    !isRecord(value) ||
    !isNonEmptyString(value.key) ||
    !Array.isArray(value.verdicts) ||
    value.verdicts.length === 0 ||
    !value.verdicts.every(isNonEmptyString)
  ) {
    throw new TypeError(`Invalid filter echo taxonomy selection ${index}`);
  }
  return { key: value.key, verdicts: value.verdicts };
};

const parseFilterEcho = (value: unknown): SummaryFilter => {
  if (!isRecord(value)) {
    throw new TypeError("Invalid filter echo");
  }
  const filter: SummaryFilter = {};
  if (value.taxonomies !== undefined) {
    if (!Array.isArray(value.taxonomies)) {
      throw new TypeError("Invalid filter echo taxonomies");
    }
    filter.taxonomies = value.taxonomies.map((entry, index) =>
      parseTaxonomyFilterSelection(entry, index),
    );
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

const parseEcdf = (value: unknown, bins: BinCatalog): FeeRateEcdf => {
  if (
    !isRecord(value) ||
    !isNonEmptyString(value.taxonomy) ||
    !Array.isArray(value.series)
  ) {
    throw new TypeError("Invalid ECDF block");
  }
  const taxonomy = bins.taxonomies.find(
    (entry) => entry.key === value.taxonomy,
  );
  if (taxonomy === undefined) {
    throw new TypeError(
      `ECDF taxonomy "${value.taxonomy}" is not in the bin catalog`,
    );
  }
  const verdictKeys = new Set(taxonomy.verdicts.map((verdict) => verdict.key));
  const series: EcdfSeries[] = value.series.map((entry, index) => {
    if (
      !isRecord(entry) ||
      !isNonEmptyString(entry.key) ||
      !verdictKeys.has(entry.key) ||
      !Array.isArray(entry.cum_vsize) ||
      !entry.cum_vsize.every(isNonNegativeInteger)
    ) {
      throw new TypeError(`Invalid ECDF series at index ${index}`);
    }
    return { key: entry.key, cum_vsize: entry.cum_vsize };
  });
  return {
    taxonomy: value.taxonomy,
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
    !isRecord(value.totals)
  ) {
    throw new TypeError("Invalid mempool summary");
  }
  const awaiting = value.totals.awaiting_rpc;
  if (!isRecord(awaiting) || !isNonNegativeInteger(awaiting.count)) {
    throw new TypeError("Invalid awaiting-RPC total");
  }
  const bins = parseBinCatalog(value.bins);
  const histograms = parseHistograms(value.histograms, bins);

  const summary: MempoolSummary = {
    source_id: value.source_id,
    as_of_ms: value.as_of_ms,
    filter_echo: parseFilterEcho(value.filter_echo),
    totals: {
      all: parseAggregateBin(value.totals.all, "totals.all"),
      matching: parseAggregateBin(value.totals.matching, "totals.matching"),
      awaiting_rpc: { count: awaiting.count },
    },
    bins,
    histograms,
    health: parseSourceHealth(value.health),
  };
  if (value.ecdf !== undefined) {
    summary.ecdf = parseEcdf(value.ecdf, bins);
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
  if (filter.taxonomies !== undefined) {
    for (const selection of filter.taxonomies) {
      if (selection.verdicts.length > 0) {
        parameters.set(`t.${selection.key}`, selection.verdicts.join(","));
      }
    }
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
