import {
  AtlasRequestError,
  fetchSources,
  fetchSourcePublication,
  fetchTransactionDetail,
  transactionDetailMatchesSnapshot,
} from "./api";
import { installAnalytics } from "./analytics";
import {
  DEFAULT_FILTERS,
  filterTransactionsCooperatively,
  type MempoolFilters,
} from "./filters";
import * as firstPublication from "./first-publication-failure";
import { pinSelectedInBoundedSample } from "./bounded-sample";
import {
  classificationPresentation,
  transactionDetailFailurePresentation,
  unclassifiedLabel,
} from "./classification-progress";
import { RequestLifecycle } from "./comparison-lifecycle";
import {
  DEFAULT_CLASSIFIER_ID,
  classificationResult,
  classifierDescriptor,
  classifierSummary,
} from "./classification-view";
import { createClassificationOverviewView } from "./classification-overview-view";
import {
  bucketTerrainRegionCanShowLabel,
  hitTestBucketTerrain,
  renderBucketTerrain,
} from "./bucket-terrain";
import {
  bip110RulePopulation,
  bip110RulePopulationSummary,
} from "./bip110-rule-index";
import {
  KNOTS_BIP110_CLASSIFIER_ID,
  classifierBucketColor,
  classifierBucketContainsLabel,
  classifierBucketDescription,
  classifierBucketForTransaction,
  classifierBucketIsSummary,
  classifierBucketLabel,
  classifierLabelSamplePopulation,
  classifierBucketPopulation,
  classifierTerrainGroups,
  classifierUsesSummaryBuckets,
  type ClassifierBucketKey,
  type ClassifierTerrainLayout,
} from "./classifier-terrain";
import { renderSwimViewCooperatively } from "./swim-view";
import { setMembershipControlsComplete } from "./membership-controls";
import {
  countFormat,
  decimalFormat,
  formatVsize,
  percentageFormat,
} from "./format";
import { createSnapshotDistributionsView } from "./snapshot-distributions-view";
import {
  chooseInitialNodeRule,
  prepareNodePublicationCommit,
} from "./node-publication-candidate";
import { renderPrimaryNodePublication } from "./node-primary-publication";
import {
  nodeSampleSelectionContains,
  resolveNodeSampleSelection,
} from "./node-sample-selection";
import { recordAtlasCandidateCommitted } from "./candidate-ready";
import {
  createNodeSourceSummaryView,
  setAtlasLoadPhase,
} from "./source-summary-view";
import { transactionFactPairs } from "./transaction-facts";
import { createTerrainSummaryView } from "./terrain-summary-view";
import {
  TERRAIN_RULES,
  hitTestTerrain,
  renderTerrain,
  signatureLabel,
  signaturePopulation,
  statusPopulation,
  terrainRegionKey,
  terrainRule,
  unknownRulesLabel,
  type TerrainLayout,
  type TerrainMode,
  type TerrainRegionKey,
  type TerrainSelection,
  type StatusRegionKey,
  type ViolationSignature,
  type ViolationSignatureKey,
} from "./terrain";
import {
  parseNodeViewState,
  serializeNodeViewState,
  type NodeViewState,
} from "./view-state";
import { findSnapshotTransaction, snapshotIsComplete } from "./packed-store";
import { markAtlasReadiness, markAtlasReadinessAfterPaint } from "./readiness";
import "./styles.css";
import "./source-summary-styles.css";
import type {
  Bip110Assessment,
  ClassificationResult,
  ClassificationProgress,
  ClassifierDescriptor,
  MempoolSnapshot,
  MempoolTransaction,
  RuleId,
  RuleAssessment,
  LoadedSourcePublication,
  SourceSummary,
  TransactionDetailResponse,
} from "./types";

installAnalytics(import.meta.env);

const requiredElement = <T extends HTMLElement>(id: string): T => {
  const element = document.getElementById(id);
  if (!(element instanceof HTMLElement)) {
    throw new Error(`Missing required element #${id}`);
  }
  return element as T;
};

const pageStatus = requiredElement<HTMLElement>("page-status");
const sourceSummaryView = createNodeSourceSummaryView();
const atlasVersion = requiredElement<HTMLElement>("atlas-version");
const statusTitle = requiredElement<HTMLElement>("status-title");
const statusDetail = requiredElement<HTMLElement>("status-detail");
const refreshButton = requiredElement<HTMLButtonElement>("refresh");
const compareLink = requiredElement<HTMLAnchorElement>("compare-link");
const sourceSelect = requiredElement<HTMLSelectElement>("source-select");
const transactionSearch =
  requiredElement<HTMLFormElement>("transaction-search");
const transactionSearchInput = requiredElement<HTMLInputElement>(
  "transaction-search-input",
);
const transactionSearchStatus = requiredElement<HTMLElement>(
  "transaction-search-status",
);
const overviewTab = requiredElement<HTMLButtonElement>("overview-tab");
const terrainTab = requiredElement<HTMLButtonElement>("terrain-tab");
const feeAgeTab = requiredElement<HTMLButtonElement>("fee-age-tab");
const overviewView = requiredElement<HTMLElement>("overview-view");
const terrainView = requiredElement<HTMLElement>("terrain-view");
const feeAgeView = requiredElement<HTMLElement>("fee-age-view");
const classificationLensSelect = requiredElement<HTMLSelectElement>(
  "classification-lens-select",
);
const classificationMethod = requiredElement<HTMLElement>(
  "classification-method",
);
const classificationEmpty = requiredElement<HTMLElement>(
  "classification-empty",
);
const classificationLabels = requiredElement<HTMLElement>(
  "classification-labels",
);
const classificationSummary = requiredElement<HTMLElement>(
  "classification-summary",
);
const coverageComplete = requiredElement<HTMLElement>("coverage-complete");
const coverageIncomplete = requiredElement<HTMLElement>("coverage-incomplete");
const coverageUnclassified = requiredElement<HTMLElement>(
  "coverage-unclassified",
);
const coverageUnclassifiedLabel = requiredElement<HTMLElement>(
  "coverage-unclassified-label",
);
const classificationLifecycle = requiredElement<HTMLElement>(
  "classification-lifecycle",
);
const classificationState = requiredElement<HTMLElement>(
  "classification-state",
);
const classificationProgress = requiredElement<HTMLElement>(
  "classification-progress",
);
const terrainSummaryView = createTerrainSummaryView();
const terrainStage = requiredElement<HTMLElement>("terrain-stage");
const terrainCanvas = requiredElement<HTMLCanvasElement>("terrain-canvas");
const terrainRegions = requiredElement<HTMLElement>("terrain-regions");
const terrainEmpty = requiredElement<HTMLElement>("terrain-empty");
const modeCount = requiredElement<HTMLButtonElement>("mode-count");
const modeVsize = requiredElement<HTMLButtonElement>("mode-vsize");
const inspectorEyebrow = requiredElement<HTMLElement>("inspector-eyebrow");
const inspectorName = requiredElement<HTMLElement>("inspector-name");
const inspectorDescription = requiredElement<HTMLElement>(
  "inspector-description",
);
const inspectorBadge = requiredElement<HTMLElement>("inspector-badge");
const inspectorNote = requiredElement<HTMLElement>("inspector-note");
const ruleCount = requiredElement<HTMLElement>("rule-count");
const ruleVsize = requiredElement<HTMLElement>("rule-vsize");
const ruleShare = requiredElement<HTMLElement>("rule-share");
const ruleList = requiredElement<HTMLElement>("rule-list");
const ruleOverlapNote = requiredElement<HTMLElement>("rule-overlap-note");
const sampleSummary = requiredElement<HTMLElement>("sample-summary");
const transactionDisclosure = requiredElement<HTMLDetailsElement>(
  "transaction-disclosure",
);
const sampleClassificationHeading = requiredElement<HTMLElement>(
  "sample-classification-heading",
);
const ruleTransactions =
  requiredElement<HTMLTableSectionElement>("rule-transactions");
const detailStatus = requiredElement<HTMLElement>("detail-status");
const detailTransaction = requiredElement<HTMLElement>("detail-transaction");
const detailClassifiers = requiredElement<HTMLElement>("detail-classifiers");
const detailRules = requiredElement<HTMLOListElement>("detail-rules");
const filtersForm = requiredElement<HTMLFormElement>("filters");
const minimumFeeRate = requiredElement<HTMLInputElement>("minimum-fee-rate");
const maximumAge = requiredElement<HTMLSelectElement>("maximum-age");
const minimumVsize = requiredElement<HTMLInputElement>("minimum-vsize");
const resetFilters = requiredElement<HTMLButtonElement>("reset-filters");
const filterSummary = requiredElement<HTMLElement>("filter-summary");
const feeAgeStage = requiredElement<HTMLElement>("fee-age-stage");
const feeAgeCanvas = requiredElement<HTMLCanvasElement>("mempool-canvas");
const feeAgeEmpty = requiredElement<HTMLElement>("fee-age-empty");
const visualSummary = requiredElement<HTMLElement>("visual-summary");

type Lens = "overview" | "terrain" | "fee-age";
type InspectorSelection = TerrainSelection;

const initialViewState = parseNodeViewState(window.location.search);
const snapshotLifecycle = new RequestLifecycle();

let configuredSources: SourceSummary[] = [];
let selectedSourceId: string | null = null;
let currentSnapshot: MempoolSnapshot | null = null;
let currentSnapshotIdentity: string | null = null;
let currentClassification: ClassificationProgress | null = null;
let filteredTransactions: MempoolTransaction[] = [];
let selectedLens: Lens = "overview";
let selectedClassifierId = DEFAULT_CLASSIFIER_ID;
let selectedClassifierLabel: string | null = null;
let selectedClassifierBucketKey: ClassifierBucketKey | null = null;
let selectedInspector: InspectorSelection = {
  kind: "rule",
  rule: "element_size",
};
let terrainMode: TerrainMode = "count";
let policyTerrainLayout: TerrainLayout | null = null;
let classifierTerrainLayout: ClassifierTerrainLayout | null = null;
let pendingTerrainFrame: number | null = null;
let pendingFeeAgeFrame: number | null = null;
let feeAgeRenderController: AbortController | null = null;
let filterController: AbortController | null = null;
let detailSequence = 0;
let detailController: AbortController | null = null;
let selectedTransactionId: string | null = null;
let nodeViewState: NodeViewState = initialViewState;

const currentTransaction = (txid: string): MempoolTransaction | undefined =>
  currentSnapshot === null
    ? undefined
    : findSnapshotTransaction(currentSnapshot, txid);

const renderClassificationLifecycle = (
  progress: ClassificationProgress | null,
  total: number | null,
): void => {
  if (progress === null || total === null) {
    classificationLifecycle.dataset.state = "waiting";
    classificationState.textContent = "Waiting";
    classificationProgress.textContent =
      "Classification starts after the first complete snapshot.";
    return;
  }
  const presentation = classificationPresentation(progress, total, (value) =>
    countFormat.format(value),
  );
  classificationLifecycle.dataset.state = progress.state;
  classificationState.textContent = presentation.label;
  classificationProgress.textContent = presentation.summary;
};

const unclassifiedDetailText = (): string => {
  if (currentSnapshot === null || currentClassification === null) {
    return "Assessment is unavailable for this transaction in the current snapshot.";
  }
  return classificationPresentation(
    currentClassification,
    currentSnapshot.transaction_count,
    (value) => countFormat.format(value),
  ).unclassifiedDetail;
};

const currentUnclassifiedLabel = (): string =>
  currentClassification === null
    ? "Assessment unavailable"
    : unclassifiedLabel(currentClassification);

const currentClassifierUnavailableLabel = (): string =>
  currentClassification?.state === "classifying"
    ? "Result pending"
    : "Result unavailable";

