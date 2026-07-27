import type {
  Bip110Assessment,
  Bip110Status,
  Bip110Summary,
  ChainTip,
  MempoolSnapshot,
  MempoolTransaction,
  RuleAssessment,
  RuleId,
  RuleVerdict,
  SourceAvailability,
  SourceSnapshotResponse,
  SourceSummary,
  SourcesResponse,
  TransactionDetailResponse,
} from "./types";
import { RULE_IDS } from "./types";

const isRecord = (value: unknown): value is Record<string, unknown> =>
  typeof value === "object" && value !== null;

export class AtlasRequestError extends Error {
  readonly status: number;

  constructor(status: number, detail: string) {
    super(`Atlas request failed (${status})${detail}`);
    this.name = "AtlasRequestError";
    this.status = status;
  }
}

const isNonNegativeInteger = (value: unknown): value is number =>
  typeof value === "number" && Number.isSafeInteger(value) && value >= 0;

const isSourceId = (value: unknown): value is string =>
  typeof value === "string" &&
  value !== "." &&
  value !== ".." &&
  /^[A-Za-z0-9._-]{1,64}$/.test(value);

const sameRuleIds = (left: RuleId[], right: RuleId[]): boolean =>
  left.length === right.length &&
  left.every((rule, index) => rule === right[index]);

const sameAssessment = (
  left: Bip110Assessment,
  right: Bip110Assessment | null,
): boolean =>
  right !== null &&
  left.status === right.status &&
  left.primary_rule === right.primary_rule &&
  sameRuleIds(left.violated_rules, right.violated_rules) &&
  sameRuleIds(left.unknown_rules, right.unknown_rules);

export const transactionDetailMatchesSnapshot = (
  snapshot: MempoolSnapshot,
  transaction: MempoolTransaction,
  detail: TransactionDetailResponse,
): boolean =>
  detail.source_id === snapshot.source_id &&
  detail.snapshot_observed_at_ms === snapshot.observed_at_ms &&
  detail.txid === transaction.txid &&
  detail.wtxid === transaction.wtxid &&
  (detail.classification_revision === snapshot.classification_revision ||
    (detail.classification_revision > snapshot.classification_revision &&
      sameAssessment(detail.assessment, transaction.bip110)));

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

const isTxid = (value: unknown): value is string =>
  typeof value === "string" && /^[0-9a-f]{64}$/.test(value);

const parseRuleId = (value: unknown): RuleId => {
  if (
    typeof value === "string" &&
    (RULE_IDS as readonly string[]).includes(value)
  ) {
    return value as RuleId;
  }
  throw new TypeError("Invalid BIP-110 rule identifier");
};

const parseRuleIds = (value: unknown, field: string): RuleId[] => {
  if (!Array.isArray(value)) {
    throw new TypeError(`Invalid ${field}`);
  }
  const rules = value.map((rule) => parseRuleId(rule));
  if (new Set(rules).size !== rules.length) {
    throw new TypeError(`${field} contains duplicate rules`);
  }
  return rules;
};

const parseBip110Status = (value: unknown): Bip110Status => {
  if (
    value === "compatible" ||
    value === "violating" ||
    value === "indeterminate"
  ) {
    return value;
  }
  throw new TypeError("Invalid BIP-110 status");
};

export const parseBip110Assessment = (
  value: unknown,
): Bip110Assessment | null => {
  if (value === null) {
    return null;
  }
  if (
    !isRecord(value) ||
    !hasOnlyKeys(value, [
      "status",
      "primary_rule",
      "violated_rules",
      "unknown_rules",
    ])
  ) {
    throw new TypeError("Invalid BIP-110 assessment");
  }

  const status = parseBip110Status(value.status);
  const primaryRule =
    value.primary_rule === null ? null : parseRuleId(value.primary_rule);
  const violatedRules = parseRuleIds(value.violated_rules, "violated rules");
  const unknownRules = parseRuleIds(value.unknown_rules, "unknown rules");
  if (
    (status === "compatible" &&
      (primaryRule !== null ||
        violatedRules.length !== 0 ||
        unknownRules.length !== 0)) ||
    (status === "violating" &&
      (violatedRules.length === 0 ||
        (primaryRule !== null && !violatedRules.includes(primaryRule)))) ||
    (status === "indeterminate" &&
      (primaryRule !== null ||
        violatedRules.length !== 0 ||
        unknownRules.length === 0))
  ) {
    throw new TypeError("Inconsistent BIP-110 assessment");
  }

  return value as unknown as Bip110Assessment;
};

