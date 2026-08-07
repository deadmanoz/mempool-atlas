import type {
  Bip110Assessment,
  Bip110Status,
  ChainTip,
  ClassificationResult,
  ClassificationProgress,
  MempoolSnapshot,
  MempoolTransaction,
  RuleAssessment,
  RuleId,
  RuleVerdict,
  SourceAvailability,
  LoadedSourcePublication,
  SourceSummary,
  SourcesResponse,
  TransactionDetailResponse,
} from "./types";
import { RULE_IDS } from "./types";
import type {
  AtlasWorkerRequest,
  AtlasWorkerResponse,
} from "./atlas-worker-protocol";
import {
  createLoadedSourcePublication,
  createPrimarySourcePublication,
} from "./packed-store";

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

const sameCompactClassifications = (
  left: ClassificationResult[],
  right: ClassificationResult[],
): boolean =>
  left.length === right.length &&
  left.every((result, index) => {
    const candidate = right[index];
    return (
      candidate !== undefined &&
      result.classifier_id === candidate.classifier_id &&
      result.state === candidate.state &&
      result.primary_label === candidate.primary_label &&
      result.labels.length === candidate.labels.length &&
      result.labels.every(
        (label, labelIndex) => label === candidate.labels[labelIndex],
      ) &&
      result.missing_facts.length === candidate.missing_facts.length &&
      result.missing_facts.every(
        (fact, factIndex) => fact === candidate.missing_facts[factIndex],
      )
    );
  });

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
      sameAssessment(detail.assessment, transaction.bip110) &&
      sameCompactClassifications(
        detail.classifications,
        transaction.classifications,
      )));

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

const parseClassificationProgress = (
  value: unknown,
): ClassificationProgress | null => {
  if (value === null) {
    return null;
  }
  if (
    !isRecord(value) ||
    !hasOnlyKeys(value, [
      "state",
      "revision",
      "classified_count",
      "unclassified_count",
    ]) ||
    (value.state !== "classifying" &&
      value.state !== "complete" &&
      value.state !== "paused") ||
    !isNonNegativeInteger(value.revision) ||
    !isNonNegativeInteger(value.classified_count) ||
    !isNonNegativeInteger(value.unclassified_count)
  ) {
    throw new TypeError("Invalid classification progress");
  }
  return value as unknown as ClassificationProgress;
};

const isTxid = (value: unknown): value is string =>
  typeof value === "string" && /^[0-9a-f]{64}$/.test(value);

const isClassifierKey = (value: unknown): value is string =>
  typeof value === "string" && /^[a-z][a-z0-9_]*$/.test(value);