const classifierUnavailableDetailText = (): string =>
  currentClassification?.state === "classifying"
    ? "This classifier has not produced a current result for this exact transaction and witness variant yet."
    : "This classifier did not produce a current result for this exact transaction and witness variant.";

const currentClassifierDescriptor = (): ClassifierDescriptor | null =>
  currentSnapshot === null
    ? null
    : classifierDescriptor(currentSnapshot, selectedClassifierId);

const selectedClassifierIsBip110 = (): boolean =>
  selectedClassifierId === KNOTS_BIP110_CLASSIFIER_ID;

const resetTerrainLayouts = (): void => {
  policyTerrainLayout = null;
  classifierTerrainLayout = null;
};

const renderCoverage = (): void => {
  const snapshot = currentSnapshot;
  if (snapshot === null) {
    coverageComplete.textContent = "0";
    coverageIncomplete.textContent = "0";
    coverageUnclassified.textContent = "0";
    coverageUnclassifiedLabel.textContent = "assessment unavailable";
    return;
  }
  const summary = classifierSummary(snapshot, selectedClassifierId);
  coverageComplete.textContent = countFormat.format(
    summary?.complete_count ?? 0,
  );
  coverageIncomplete.textContent = countFormat.format(
    summary?.partial_count ?? 0,
  );
  coverageUnclassified.textContent = countFormat.format(
    summary?.unclassified_count ?? snapshot.transaction_count,
  );
  coverageUnclassifiedLabel.textContent = "result unavailable";
};

const classificationOverviewView = createClassificationOverviewView(
  {
    lensSelect: classificationLensSelect,
    method: classificationMethod,
    empty: classificationEmpty,
    labels: classificationLabels,
    summary: classificationSummary,
  },
  (label) => selectClassifierLabel(label, false),
);

const renderClassificationOverview = (): void => {
  const selection = classificationOverviewView.render(currentSnapshot, {
    classifierId: selectedClassifierId,
    label: selectedClassifierLabel,
  });
  selectedClassifierId = selection.classifierId;
  selectedClassifierLabel = selection.label;
};

const inspectorRules = (): RuleId[] => TERRAIN_RULES.map(({ id }) => id);

const isViolationSignatureKey = (
  key: TerrainRegionKey,
): key is ViolationSignatureKey =>
  key.startsWith("exact:") || key.startsWith("partial:");

const isStatusRegionKey = (key: string): key is StatusRegionKey =>
  key === "compatible" || key === "indeterminate" || key === "unclassified";

const selectionsMatch = (
  left: InspectorSelection,
  right: InspectorSelection,
): boolean =>
  left.kind === right.kind &&
  (left.kind === "rule"
    ? left.rule === (right as { kind: "rule"; rule: RuleId }).rule
    : left.regionKey ===
      (right as { kind: "region"; regionKey: TerrainRegionKey }).regionKey);

const setTransactionSearchStatus = (
  message: string,
  state?: "found" | "absent" | "error",
): void => {
  transactionSearchStatus.textContent = message;
  if (state === undefined) {
    delete transactionSearchStatus.dataset.state;
  } else {
    transactionSearchStatus.dataset.state = state;
  }
};

const updateCompareLink = (): void => {
  const current = selectedSourceId;
  const other = configuredSources.find(
    ({ source_id: source }) => source !== current,
  )?.source_id;
  if (current === null || other === undefined) {
    compareLink.href = "./compare/";
    return;
  }
  const params = new URLSearchParams({ left: current, right: other });
  compareLink.href = `./compare/?${params.toString()}`;
};

const replaceViewUrl = (): void => {
  const query = serializeNodeViewState(nodeViewState);
  const next = `${window.location.pathname}${query.length === 0 ? "" : `?${query}`}${window.location.hash}`;
  window.history.replaceState(null, "", next);
  updateCompareLink();
};

const readFilters = (): MempoolFilters => {
  const feeRate = Number.parseFloat(minimumFeeRate.value);
  const size = Number.parseInt(minimumVsize.value, 10);
  const age = maximumAge.value;
  return {
    minimumFeeRate: Number.isFinite(feeRate) && feeRate >= 0 ? feeRate : 0,
    maximumAgeMs: age === "all" ? null : Number.parseInt(age, 10),
    minimumVsize: Number.isSafeInteger(size) && size >= 0 ? size : 0,
  };
};

const clearDetail = (
  message: string,
  clearAddressedTransaction = true,
): void => {
  detailController?.abort();
  detailController = null;
  detailSequence += 1;
  selectedTransactionId = null;
  if (clearAddressedTransaction) {
    nodeViewState = { ...nodeViewState, txid: null };
    transactionSearchInput.value = "";
    setTransactionSearchStatus("Search this snapshot by txid.");
  }
  detailStatus.textContent = message;
  transactionDisclosure.open = false;
  detailTransaction.replaceChildren();
  detailClassifiers.replaceChildren();
  detailRules.replaceChildren();
};

const detailValue = (label: string, value: string): HTMLElement => {
  const wrapper = document.createElement("div");
  const term = document.createElement("span");
  term.textContent = label;
  const code = document.createElement("code");
  code.textContent = value;
  code.title = value;
  wrapper.append(term, code);
  return wrapper;
};

const compactJson = (value: unknown[]): string => {
  if (value.length === 0) {
    return "None returned";
  }
  const encoded = JSON.stringify(value);
  return encoded.length > 420 ? `${encoded.slice(0, 417)}…` : encoded;
};

const exemplarLabel = (count: number): string =>
  `1 exemplar of ${countFormat.format(count)}`;

const renderRuleDetail = (rule: RuleAssessment): HTMLLIElement => {
  const item = document.createElement("li");
  item.className = `detail-rule ${rule.verdict}`;

  const heading = document.createElement("div");
  const title = document.createElement("strong");
  title.textContent = `R${rule.number} · ${terrainRule(rule.rule).label}`;
  const verdict = document.createElement("span");
  verdict.textContent = rule.verdict;
  heading.append(title, verdict);

  const observations: HTMLParagraphElement[] = [];
  if (rule.evidence_count > 0) {
    const evidence = document.createElement("p");
    evidence.textContent = `Evidence · ${exemplarLabel(rule.evidence_count)}: ${compactJson(rule.evidence)}`;
    observations.push(evidence);
  }
  if (rule.missing_count > 0) {
    const missing = document.createElement("p");
    missing.textContent = `Missing · ${exemplarLabel(rule.missing_count)}: ${compactJson(rule.missing)}`;
    observations.push(missing);
  }
  if (observations.length === 0) {
    const empty = document.createElement("p");
    empty.textContent = "No evidence or missing facts returned.";
    observations.push(empty);
  }
  item.append(heading, ...observations);
  return item;
};

const ruleSetText = (rules: readonly RuleId[]): string =>
  TERRAIN_RULES.filter(({ id }) => rules.includes(id))
    .map(({ number }) => `R${number}`)
    .join(" + ");

const firstRejectionText = (
  status: Bip110Assessment["status"],
  rule: RuleId | null,
): string => {
  if (rule !== null) {
    return `R${terrainRule(rule).number} · ${terrainRule(rule).shortLabel}`;
  }
  if (status === "compatible") {
    return "None";
  }
  return status === "indeterminate" ? "Not established" : "Unresolved";
};

const createRuleChip = (
  rule: RuleId,
  state: "violated" | "unknown",
): HTMLElement => {
  const chip = document.createElement("span");
  chip.className = `rule-chip ${state}`;
  chip.style.setProperty("--rule-color", terrainRule(rule).color);
  chip.textContent = `R${terrainRule(rule).number}${state === "unknown" ? "?" : ""}`;
  chip.title = `${terrainRule(rule).label}${state === "unknown" ? " unresolved" : " violated"}`;
  return chip;
};

const transactionRuleChips = (transaction: MempoolTransaction): HTMLElement => {
  const wrapper = document.createElement("span");
  wrapper.className = "rule-chips";
  const assessment = transaction.bip110;
  if (assessment === null) {
    const empty = document.createElement("span");
    empty.className = "rule-chip unclassified";
    empty.textContent = currentUnclassifiedLabel();
    wrapper.append(empty);
    return wrapper;
  }
  for (const { id } of TERRAIN_RULES) {
    if (assessment.violated_rules.includes(id)) {
      wrapper.append(createRuleChip(id, "violated"));
    }
    if (assessment.unknown_rules.includes(id)) {
      wrapper.append(createRuleChip(id, "unknown"));
    }
  }
  if (wrapper.childElementCount === 0) {
    const empty = document.createElement("span");
    empty.className = "rule-chip compatible";
    empty.textContent = "Pass";
    wrapper.append(empty);
  }
  return wrapper;
};

const transactionClassifierChips = (
  transaction: MempoolTransaction,
): HTMLElement => {
  const wrapper = document.createElement("span");
  wrapper.className = "classifier-chips";
  const descriptor = currentClassifierDescriptor();
  const result = classificationResult(transaction, selectedClassifierId);
  if (descriptor === null || result === null) {
    const unavailable = document.createElement("span");
    unavailable.className = "classifier-chip unavailable";
    unavailable.textContent = "Unavailable";
    wrapper.append(unavailable);
    return wrapper;
  }
  for (const label of result.labels) {
    const chip = document.createElement("span");
    chip.className = "classifier-chip";
    chip.textContent =
      descriptor.labels.find(({ key }) => key === label)?.label ?? label;
    wrapper.append(chip);
  }
  if (result.labels.length === 0) {
    const empty = document.createElement("span");
    empty.className = "classifier-chip";
    empty.textContent = "No labels";
    wrapper.append(empty);
  }
  return wrapper;
};

const renderClassifierDetails = (
  results: readonly ClassificationResult[],
): void => {
  const catalog = currentSnapshot?.classifier_catalog ?? [];
  const cards = results.map((result) => {
    const descriptor = catalog.find(({ id }) => id === result.classifier_id);
    const card = document.createElement("article");
    card.className = "detail-classifier";
    card.dataset.state = result.state;
    const heading = document.createElement("div");
    const title = document.createElement("strong");
    title.textContent = descriptor?.title ?? result.classifier_id;
    const state = document.createElement("span");
    state.textContent = result.state;
    heading.append(title, state);
    const labels = document.createElement("p");
    labels.textContent = result.labels
      .map(
        (key) =>
          descriptor?.labels.find((label) => label.key === key)?.label ?? key,
      )
      .join(" · ");
    card.append(heading, labels);
    if (result.missing_facts.length > 0) {
      const missing = document.createElement("small");
      missing.textContent = `Missing facts: ${result.missing_facts.join(", ")}`;
      card.append(missing);
    }
    if (result.evidence !== null) {
      const evidence = document.createElement("details");
      const summary = document.createElement("summary");
      summary.textContent = "Evidence";
      const encoded = document.createElement("code");
      const value = JSON.stringify(result.evidence);
      encoded.textContent =
        value.length > 800 ? `${value.slice(0, 797)}…` : value;
      evidence.append(summary, encoded);
      card.append(evidence);
    }
    return card;
  });
  detailClassifiers.replaceChildren(...cards);
};

const transactionFactRows = (transaction: MempoolTransaction): HTMLElement[] =>
  transactionFactPairs(transaction).map(([label, value]) =>
    detailValue(label, value),
  );

const appendTransactionFacts = (txid: string): void => {
  const entry = currentTransaction(txid);
  if (entry !== undefined) {
    detailTransaction.append(...transactionFactRows(entry));
  }
};

