import type {
  Bip110Assessment,
  Bip110Status,
  Bip110Summary,
  ChainTip,
  ClassificationResult,
  ClassificationProgress,
  ClassifierDescriptor,
  ClassifierMethodology,
  ClassifierSemantics,
  ClassifierSummary,
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
  TransactionStructure,
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

/**
 * Signed counterpart for delta-adjusted amounts. `prioritisetransaction` can
 * drive an ancestor fee below zero, so those fields admit negative integers.
 */
const isSignedInteger = (value: unknown): value is number =>
  typeof value === "number" && Number.isSafeInteger(value);

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

const parseClassifierMethodology = (value: unknown): ClassifierMethodology => {
  if (
    value === "exact" ||
    value === "heuristic" ||
    value === "fingerprint" ||
    value === "policy"
  ) {
    return value;
  }
  throw new TypeError("Invalid classifier methodology");
};

const parseClassifierSemantics = (value: unknown): ClassifierSemantics => {
  if (value === "multi_label" || value === "rule_set") {
    return value;
  }
  throw new TypeError("Invalid classifier semantics");
};

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

const parseClassifierCatalog = (value: unknown): ClassifierDescriptor[] => {
  if (!Array.isArray(value) || value.length === 0) {
    throw new TypeError("Invalid classifier catalog");
  }
  const ids = new Set<string>();
  const catalog = value.map((item, classifierIndex) => {
    if (
      !isRecord(item) ||
      !hasOnlyKeys(item, [
        "id",
        "version",
        "title",
        "methodology",
        "semantics",
        "required_facts",
        "labels",
      ]) ||
      !isClassifierKey(item.id) ||
      ids.has(item.id) ||
      typeof item.version !== "string" ||
      item.version.trim().length === 0 ||
      typeof item.title !== "string" ||
      item.title.trim().length === 0 ||
      !Array.isArray(item.labels) ||
      item.labels.length === 0
    ) {
      throw new TypeError(
        `Invalid classifier descriptor at index ${classifierIndex}`,
      );
    }
    ids.add(item.id);
    parseClassifierMethodology(item.methodology);
    parseClassifierSemantics(item.semantics);
    parseStringKeys(item.required_facts, "classifier required facts");
    const labelKeys = new Set<string>();
    for (const [labelIndex, label] of item.labels.entries()) {
      if (
        !isRecord(label) ||
        !hasOnlyKeys(label, ["key", "label", "description"]) ||
        !isClassifierKey(label.key) ||
        labelKeys.has(label.key) ||
        typeof label.label !== "string" ||
        label.label.trim().length === 0 ||
        typeof label.description !== "string" ||
        label.description.trim().length === 0
      ) {
        throw new TypeError(
          `Invalid classifier label at ${classifierIndex}:${labelIndex}`,
        );
      }
      labelKeys.add(label.key);
    }
    return item as unknown as ClassifierDescriptor;
  });
  return catalog;
};

const parseClassificationResult = (
  value: unknown,
  descriptor: ClassifierDescriptor,
  detailed: boolean,
): ClassificationResult => {
  if (
    !isRecord(value) ||
    !hasOnlyKeys(value, [
      "classifier_id",
      "state",
      "primary_label",
      "labels",
      "missing_facts",
      "evidence",
    ]) ||
    value.classifier_id !== descriptor.id ||
    (value.state !== "complete" && value.state !== "partial") ||
    (value.primary_label !== null && !isClassifierKey(value.primary_label)) ||
    (detailed ? value.evidence === null : value.evidence !== null)
  ) {
    throw new TypeError(`Invalid ${descriptor.id} classification result`);
  }
  const labels = parseStringKeys(value.labels, `${descriptor.id} labels`);
  const missingFacts = parseStringKeys(
    value.missing_facts,
    `${descriptor.id} missing facts`,
  );
  const declaredLabels = new Set(descriptor.labels.map(({ key }) => key));
  // A partial result may carry no labels at all: nothing was proven while a
  // rule remained unresolved. A complete result always names a label.
  if (
    (labels.length === 0 && value.state !== "partial") ||
    labels.some((label) => !declaredLabels.has(label)) ||
    (value.primary_label !== null && !labels.includes(value.primary_label)) ||
    (value.state === "complete" && missingFacts.length !== 0) ||
    (value.state === "partial" && missingFacts.length === 0)
  ) {
    throw new TypeError(`Inconsistent ${descriptor.id} classification result`);
  }
  return value as unknown as ClassificationResult;
};

const parseClassificationResults = (
  value: unknown,
  catalog: ClassifierDescriptor[],
  detailed: boolean,
): ClassificationResult[] => {
  if (!Array.isArray(value) || value.length !== catalog.length) {
    throw new TypeError("Invalid transaction classifications");
  }
  return value.map((result, index) =>
    parseClassificationResult(result, catalog[index]!, detailed),
  );
};

const parseClassifierSummary = (
  value: unknown,
  descriptor: ClassifierDescriptor,
  transactionCount: number,
): ClassifierSummary => {
  if (
    !isRecord(value) ||
    !hasOnlyKeys(value, [
      "classifier_id",
      "complete_count",
      "partial_count",
      "unclassified_count",
      "label_counts",
    ]) ||
    value.classifier_id !== descriptor.id ||
    !isNonNegativeInteger(value.complete_count) ||
    !isNonNegativeInteger(value.partial_count) ||
    !isNonNegativeInteger(value.unclassified_count) ||
    value.complete_count + value.partial_count + value.unclassified_count !==
      transactionCount ||
    !isRecord(value.label_counts)
  ) {
    throw new TypeError(`Invalid ${descriptor.id} classifier summary`);
  }
  const labelCounts = value.label_counts;
  const expectedLabels = descriptor.labels.map(({ key }) => key);
  if (
    !hasOnlyKeys(labelCounts, expectedLabels) ||
    !expectedLabels.every((key) => isNonNegativeInteger(labelCounts[key]))
  ) {
    throw new TypeError(`Invalid ${descriptor.id} label counts`);
  }
  return value as unknown as ClassifierSummary;
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

const parseTransactionStructure = (
  value: unknown,
  index: number,
): TransactionStructure | null => {
  if (value === null) {
    return null;
  }
  if (
    !isRecord(value) ||
    !hasOnlyKeys(value, [
      "input_count",
      "output_count",
      "op_return_bytes",
      "output_sats",
      "witness_bytes",
    ]) ||
    !isNonNegativeInteger(value.input_count) ||
    value.input_count === 0 ||
    !isNonNegativeInteger(value.output_count) ||
    value.output_count === 0 ||
    !isNonNegativeInteger(value.op_return_bytes) ||
    !isNonNegativeInteger(value.output_sats) ||
    !isNonNegativeInteger(value.witness_bytes)
  ) {
    throw new TypeError(`Invalid transaction structure at index ${index}`);
  }
  return value as unknown as TransactionStructure;
};

const parseTransaction = (
  value: unknown,
  index: number,
  catalog: ClassifierDescriptor[],
): MempoolTransaction => {
  if (
    !isRecord(value) ||
    !hasOnlyKeys(value, [
      "txid",
      "wtxid",
      "vsize",
      "weight",
      "fee_sats",
      "entered_at_ms",
      "ancestor_count",
      "ancestor_vsize",
      "ancestor_fee_sats",
      "descendant_count",
      "descendant_vsize",
      "replaceable",
      "structure",
      "classifications",
      "bip110",
    ]) ||
    !isTxid(value.txid) ||
    !isTxid(value.wtxid) ||
    !isNonNegativeInteger(value.vsize) ||
    value.vsize === 0 ||
    !isNonNegativeInteger(value.weight) ||
    !isNonNegativeInteger(value.fee_sats) ||
    !isNonNegativeInteger(value.entered_at_ms) ||
    !isNonNegativeInteger(value.ancestor_count) ||
    !isNonNegativeInteger(value.ancestor_vsize) ||
    !isSignedInteger(value.ancestor_fee_sats) ||
    !isNonNegativeInteger(value.descendant_count) ||
    !isNonNegativeInteger(value.descendant_vsize) ||
    typeof value.replaceable !== "boolean"
  ) {
    throw new TypeError(`Invalid transaction at index ${index}`);
  }
  if (value.weight === 0 || value.weight > value.vsize * 4) {
    throw new TypeError(
      `Transaction at index ${index} reports an inconsistent weight`,
    );
  }
  if (
    value.ancestor_count === 0 ||
    value.descendant_count === 0 ||
    value.ancestor_vsize < value.vsize ||
    value.descendant_vsize < value.vsize
  ) {
    throw new TypeError(
      `Transaction at index ${index} reports inconsistent ancestry`,
    );
  }
  const structure = parseTransactionStructure(value.structure, index);
  const assessment = parseBip110Assessment(value.bip110);
  if ((structure === null) !== (assessment === null)) {
    throw new TypeError(
      `Transaction at index ${index} couples structure and assessment inconsistently`,
    );
  }
  const classifications =
    assessment === null
      ? (() => {
          if (
            !Array.isArray(value.classifications) ||
            value.classifications.length !== 0
          ) {
            throw new TypeError(
              `Unclassified transaction at index ${index} contains classifier results`,
            );
          }
          return [];
        })()
      : parseClassificationResults(value.classifications, catalog, false);
  const policy = classifications.find(
    ({ classifier_id }) => classifier_id === "knots_bip110",
  );
  if (
    assessment !== null &&
    (policy === undefined ||
      policy.primary_label !== assessment.status ||
      (assessment.unknown_rules.length === 0
        ? policy.state !== "complete" || policy.missing_facts.length !== 0
        : policy.state !== "partial" ||
          !policy.missing_facts.includes("policy_facts")))
  ) {
    throw new TypeError(
      `Transaction at index ${index} has inconsistent policy projections`,
    );
  }
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
      "classifier_catalog",
      "classification_summaries",
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
  const transactionCount = value.transaction_count;
  const classifierCatalog = parseClassifierCatalog(value.classifier_catalog);
  const bip110Summary = parseBip110Summary(value.bip110_summary);
  let totalVsize = 0;
  let previousTxid: string | null = null;
  const statusCounts = {
    compatible: 0,
    violating: 0,
    indeterminate: 0,
    unclassified: 0,
  };
  const resultCounts = classifierCatalog.map((descriptor) => ({
    classifierId: descriptor.id,
    complete: 0,
    partial: 0,
    unclassified: 0,
    labels: Object.fromEntries(
      descriptor.labels.map(({ key }) => [key, 0]),
    ) as Record<string, number>,
  }));
  for (const [index, item] of value.transactions.entries()) {
    const transaction = parseTransaction(item, index, classifierCatalog);
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
    for (const [classifierIndex, descriptor] of classifierCatalog.entries()) {
      const counts = resultCounts[classifierIndex]!;
      const result = transaction.classifications[classifierIndex];
      if (result === undefined) {
        counts.unclassified += 1;
        continue;
      }
      if (result.classifier_id !== descriptor.id) {
        throw new TypeError(
          "Transaction classifier order does not match catalog",
        );
      }
      counts[result.state] += 1;
      for (const label of result.labels) {
        counts.labels[label] = (counts.labels[label] ?? 0) + 1;
      }
    }
  }
  if (value.transaction_count !== value.transactions.length) {
    throw new TypeError("Snapshot transaction count does not match payload");
  }
  if (value.total_vsize !== totalVsize) {
    throw new TypeError("Snapshot total vsize does not match payload");
  }
  if (
    !Array.isArray(value.classification_summaries) ||
    value.classification_summaries.length !== classifierCatalog.length
  ) {
    throw new TypeError("Invalid classification summaries");
  }
  const classifierSummaries = value.classification_summaries.map(
    (summary, index) =>
      parseClassifierSummary(
        summary,
        classifierCatalog[index]!,
        transactionCount,
      ),
  );
  if (
    bip110Summary.compatible_count !== statusCounts.compatible ||
    bip110Summary.violating_count !== statusCounts.violating ||
    bip110Summary.indeterminate_count !== statusCounts.indeterminate ||
    bip110Summary.unclassified_count !== statusCounts.unclassified
  ) {
    throw new TypeError("BIP-110 summary does not match payload");
  }
  for (const [index, summary] of classifierSummaries.entries()) {
    const counts = resultCounts[index]!;
    if (
      summary.classifier_id !== counts.classifierId ||
      summary.complete_count !== counts.complete ||
      summary.partial_count !== counts.partial ||
      summary.unclassified_count !== counts.unclassified ||
      Object.entries(summary.label_counts).some(
        ([label, count]) => counts.labels[label] !== count,
      )
    ) {
      throw new TypeError("Classifier summary does not match payload");
    }
  }

  return value as unknown as MempoolSnapshot;
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
    snapshot.chain_tip.hash !== source.chain_tip.hash ||
    source.classification === null ||
    source.classification.revision !== snapshot.classification_revision ||
    source.classification.classified_count !==
      snapshot.bip110_summary.compatible_count +
        snapshot.bip110_summary.violating_count +
        snapshot.bip110_summary.indeterminate_count ||
    source.classification.unclassified_count !==
      snapshot.bip110_summary.unclassified_count
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