const parseBip110Summary = (value: unknown): Bip110Summary => {
  if (
    !isRecord(value) ||
    !hasOnlyKeys(value, [
      "evaluator_id",
      "evaluator_version",
      "scope",
      "compatible_count",
      "violating_count",
      "indeterminate_count",
      "unclassified_count",
    ]) ||
    typeof value.evaluator_id !== "string" ||
    value.evaluator_id.trim().length === 0 ||
    typeof value.evaluator_version !== "string" ||
    value.evaluator_version.trim().length === 0 ||
    value.scope !== "knots_mempool_policy" ||
    !isNonNegativeInteger(value.compatible_count) ||
    !isNonNegativeInteger(value.violating_count) ||
    !isNonNegativeInteger(value.indeterminate_count) ||
    !isNonNegativeInteger(value.unclassified_count)
  ) {
    throw new TypeError("Invalid BIP-110 summary");
  }
  return value as unknown as Bip110Summary;
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
  const lastPollStartedAtMs = parseNullableInteger(
    value.last_poll_started_at_ms,
    "last poll time",
  );
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

const parseTransaction = (
  value: unknown,
  index: number,
): MempoolTransaction => {
  if (
    !isRecord(value) ||
    !hasOnlyKeys(value, [
      "txid",
      "wtxid",
      "vsize",
      "fee_sats",
      "entered_at_ms",
      "bip110",
    ]) ||
    !isTxid(value.txid) ||
    !isTxid(value.wtxid) ||
    !isNonNegativeInteger(value.vsize) ||
    value.vsize === 0 ||
    !isNonNegativeInteger(value.fee_sats) ||
    !isNonNegativeInteger(value.entered_at_ms)
  ) {
    throw new TypeError(`Invalid transaction at index ${index}`);
  }
  parseBip110Assessment(value.bip110);
  return value as unknown as MempoolTransaction;
};

export const parseMempoolSnapshot = (value: unknown): MempoolSnapshot => {
  if (
    !isRecord(value) ||
    !hasOnlyKeys(value, [
      "source_id",
      "source_label",
      "collection_started_at_ms",
      "collection_completed_at_ms",
      "collection_duration_ms",
      "observed_at_ms",
      "classification_revision",
      "chain_tip",
      "transaction_count",
      "total_vsize",
      "bip110_summary",
      "transactions",
    ]) ||
    !isSourceId(value.source_id) ||
    typeof value.source_label !== "string" ||
    value.source_label.trim().length === 0 ||
    !isNonNegativeInteger(value.collection_started_at_ms) ||
    !isNonNegativeInteger(value.collection_completed_at_ms) ||
    !isNonNegativeInteger(value.collection_duration_ms) ||
    !isNonNegativeInteger(value.observed_at_ms) ||
    !isNonNegativeInteger(value.classification_revision) ||
    !isNonNegativeInteger(value.transaction_count) ||
    !isNonNegativeInteger(value.total_vsize) ||
    !Array.isArray(value.transactions)
  ) {
    throw new TypeError("Invalid mempool snapshot");
  }

  if (
    value.collection_completed_at_ms < value.collection_started_at_ms ||
    value.collection_duration_ms !==
      value.collection_completed_at_ms - value.collection_started_at_ms ||
    value.observed_at_ms !== value.collection_completed_at_ms
  ) {
    throw new TypeError("Invalid mempool snapshot collection timing");
  }

  parseChainTip(value.chain_tip);
  const bip110Summary = parseBip110Summary(value.bip110_summary);
  let totalVsize = 0;
  let previousTxid: string | null = null;
  const statusCounts = {
    compatible: 0,
    violating: 0,
    indeterminate: 0,
    unclassified: 0,
  };
  for (const [index, item] of value.transactions.entries()) {
    const transaction = parseTransaction(item, index);
    if (previousTxid !== null && transaction.txid <= previousTxid) {
      throw new TypeError("Transactions are not strictly ordered by txid");
    }
    previousTxid = transaction.txid;
    totalVsize += transaction.vsize;
    if (!Number.isSafeInteger(totalVsize)) {
      throw new TypeError("Snapshot total vsize is not safely representable");
    }
    if (transaction.bip110 === null) {
      statusCounts.unclassified += 1;
    } else {
      statusCounts[transaction.bip110.status] += 1;
    }
  }
  if (value.transaction_count !== value.transactions.length) {
    throw new TypeError("Snapshot transaction count does not match payload");
  }
  if (value.total_vsize !== totalVsize) {
    throw new TypeError("Snapshot total vsize does not match payload");
  }
  if (
    bip110Summary.compatible_count !== statusCounts.compatible ||
    bip110Summary.violating_count !== statusCounts.violating ||
    bip110Summary.indeterminate_count !== statusCounts.indeterminate ||
    bip110Summary.unclassified_count !== statusCounts.unclassified
  ) {
    throw new TypeError("BIP-110 summary does not match payload");
  }

  return value as unknown as MempoolSnapshot;
};

const parseRuleVerdict = (value: unknown): RuleVerdict => {
  if (value === "pass" || value === "violate" || value === "unknown") {
    return value;
  }
  throw new TypeError("Invalid rule verdict");
};

const parseRuleAssessment = (value: unknown, index: number): RuleAssessment => {
  if (
    !isRecord(value) ||
    !hasOnlyKeys(value, [
      "rule",
      "number",
      "verdict",
      "evidence_count",
      "evidence",
      "missing_count",
      "missing",
    ]) ||
    value.rule !== RULE_IDS[index] ||
    value.number !== index + 1 ||
    !isNonNegativeInteger(value.evidence_count) ||
    !Array.isArray(value.evidence) ||
    !isNonNegativeInteger(value.missing_count) ||
    !Array.isArray(value.missing) ||
    value.evidence.length > 1 ||
    value.missing.length > 1 ||
    (value.evidence_count === 0) !== (value.evidence.length === 0) ||
    (value.missing_count === 0) !== (value.missing.length === 0)
  ) {
    throw new TypeError(`Invalid rule assessment at index ${index}`);
  }
  const verdict = parseRuleVerdict(value.verdict);
  if (
    (verdict === "pass" &&
      (value.evidence_count !== 0 || value.missing_count !== 0)) ||
    (verdict === "violate" && value.evidence_count === 0) ||
    (verdict === "unknown" &&
      (value.evidence_count !== 0 || value.missing_count === 0))
  ) {
    throw new TypeError(`Inconsistent rule verdict at index ${index}`);
  }
  return value as unknown as RuleAssessment;
};

export const parseTransactionDetailResponse = (
  value: unknown,
): TransactionDetailResponse => {
  if (
    !isRecord(value) ||
    !hasOnlyKeys(value, [
      "source_id",
      "snapshot_observed_at_ms",
      "classification_revision",
      "txid",
      "wtxid",
      "assessment",
      "rules",
    ]) ||
    !isSourceId(value.source_id) ||
    !isNonNegativeInteger(value.snapshot_observed_at_ms) ||
    !isNonNegativeInteger(value.classification_revision) ||
    !isTxid(value.txid) ||
    !isTxid(value.wtxid) ||
    !Array.isArray(value.rules) ||
    value.rules.length !== RULE_IDS.length
  ) {
    throw new TypeError("Invalid transaction detail response");
  }

  const assessment = parseBip110Assessment(value.assessment);
  if (assessment === null) {
    throw new TypeError(
      "Classified transaction detail is missing its assessment",
    );
  }
  const rules = value.rules.map((rule, index) =>
    parseRuleAssessment(rule, index),
  );
  const violatedRules = rules
    .filter((rule) => rule.verdict === "violate")
    .map((rule) => rule.rule);
  const unknownRules = rules
    .filter((rule) => rule.missing_count > 0)
    .map((rule) => rule.rule);
  if (
    assessment.violated_rules.length !== violatedRules.length ||
    assessment.unknown_rules.length !== unknownRules.length ||
    assessment.violated_rules.some((rule) => !violatedRules.includes(rule)) ||
    assessment.unknown_rules.some((rule) => !unknownRules.includes(rule))
  ) {
    throw new TypeError("Transaction detail rules do not match assessment");
  }

  return value as unknown as TransactionDetailResponse;
};

export const parseSourcesResponse = (value: unknown): SourcesResponse => {
  if (
    !isRecord(value) ||
    !hasOnlyKeys(value, ["sources"]) ||
    !Array.isArray(value.sources)
  ) {
    throw new TypeError("Invalid sources response");
  }
  for (const source of value.sources) {
    parseSourceSummary(source);
  }
  return value as unknown as SourcesResponse;
};

export const parseSourceSnapshotResponse = (
  value: unknown,
): SourceSnapshotResponse => {
  if (!isRecord(value) || !hasOnlyKeys(value, ["source", "snapshot"])) {
    throw new TypeError("Invalid source snapshot response");
  }
  const source = parseSourceSummary(value.source);
  if (value.snapshot === null) {
    if (source.snapshot_observed_at_ms !== null) {
      throw new TypeError("Source summary references a missing snapshot");
    }
    return value as unknown as SourceSnapshotResponse;
  }

  const snapshot = parseMempoolSnapshot(value.snapshot);
  if (
    snapshot.source_id !== source.source_id ||
    snapshot.source_label !== source.source_label ||
    snapshot.observed_at_ms !== source.snapshot_observed_at_ms ||
    snapshot.transaction_count !== source.transaction_count ||
    snapshot.total_vsize !== source.total_vsize ||
    snapshot.chain_tip.height !== source.chain_tip?.height ||
    snapshot.chain_tip.hash !== source.chain_tip.hash
  ) {
    throw new TypeError("Source summary does not match its snapshot");
  }
  return value as unknown as SourceSnapshotResponse;
};

const fetchJson = async (
  path: string,
  signal?: AbortSignal,
): Promise<unknown> => {
  const options: RequestInit = {
    headers: { Accept: "application/json" },
  };
  if (signal !== undefined) {
    options.signal = signal;
  }
  const response = await fetch(path, options);
  if (!response.ok) {
    let detail = "";
    try {
      const body: unknown = await response.json();
      if (isRecord(body) && typeof body.error === "string") {
        detail = `: ${body.error}`;
      }
    } catch {
      // The HTTP status remains useful when the body is not JSON.
    }
    throw new AtlasRequestError(response.status, detail);
  }
  return response.json();
};

export const fetchSources = async (
  signal?: AbortSignal,
): Promise<SourcesResponse> =>
  parseSourcesResponse(await fetchJson("/api/v1/sources", signal));

export const fetchSourceSnapshot = async (
  sourceId: string,
  signal?: AbortSignal,
): Promise<SourceSnapshotResponse> =>
  parseSourceSnapshotResponse(
    await fetchJson(
      `/api/v1/sources/${encodeURIComponent(sourceId)}/mempool`,
      signal,
    ),
  );

export const fetchTransactionDetail = async (
  sourceId: string,
  txid: string,
  signal?: AbortSignal,
): Promise<TransactionDetailResponse> =>
  parseTransactionDetailResponse(
    await fetchJson(
      `/api/v1/sources/${encodeURIComponent(sourceId)}/transactions/${encodeURIComponent(txid)}`,
      signal,
    ),
  );