const renderTransactionDetail = (detail: TransactionDetailResponse): void => {
  if (!selectedClassifierIsBip110()) {
    const descriptor = currentClassifierDescriptor();
    const result = detail.classifications.find(
      ({ classifier_id: id }) => id === selectedClassifierId,
    );
    const labels =
      result?.labels.map(
        (key) =>
          descriptor?.labels.find((label) => label.key === key)?.label ?? key,
      ) ?? [];
    detailStatus.textContent =
      result === undefined
        ? "Result unavailable"
        : result.state === "complete"
          ? "Complete result"
          : "Partial result";
    detailTransaction.replaceChildren(
      detailValue("txid", detail.txid),
      detailValue("wtxid", detail.wtxid),
      detailValue("Classifier", descriptor?.title ?? selectedClassifierId),
      detailValue("Labels", labels.join(" · ") || "No labels"),
    );
    appendTransactionFacts(detail.txid);
    renderClassifierDetails(detail.classifications);
    detailRules.replaceChildren();
    return;
  }
  detailStatus.textContent =
    detail.assessment.status === "violating"
      ? "Would violate policy"
      : detail.assessment.status === "compatible"
        ? "Compatible"
        : "Indeterminate";
  detailTransaction.replaceChildren(
    detailValue("txid", detail.txid),
    detailValue("wtxid", detail.wtxid),
    detailValue(
      "Violated rules",
      ruleSetText(detail.assessment.violated_rules) || "None",
    ),
    detailValue(
      "First rejection",
      firstRejectionText(
        detail.assessment.status,
        detail.assessment.primary_rule,
      ),
    ),
  );
  appendTransactionFacts(detail.txid);
  renderClassifierDetails(detail.classifications);
  detailRules.replaceChildren(...detail.rules.map(renderRuleDetail));
};

const showAbsentTransaction = (txid: string): void => {
  detailController?.abort();
  detailController = null;
  detailSequence += 1;
  nodeViewState = { ...nodeViewState, txid };
  selectedTransactionId = null;
  transactionSearchInput.value = txid;
  setTransactionSearchStatus(
    "This transaction is not present in this snapshot.",
    "absent",
  );
  detailStatus.textContent = "Not present in this snapshot";
  transactionDisclosure.open = true;
  detailTransaction.replaceChildren(detailValue("txid", txid));
  detailClassifiers.replaceChildren();
  const message = document.createElement("p");
  message.className = "detail-error";
  message.textContent =
    "Atlas did not observe this transaction in the selected node's current mempool snapshot.";
  detailTransaction.append(message);
  detailRules.replaceChildren();
  renderInspectorSamples();
  scheduleTerrainRender();
  replaceViewUrl();
};

const loadTransactionDetail = async (
  transaction: MempoolTransaction,
): Promise<void> => {
  const snapshot = currentSnapshot;
  const classification = currentClassification;
  if (snapshot === null || classification === null) {
    return;
  }
  detailController?.abort();
  detailController = null;
  if (!snapshotIsComplete(snapshot)) {
    selectedTransactionId = transaction.txid;
    nodeViewState = { ...nodeViewState, txid: transaction.txid };
    transactionSearchInput.value = transaction.txid;
    setTransactionSearchStatus("Present in this snapshot.", "found");
    transactionDisclosure.open = true;
    detailStatus.textContent = "Loading membership stage…";
    detailTransaction.replaceChildren(detailValue("txid", transaction.txid));
    detailClassifiers.replaceChildren();
    detailRules.replaceChildren();
    scheduleTerrainRender();
    replaceViewUrl();
    return;
  }
  const sequence = ++detailSequence;
  const controller = new AbortController();
  detailController = controller;
  transactionDisclosure.open = true;
  const descriptor = currentClassifierDescriptor();
  const transactionSelection: InspectorSelection | null =
    selectedClassifierIsBip110()
      ? {
          kind: "region",
          regionKey: terrainRegionKey(transaction),
        }
      : null;
  const transactionBucket =
    descriptor === null || transactionSelection !== null
      ? null
      : classifierBucketForTransaction(transaction, descriptor).key;
  const selectionChanged =
    transactionSelection === null
      ? selectedClassifierBucketKey !== transactionBucket
      : !selectionsMatch(selectedInspector, transactionSelection);
  if (transactionSelection !== null) {
    selectedInspector = transactionSelection;
    selectedClassifierBucketKey = null;
  } else {
    selectedClassifierBucketKey = transactionBucket;
  }
  selectedTransactionId = transaction.txid;
  nodeViewState = {
    source: selectedSourceId,
    classifier: selectedClassifierId,
    selection: transactionSelection,
    txid: transaction.txid,
  };
  transactionSearchInput.value = transaction.txid;
  setTransactionSearchStatus("Present in this snapshot.", "found");
  detailStatus.textContent = "Loading classifier evidence…";
  detailTransaction.replaceChildren(
    detailValue("txid", transaction.txid),
    detailValue("wtxid", transaction.wtxid),
    ...transactionFactRows(transaction),
  );
  detailClassifiers.replaceChildren();
  detailRules.replaceChildren();
  if (selectionChanged) {
    renderInspector();
  } else {
    renderSampleTable();
  }
  scheduleTerrainRender();
  replaceViewUrl();
  try {
    const detail = await fetchTransactionDetail(
      snapshot.source_id,
      transaction.txid,
      controller.signal,
    );
    if (
      sequence !== detailSequence ||
      currentSnapshot?.observed_at_ms !== snapshot.observed_at_ms ||
      currentSnapshot.classification_revision !==
        snapshot.classification_revision
    ) {
      return;
    }
    if (!transactionDetailMatchesSnapshot(snapshot, transaction, detail)) {
      throw new Error("Transaction detail does not match this snapshot");
    }
    renderTransactionDetail(detail);
  } catch (error) {
    if (controller.signal.aborted) {
      return;
    }
    if (sequence !== detailSequence) {
      return;
    }
    if (
      error instanceof AtlasRequestError &&
      error.status === 503 &&
      error.problemType === null
    ) {
      const selectedResultAvailable =
        classificationResult(transaction, selectedClassifierId) !== null;
      if (!selectedClassifierIsBip110()) {
        detailStatus.textContent = selectedResultAvailable
          ? "Detail no longer current"
          : currentClassifierUnavailableLabel();
        const message = document.createElement("p");
        message.className = selectedResultAvailable
          ? "detail-error"
          : "detail-unclassified";
        message.textContent = selectedResultAvailable
          ? "Atlas no longer has classifier evidence matching this displayed snapshot. Refresh before relying on transaction detail."
          : `${classifierUnavailableDetailText()} Atlas has no classifier evidence to show.`;
        detailClassifiers.replaceChildren();
        detailRules.replaceChildren();
        detailTransaction.append(message);
        return;
      }
      const presentation = transactionDetailFailurePresentation(
        classification,
        snapshot.transaction_count,
        selectedResultAvailable,
        (value) => countFormat.format(value),
      );
      detailStatus.textContent = presentation.status;
      const message = document.createElement("p");
      message.className = selectedResultAvailable
        ? "detail-error"
        : "detail-unclassified";
      message.textContent = presentation.detail;
      detailClassifiers.replaceChildren();
      detailRules.replaceChildren();
      detailTransaction.append(message);
      return;
    }
    detailStatus.textContent = "Detail unavailable";
    const message = document.createElement("p");
    message.className = "detail-error";
    message.textContent =
      error instanceof Error
        ? error.message
        : "Unable to load classifier evidence";
    detailClassifiers.replaceChildren();
    detailRules.replaceChildren();
    detailTransaction.append(message);
  } finally {
    if (detailController === controller) {
      detailController = null;
    }
  }
};

const populationForSelection = (
  snapshot: MempoolSnapshot,
  selection: InspectorSelection,
) => {
  if (selection.kind === "rule") {
    return bip110RulePopulation(snapshot.transactions, selection.rule);
  }
  return isViolationSignatureKey(selection.regionKey)
    ? signaturePopulation(snapshot.transactions, selection.regionKey)
    : statusPopulation(snapshot.transactions, selection.regionKey);
};

const signatureForSelection = (
  snapshot: MempoolSnapshot,
  selection: InspectorSelection,
): ViolationSignature | null =>
  selection.kind === "region" && isViolationSignatureKey(selection.regionKey)
    ? (signaturePopulation(snapshot.transactions, selection.regionKey)
        ?.signature ?? null)
    : null;

const overviewPopulation = (): ReturnType<
  typeof classifierLabelSamplePopulation
> => {
  if (currentSnapshot === null || selectedClassifierLabel === null) {
    return null;
  }
  const descriptor = currentClassifierDescriptor();
  if (descriptor === null) return null;
  return classifierLabelSamplePopulation(
    currentSnapshot.transactions,
    descriptor,
    selectedClassifierLabel,
  );
};

const classifierBucketSelectionPopulation = (): ReturnType<
  typeof classifierBucketPopulation
> => {
  const descriptor = currentClassifierDescriptor();
  if (
    currentSnapshot === null ||
    descriptor === null ||
    selectedClassifierBucketKey === null
  ) {
    return null;
  }
  return classifierBucketPopulation(
    currentSnapshot.transactions,
    descriptor,
    selectedClassifierBucketKey,
  );
};

const currentInspectorPopulation = () => {
  if (currentSnapshot === null) {
    detailController?.abort();
    detailController = null;
    return null;
  }
  if (selectedLens !== "terrain") {
    return overviewPopulation();
  }
  if (selectedClassifierIsBip110()) {
    return populationForSelection(currentSnapshot, selectedInspector);
  }
  return classifierBucketSelectionPopulation() ?? overviewPopulation();
};

const renderSampleTable = (): void => {
  if (currentSnapshot === null) {
    ruleTransactions.replaceChildren();
    sampleSummary.textContent = "No snapshot";
    return;
  }
  const population = currentInspectorPopulation();
  const selection = resolveNodeSampleSelection({
    terrain: selectedLens === "terrain",
    bip110: selectedClassifierIsBip110(),
    inspector: selectedInspector,
    descriptor: currentClassifierDescriptor(),
    bucketKey: selectedClassifierBucketKey,
    classifierId: selectedClassifierId,
    label: selectedClassifierLabel,
  });
  const largest = population?.transactions.slice(0, 8) ?? [];
  const { entries: sample, pinsSelected } = pinSelectedInBoundedSample(
    largest,
    selectedTransactionId === null
      ? undefined
      : currentTransaction(selectedTransactionId),
    8,
    (transaction) =>
      population !== null &&
      selection !== null &&
      nodeSampleSelectionContains(transaction, selection),
  );
  sampleSummary.textContent =
    population === null || population.count === 0
      ? "No matches"
      : pinsSelected
        ? `Selected + ${countFormat.format(sample.length - 1)} largest of ${countFormat.format(population.count)}`
        : `Largest ${countFormat.format(sample.length)} of ${countFormat.format(population.count)}`;

  const rows = sample.map((transaction) => {
    const row = document.createElement("tr");
    if (transaction.txid === selectedTransactionId) {
      row.className = "selected";
    }
    const transactionCell = document.createElement("td");
    const button = document.createElement("button");
    button.type = "button";
    button.className = "tx-select";
    button.textContent = `${transaction.txid.slice(0, 10)}…${transaction.txid.slice(-6)}`;
    button.title = transaction.txid;
    button.setAttribute(
      "aria-label",
      `Inspect transaction ${transaction.txid}`,
    );
    button.addEventListener("click", () => {
      void loadTransactionDetail(transaction);
    });
    transactionCell.append(button);

    const rulesCell = document.createElement("td");
    rulesCell.append(
      selectedLens !== "terrain" || !selectedClassifierIsBip110()
        ? transactionClassifierChips(transaction)
        : transactionRuleChips(transaction),
    );
    const sizeCell = document.createElement("td");
    sizeCell.textContent = countFormat.format(transaction.vsize);
    const feeCell = document.createElement("td");
    feeCell.textContent = decimalFormat.format(
      transaction.fee_sats / transaction.vsize,
    );
    row.append(transactionCell, rulesCell, sizeCell, feeCell);
    return row;
  });
  ruleTransactions.replaceChildren(...rows);
};

