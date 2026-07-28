import type {
  ComparisonPolicyFilter,
  ComparisonPolicyStatus,
  ComparisonRegionKey,
  ComparisonSide,
} from "./comparison-model";
import type {
  StatusRegionKey,
  TerrainRegionKey,
  TerrainSelection,
  ViolationSignatureKey,
} from "./terrain";
import { RULE_IDS, type RuleId } from "./types";

export interface NodeViewState {
  source: string | null;
  selection: TerrainSelection | null;
  txid: string | null;
}

export interface ComparisonViewState {
  left: string | null;
  right: string | null;
  region: ComparisonRegionKey | null;
  side: ComparisonSide | null;
  filter: ComparisonPolicyFilter;
  txid: string | null;
}

type SearchInput = string | URLSearchParams;

const SOURCE_ID_PATTERN = /^[A-Za-z0-9._-]{1,64}$/;
const TXID_PATTERN = /^[0-9a-f]{64}$/i;
const SIGNATURE_MASK_LIMIT = (1 << RULE_IDS.length) - 1;

const searchParams = (input: SearchInput): URLSearchParams =>
  typeof input === "string" ? new URLSearchParams(input) : input;

const singleValue = (params: URLSearchParams, key: string): string | null => {
  const values = params.getAll(key);
  return values.length === 1 ? (values[0] ?? null) : null;
};

const sourceId = (value: string | null): string | null =>
  value !== null &&
  value !== "." &&
  value !== ".." &&
  SOURCE_ID_PATTERN.test(value)
    ? value
    : null;

const txid = (value: string | null): string | null =>
  value !== null && TXID_PATTERN.test(value) ? value.toLowerCase() : null;

const ruleId = (value: string | null): RuleId | null =>
  value !== null && (RULE_IDS as readonly string[]).includes(value)
    ? (value as RuleId)
    : null;

const statusRegion = (value: string): StatusRegionKey | null =>
  value === "compatible" ||
  value === "indeterminate" ||
  value === "unclassified"
    ? value
    : null;

const comparisonPolicyStatus = (
  value: string,
): ComparisonPolicyStatus | null =>
  value === "violating" ? value : statusRegion(value);

const encodedMask = (value: string): string | null => {
  if (!/^[0-9a-f]{1,2}$/i.test(value)) {
    return null;
  }
  const mask = Number.parseInt(value, 16);
  return mask > 0 && mask <= SIGNATURE_MASK_LIMIT
    ? mask.toString(16).padStart(2, "0")
    : null;
};

const violationSignature = (value: string): ViolationSignatureKey | null => {
  const parts = value.split(":");
  if (parts[0] === "exact" && parts.length === 2) {
    const violated = encodedMask(parts[1] ?? "");
    return violated === null ? null : `exact:${violated}`;
  }
  if (parts[0] === "partial" && parts.length === 3) {
    const violated = encodedMask(parts[1] ?? "");
    const unknown = encodedMask(parts[2] ?? "");
    return violated === null || unknown === null
      ? null
      : `partial:${violated}:${unknown}`;
  }
  return null;
};

const terrainRegion = (value: string | null): TerrainRegionKey | null => {
  if (value === null) {
    return null;
  }
  return statusRegion(value) ?? violationSignature(value);
};

const comparisonRegion = (value: string | null): ComparisonRegionKey | null =>
  value === "common" || value === "left_only" || value === "right_only"
    ? value
    : null;

const comparisonSide = (value: string | null): ComparisonSide | null =>
  value === "left" || value === "right" ? value : null;

