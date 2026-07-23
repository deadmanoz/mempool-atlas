import type {
  ChainTip,
  MempoolSnapshot,
  MempoolTransaction,
  SourceAvailability,
  SourceSnapshotResponse,
  SourceSummary,
  SourcesResponse,
} from "./types";

const isRecord = (value: unknown): value is Record<string, unknown> =>
  typeof value === "object" && value !== null;

const isNonNegativeInteger = (value: unknown): value is number =>
  typeof value === "number" && Number.isSafeInteger(value) && value >= 0;

const isSourceId = (value: unknown): value is string =>
  typeof value === "string" &&
  value !== "." &&
  value !== ".." &&
  /^[A-Za-z0-9._-]{1,64}$/.test(value);

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
    !hasOnlyKeys(value, ["txid", "vsize", "fee_sats", "entered_at_ms"]) ||
    typeof value.txid !== "string" ||
    !/^[0-9a-f]{64}$/.test(value.txid) ||
    !isNonNegativeInteger(value.vsize) ||
    value.vsize === 0 ||
    !isNonNegativeInteger(value.fee_sats) ||
    !isNonNegativeInteger(value.entered_at_ms)
  ) {
    throw new TypeError(`Invalid transaction at index ${index}`);
  }
  return value as unknown as MempoolTransaction;
};

export const parseMempoolSnapshot = (value: unknown): MempoolSnapshot => {
  if (
    !isRecord(value) ||
    !hasOnlyKeys(value, [
      "source_id",
      "source_label",
      "observed_at_ms",
      "chain_tip",
      "transaction_count",
      "total_vsize",
      "transactions",
    ]) ||
    !isSourceId(value.source_id) ||
    typeof value.source_label !== "string" ||
    value.source_label.trim().length === 0 ||
    !isNonNegativeInteger(value.observed_at_ms) ||
    !isNonNegativeInteger(value.transaction_count) ||
    !isNonNegativeInteger(value.total_vsize) ||
    !Array.isArray(value.transactions)
  ) {
    throw new TypeError("Invalid mempool snapshot");
  }

  parseChainTip(value.chain_tip);
  let totalVsize = 0;
  let previousTxid: string | null = null;
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
  }
  if (value.transaction_count !== value.transactions.length) {
    throw new TypeError("Snapshot transaction count does not match payload");
  }
  if (value.total_vsize !== totalVsize) {
    throw new TypeError("Snapshot total vsize does not match payload");
  }

  return value as unknown as MempoolSnapshot;
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

const fetchJson = async (path: string): Promise<unknown> => {
  const response = await fetch(path, {
    headers: { Accept: "application/json" },
  });
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
    throw new Error(`Atlas request failed (${response.status})${detail}`);
  }
  return response.json();
};

export const fetchSources = async (): Promise<SourcesResponse> =>
  parseSourcesResponse(await fetchJson("/api/v1/sources"));

export const fetchSourceSnapshot = async (
  sourceId: string,
): Promise<SourceSnapshotResponse> =>
  parseSourceSnapshotResponse(
    await fetchJson(`/api/v1/sources/${encodeURIComponent(sourceId)}/mempool`),
  );