const renderInspectorSamples = (): void => {
  if (currentSnapshot !== null && !snapshotIsComplete(currentSnapshot)) {
    ruleTransactions.replaceChildren();
    sampleSummary.textContent = "Samples load with membership data.";
    return;
  }
  renderSampleTable();
};

const selectClassifierLabel = (labelKey: string, loadSample: boolean): void => {
  const changed =
    selectedClassifierLabel !== labelKey ||
    selectedClassifierBucketKey !== null;
  selectedClassifierLabel = labelKey;
  selectedClassifierBucketKey = null;
  nodeViewState = {
    ...nodeViewState,
    classifier: selectedClassifierId,
    selection: null,
  };
  if (changed) {
    clearDetail("Choose a sample");
  }
  renderClassificationOverview();
  renderInspector();
  scheduleTerrainRender();
  replaceViewUrl();
  if (loadSample) {
    const first = overviewPopulation()?.transactions[0];
    if (first !== undefined) {
      void loadTransactionDetail(first);
    }
  }
};

const renderRuleNavigation = (): void => {
  if (selectedLens !== "terrain") {
    ruleList.replaceChildren();
    return;
  }
  if (currentSnapshot === null) {
    ruleList.replaceChildren();
    return;
  }
  const snapshot = currentSnapshot;
  if (!selectedClassifierIsBip110()) {
    const descriptor = currentClassifierDescriptor();
    if (descriptor === null) {
      ruleList.replaceChildren();
      return;
    }
    const summary = classifierSummary(snapshot, descriptor.id);
    ruleList.setAttribute("aria-label", `${descriptor.title} label filters`);
    const buttons = descriptor.labels.map((label, index) => {
      const populationCount = summary?.label_counts[label.key] ?? 0;
      const button = document.createElement("button");
      button.type = "button";
      button.dataset.label = label.key;
      button.dataset.member = String(
        classifierBucketSelectionPopulation()?.labelKeys.includes(label.key) ??
          false,
      );
      button.setAttribute(
        "aria-pressed",
        String(
          selectedClassifierBucketKey === null &&
            selectedClassifierLabel === label.key,
        ),
      );
      button.tabIndex = selectedClassifierLabel === label.key ? 0 : -1;
      button.setAttribute(
        "aria-label",
        `${label.label}: ${countFormat.format(populationCount)} transactions carry this label. Label totals may overlap.`,
      );
      const ordinal = document.createElement("span");
      ordinal.textContent = `L${index + 1}`;
      const name = document.createElement("strong");
      name.textContent = label.label;
      const count = document.createElement("small");
      count.textContent = countFormat.format(populationCount);
      button.append(ordinal, name, count);
      button.addEventListener("click", () => {
        selectClassifierLabel(label.key, false);
      });
      return button;
    });
    ruleList.replaceChildren(...buttons);
    return;
  }
  ruleList.setAttribute("aria-label", "BIP-110 rule filters");
  const selectedSignature = signatureForSelection(snapshot, selectedInspector);
  const navigationTabRule =
    selectedInspector.kind === "rule"
      ? selectedInspector.rule
      : (selectedSignature?.foundationRule ?? TERRAIN_RULES[0]?.id);
  const buttons = TERRAIN_RULES.map((rule) => {
    const population = bip110RulePopulationSummary(
      snapshot.transactions,
      rule.id,
    );
    const button = document.createElement("button");
    button.type = "button";
    button.dataset.rule = rule.id;
    button.dataset.member = String(
      selectedSignature?.violatedRules.includes(rule.id) ?? false,
    );
    button.setAttribute(
      "aria-pressed",
      String(
        selectedInspector.kind === "rule" && rule.id === selectedInspector.rule,
      ),
    );
    button.tabIndex = rule.id === navigationTabRule ? 0 : -1;
    button.setAttribute(
      "aria-label",
      `${rule.label}: ${countFormat.format(population.count)} transactions violate this rule. Rule totals overlap.`,
    );
    button.innerHTML = `<span>R${rule.number}</span><strong>${rule.shortLabel}</strong><small>${countFormat.format(population.count)}</small>`;
    button.addEventListener("click", () => {
      selectInspector({ kind: "rule", rule: rule.id }, false);
    });
    return button;
  });
  ruleList.replaceChildren(...buttons);
};

const renderInspector = (): void => {
  if (selectedLens !== "terrain" || !selectedClassifierIsBip110()) {
    const descriptor = currentClassifierDescriptor();
    const label = descriptor?.labels.find(
      ({ key }) => key === selectedClassifierLabel,
    );
    const bucket =
      selectedLens === "terrain" ? classifierBucketSelectionPopulation() : null;
    const population = bucket ?? overviewPopulation();
    if (bucket !== null && descriptor !== null) {
      const summaryBucket = classifierBucketIsSummary(bucket);
      inspectorEyebrow.textContent = `${descriptor.title} ${summaryBucket ? "group" : "bucket"}`;
      inspectorName.textContent = classifierBucketLabel(descriptor, bucket);
      inspectorDescription.textContent =
        bucket.state === "unavailable"
          ? classifierUnavailableDetailText()
          : summaryBucket
            ? `${classifierBucketDescription(bucket) ?? "Transactions in this broad presentation group."} Exact property labels vary by transaction${bucket.state === "partial" ? ", and at least one required fact remains unresolved" : ""}.`
            : bucket.state === "partial"
              ? "Partial classifier results with exactly these observed labels. At least one required fact remains unresolved."
              : "Complete classifier results with exactly this observed label set.";
      inspectorBadge.textContent =
        bucket.state === "unavailable"
          ? "Result unavailable"
          : summaryBucket
            ? bucket.state === "partial"
              ? "Partial presentation group"
              : "Presentation group"
            : bucket.state === "partial"
              ? "Partial label set"
              : "Exact label set";
      ruleOverlapNote.textContent = summaryBucket
        ? "This group simplifies the terrain only. Select a transaction for its exact properties, or use marginal labels to highlight individual blocks."
        : "This bucket is an exact partition. Marginal label controls may highlight this and other buckets.";
    } else {
      inspectorEyebrow.textContent = descriptor?.title ?? "Classifier lens";
      inspectorName.textContent = label?.label ?? "Choose a classifier label";
      inspectorDescription.textContent =
        label?.description ??
        "Choose one label to inspect its marginal transaction population.";
      inspectorBadge.textContent = descriptor?.methodology ?? "Classifier";
      ruleOverlapNote.textContent =
        descriptor?.semantics === "multi_label"
          ? "Label totals are marginal and may overlap within this classifier."
          : "This classifier reports one specialized rule-set outcome.";
    }
    inspectorNote.textContent =
      descriptor?.methodology === "heuristic"
        ? "Heuristic labels describe matching transaction shapes. They are not proof of wallet ownership or intent."
        : descriptor?.methodology === "fingerprint"
          ? "Fingerprints identify supported byte or script patterns. They do not validate external protocol state."
          : descriptor?.methodology === "policy"
            ? "Policy compatibility is source-local and does not prove acceptance, relay, rejection, or consensus validity."
            : "Exact labels describe available transaction and spent-output facts without inferring intent.";
    sampleClassificationHeading.textContent =
      descriptor?.title ?? "Classification";
    if (population === null) {
      ruleCount.textContent = "0";
      ruleVsize.textContent = "0 vB";
      ruleShare.textContent = "0%";
    } else {
      ruleCount.textContent = countFormat.format(population.count);
      ruleVsize.textContent = formatVsize(population.vsize);
      ruleShare.textContent = percentageFormat.format(population.totalShare);
    }
    renderRuleNavigation();
    renderInspectorSamples();
    return;
  }

  inspectorBadge.textContent = "Mempool policy";
  inspectorNote.textContent =
    "This describes compatibility with rules applied by Bitcoin Knots as mempool policy. It does not show that this source rejected a transaction, or that a transaction is consensus-invalid.";
  ruleOverlapNote.textContent =
    "Rule totals overlap. A transaction is counted under every rule it violates.";
  sampleClassificationHeading.textContent = "Rules";
  const population =
    currentSnapshot === null
      ? null
      : populationForSelection(currentSnapshot, selectedInspector);
  if (selectedInspector.kind === "rule") {
    const rule = terrainRule(selectedInspector.rule);
    inspectorEyebrow.textContent = `Rule ${rule.number}`;
    inspectorName.textContent = rule.label;
    inspectorDescription.textContent = `${rule.description} The population includes every transaction with a proven violation of this rule.`;
  } else if (isStatusRegionKey(selectedInspector.regionKey)) {
    inspectorEyebrow.textContent = "Status bucket";
    if (selectedInspector.regionKey === "compatible") {
      inspectorName.textContent = "Compatible";
      inspectorDescription.textContent =
        "Complete assessments with no proven violation of the deployed BIP-110 mempool policy.";
    } else if (selectedInspector.regionKey === "indeterminate") {
      inspectorName.textContent = "Indeterminate";
      inspectorDescription.textContent =
        "Assessments with unresolved facts and no proven policy violation.";
    } else {
      inspectorName.textContent = currentUnclassifiedLabel();
      inspectorDescription.textContent = unclassifiedDetailText();
    }
  } else {
    const signature =
      currentSnapshot === null
        ? null
        : signatureForSelection(currentSnapshot, selectedInspector);
    if (signature === null) {
      inspectorEyebrow.textContent = "Rule bucket";
      inspectorName.textContent = "Bucket no longer present";
      inspectorDescription.textContent =
        "The selected combination is not present in this snapshot.";
    } else if (signature.completeness === "exact") {
      inspectorEyebrow.textContent = "Exact rule bucket";
      inspectorName.textContent = signatureLabel(signature);
      inspectorDescription.textContent = `Complete assessments that violate exactly ${ruleSetText(signature.violatedRules)}. First rejection remains secondary transaction metadata.`;
    } else {
      const unknown = unknownRulesLabel(signature);
      inspectorEyebrow.textContent = "Incomplete rule bucket";
      inspectorName.textContent = signatureLabel(signature);
      inspectorDescription.textContent = `Confirmed ${ruleSetText(signature.violatedRules)}; ${unknown || "other checks"} remain unresolved. This is not an exact rule set.`;
    }
  }
  if (population === null) {
    ruleCount.textContent = "0";
    ruleVsize.textContent = "0 vB";
    ruleShare.textContent = "0%";
  } else {
    ruleCount.textContent = countFormat.format(population.count);
    ruleVsize.textContent = formatVsize(population.vsize);
    ruleShare.textContent = percentageFormat.format(population.totalShare);
  }
  renderRuleNavigation();
  renderInspectorSamples();
};

const sectionLabel = (
  key: TerrainLayout["sections"][number]["key"],
): string => {
  if (key === "compatible") {
    return "Compatible";
  }
  if (key === "indeterminate") {
    return "Indeterminate";
  }
  if (key === "violating_exact") {
    return "Would violate policy · exact sets";
  }
  if (key === "violating_incomplete") {
    return "Confirmed violations · unresolved checks";
  }
  return currentUnclassifiedLabel();
};

const terrainTabStopKey = (layout: TerrainLayout): TerrainRegionKey | null => {
  const selectableRegions = layout.regions;
  const selection = selectedInspector;
  if (selection.kind === "region") {
    const selectedKey = selection.regionKey;
    return selectableRegions.some(({ key }) => key === selectedKey)
      ? selectedKey
      : (selectableRegions[0]?.key ?? null);
  }
  const selectedRule = selection.rule;
  return (
    selectableRegions.find(({ signature }) =>
      signature?.violatedRules.includes(selectedRule),
    )?.key ??
    selectableRegions.find(({ signature }) => signature !== null)?.key ??
    selectableRegions[0]?.key ??
    null
  );
};

