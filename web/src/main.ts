import {
  AtlasRequestError,
  fetchSources,
  fetchSourceSnapshot,
  fetchTransactionDetail,
  transactionDetailMatchesSnapshot,
} from "./api";
import {
  DEFAULT_FILTERS,
  filterTransactions,
  type MempoolFilters,
} from "./filters";
import { RequestLifecycle } from "./comparison-lifecycle";
import { formatMembershipAge } from "./membership-table";
import { renderSwimView } from "./swim-view";
import {
  TERRAIN_RULES,
  classificationTotals,
  hitTestTerrain,
  rulePopulation,
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
import "./styles.css";
import type {
  Bip110Assessment,
  MempoolSnapshot,
  MempoolTransaction,
  RuleId,
  RuleAssessment,
  SourceSnapshotResponse,
  SourceSummary,
  TransactionDetailResponse,
} from "./types";

const requiredElement = <T extends HTMLElement>(id: string): T => {
  const element = document.getElementById(id);
  if (!(element instanceof HTMLElement)) {
    throw new Error(`Missing required element #${id}`);
  }
  return element as T;
};

const pageStatus = requiredElement<HTMLElement>("page-status");
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
const sourceLabel = requiredElement<HTMLElement>("source-label");
const sourceId = requiredElement<HTMLElement>("source-id");
const observedValue = requiredElement<HTMLElement>("observed-value");
const tipValue = requiredElement<HTMLElement>("tip-value");
const transactionCount = requiredElement<HTMLElement>("transaction-count");
const totalVsize = requiredElement<HTMLElement>("total-vsize");
const terrainTab = requiredElement<HTMLButtonElement>("terrain-tab");
const feeAgeTab = requiredElement<HTMLButtonElement>("fee-age-tab");
const terrainView = requiredElement<HTMLElement>("terrain-view");
const feeAgeView = requiredElement<HTMLElement>("fee-age-view");
const coverageComplete = requiredElement<HTMLElement>("coverage-complete");
const coverageIncomplete = requiredElement<HTMLElement>("coverage-incomplete");
const coverageUnclassified = requiredElement<HTMLElement>(
  "coverage-unclassified",
);
const compatibleCount = requiredElement<HTMLElement>("compatible-count");
const indeterminateCount = requiredElement<HTMLElement>("indeterminate-count");
const violatingCount = requiredElement<HTMLElement>("violating-count");
const terrainStage = requiredElement<HTMLElement>("terrain-stage");
const terrainCanvas = requiredElement<HTMLCanvasElement>("terrain-canvas");
const terrainRegions = requiredElement<HTMLElement>("terrain-regions");
const terrainEmpty = requiredElement<HTMLElement>("terrain-empty");
const terrainSummary = requiredElement<HTMLElement>("terrain-summary");
const modeCount = requiredElement<HTMLButtonElement>("mode-count");
const modeVsize = requiredElement<HTMLButtonElement>("mode-vsize");
const inspectorEyebrow = requiredElement<HTMLElement>("inspector-eyebrow");
const inspectorName = requiredElement<HTMLElement>("inspector-name");
const inspectorDescription = requiredElement<HTMLElement>(
  "inspector-description",
);
const ruleCount = requiredElement<HTMLElement>("rule-count");
const ruleVsize = requiredElement<HTMLElement>("rule-vsize");
const ruleShare = requiredElement<HTMLElement>("rule-share");
const ruleList = requiredElement<HTMLElement>("rule-list");
const sampleSummary = requiredElement<HTMLElement>("sample-summary");
const ruleTransactions =
  requiredElement<HTMLTableSectionElement>("rule-transactions");
const detailStatus = requiredElement<HTMLElement>("detail-status");
const detailTransaction = requiredElement<HTMLElement>("detail-transaction");
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

const countFormat = new Intl.NumberFormat();
const decimalFormat = new Intl.NumberFormat(undefined, {
  maximumFractionDigits: 2,
});
const percentageFormat = new Intl.NumberFormat(undefined, {
  maximumFractionDigits: 2,
  style: "percent",
});

type Lens = "terrain" | "fee-age";
type InspectorSelection = TerrainSelection;

const initialViewState = parseNodeViewState(window.location.search);
const snapshotLifecycle = new RequestLifecycle();

let configuredSources: SourceSummary[] = [];
let selectedSourceId: string | null = null;
let currentSnapshot: MempoolSnapshot | null = null;
let transactionById = new Map<string, MempoolTransaction>();
let filteredTransactions: MempoolTransaction[] = [];
let selectedLens: Lens = "terrain";
let selectedInspector: InspectorSelection = {
  kind: "rule",
  rule: "element_size",
};
let terrainMode: TerrainMode = "count";
let terrainLayout: TerrainLayout | null = null;
let pendingTerrainFrame: number | null = null;
let pendingFeeAgeFrame: number | null = null;
let detailSequence = 0;
let selectedTransactionId: string | null = null;
let nodeViewState: NodeViewState = initialViewState;

const formatVsize = (value: number): string => {
  if (value >= 1_000_000_000) {
    return `${decimalFormat.format(value / 1_000_000_000)} GvB`;
  }
  if (value >= 1_000_000) {
    return `${decimalFormat.format(value / 1_000_000)} MvB`;
  }
  if (value >= 1_000) {
    return `${decimalFormat.format(value / 1_000)} kvB`;
  }
  return `${countFormat.format(value)} vB`;
};

const formatPollInterval = (seconds: number): string => {
  if (seconds % 3_600 === 0) {
    const hours = seconds / 3_600;
    return `${countFormat.format(hours)} hour${hours === 1 ? "" : "s"}`;
  }
  if (seconds % 60 === 0) {
    const minutes = seconds / 60;
    return `${countFormat.format(minutes)} minute${minutes === 1 ? "" : "s"}`;
  }
  return `${countFormat.format(seconds)} second${seconds === 1 ? "" : "s"}`;
};

const formatSnapshotFreshness = (observedAtMs: number): string => {
  const ageMs = Math.max(0, Date.now() - observedAtMs);
  if (ageMs < 60_000) {
    return "just now";
  }
  return `${formatMembershipAge(ageMs)} ago`;
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
  detailSequence += 1;
  selectedTransactionId = null;
  if (clearAddressedTransaction) {
    nodeViewState = { ...nodeViewState, txid: null };
    transactionSearchInput.value = "";
    setTransactionSearchStatus("Search this snapshot by txid.");
  }
  detailStatus.textContent = message;
  detailTransaction.replaceChildren();
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
    empty.textContent = "Pending";
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

const renderTransactionDetail = (detail: TransactionDetailResponse): void => {
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
  detailRules.replaceChildren(...detail.rules.map(renderRuleDetail));
};

const showAbsentTransaction = (txid: string): void => {
  detailSequence += 1;
  nodeViewState = { ...nodeViewState, txid };
  selectedTransactionId = null;
  transactionSearchInput.value = txid;
  setTransactionSearchStatus(
    "This transaction is not present in this snapshot.",
    "absent",
  );
  detailStatus.textContent = "Not present in this snapshot";
  detailTransaction.replaceChildren(detailValue("txid", txid));
  const message = document.createElement("p");
  message.className = "detail-error";
  message.textContent =
    "Atlas did not observe this transaction in the selected node's current mempool snapshot.";
  detailTransaction.append(message);
  detailRules.replaceChildren();
  renderSampleTable();
  scheduleTerrainRender();
  replaceViewUrl();
};

const loadTransactionDetail = async (
  transaction: MempoolTransaction,
): Promise<void> => {
  const snapshot = currentSnapshot;
  if (snapshot === null) {
    return;
  }
  const sequence = ++detailSequence;
  const transactionSelection: InspectorSelection = {
    kind: "region",
    regionKey: terrainRegionKey(transaction),
  };
  const selectionChanged = !selectionsMatch(
    selectedInspector,
    transactionSelection,
  );
  selectedInspector = transactionSelection;
  selectedTransactionId = transaction.txid;
  nodeViewState = {
    source: selectedSourceId,
    selection: transactionSelection,
    txid: transaction.txid,
  };
  transactionSearchInput.value = transaction.txid;
  setTransactionSearchStatus("Present in this snapshot.", "found");
  detailStatus.textContent = "Loading rule evidence…";
  detailTransaction.replaceChildren(
    detailValue("txid", transaction.txid),
    detailValue("wtxid", transaction.wtxid),
  );
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
    if (sequence !== detailSequence) {
      return;
    }
    if (error instanceof AtlasRequestError && error.status === 503) {
      detailStatus.textContent = "Present, not classified";
      const message = document.createElement("p");
      message.className = "detail-unclassified";
      message.textContent =
        "This transaction is present in the snapshot, but no complete policy assessment is available. Atlas has no rule evidence to show.";
      detailRules.replaceChildren();
      detailTransaction.append(message);
      return;
    }
    detailStatus.textContent = "Detail unavailable";
    const message = document.createElement("p");
    message.className = "detail-error";
    message.textContent =
      error instanceof Error ? error.message : "Unable to load rule evidence";
    detailRules.replaceChildren();
    detailTransaction.append(message);
  }
};

const populationForSelection = (
  snapshot: MempoolSnapshot,
  selection: InspectorSelection,
) => {
  if (selection.kind === "rule") {
    return rulePopulation(snapshot.transactions, selection.rule);
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

const renderSampleTable = (): void => {
  if (currentSnapshot === null) {
    ruleTransactions.replaceChildren();
    sampleSummary.textContent = "No snapshot";
    return;
  }
  const population = populationForSelection(currentSnapshot, selectedInspector);
  const largest = population?.transactions.slice(0, 8) ?? [];
  const selected =
    population === null || selectedTransactionId === null
      ? undefined
      : population.transactions.find(
          ({ txid }) => txid === selectedTransactionId,
        );
  const pinsSelected =
    selected !== undefined &&
    !largest.some(({ txid }) => txid === selected.txid);
  const sample = pinsSelected ? [selected, ...largest.slice(0, 7)] : largest;
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
    rulesCell.append(transactionRuleChips(transaction));
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

const renderRuleNavigation = (): void => {
  if (currentSnapshot === null) {
    ruleList.replaceChildren();
    return;
  }
  const selectedSignature = signatureForSelection(
    currentSnapshot,
    selectedInspector,
  );
  const navigationTabRule =
    selectedInspector.kind === "rule"
      ? selectedInspector.rule
      : (selectedSignature?.foundationRule ?? TERRAIN_RULES[0]?.id);
  const buttons = TERRAIN_RULES.map((rule) => {
    const population = rulePopulation(
      currentSnapshot?.transactions ?? [],
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
      selectInspector({ kind: "rule", rule: rule.id }, true);
    });
    return button;
  });
  ruleList.replaceChildren(...buttons);
};

const renderInspector = (): void => {
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
      inspectorName.textContent = "Not classified";
      inspectorDescription.textContent =
        "Transactions present in this mempool snapshot without a policy assessment yet.";
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
  renderSampleTable();
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
  return "Not classified";
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
          selectInspector({ kind: "region", regionKey: statusKey }, true);
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
    label.dataset.matchesRule = String(
      selectedInspector.kind === "rule" &&
        signature.violatedRules.includes(selectedInspector.rule),
    );
    label.style.left = `${(region.rect.x / layout.width) * 100}%`;
    label.style.top = `${(region.rect.y / layout.height) * 100}%`;
    label.style.width = `${(region.rect.width / layout.width) * 100}%`;
    label.style.height = `${region.labelHeight}px`;
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
    label.append(identity, count);
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
      selectInspector({ kind: "region", regionKey: signature.key }, true);
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

const renderTerrainFrame = (): void => {
  pendingTerrainFrame = null;
  if (
    currentSnapshot === null ||
    currentSnapshot.transaction_count === 0 ||
    terrainView.hidden
  ) {
    return;
  }
  terrainLayout = renderTerrain(
    terrainCanvas,
    currentSnapshot.transactions,
    terrainMode,
    selectedInspector,
    terrainLayout,
    selectedTransactionId,
  );
  renderTerrainRegions(terrainLayout);
  terrainCanvas.setAttribute(
    "aria-label",
    `Rule-combination terrain for ${countFormat.format(currentSnapshot.transaction_count)} transactions. Complete violations appear once in their exact rule-set bucket; incomplete violations are separate. Transaction area represents ${terrainMode === "count" ? "one equal membership" : "virtual size"}.`,
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
  const summary = renderSwimView(
    feeAgeCanvas,
    filteredTransactions,
    currentSnapshot.observed_at_ms,
  );
  visualSummary.textContent = `${countFormat.format(summary.transactionCount)} transactions representing ${formatVsize(summary.totalVsize)}. Rows are base fee rate, columns and colour are age, and square area is virtual size.`;
  feeAgeCanvas.setAttribute(
    "aria-label",
    `Fee rate by age view containing ${countFormat.format(summary.transactionCount)} filtered transactions.`,
  );
};

const scheduleFeeAgeRender = (): void => {
  if (
    pendingFeeAgeFrame !== null ||
    currentSnapshot === null ||
    filteredTransactions.length === 0
  ) {
    return;
  }
  pendingFeeAgeFrame = window.requestAnimationFrame(renderFeeAgeFrame);
};

const applyFilters = (): void => {
  if (currentSnapshot === null) {
    filteredTransactions = [];
    filterSummary.textContent = "No snapshot loaded.";
    return;
  }
  filteredTransactions = filterTransactions(
    currentSnapshot.transactions,
    readFilters(),
    currentSnapshot.observed_at_ms,
  );
  filterSummary.textContent = `Showing ${countFormat.format(filteredTransactions.length)} of ${countFormat.format(currentSnapshot.transaction_count)} transactions.`;
  feeAgeStage.hidden = filteredTransactions.length === 0;
  feeAgeEmpty.hidden = filteredTransactions.length !== 0;
  feeAgeEmpty.textContent =
    currentSnapshot.transaction_count === 0
      ? "This snapshot contains an empty mempool."
      : "No transactions match the current filters.";
  scheduleFeeAgeRender();
};

const selectLens = (lens: Lens): void => {
  selectedLens = lens;
  const terrainSelected = lens === "terrain";
  terrainTab.setAttribute("aria-selected", String(terrainSelected));
  feeAgeTab.setAttribute("aria-selected", String(!terrainSelected));
  terrainTab.tabIndex = terrainSelected ? 0 : -1;
  feeAgeTab.tabIndex = terrainSelected ? -1 : 0;
  terrainView.hidden = !terrainSelected;
  feeAgeView.hidden = terrainSelected;
  if (terrainSelected) {
    scheduleTerrainRender();
  } else {
    scheduleFeeAgeRender();
  }
};

const chooseInitialRule = (snapshot: MempoolSnapshot): RuleId => {
  let chosen: RuleId = "element_size";
  let maximum = -1;
  for (const rule of TERRAIN_RULES) {
    const count = rulePopulation(snapshot.transactions, rule.id).count;
    if (count > maximum) {
      chosen = rule.id;
      maximum = count;
    }
  }
  return chosen;
};

const selectInspector = (
  inspector: InspectorSelection,
  loadSample: boolean,
): void => {
  const changed = !selectionsMatch(selectedInspector, inspector);
  selectedInspector = inspector;
  nodeViewState = { ...nodeViewState, selection: inspector };
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

const renderClassification = (snapshot: MempoolSnapshot): void => {
  const totals = classificationTotals(snapshot.transactions);
  compatibleCount.textContent = countFormat.format(totals.compatible.count);
  indeterminateCount.textContent = countFormat.format(
    totals.indeterminate.count,
  );
  violatingCount.textContent = countFormat.format(totals.violating.count);
  coverageComplete.textContent = countFormat.format(
    totals.completeCoverage.count,
  );
  coverageIncomplete.textContent = countFormat.format(
    totals.incompleteCoverage.count,
  );
  coverageUnclassified.textContent = countFormat.format(
    totals.unclassified.count,
  );
  terrainSummary.textContent = `${snapshot.bip110_summary.evaluator_id} ${snapshot.bip110_summary.evaluator_version} classified this source snapshot. Each transaction appears once. Complete violations use exact rule-combination buckets; proven violations with unresolved checks remain separate. Bucket frames preserve readability, while tile area uses the selected metric.`;
};

const renderResponse = (
  response: SourceSnapshotResponse,
  requestedState: NodeViewState,
): void => {
  const { source, snapshot } = response;
  sourceLabel.textContent = source.source_label;
  sourceId.textContent = source.source_id;
  pageStatus.dataset.state = source.availability;
  sourceSelect.value = source.source_id;

  if (snapshot === null) {
    currentSnapshot = null;
    transactionById = new Map();
    filteredTransactions = [];
    terrainLayout = null;
    terrainStage.hidden = true;
    feeAgeStage.hidden = true;
    terrainEmpty.hidden = false;
    feeAgeEmpty.hidden = false;
    transactionCount.textContent = "Waiting";
    totalVsize.textContent = "Waiting";
    observedValue.textContent = "No snapshot";
    tipValue.textContent = "Unknown";
    compatibleCount.textContent = "0";
    indeterminateCount.textContent = "0";
    violatingCount.textContent = "0";
    coverageComplete.textContent = "0";
    coverageIncomplete.textContent = "0";
    coverageUnclassified.textContent = "0";
    filterSummary.textContent = "No snapshot loaded.";
    selectedInspector =
      requestedState.selection ??
      ({ kind: "rule", rule: "element_size" } as const);
    nodeViewState = {
      source: source.source_id,
      selection: requestedState.selection,
      txid: requestedState.txid,
    };
    transactionSearchInput.value = requestedState.txid ?? "";
    clearDetail(
      requestedState.txid === null
        ? "Choose a sample"
        : "No snapshot available",
      false,
    );
    setTransactionSearchStatus(
      requestedState.txid === null
        ? "Search this snapshot by txid."
        : "No snapshot is available to search yet.",
      requestedState.txid === null ? undefined : "absent",
    );
    renderInspector();
    if (source.availability === "error") {
      statusTitle.textContent = "Node snapshot unavailable";
      statusDetail.textContent = `Latest poll failed: ${source.last_error ?? "unknown error"}`;
      terrainEmpty.textContent = "Atlas has not received a valid snapshot yet.";
      feeAgeEmpty.textContent = terrainEmpty.textContent;
    } else {
      statusTitle.textContent = "Waiting for the first snapshot";
      statusDetail.textContent = `Atlas polls this source every ${formatPollInterval(source.poll_interval_seconds)}.`;
      terrainEmpty.textContent =
        "The first complete mempool snapshot is being collected.";
      feeAgeEmpty.textContent = terrainEmpty.textContent;
    }
    replaceViewUrl();
    return;
  }

  currentSnapshot = snapshot;
  terrainLayout = null;
  transactionById = new Map(
    snapshot.transactions.map((transaction) => [transaction.txid, transaction]),
  );
  transactionCount.textContent = countFormat.format(snapshot.transaction_count);
  totalVsize.textContent = formatVsize(snapshot.total_vsize);
  observedValue.textContent = formatSnapshotFreshness(snapshot.observed_at_ms);
  observedValue.title = new Date(snapshot.observed_at_ms).toLocaleString();
  tipValue.textContent = countFormat.format(snapshot.chain_tip.height);
  tipValue.title = snapshot.chain_tip.hash;
  terrainStage.hidden = snapshot.transaction_count === 0;
  terrainEmpty.hidden = snapshot.transaction_count !== 0;
  terrainEmpty.textContent = "This snapshot contains an empty mempool.";
  const initialRule = chooseInitialRule(snapshot);
  const requestedTransaction =
    requestedState.txid === null
      ? undefined
      : transactionById.get(requestedState.txid);
  selectedInspector =
    requestedTransaction !== undefined
      ? {
          kind: "region",
          regionKey: terrainRegionKey(requestedTransaction),
        }
      : (requestedState.selection ?? { kind: "rule", rule: initialRule });
  nodeViewState = {
    source: source.source_id,
    selection: selectedInspector,
    txid: requestedState.txid,
  };
  transactionSearchInput.value = requestedState.txid ?? "";
  clearDetail("Choose a sample", false);
  setTransactionSearchStatus("Search this snapshot by txid.");
  renderClassification(snapshot);
  renderInspector();
  applyFilters();

  if (source.availability === "stale") {
    statusTitle.textContent = "Showing the last good snapshot";
    statusDetail.textContent = `Observed ${new Date(snapshot.observed_at_ms).toLocaleString()}. Latest poll failed: ${source.last_error ?? "unknown error"}`;
  } else {
    statusTitle.textContent = "Snapshot healthy";
    statusDetail.textContent = `Observed ${new Date(snapshot.observed_at_ms).toLocaleString()}. Browser refresh does not trigger a node poll.`;
  }
  scheduleTerrainRender();
  replaceViewUrl();

  if (requestedTransaction !== undefined) {
    void loadTransactionDetail(requestedTransaction);
  } else if (requestedState.txid !== null) {
    showAbsentTransaction(requestedState.txid);
  }
};

const discoverSources = async (): Promise<void> => {
  const response = await fetchSources();
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
};

const loadSnapshot = async (): Promise<void> => {
  const requestedSourceId = selectedSourceId;
  if (requestedSourceId === null) {
    return;
  }
  const ticket = snapshotLifecycle.begin();
  refreshButton.disabled = true;
  refreshButton.textContent = "Loading…";
  try {
    const response = await fetchSourceSnapshot(
      requestedSourceId,
      ticket.signal,
    );
    if (
      !snapshotLifecycle.isCurrent(ticket) ||
      selectedSourceId !== requestedSourceId
    ) {
      return;
    }
    renderResponse(response, nodeViewState);
  } catch (error) {
    if (!snapshotLifecycle.isCurrent(ticket)) {
      return;
    }
    pageStatus.dataset.state = "error";
    statusTitle.textContent = "Atlas website unavailable";
    statusDetail.textContent =
      error instanceof Error ? error.message : "Unable to load snapshot";
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
  try {
    await discoverSources();
    nodeViewState = { ...nodeViewState, source: selectedSourceId };
    selectedInspector =
      nodeViewState.selection ??
      ({ kind: "rule", rule: "element_size" } as const);
    transactionSearchInput.value = nodeViewState.txid ?? "";
    replaceViewUrl();
    await loadSnapshot();
  } catch (error) {
    pageStatus.dataset.state = "error";
    statusTitle.textContent = "Atlas website unavailable";
    statusDetail.textContent =
      error instanceof Error ? error.message : "Unable to load snapshot";
    refreshButton.disabled = false;
    refreshButton.textContent = "Refresh";
  }
};

const prepareForSourceLoad = (source: SourceSummary): void => {
  nodeViewState = {
    source: source.source_id,
    selection: null,
    txid: null,
  };
  currentSnapshot = null;
  transactionById = new Map();
  filteredTransactions = [];
  terrainLayout = null;
  terrainStage.hidden = true;
  feeAgeStage.hidden = true;
  terrainEmpty.hidden = false;
  feeAgeEmpty.hidden = false;
  terrainEmpty.textContent = "Loading this node's current snapshot.";
  feeAgeEmpty.textContent = terrainEmpty.textContent;
  sourceLabel.textContent = source.source_label;
  sourceId.textContent = source.source_id;
  observedValue.textContent = "Loading";
  tipValue.textContent = "Loading";
  transactionCount.textContent = "Loading";
  totalVsize.textContent = "Loading";
  compatibleCount.textContent = "0";
  indeterminateCount.textContent = "0";
  violatingCount.textContent = "0";
  coverageComplete.textContent = "0";
  coverageIncomplete.textContent = "0";
  coverageUnclassified.textContent = "0";
  filterSummary.textContent = "No snapshot loaded.";
  selectedInspector = { kind: "rule", rule: "element_size" };
  clearDetail("Choose a sample");
  renderInspector();
  pageStatus.dataset.state = "waiting";
  statusTitle.textContent = "Loading node snapshot";
  statusDetail.textContent = `Reading the latest complete snapshot for ${source.source_label}.`;
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
    detailRules.replaceChildren();
    setTransactionSearchStatus(
      "No snapshot is available to search yet.",
      "absent",
    );
    replaceViewUrl();
    return;
  }
  const transaction = transactionById.get(txid);
  if (transaction === undefined) {
    showAbsentTransaction(txid);
    return;
  }
  selectInspector(
    { kind: "region", regionKey: terrainRegionKey(transaction) },
    false,
  );
  void loadTransactionDetail(transaction);
};

terrainTab.addEventListener("click", () => {
  selectLens("terrain");
});

feeAgeTab.addEventListener("click", () => {
  selectLens("fee-age");
});

for (const tab of [terrainTab, feeAgeTab]) {
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
    const next =
      event.key === "ArrowLeft" || event.key === "Home"
        ? terrainTab
        : feeAgeTab;
    selectLens(next === terrainTab ? "terrain" : "fee-age");
    next.focus();
  });
}

modeCount.addEventListener("click", () => {
  terrainMode = "count";
  terrainLayout = null;
  modeCount.setAttribute("aria-pressed", "true");
  modeVsize.setAttribute("aria-pressed", "false");
  scheduleTerrainRender();
});

modeVsize.addEventListener("click", () => {
  terrainMode = "vsize";
  terrainLayout = null;
  modeCount.setAttribute("aria-pressed", "false");
  modeVsize.setAttribute("aria-pressed", "true");
  scheduleTerrainRender();
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
  const regionKey = next?.dataset.region as TerrainRegionKey | undefined;
  if (next !== undefined && regionKey !== undefined) {
    selectInspector({ kind: "region", regionKey }, false);
    next.focus();
  }
});

terrainCanvas.addEventListener("click", (event) => {
  if (terrainLayout === null) {
    return;
  }
  const bounds = terrainCanvas.getBoundingClientRect();
  const hit = hitTestTerrain(
    terrainLayout,
    event.clientX - bounds.left,
    event.clientY - bounds.top,
  );
  if (hit?.kind === "region") {
    selectInspector({ kind: "region", regionKey: hit.region.key }, true);
    return;
  }
  if (hit?.kind === "transaction") {
    selectInspector({ kind: "region", regionKey: hit.glyph.regionKey }, false);
    const transaction = transactionById.get(hit.glyph.txid);
    if (transaction !== undefined) {
      void loadTransactionDetail(transaction);
    }
  }
});

terrainCanvas.addEventListener("keydown", (event) => {
  if (event.key === "Enter" || event.key === " ") {
    event.preventDefault();
    const tabStop =
      terrainLayout === null ? null : terrainTabStopKey(terrainLayout);
    if (tabStop !== null) {
      terrainRegions
        .querySelector<HTMLButtonElement>(`[data-region="${tabStop}"]`)
        ?.focus();
    }
  }
});

filtersForm.addEventListener("submit", (event) => {
  event.preventDefault();
  applyFilters();
});

resetFilters.addEventListener("click", () => {
  minimumFeeRate.value = String(DEFAULT_FILTERS.minimumFeeRate);
  maximumAge.value = "all";
  minimumVsize.value = String(DEFAULT_FILTERS.minimumVsize);
  applyFilters();
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
