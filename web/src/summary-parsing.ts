// Shared strict parsers for the aggregate wire shapes that appear in more than
// one response. The mempool summary and every comparison region carry the same
// `bins` (bin catalog) and `histograms` (per-taxonomy plus fixed shape)
// payloads, so their guards live here and are reused rather than copied.
//
// Every guard throws `TypeError` on malformed input, matching the rigor of the
// rest of the client: nothing is coerced or defaulted, alignment between the
// catalog and its histograms is validated, and array positions are checked
// against the declared vocabularies.

import type {
  AggregateBin,
  BinCatalog,
  DimensionHistogram,
  ScriptTypeKey,
  ShapeDimension,
  SummaryHistograms,
  TaxonomyDescriptor,
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

export const SHAPE_DIMENSIONS: readonly ShapeDimension[] = [
  "script",
  "value",
  "inputs",
  "outputs",
  "age",
  "feerate",
];

export const isRecord = (value: unknown): value is Record<string, unknown> =>
  typeof value === "object" && value !== null;

export const isNonNegativeInteger = (value: unknown): value is number =>
  typeof value === "number" && Number.isSafeInteger(value) && value >= 0;

export const isFiniteNumber = (value: unknown): value is number =>
  typeof value === "number" && Number.isFinite(value);

export const isNonEmptyString = (value: unknown): value is string =>
  typeof value === "string" && value.length > 0;

export const isScriptTypeKey = (value: unknown): value is ScriptTypeKey =>
  typeof value === "string" &&
  (SCRIPT_TYPE_KEYS as readonly string[]).includes(value);

export const parseNumberArray = (value: unknown, context: string): number[] => {
  if (!Array.isArray(value) || !value.every(isFiniteNumber)) {
    throw new TypeError(`Invalid ${context}`);
  }
  return value;
};

export const parseAggregateBin = (
  value: unknown,
  context: string,
): AggregateBin => {
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

export const parseDimensionHistogram = (
  value: unknown,
  context: string,
  expectedBinCount?: number,
): DimensionHistogram => {
  if (!isRecord(value)) {
    throw new TypeError(`Invalid ${context}`);
  }
  if (value.status === "available" && Array.isArray(value.bins)) {
    if (
      expectedBinCount !== undefined &&
      value.bins.length !== expectedBinCount
    ) {
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

export const parseBinCatalog = (value: unknown): BinCatalog => {
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

export const parseHistograms = (
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
  const expectedShapeBinCount: Record<ShapeDimension, number> = {
    script: bins.script_keys.length,
    value: bins.value_sats_edges.length + 1,
    inputs: bins.input_count_uppers.length + 1,
    outputs: bins.output_count_uppers.length + 1,
    age: bins.age_ms_edges.length + 1,
    feerate: bins.feerate_sat_per_vb_edges.length + 1,
  };
  const shapeEntries = Object.fromEntries(
    SHAPE_DIMENSIONS.map((dimension) => [
      dimension,
      parseDimensionHistogram(
        value[dimension],
        `${dimension} histogram`,
        expectedShapeBinCount[dimension],
      ),
    ]),
  ) as Record<ShapeDimension, DimensionHistogram>;
  return { taxonomies, ...shapeEntries };
};