const renderTerrainRegions = (layout: TerrainLayout): void => {
  const focusedRegion = terrainRegions.contains(document.activeElement)
    ? (document.activeElement as HTMLElement).dataset.region
    : undefined;
  const tabStopKey = terrainTabStopKey(layout);
  const sectionLabels = layout.sections.map((section) => {
    const statusKey = isStatusRegionKey(section.key) ? section.key : null;
    const label = document.createElement(statusKey === null ? "div" : "button");
    if (label instanceof HTMLButtonElement && statusKey !== null) {
      label.type = "button";
      label.className = `terrain-section-label ${section.key} status-region`;
      label.dataset.region = statusKey;
      label.setAttribute(
        "aria-pressed",
        String(
          selectedInspector.kind === "region" &&
            selectedInspector.regionKey === section.key,
        ),
      );
      label.tabIndex = statusKey === tabStopKey ? 0 : -1;
      label.addEventListener("click", () => {
        if (statusKey !== null) {
          selectInspector({ kind: "region", regionKey: statusKey }, false);
        }
      });
    } else {
      label.className = `terrain-section-label ${section.key}`;
    }
    label.style.left = `${(section.rect.x / layout.width) * 100}%`;
    label.style.top = `${(section.rect.y / layout.height) * 100}%`;
    label.style.width = `${(section.rect.width / layout.width) * 100}%`;
    label.style.height = `${section.labelHeight}px`;
    const name = document.createElement("strong");
    name.textContent = sectionLabel(section.key);
    const count = document.createElement("span");
    count.textContent = countFormat.format(section.transactionCount);
    label.append(name, count);
    label.title = `${sectionLabel(section.key)}: ${countFormat.format(section.transactionCount)} transactions, ${formatVsize(section.totalVsize)}`;
    if (label instanceof HTMLButtonElement) {
      label.setAttribute("aria-label", label.title);
    }
    return label;
  });
  const bucketLabels = layout.regions.flatMap((region) => {
    const signature = region.signature;
    if (signature === null) {
      return [];
    }
    const label = document.createElement("button");
    label.type = "button";
    label.className = "terrain-region-label combination-region";
    label.dataset.region = signature.key;
    label.dataset.completeness = signature.completeness;
    label.dataset.matchesFilter = String(
      selectedInspector.kind === "rule" &&
        signature.violatedRules.includes(selectedInspector.rule),
    );
    label.style.left = `${(region.rect.x / layout.width) * 100}%`;
    label.style.top = `${(region.rect.y / layout.height) * 100}%`;
    label.style.width = `${(region.rect.width / layout.width) * 100}%`;
    label.style.height = `${region.labelHeight}px`;
    const labelVisible = bucketTerrainRegionCanShowLabel(region);
    label.dataset.compact = String(!labelVisible);
    const foundation = signature.foundationRule;
    const extraRules = signature.violatedRules.filter(
      (rule) => rule !== foundation,
    );
    const accentRule = extraRules[0] ?? foundation;
    if (foundation !== null) {
      label.style.setProperty(
        "--foundation-color",
        terrainRule(foundation).color,
      );
    }
    label.style.setProperty(
      "--signature-accent",
      signature.completeness === "partial"
        ? "#e1aa4b"
        : accentRule === null
          ? "#f46f93"
          : terrainRule(accentRule).color,
    );

    const identity = document.createElement("span");
    identity.className = "signature-identity";
    const name = document.createElement("strong");
    name.textContent = signatureLabel(signature);
    identity.append(name);
    if (extraRules.length > 0) {
      const marker = document.createElement("span");
      marker.className = "signature-marker";
      marker.textContent = extraRules
        .map((rule) => `+R${terrainRule(rule).number}`)
        .join(" ");
      identity.append(marker);
    }
    if (signature.completeness === "partial") {
      const unresolved = document.createElement("span");
      unresolved.className = "signature-unresolved";
      unresolved.textContent = `? ${unknownRulesLabel(signature)}`;
      identity.append(unresolved);
    }
    const count = document.createElement("span");
    count.className = "signature-count";
    count.textContent = countFormat.format(region.transactionCount);
    if (labelVisible) {
      label.append(identity, count);
    }
    const completeness =
      signature.completeness === "exact"
        ? "complete rule set"
        : `confirmed violations with ${unknownRulesLabel(signature)} unresolved`;
    label.title = `${signatureLabel(signature)}: ${countFormat.format(region.transactionCount)} transactions, ${formatVsize(region.totalVsize)}; ${completeness}`;
    label.setAttribute(
      "aria-label",
      `${signatureLabel(signature)}, ${countFormat.format(region.transactionCount)} transactions, ${formatVsize(region.totalVsize)}. ${completeness}.`,
    );
    label.setAttribute(
      "aria-pressed",
      String(
        selectedInspector.kind === "region" &&
          selectedInspector.regionKey === signature.key,
      ),
    );
    label.tabIndex = signature.key === tabStopKey ? 0 : -1;
    label.addEventListener("click", () => {
      selectInspector({ kind: "region", regionKey: signature.key }, false);
    });
    return [label];
  });
  const overlayElements = [...sectionLabels, ...bucketLabels].sort(
    (left, right) =>
      Number.parseFloat(left.style.top) - Number.parseFloat(right.style.top) ||
      Number.parseFloat(left.style.left) - Number.parseFloat(right.style.left),
  );
  terrainRegions.replaceChildren(...overlayElements);
  if (focusedRegion !== undefined) {
    terrainRegions
      .querySelector<HTMLButtonElement>(`[data-region="${focusedRegion}"]`)
      ?.focus();
  }
};

const classifierSectionLabel = (
  key: ClassifierTerrainLayout["sections"][number]["key"],
): string => {
  if (key === "complete") {
    return "Complete results";
  }
  if (key === "partial") {
    return "Partial results";
  }
  return "Result unavailable";
};

const classifierTerrainTabStopKey = (
  layout: ClassifierTerrainLayout,
): ClassifierBucketKey | null =>
  selectedClassifierBucketKey !== null &&
  layout.regions.some(({ key }) => key === selectedClassifierBucketKey)
    ? selectedClassifierBucketKey
    : (layout.regions[0]?.key ?? null);

const renderClassifierTerrainRegions = (
  layout: ClassifierTerrainLayout,
  descriptor: ClassifierDescriptor,
): void => {
  const focusedRegion = terrainRegions.contains(document.activeElement)
    ? (document.activeElement as HTMLElement).dataset.region
    : undefined;
  const tabStopKey = classifierTerrainTabStopKey(layout);
  const sectionLabels = layout.sections.map((section) => {
    const label = document.createElement("div");
    label.className = `terrain-section-label ${section.key}`;
    label.style.left = `${(section.rect.x / layout.width) * 100}%`;
    label.style.top = `${(section.rect.y / layout.height) * 100}%`;
    label.style.width = `${(section.rect.width / layout.width) * 100}%`;
    label.style.height = `${section.labelHeight}px`;
    const name = document.createElement("strong");
    name.textContent = classifierSectionLabel(section.key);
    const count = document.createElement("span");
    count.textContent = countFormat.format(section.transactionCount);
    label.append(name, count);
    label.title = `${classifierSectionLabel(section.key)}: ${countFormat.format(section.transactionCount)} transactions, ${formatVsize(section.totalVsize)}`;
    return label;
  });
  const bucketLabels = layout.regions.map((region) => {
    const signature = region.signature;
    const label = document.createElement("button");
    label.type = "button";
    label.className = "terrain-region-label combination-region";
    label.dataset.region = signature.key;
    label.dataset.completeness = signature.state;
    label.dataset.matchesFilter = String(
      selectedClassifierBucketKey === null &&
        selectedClassifierLabel !== null &&
        classifierBucketContainsLabel(signature, selectedClassifierLabel),
    );
    label.style.left = `${(region.rect.x / layout.width) * 100}%`;
    label.style.top = `${(region.rect.y / layout.height) * 100}%`;
    label.style.width = `${(region.rect.width / layout.width) * 100}%`;
    label.style.height = `${region.labelHeight}px`;
    const labelVisible = bucketTerrainRegionCanShowLabel(region);
    label.dataset.compact = String(!labelVisible);
    const color = classifierBucketColor(descriptor, signature);
    label.style.setProperty("--foundation-color", color);
    label.style.setProperty(
      "--signature-accent",
      signature.state === "partial" ? "#e1aa4b" : color,
    );

    const identity = document.createElement("span");
    identity.className = "signature-identity";
    const name = document.createElement("strong");
    name.textContent = classifierBucketLabel(descriptor, signature);
    identity.append(name);
    if (signature.state === "partial") {
      const partial = document.createElement("span");
      partial.className = "signature-unresolved";
      partial.textContent = "Partial";
      identity.append(partial);
    }
    const count = document.createElement("span");
    count.className = "signature-count";
    count.textContent = countFormat.format(region.transactionCount);
    if (labelVisible) {
      label.append(identity, count);
    }
    const stateText =
      signature.state === "unavailable"
        ? "result unavailable"
        : `${signature.state} result`;
    label.title = `${classifierBucketLabel(descriptor, signature)}: ${countFormat.format(region.transactionCount)} transactions, ${formatVsize(region.totalVsize)}; ${stateText}`;
    label.setAttribute("aria-label", `${label.title}.`);
    label.setAttribute(
      "aria-pressed",
      String(selectedClassifierBucketKey === signature.key),
    );
    label.tabIndex = signature.key === tabStopKey ? 0 : -1;
    label.addEventListener("click", () => {
      selectClassifierBucket(signature.key, false);
    });
    return label;
  });
  const overlayElements = [...sectionLabels, ...bucketLabels].sort(
    (left, right) =>
      Number.parseFloat(left.style.top) - Number.parseFloat(right.style.top) ||
      Number.parseFloat(left.style.left) - Number.parseFloat(right.style.left),
  );
  terrainRegions.replaceChildren(...overlayElements);
  if (focusedRegion !== undefined) {
    terrainRegions
      .querySelector<HTMLButtonElement>(`[data-region="${focusedRegion}"]`)
      ?.focus();
  }
};