const parseStringKeys = (value: unknown, field: string): string[] => {
  if (
    !Array.isArray(value) ||
    !value.every((item) => isClassifierKey(item)) ||
    new Set(value).size !== value.length
  ) {
    throw new TypeError(`Invalid ${field}`);
  }
  return value as string[];
};

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
      "classification",
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
  parseNullableInteger(value.last_poll_started_at_ms, "last poll time");
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
  const classification = parseClassificationProgress(value.classification);

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
  if (hasSnapshot) {
    if (
      classification === null ||
      classification.classified_count + classification.unclassified_count !==
        transactionCount
    ) {
      throw new TypeError(
        "Source summary classification does not match its snapshot metadata",
      );
    }
  } else if (classification !== null) {
    throw new TypeError(
      "Source without a snapshot unexpectedly contains classification progress",
    );
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

const parseRuleVerdict = (value: unknown): RuleVerdict => {
  if (value === "pass" || value === "violate" || value === "unknown") {
    return value;
  }
  throw new TypeError("Invalid rule verdict");
};

const parseDetailedClassificationResults = (
  value: unknown,
): ClassificationResult[] => {
  if (!Array.isArray(value) || value.length === 0) {
    throw new TypeError("Invalid detailed transaction classifications");
  }
  const classifierIds = new Set<string>();
  return value.map((result, index) => {
    if (
      !isRecord(result) ||
      !hasOnlyKeys(result, [
        "classifier_id",
        "state",
        "primary_label",
        "labels",
        "missing_facts",
        "evidence",
      ]) ||
      !isClassifierKey(result.classifier_id) ||
      classifierIds.has(result.classifier_id) ||
      (result.state !== "complete" && result.state !== "partial") ||
      (result.primary_label !== null &&
        !isClassifierKey(result.primary_label)) ||
      result.evidence === null
    ) {
      throw new TypeError(`Invalid detailed classification at index ${index}`);
    }
    classifierIds.add(result.classifier_id);
    const labels = parseStringKeys(
      result.labels,
      `detailed classification ${index} labels`,
    );
    const missingFacts = parseStringKeys(
      result.missing_facts,
      `detailed classification ${index} missing facts`,
    );
    if (
      (labels.length === 0 && result.state !== "partial") ||
      (result.primary_label !== null &&
        !labels.includes(result.primary_label)) ||
      (result.state === "complete" && missingFacts.length !== 0) ||
      (result.state === "partial" && missingFacts.length === 0)
    ) {
      throw new TypeError(
        `Inconsistent detailed classification at index ${index}`,
      );
    }
    return result as unknown as ClassificationResult;
  });
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
      "classifications",
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
  const classifications = parseDetailedClassificationResults(
    value.classifications,
  );
  const policy = classifications.find(
    ({ classifier_id }) => classifier_id === "knots_bip110",
  );
  if (
    policy === undefined ||
    policy.primary_label !== assessment.status ||
    (assessment.unknown_rules.length === 0
      ? policy.state !== "complete" || policy.missing_facts.length !== 0
      : policy.state !== "partial" ||
        !policy.missing_facts.includes("policy_facts"))
  ) {
    throw new TypeError(
      "Transaction detail classifiers do not match policy assessment",
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
    !hasOnlyKeys(value, ["atlas_version", "sources"]) ||
    typeof value.atlas_version !== "string" ||
    !/^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)(?:-[0-9A-Za-z.-]+)?(?:\+[0-9A-Za-z.-]+)?$/.test(
      value.atlas_version,
    ) ||
    !Array.isArray(value.sources)
  ) {
    throw new TypeError("Invalid sources response");
  }
  const sourceIds = new Set<string>();
  for (const source of value.sources) {
    const { source_id } = parseSourceSummary(source);
    if (sourceIds.has(source_id)) {
      throw new TypeError("Sources response repeats a source ID");
    }
    sourceIds.add(source_id);
  }
  return value as unknown as SourcesResponse;
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
  parseSourcesResponse(await fetchJson("/api/v2/sources", signal));

interface PendingPublication {
  resolve: (response: LoadedSourcePublication) => void;
  reject: (error: unknown) => void;
  sourceId: string;
  onPrimary:
    | ((publication: LoadedSourcePublication) => void | Promise<void>)
    | undefined;
  primaryReady: Promise<void>;
  primaryDelivered: boolean;
  removeAbortListener: () => void;
}

let publicationWorker: Worker | null = null;
let publicationSequence = 0;
const pendingPublications = new Map<number, PendingPublication>();

const errorValue = (error: unknown, fallback: string): Error =>
  error instanceof Error ? error : new Error(fallback);

const rejectPendingAfterPrimary = (
  requestId: number,
  pending: PendingPublication,
  error: Error,
): void => {
  if (pendingPublications.get(requestId) !== pending) return;
  pendingPublications.delete(requestId);
  pending.removeAbortListener();
  void pending.primaryReady.then(
    () => pending.reject(error),
    (primaryError) =>
      pending.reject(errorValue(primaryError, "Primary publication failed")),
  );
};

const failPublicationWorker = (failed: Worker, error: Error): void => {
  if (publicationWorker !== failed) return;
  publicationWorker = null;
  failed.terminate();
  for (const [requestId, pending] of pendingPublications) {
    rejectPendingAfterPrimary(requestId, pending, error);
  }
};

const cancelPendingPublication = (
  requestId: number,
  pending: PendingPublication,
  error: unknown,
): void => {
  if (pendingPublications.get(requestId) !== pending) return;
  pendingPublications.delete(requestId);
  pending.removeAbortListener();
  const activeWorker = publicationWorker;
  if (activeWorker !== null) {
    try {
      activeWorker.postMessage({
        type: "cancel",
        requestId,
      } satisfies AtlasWorkerRequest);
    } catch (cancelError) {
      failPublicationWorker(
        activeWorker,
        errorValue(cancelError, "Atlas v2 worker cancellation failed"),
      );
    }
  }
  pending.reject(error);
};

const worker = (): Worker => {
  if (publicationWorker !== null) return publicationWorker;
  const next = new Worker(new URL("./atlas-worker.ts", import.meta.url), {
    type: "module",
    name: "mempool-atlas-v2",
  });
  next.addEventListener(
    "message",
    (event: MessageEvent<AtlasWorkerResponse>): void => {
      const response = event.data;
      const pending = pendingPublications.get(response.requestId);
      if (pending === undefined) return;
      if (response.type === "primary") {
        if (pending.primaryDelivered) return;
        pending.primaryDelivered = true;
        performance.mark(`atlas:${pending.sourceId}:primary-worker-timing`, {
          detail: response.timing,
        });
        performance.mark(`atlas:${pending.sourceId}:primary-decoded`);
        pending.primaryReady = Promise.resolve().then(() =>
          pending.onPrimary?.(
            createPrimarySourcePublication(response.publication),
          ),
        );
        void pending.primaryReady.catch((error) => {
          cancelPendingPublication(response.requestId, pending, error);
        });
        return;
      }
      if (response.type === "error") {
        rejectPendingAfterPrimary(
          response.requestId,
          pending,
          response.status === null
            ? new TypeError(response.message)
            : new AtlasRequestError(response.status, `: ${response.message}`),
        );
        return;
      }
      performance.mark(`atlas:${pending.sourceId}:complete-worker-timing`, {
        detail: response.timing,
      });
      performance.mark(`atlas:${pending.sourceId}:complete-decoded`);
      void pending.primaryReady.then(
        () => {
          if (pendingPublications.get(response.requestId) !== pending) return;
          let complete: LoadedSourcePublication;
          try {
            complete = createLoadedSourcePublication(response.publication);
          } catch (error) {
            failPublicationWorker(
              next,
              errorValue(error, "Atlas v2 publication materialization failed"),
            );
            return;
          }
          pendingPublications.delete(response.requestId);
          pending.removeAbortListener();
          pending.resolve(complete);
        },
        () => undefined,
      );
    },
  );
  next.addEventListener("error", (event): void => {
    event.preventDefault();
    failPublicationWorker(
      next,
      new Error(event.message || "Atlas v2 worker failed"),
    );
  });
  next.addEventListener("messageerror", (): void => {
    failPublicationWorker(
      next,
      new Error("Atlas v2 worker returned an unreadable message"),
    );
  });
  publicationWorker = next;
  return next;
};

export const fetchSourcePublication = async (
  sourceId: string,
  signal?: AbortSignal,
  selectedClassifierId = "transaction_properties",
  onPrimary?: (publication: LoadedSourcePublication) => void | Promise<void>,
): Promise<LoadedSourcePublication> => {
  if (!isSourceId(sourceId)) throw new TypeError("Invalid source ID");
  if (!isClassifierKey(selectedClassifierId)) {
    throw new TypeError("Invalid selected classifier ID");
  }
  if (signal?.aborted === true) throw new DOMException("Aborted", "AbortError");
  publicationSequence += 1;
  const requestId = publicationSequence;
  return new Promise<LoadedSourcePublication>((resolve, reject) => {
    const abort = (): void => {
      const pending = pendingPublications.get(requestId);
      if (pending === undefined) return;
      cancelPendingPublication(
        requestId,
        pending,
        new DOMException("Aborted", "AbortError"),
      );
    };
    signal?.addEventListener("abort", abort, { once: true });
    pendingPublications.set(requestId, {
      resolve,
      reject,
      sourceId,
      onPrimary,
      primaryReady: Promise.resolve(),
      primaryDelivered: false,
      removeAbortListener: () => signal?.removeEventListener("abort", abort),
    });
    const activeWorker = worker();
    try {
      activeWorker.postMessage({
        type: "load",
        requestId,
        sourceId,
        selectedClassifierId,
      } satisfies AtlasWorkerRequest);
    } catch (error) {
      failPublicationWorker(
        activeWorker,
        errorValue(error, "Atlas v2 worker request failed"),
      );
    }
  });
};

export const fetchTransactionDetail = async (
  sourceId: string,
  txid: string,
  signal?: AbortSignal,
): Promise<TransactionDetailResponse> =>
  parseTransactionDetailResponse(
    await fetchJson(
      `/api/v2/sources/${encodeURIComponent(sourceId)}/transactions/${encodeURIComponent(txid)}`,
      signal,
    ),
  );
