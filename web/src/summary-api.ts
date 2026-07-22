import { parseReplicaCursor, parseSourceHealth } from "./api";
import {
  isFiniteNumber,
  isNonEmptyString,
  isNonNegativeInteger,
  isRecord,
  isScriptTypeKey,
  parseBinCatalog,
  parseHistograms,
  parseNumberArray,
  parseAggregateBin,
  SCRIPT_TYPE_KEYS,
} from "./summary-parsing";
import type {
  BinCatalog,
  EcdfSeries,
  FeeRateEcdf,
  JointFeeSize,
  MempoolSummary,
  SourceDescriptor,
  SummaryFilter,
  TaxonomyFilterSelection,
} from "./types";

export { SCRIPT_TYPE_KEYS };

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
      !isNonNegativeInteger(entry.state_observed_at_ms) ||
      !isNonNegativeInteger(entry.membership_count)
    ) {
      throw new TypeError(`Invalid source descriptor at index ${index}`);
    }
    return {
      source_id: entry.source_id,
      state_cursor: parseReplicaCursor(entry.state_cursor),
      state_observed_at_ms: entry.state_observed_at_ms,
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