const renderTerrainFrame = (): void => {
  pendingTerrainFrame = null;
  if (
    currentSnapshot === null ||
    currentSnapshot.transaction_count === 0 ||
    terrainView.hidden
  ) {
    return;
  }
  if (selectedClassifierIsBip110()) {
    policyTerrainLayout = renderTerrain(
      terrainCanvas,
      currentSnapshot.transactions,
      terrainMode,
      selectedInspector,
      policyTerrainLayout,
      selectedTransactionId,
    );
    renderTerrainRegions(policyTerrainLayout);
    terrainCanvas.setAttribute(
      "aria-label",
      `BIP-110 rule-combination buckets for ${countFormat.format(currentSnapshot.transaction_count)} transactions. Complete violations appear once in their exact rule-set bucket; incomplete violations are separate. Bucket area represents ${terrainMode === "count" ? "transaction count" : "virtual size"}.`,
    );
    return;
  }
  const descriptor = currentClassifierDescriptor();
  if (descriptor === null) {
    return;
  }
  const hasFilter =
    selectedClassifierBucketKey !== null || selectedClassifierLabel !== null;
  const glyphMatchesSelectedLabel = (txid: string): boolean => {
    if (
      selectedClassifierBucketKey !== null ||
      selectedClassifierLabel === null
    ) {
      return false;
    }
    const transaction = currentTransaction(txid);
    return (
      transaction !== undefined &&
      (classificationResult(transaction, selectedClassifierId)?.labels.includes(
        selectedClassifierLabel,
      ) ??
        false)
    );
  };
  classifierTerrainLayout = renderBucketTerrain(
    terrainCanvas,
    classifierTerrainGroups(currentSnapshot.transactions, descriptor),
    terrainMode,
    {
      color: (region) => classifierBucketColor(descriptor, region.signature),
      selected: (region) =>
        selectedClassifierBucketKey !== null
          ? region.key === selectedClassifierBucketKey
          : selectedClassifierLabel !== null &&
            !classifierBucketIsSummary(region.signature) &&
            classifierBucketContainsLabel(
              region.signature,
              selectedClassifierLabel,
            ),
      partial: (region) => region.signature.state === "partial",
      glyphOpacity: (region, selected, glyph) =>
        region.signature.state === "unavailable"
          ? hasFilter
            ? 0.2
            : 0.82
          : selected
            ? 1
            : selectedClassifierBucketKey !== null
              ? 0.46
              : selectedClassifierLabel !== null
                ? glyphMatchesSelectedLabel(glyph.txid)
                  ? 1
                  : 0.2
                : 0.82,
    },
    classifierTerrainLayout,
    selectedTransactionId,
  );
  renderClassifierTerrainRegions(classifierTerrainLayout, descriptor);
  terrainCanvas.setAttribute(
    "aria-label",
    classifierUsesSummaryBuckets(descriptor)
      ? `${descriptor.title} presentation groups for ${countFormat.format(currentSnapshot.transaction_count)} transactions. Every transaction appears once in a broad script-profile and coverage-state group; exact properties remain available per transaction. Group area represents ${terrainMode === "count" ? "transaction count" : "virtual size"}.`
      : `${descriptor.title} buckets for ${countFormat.format(currentSnapshot.transaction_count)} transactions. Every transaction appears once in its exact observed label-set and coverage-state bucket. Bucket area represents ${terrainMode === "count" ? "transaction count" : "virtual size"}.`,
  );
};

const scheduleTerrainRender = (): void => {
  if (
    pendingTerrainFrame !== null ||
    currentSnapshot === null ||
    currentSnapshot.transaction_count === 0
  ) {
    return;
  }
  pendingTerrainFrame = window.requestAnimationFrame(renderTerrainFrame);
};

const renderFeeAgeFrame = (): void => {
  pendingFeeAgeFrame = null;
  if (
    currentSnapshot === null ||
    filteredTransactions.length === 0 ||
    feeAgeView.hidden
  ) {
    return;
  }
  const snapshot = currentSnapshot;
  const transactions = filteredTransactions;
  const controller = new AbortController();
  feeAgeRenderController = controller;
  void renderSwimViewCooperatively(
    feeAgeCanvas,
    transactions,
    snapshot.observed_at_ms,
    { signal: controller.signal },
  )
    .then((summary) => {
      if (
        controller.signal.aborted ||
        feeAgeRenderController !== controller ||
        currentSnapshot !== snapshot ||
        filteredTransactions !== transactions ||
        feeAgeView.hidden
      ) {
        return;
      }
      visualSummary.textContent = `${countFormat.format(summary.transactionCount)} transactions representing ${formatVsize(summary.totalVsize)}. Rows are base fee rate, columns and colour are age, and square area is virtual size.`;
      feeAgeCanvas.setAttribute(
        "aria-label",
        `Fee rate by age view containing ${countFormat.format(summary.transactionCount)} filtered transactions.`,
      );
    })
    .catch((error: unknown) => {
      if (
        !controller.signal.aborted &&
        !(error instanceof DOMException && error.name === "AbortError")
      ) {
        console.error("Unable to render fee-rate by age view", error);
      }
    })
    .finally(() => {
      if (feeAgeRenderController === controller) {
        feeAgeRenderController = null;
      }
    });
};

const scheduleFeeAgeRender = (): void => {
  if (currentSnapshot === null || filteredTransactions.length === 0) {
    return;
  }
  feeAgeRenderController?.abort();
  if (pendingFeeAgeFrame !== null) {
    window.cancelAnimationFrame(pendingFeeAgeFrame);
  }
  const callback = function feeAgeRenderFrame() {
    renderFeeAgeFrame();
  };
  Object.assign(callback, { __atlasPerfLabel: "fee-age-render" });
  pendingFeeAgeFrame = window.requestAnimationFrame(callback);
};

const selectCompositionSegment = (
  classifierId: string,
  segmentKey: string,
): void => {
  const snapshot = currentSnapshot;
  if (
    snapshot === null ||
    classifierId === KNOTS_BIP110_CLASSIFIER_ID ||
    !snapshot.classifier_catalog.some(({ id }) => id === classifierId)
  ) {
    return;
  }
  if (classifierId !== selectedClassifierId) {
    selectedClassifierId = classifierId;
    classificationLensSelect.value = classifierId;
    selectedClassifierLabel = null;
    resetTerrainLayouts();
    renderClassificationOverview();
    if (currentClassification !== null) {
      renderClassification(snapshot, currentClassification);
    }
    startSnapshotDistributionsRender();
  }
  selectLens("terrain");
  selectClassifierBucket(segmentKey as ClassifierBucketKey, false);
};

const snapshotDistributionsView = createSnapshotDistributionsView({
  onSelectBucket: ({ classifierId, bucketKey }) => {
    selectCompositionSegment(classifierId, bucketKey);
  },
});

const renderSnapshotDistributions = (): Promise<void> => {
  const snapshot = currentSnapshot;
  return snapshot === null || !snapshotIsComplete(snapshot)
    ? Promise.resolve()
    : snapshotDistributionsView.render(snapshot, {
        classifierId: selectedClassifierId,
        bucketKey: selectedClassifierBucketKey,
        metric: terrainMode,
      });
};

const startSnapshotDistributionsRender = (): void => {
  void renderSnapshotDistributions().catch((error) => {
    pageStatus.dataset.state = "error";
    statusTitle.textContent = "Distribution view unavailable";
    statusDetail.textContent =
      error instanceof Error ? error.message : "Unable to render distributions";
  });
};

const commitFilteredTransactions = (
  snapshot: MempoolSnapshot,
  transactions: MempoolTransaction[],
): void => {
  filteredTransactions = transactions;
  filterSummary.textContent = `Showing ${countFormat.format(filteredTransactions.length)} of ${countFormat.format(snapshot.transaction_count)} transactions.`;
  feeAgeStage.hidden = filteredTransactions.length === 0;
  feeAgeEmpty.hidden = filteredTransactions.length !== 0;
  feeAgeEmpty.textContent =
    snapshot.transaction_count === 0
      ? "This snapshot contains an empty mempool."
      : "No transactions match the current filters.";
  scheduleFeeAgeRender();
};

const applyFilters = async (): Promise<boolean> => {
  if (currentSnapshot === null || !snapshotIsComplete(currentSnapshot)) {
    return false;
  }
  filterController?.abort();
  filterController = null;
  const snapshot = currentSnapshot;
  const filters = readFilters();
  const controller = new AbortController();
  filterController = controller;
  filterSummary.textContent = "Applying membership filters…";
  try {
    const nextTransactions = await filterTransactionsCooperatively(
      snapshot.transactions,
      filters,
      snapshot.observed_at_ms,
      { signal: controller.signal },
    );
    if (
      controller.signal.aborted ||
      filterController !== controller ||
      currentSnapshot !== snapshot
    ) {
      return false;
    }
    commitFilteredTransactions(snapshot, nextTransactions);
    return true;
  } catch (error) {
    if (controller.signal.aborted) return false;
    throw error;
  } finally {
    if (filterController === controller) filterController = null;
  }
};

const startFilterInteraction = (): void => {
  void applyFilters().catch((error: unknown) => {
    pageStatus.dataset.state = "error";
    statusTitle.textContent = "Membership filters unavailable";
    statusDetail.textContent =
      error instanceof Error ? error.message : "Unable to filter this snapshot";
  });
};

const selectLens = (lens: Lens): void => {
  selectedLens = lens;
  const overviewSelected = lens === "overview";
  const terrainSelected = lens === "terrain";
  const feeAgeSelected = lens === "fee-age";
  overviewTab.setAttribute("aria-selected", String(overviewSelected));
  terrainTab.setAttribute("aria-selected", String(terrainSelected));
  feeAgeTab.setAttribute("aria-selected", String(feeAgeSelected));
  overviewTab.tabIndex = overviewSelected ? 0 : -1;
  terrainTab.tabIndex = terrainSelected ? 0 : -1;
  feeAgeTab.tabIndex = feeAgeSelected ? 0 : -1;
  overviewView.hidden = !overviewSelected;
  terrainView.hidden = !terrainSelected;
  feeAgeView.hidden = !feeAgeSelected;
  if (!feeAgeSelected) {
    feeAgeRenderController?.abort();
    feeAgeRenderController = null;
  }
  if (terrainSelected) {
    scheduleTerrainRender();
  } else if (feeAgeSelected) {
    scheduleFeeAgeRender();
  } else {
    renderClassificationOverview();
  }
  renderCoverage();
  renderInspector();
};

const selectInspector = (
  inspector: InspectorSelection,
  loadSample: boolean,
): void => {
  const changed = !selectionsMatch(selectedInspector, inspector);
  selectedInspector = inspector;
  nodeViewState = {
    ...nodeViewState,
    classifier: selectedClassifierId,
    selection: inspector,
  };
  if (changed) {
    clearDetail("Choose a sample");
  }
  renderInspector();
  scheduleTerrainRender();
  replaceViewUrl();
  if (loadSample && currentSnapshot !== null) {
    const first = populationForSelection(currentSnapshot, selectedInspector)
      ?.transactions[0];
    if (first !== undefined) {
      void loadTransactionDetail(first);
    }
  }
};

const selectClassifierBucket = (
  bucketKey: ClassifierBucketKey,
  loadSample: boolean,
): void => {
  const changed = selectedClassifierBucketKey !== bucketKey;
  selectedClassifierBucketKey = bucketKey;
  nodeViewState = {
    ...nodeViewState,
    classifier: selectedClassifierId,
    selection: null,
  };
  if (changed) {
    clearDetail("Choose a sample");
  }
  renderInspector();
  scheduleTerrainRender();
  replaceViewUrl();
  snapshotDistributionsView.setSelection(
    selectedClassifierId,
    selectedClassifierBucketKey,
  );
  if (loadSample) {
    const first = classifierBucketSelectionPopulation()?.transactions[0];
    if (first !== undefined) {
      void loadTransactionDetail(first);
    }
  }
};

const renderClassification = (
  snapshot: MempoolSnapshot,
  progress: ClassificationProgress,
): void => {
  terrainSummaryView.render(
    snapshot,
    currentClassifierDescriptor(),
    terrainMode,
  );
  renderCoverage();
  renderClassificationLifecycle(progress, snapshot.transaction_count);
};