export const parseComparisonPolicyFilter = (
  value: string | null,
): ComparisonPolicyFilter => {
  if (value === null || value === "all") {
    return { kind: "all" };
  }

  const separator = value.indexOf(":");
  if (separator < 0) {
    return { kind: "all" };
  }
  const kind = value.slice(0, separator);
  const payload = value.slice(separator + 1);
  if (kind === "rule") {
    const rule = ruleId(payload);
    return rule === null ? { kind: "all" } : { kind: "rule", rule };
  }
  if (kind === "status") {
    const status = comparisonPolicyStatus(payload);
    return status === null ? { kind: "all" } : { kind: "status", status };
  }
  if (kind === "signature") {
    const signature = violationSignature(payload);
    return signature === null
      ? { kind: "all" }
      : { kind: "signature", signature };
  }
  return { kind: "all" };
};

export const serializeComparisonPolicyFilter = (
  filter: ComparisonPolicyFilter,
): string | null => {
  if (filter.kind === "all") {
    return null;
  }
  if (filter.kind === "rule") {
    const rule = ruleId(filter.rule);
    return rule === null ? null : `rule:${rule}`;
  }
  if (filter.kind === "status") {
    const status = comparisonPolicyStatus(filter.status);
    return status === null ? null : `status:${status}`;
  }
  const signature = violationSignature(filter.signature);
  return signature === null ? null : `signature:${signature}`;
};

export const parseNodeViewState = (input: SearchInput): NodeViewState => {
  const params = searchParams(input);
  const rule = ruleId(singleValue(params, "rule"));
  const region = terrainRegion(singleValue(params, "region"));
  const selection: TerrainSelection | null =
    rule !== null && region !== null
      ? null
      : rule !== null
        ? { kind: "rule", rule }
        : region !== null
          ? { kind: "region", regionKey: region }
          : null;

  return {
    source: sourceId(singleValue(params, "source")),
    selection,
    txid: txid(singleValue(params, "txid")),
  };
};

export const serializeNodeViewState = (state: NodeViewState): string => {
  const params = new URLSearchParams();
  const source = sourceId(state.source);
  if (source !== null) {
    params.set("source", source);
  }
  if (state.selection?.kind === "rule") {
    const rule = ruleId(state.selection.rule);
    if (rule !== null) {
      params.set("rule", rule);
    }
  } else if (state.selection?.kind === "region") {
    const region = terrainRegion(state.selection.regionKey);
    if (region !== null) {
      params.set("region", region);
    }
  }
  const selectedTxid = txid(state.txid);
  if (selectedTxid !== null) {
    params.set("txid", selectedTxid);
  }
  return params.toString();
};

export const parseComparisonViewState = (
  input: SearchInput,
): ComparisonViewState => {
  const params = searchParams(input);
  const requestedLeft = sourceId(singleValue(params, "left"));
  const requestedRight = sourceId(singleValue(params, "right"));
  const hasValidPair =
    requestedLeft !== null &&
    requestedRight !== null &&
    requestedLeft !== requestedRight;

  return {
    left: hasValidPair ? requestedLeft : null,
    right: hasValidPair ? requestedRight : null,
    region: comparisonRegion(singleValue(params, "region")),
    side: comparisonSide(singleValue(params, "side")),
    filter: parseComparisonPolicyFilter(singleValue(params, "filter")),
    txid: txid(singleValue(params, "txid")),
  };
};

export const serializeComparisonViewState = (
  state: ComparisonViewState,
): string => {
  const params = new URLSearchParams();
  const left = sourceId(state.left);
  const right = sourceId(state.right);
  if (left !== null && right !== null && left !== right) {
    params.set("left", left);
    params.set("right", right);
  }
  const region = state.region === null ? null : comparisonRegion(state.region);
  if (region !== null) {
    params.set("region", region);
  }
  const side = state.side === null ? null : comparisonSide(state.side);
  if (side !== null) {
    params.set("side", side);
  }
  const filter = serializeComparisonPolicyFilter(state.filter);
  if (filter !== null) {
    params.set("filter", filter);
  }
  const selectedTxid = txid(state.txid);
  if (selectedTxid !== null) {
    params.set("txid", selectedTxid);
  }
  return params.toString();
};