const renderResponse = async (
  response: LoadedSourcePublication,
  complete: boolean,
  isCurrent: () => boolean,
  signal: AbortSignal,
): Promise<void> => {
  const replacesCompletePublication =
    complete && currentSnapshot !== null && snapshotIsComplete(currentSnapshot);
  const prepared = await prepareNodePublicationCommit(
    response,
    complete,
    replacesCompletePublication,
    signal,
    isCurrent,
    () => ({
      viewState: nodeViewState,
      terrainMode,
      filters: readFilters(),
      currentSnapshotIdentity,
      selectedClassifierLabel,
      selectedClassifierBucketKey,
      selectedInspector,
    }),
    snapshotDistributionsView,
  );
  const {
    candidate,
    distributionSelection,
    distributions,
    filteredTransactions: preparedFilteredTransactions,
  } = prepared;
  const { source, snapshot, requestedTransaction } = candidate;

  let completeFilteredTransactions: MempoolTransaction[] | null = null;
  if (complete) {
    if (preparedFilteredTransactions === null || distributions === null) {
      throw new Error("Prepared complete node view is incomplete");
    }
    if (
      !snapshotDistributionsView.commit(
        distributions,
        snapshot,
        distributionSelection,
      )
    ) {
      throw new Error("Prepared complete node view became stale");
    }
    completeFilteredTransactions = preparedFilteredTransactions;
  }

  pageStatus.dataset.state = source.availability;
  sourceSelect.value = source.source_id;
  sourceSummaryView.renderSnapshot(source, snapshot);
  filterController?.abort();
  filterController = null;
  currentSnapshot = snapshot;
  currentSnapshotIdentity = candidate.snapshotIdentity;
  currentClassification = candidate.classification;
  selectedClassifierId = candidate.selectedClassifierId;
  selectedClassifierLabel = candidate.selectedClassifierLabel;
  selectedClassifierBucketKey = candidate.selectedClassifierBucketKey;
  setMembershipControlsComplete(complete);
  resetTerrainLayouts();
  terrainStage.hidden = snapshot.transaction_count === 0;
  terrainEmpty.hidden = snapshot.transaction_count !== 0;
  terrainEmpty.textContent = "This snapshot contains an empty mempool.";
  selectedInspector = candidate.selectedInspector;
  nodeViewState = candidate.viewState;
  transactionSearchInput.value = candidate.viewState.txid ?? "";
  if (completeFilteredTransactions !== null) {
    commitFilteredTransactions(snapshot, completeFilteredTransactions);
    clearDetail("Choose a sample", false);
    setTransactionSearchStatus("Search this snapshot by txid.");
  } else {
    selectedTransactionId = requestedTransaction?.txid ?? null;
    feeAgeStage.hidden = true;
    feeAgeEmpty.hidden = false;
    feeAgeEmpty.textContent = "Loading membership and fee data…";
    filterSummary.textContent = "Loading membership filters…";
    snapshotDistributionsView.reset("Loading remaining snapshot stages…");
    setTransactionSearchStatus(
      requestedTransaction === undefined
        ? candidate.viewState.txid === null
          ? "Search this snapshot by txid."
          : "Not present in this snapshot."
        : "Present in this snapshot.",
      requestedTransaction === undefined
        ? candidate.viewState.txid === null
          ? undefined
          : "absent"
        : "found",
    );
    detailStatus.textContent =
      requestedTransaction === undefined
        ? "Loading membership stage…"
        : "Present · loading membership stage…";
    detailTransaction.replaceChildren(
      ...(requestedTransaction === undefined
        ? []
        : [detailValue("txid", requestedTransaction.txid)]),
    );
    detailClassifiers.replaceChildren();
    detailRules.replaceChildren();
  }
  renderClassificationOverview();
  renderClassification(snapshot, currentClassification);
  renderInspector();
  if (!complete) {
    statusTitle.textContent = "Snapshot classifications ready";
    statusDetail.textContent =
      "Search and the selected classifier are interactive while membership and remaining lenses load.";
  } else if (source.availability === "stale") {
    statusTitle.textContent = "Showing the last good snapshot";
    statusDetail.textContent = `Observed ${new Date(snapshot.observed_at_ms).toLocaleString()}. Latest poll failed: ${source.last_error ?? "unknown error"}`;
  } else {
    statusTitle.textContent = "Snapshot healthy";
    statusDetail.textContent = `Observed ${new Date(snapshot.observed_at_ms).toLocaleString()}. Browser refresh does not trigger a node poll.`;
  }
  scheduleTerrainRender();
  replaceViewUrl();
  setAtlasLoadPhase(pageStatus, "interactive");
  if (replacesCompletePublication) {
    recordAtlasCandidateCommitted(prepared.detail);
  }
  const readiness = markAtlasReadinessAfterPaint(
    pageStatus,
    "node",
    complete ? "complete-feature-ready" : "primary-interactive",
    () => isCurrent() && currentSnapshot === snapshot,
  );

  if (complete && requestedTransaction !== undefined) {
    void loadTransactionDetail(requestedTransaction);
  } else if (complete && candidate.viewState.txid !== null) {
    showAbsentTransaction(candidate.viewState.txid);
  }
  return readiness;
};

const discoverSources = async (): Promise<void> => {
  const response = await fetchSources();
  atlasVersion.textContent = `v${response.atlas_version}`;
  if (response.sources.length === 0) {
    throw new Error("Atlas has no configured Bitcoin source");
  }
  configuredSources = response.sources;
  const options = configuredSources.map((source) => {
    const option = document.createElement("option");
    option.value = source.source_id;
    option.textContent = source.source_label;
    return option;
  });
  sourceSelect.replaceChildren(...options);
  selectedSourceId =
    configuredSources.find(
      ({ source_id: source }) => source === initialViewState.source,
    )?.source_id ??
    configuredSources[0]?.source_id ??
    null;
  if (selectedSourceId === null) {
    throw new Error("Atlas has no configured Bitcoin source");
  }
  sourceSelect.value = selectedSourceId;
  sourceSelect.disabled = configuredSources.length < 2;
  updateCompareLink();
  const selectedSource = configuredSources.find(
    ({ source_id: source }) => source === selectedSourceId,
  );
  if (selectedSource === undefined) {
    throw new Error("Selected Bitcoin source is unavailable");
  }
  sourceSummaryView.renderMetadata(selectedSource);
  markAtlasReadiness(pageStatus, "node", "metadata-usable");
  pageStatus.dataset.state = "waiting";
  statusTitle.textContent = "Source metadata ready";
  statusDetail.textContent = `Loading the complete snapshot for ${selectedSource.source_label}.`;
  setAtlasLoadPhase(pageStatus, "metadata-ready");
};

const loadSnapshot = async (): Promise<void> => {
  const requestedSourceId = selectedSourceId;
  if (requestedSourceId === null) {
    return;
  }
  const ticket = snapshotLifecycle.begin();
  const retainActivePublication =
    currentSnapshot !== null && snapshotIsComplete(currentSnapshot);
  refreshButton.disabled = true;
  refreshButton.textContent = "Loading…";
  sourceSummaryView.setBusy(true);
  setAtlasLoadPhase(pageStatus, "loading-snapshot");
  try {
    const response = await fetchSourcePublication(
      requestedSourceId,
      ticket.signal,
      nodeViewState.classifier ?? DEFAULT_CLASSIFIER_ID,
      (primary) => {
        if (
          snapshotLifecycle.isCurrent(ticket) &&
          selectedSourceId === requestedSourceId
        ) {
          if (retainActivePublication) return undefined;
          return renderPrimaryNodePublication(() =>
            renderResponse(
              primary,
              false,
              () =>
                snapshotLifecycle.isCurrent(ticket) &&
                selectedSourceId === requestedSourceId,
              ticket.signal,
            ),
          );
        }
        return undefined;
      },
    );
    if (
      !snapshotLifecycle.isCurrent(ticket) ||
      selectedSourceId !== requestedSourceId
    ) {
      return;
    }
    setAtlasLoadPhase(pageStatus, "deriving-view");
    await renderResponse(
      response,
      true,
      () =>
        snapshotLifecycle.isCurrent(ticket) &&
        selectedSourceId === requestedSourceId,
      ticket.signal,
    );
  } catch (error) {
    if (!snapshotLifecycle.isCurrent(ticket)) {
      return;
    }
    let source = configuredSources.find(
      ({ source_id: source }) => source === requestedSourceId,
    );
    const refreshed = await firstPublication.refreshSources(
      error,
      currentSnapshot !== null,
      ticket.signal,
    );
    if (refreshed !== undefined) {
      if (!snapshotLifecycle.isCurrent(ticket)) return;
      if (refreshed === null) {
        source = undefined;
      } else {
        configuredSources = refreshed.sources;
        source = refreshed.sources.find(
          ({ source_id: sourceId }) => sourceId === requestedSourceId,
        );
      }
    }
    const firstFailure =
      currentSnapshot === null
        ? firstPublication.presentation(error, source)
        : null;
    pageStatus.dataset.state = firstFailure?.state ?? "error";
    const message =
      error instanceof Error ? error.message : "Unable to load snapshot";
    const partialAvailable =
      currentSnapshot !== null && !snapshotIsComplete(currentSnapshot);
    statusTitle.textContent =
      firstFailure?.title ??
      (partialAvailable
        ? "Snapshot partly available"
        : "Refresh failed · showing the prior snapshot");
    statusDetail.textContent =
      firstFailure?.detail ??
      (partialAvailable
        ? `The selected classifier and transaction search remain available. Membership-dependent features did not finish loading: ${message}`
        : `The prior complete snapshot remains usable. Refresh failed: ${message}`);
    if (currentSnapshot === null) {
      terrainEmpty.textContent = statusDetail.textContent;
      feeAgeEmpty.textContent = statusDetail.textContent;
      snapshotDistributionsView.reset(statusDetail.textContent);
      if (source === undefined) {
        sourceSummaryView.renderDiscoveryFailure();
      } else {
        sourceSummaryView.renderMetadata(source, false);
      }
    } else {
      sourceSummaryView.setBusy(false);
    }
    setAtlasLoadPhase(
      pageStatus,
      currentSnapshot === null && source === undefined
        ? "discovering-sources"
        : currentSnapshot === null
          ? "metadata-ready"
          : "interactive",
    );
  } finally {
    if (snapshotLifecycle.isCurrent(ticket)) {
      refreshButton.disabled = false;
      refreshButton.textContent = "Refresh";
    }
  }
};

const initialize = async (): Promise<void> => {
  refreshButton.disabled = true;
  sourceSelect.disabled = true;
  sourceSummaryView.renderDiscovering();
  setAtlasLoadPhase(pageStatus, "discovering-sources");
  try {
    await discoverSources();
    selectedClassifierId =
      nodeViewState.classifier ??
      (nodeViewState.selection === null
        ? DEFAULT_CLASSIFIER_ID
        : KNOTS_BIP110_CLASSIFIER_ID);
    nodeViewState = {
      ...nodeViewState,
      source: selectedSourceId,
      classifier: selectedClassifierId,
      selection: selectedClassifierIsBip110() ? nodeViewState.selection : null,
    };
    selectedInspector =
      nodeViewState.selection ??
      ({ kind: "rule", rule: "element_size" } as const);
    transactionSearchInput.value = nodeViewState.txid ?? "";
    replaceViewUrl();
    await loadSnapshot();
  } catch (error) {
    pageStatus.dataset.state = "error";
    sourceSummaryView.renderDiscoveryFailure();
    statusTitle.textContent = "Atlas website unavailable";
    statusDetail.textContent =
      error instanceof Error ? error.message : "Unable to load snapshot";
    setAtlasLoadPhase(pageStatus, "discovering-sources");
    refreshButton.disabled = false;
    refreshButton.textContent = "Refresh";
  }
};

const prepareForSourceLoad = (source: SourceSummary): void => {
  filterController?.abort();
  filterController = null;
  nodeViewState = {
    source: source.source_id,
    classifier: selectedClassifierId,
    selection: null,
    txid: null,
  };
  currentSnapshot = null;
  setMembershipControlsComplete(false);
  currentSnapshotIdentity = null;
  currentClassification = null;
  filteredTransactions = [];
  selectedClassifierLabel = null;
  selectedClassifierBucketKey = null;
  if (selectedLens === "fee-age") selectLens("overview");
  resetTerrainLayouts();
  terrainStage.hidden = true;
  feeAgeStage.hidden = true;
  terrainEmpty.hidden = false;
  feeAgeEmpty.hidden = false;
  terrainEmpty.textContent = "Loading this node's current snapshot.";
  feeAgeEmpty.textContent = terrainEmpty.textContent;
  sourceSummaryView.renderMetadata(source);
  markAtlasReadiness(pageStatus, "node", "metadata-usable");
  terrainSummaryView.reset();
  coverageComplete.textContent = "0";
  coverageIncomplete.textContent = "0";
  coverageUnclassified.textContent = "0";
  renderCoverage();
  renderClassificationLifecycle(null, null);
  renderClassificationOverview();
  snapshotDistributionsView.reset("Loading this node's current snapshot.");
  filterSummary.textContent = "No snapshot loaded.";
  selectedInspector = { kind: "rule", rule: "element_size" };
  clearDetail("Choose a sample");
  renderInspector();
  pageStatus.dataset.state = "waiting";
  statusTitle.textContent = "Loading node snapshot";
  statusDetail.textContent = `Source metadata is ready. Reading the latest complete snapshot for ${source.source_label}.`;
  setAtlasLoadPhase(pageStatus, "metadata-ready");
  replaceViewUrl();
};

const searchForTransaction = (): void => {
  const raw = transactionSearchInput.value.trim();
  const params = new URLSearchParams({ txid: raw });
  const txid = parseNodeViewState(params).txid;
  if (txid === null) {
    setTransactionSearchStatus(
      "Enter a complete 64-character hexadecimal transaction ID.",
      "error",
    );
    return;
  }
  transactionSearchInput.value = txid;
  if (currentSnapshot === null) {
    nodeViewState = { ...nodeViewState, txid };
    selectedTransactionId = null;
    detailSequence += 1;
    detailStatus.textContent = "No snapshot available";
    detailTransaction.replaceChildren(detailValue("txid", txid));
    detailClassifiers.replaceChildren();
    detailRules.replaceChildren();
    setTransactionSearchStatus(
      "No snapshot is available to search yet.",
      "absent",
    );
    replaceViewUrl();
    return;
  }
  const transaction = currentTransaction(txid);
  if (transaction === undefined) {
    showAbsentTransaction(txid);
    return;
  }
  void loadTransactionDetail(transaction);
};

overviewTab.addEventListener("click", () => {
  selectLens("overview");
});

terrainTab.addEventListener("click", () => {
  selectLens("terrain");
});

feeAgeTab.addEventListener("click", () => {
  selectLens("fee-age");
});

const lensTabs = [overviewTab, terrainTab, feeAgeTab] as const;
const lensForTab = (tab: HTMLButtonElement): Lens =>
  tab === overviewTab ? "overview" : tab === terrainTab ? "terrain" : "fee-age";

for (const tab of lensTabs) {
  tab.addEventListener("keydown", (event) => {
    if (
      event.key !== "ArrowLeft" &&
      event.key !== "ArrowRight" &&
      event.key !== "Home" &&
      event.key !== "End"
    ) {
      return;
    }
    event.preventDefault();
    const currentIndex = lensTabs.indexOf(tab);
    const nextIndex =
      event.key === "Home"
        ? 0
        : event.key === "End"
          ? lensTabs.length - 1
          : (currentIndex +
              (event.key === "ArrowLeft" ? -1 : 1) +
              lensTabs.length) %
            lensTabs.length;
    const next = lensTabs[nextIndex]!;
    selectLens(lensForTab(next));
    next.focus();
  });
}

classificationLensSelect.addEventListener("change", () => {
  const snapshot = currentSnapshot;
  if (
    snapshot === null ||
    !snapshot.classifier_catalog.some(
      ({ id }) => id === classificationLensSelect.value,
    )
  ) {
    return;
  }
  selectedClassifierId = classificationLensSelect.value;
  selectedClassifierLabel = null;
  selectedClassifierBucketKey = null;
  resetTerrainLayouts();
  if (selectedClassifierIsBip110()) {
    selectedInspector = {
      kind: "rule",
      rule: chooseInitialNodeRule(snapshot),
    };
  }
  clearDetail("Choose a sample");
  nodeViewState = {
    ...nodeViewState,
    classifier: selectedClassifierId,
    selection: selectedClassifierIsBip110() ? selectedInspector : null,
  };
  renderClassificationOverview();
  if (currentClassification !== null) {
    renderClassification(snapshot, currentClassification);
  }
  renderInspector();
  scheduleTerrainRender();
  startSnapshotDistributionsRender();
  replaceViewUrl();
});

modeCount.addEventListener("click", () => {
  terrainMode = "count";
  resetTerrainLayouts();
  modeCount.setAttribute("aria-pressed", "true");
  modeVsize.setAttribute("aria-pressed", "false");
  if (currentSnapshot !== null && currentClassification !== null) {
    renderClassification(currentSnapshot, currentClassification);
  }
  scheduleTerrainRender();
  startSnapshotDistributionsRender();
});

modeVsize.addEventListener("click", () => {
  terrainMode = "vsize";
  resetTerrainLayouts();
  modeCount.setAttribute("aria-pressed", "false");
  modeVsize.setAttribute("aria-pressed", "true");
  if (currentSnapshot !== null && currentClassification !== null) {
    renderClassification(currentSnapshot, currentClassification);
  }
  scheduleTerrainRender();
  startSnapshotDistributionsRender();
});

const moveRuleFocus = (event: KeyboardEvent, container: HTMLElement): void => {
  if (
    event.key !== "ArrowLeft" &&
    event.key !== "ArrowRight" &&
    event.key !== "ArrowUp" &&
    event.key !== "ArrowDown" &&
    event.key !== "Home" &&
    event.key !== "End"
  ) {
    return;
  }
  event.preventDefault();
  if (!selectedClassifierIsBip110()) {
    const buttons = [
      ...container.querySelectorAll<HTMLButtonElement>("button[data-label]"),
    ];
    if (buttons.length === 0) {
      return;
    }
    const activeIndex = Math.max(
      0,
      buttons.indexOf(document.activeElement as HTMLButtonElement),
    );
    const nextIndex =
      event.key === "Home"
        ? 0
        : event.key === "End"
          ? buttons.length - 1
          : (activeIndex +
              (event.key === "ArrowLeft" || event.key === "ArrowUp" ? -1 : 1) +
              buttons.length) %
            buttons.length;
    const next = buttons[nextIndex];
    const labelKey = next?.dataset.label;
    if (next !== undefined && labelKey !== undefined) {
      selectClassifierLabel(labelKey, false);
      next.focus();
    }
    return;
  }
  const keys = inspectorRules();
  const activeRule = (document.activeElement as HTMLElement | null)?.dataset
    .rule as RuleId | undefined;
  const current = Math.max(
    0,
    keys.indexOf(
      activeRule ??
        (selectedInspector.kind === "rule"
          ? selectedInspector.rule
          : (keys[0] ?? "output_size")),
    ),
  );
  const nextIndex =
    event.key === "Home"
      ? 0
      : event.key === "End"
        ? keys.length - 1
        : (current +
            (event.key === "ArrowLeft" || event.key === "ArrowUp" ? -1 : 1) +
            keys.length) %
          keys.length;
  const rule = keys[nextIndex];
  if (rule !== undefined) {
    selectInspector({ kind: "rule", rule }, false);
    container
      .querySelector<HTMLButtonElement>(`[data-rule="${rule}"]`)
      ?.focus();
  }
};

ruleList.addEventListener("keydown", (event) => {
  moveRuleFocus(event, ruleList);
});

terrainRegions.addEventListener("keydown", (event) => {
  if (
    event.key !== "ArrowLeft" &&
    event.key !== "ArrowRight" &&
    event.key !== "ArrowUp" &&
    event.key !== "ArrowDown" &&
    event.key !== "Home" &&
    event.key !== "End"
  ) {
    return;
  }
  const buttons = [
    ...terrainRegions.querySelectorAll<HTMLButtonElement>(
      "button[data-region]",
    ),
  ];
  if (buttons.length === 0) {
    return;
  }
  event.preventDefault();
  const activeIndex = Math.max(
    0,
    buttons.indexOf(document.activeElement as HTMLButtonElement),
  );
  const nextIndex =
    event.key === "Home"
      ? 0
      : event.key === "End"
        ? buttons.length - 1
        : (activeIndex +
            (event.key === "ArrowLeft" || event.key === "ArrowUp" ? -1 : 1) +
            buttons.length) %
          buttons.length;
  const next = buttons[nextIndex];
  const regionKey = next?.dataset.region;
  if (next !== undefined && regionKey !== undefined) {
    if (selectedClassifierIsBip110()) {
      selectInspector(
        { kind: "region", regionKey: regionKey as TerrainRegionKey },
        false,
      );
    } else {
      selectClassifierBucket(regionKey as ClassifierBucketKey, false);
    }
    next.focus();
  }
});

terrainCanvas.addEventListener("click", (event) => {
  const bounds = terrainCanvas.getBoundingClientRect();
  const x = event.clientX - bounds.left;
  const y = event.clientY - bounds.top;
  if (selectedClassifierIsBip110()) {
    if (policyTerrainLayout === null) {
      return;
    }
    const hit = hitTestTerrain(policyTerrainLayout, x, y);
    if (hit?.kind === "region") {
      selectInspector({ kind: "region", regionKey: hit.region.key }, false);
      return;
    }
    if (hit?.kind === "transaction") {
      selectInspector(
        { kind: "region", regionKey: hit.glyph.regionKey },
        false,
      );
      const transaction = currentTransaction(hit.glyph.txid);
      if (transaction !== undefined) {
        void loadTransactionDetail(transaction);
      }
    }
    return;
  }
  if (classifierTerrainLayout === null) {
    return;
  }
  const hit = hitTestBucketTerrain(classifierTerrainLayout, x, y);
  if (hit?.kind === "region") {
    selectClassifierBucket(hit.region.key, false);
    return;
  }
  if (hit?.kind === "transaction") {
    selectClassifierBucket(hit.glyph.regionKey, false);
    const transaction = currentTransaction(hit.glyph.txid);
    if (transaction !== undefined) {
      void loadTransactionDetail(transaction);
    }
  }
});

terrainCanvas.addEventListener("keydown", (event) => {
  if (event.key === "Enter" || event.key === " ") {
    event.preventDefault();
    const tabStop = selectedClassifierIsBip110()
      ? policyTerrainLayout === null
        ? null
        : terrainTabStopKey(policyTerrainLayout)
      : classifierTerrainLayout === null
        ? null
        : classifierTerrainTabStopKey(classifierTerrainLayout);
    if (tabStop !== null) {
      terrainRegions
        .querySelector<HTMLButtonElement>(`[data-region="${tabStop}"]`)
        ?.focus();
    }
  }
});

filtersForm.addEventListener("submit", (event) => {
  event.preventDefault();
  startFilterInteraction();
});

resetFilters.addEventListener("click", () => {
  minimumFeeRate.value = String(DEFAULT_FILTERS.minimumFeeRate);
  maximumAge.value = "all";
  minimumVsize.value = String(DEFAULT_FILTERS.minimumVsize);
  startFilterInteraction();
});

sourceSelect.addEventListener("change", () => {
  const nextSource = configuredSources.find(
    ({ source_id: source }) => source === sourceSelect.value,
  );
  if (nextSource === undefined || nextSource.source_id === selectedSourceId) {
    return;
  }
  selectedSourceId = nextSource.source_id;
  snapshotLifecycle.invalidate();
  prepareForSourceLoad(nextSource);
  void loadSnapshot();
});

transactionSearch.addEventListener("submit", (event) => {
  event.preventDefault();
  searchForTransaction();
});

refreshButton.addEventListener("click", () => {
  if (selectedSourceId === null) {
    void initialize();
    return;
  }
  void loadSnapshot();
});

new ResizeObserver(scheduleTerrainRender).observe(terrainCanvas);
new ResizeObserver(scheduleFeeAgeRender).observe(feeAgeCanvas);
selectLens(selectedLens);
renderInspector();
void initialize();
