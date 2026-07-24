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
import { formatMembershipAge } from "./membership-table";
import { renderSwimView } from "./swim-view";
import {
  TERRAIN_RULES,
  classificationTotals,
  hitTestTerrain,
  renderTerrain,
  selectedRulePopulation,
  terrainRule,
  unresolvedPrimaryPopulation,
  type TerrainLayout,
  type TerrainMode,
  type TerrainRegionKey,
} from "./terrain";
import "./styles.css";
import type {
  MempoolSnapshot,
  MempoolTransaction,
  RuleId,
  RuleAssessment,
  SourceSnapshotResponse,
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
const ruleNumber = requiredElement<HTMLElement>("rule-number");
const ruleName = requiredElement<HTMLElement>("rule-name");
const ruleDescription = requiredElement<HTMLElement>("rule-description");
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
type InspectorKey = RuleId | "violating_unresolved";

let selectedSourceId: string | null = null;
let currentSnapshot: MempoolSnapshot | null = null;
let transactionById = new Map<string, MempoolTransaction>();
let filteredTransactions: MempoolTransaction[] = [];
let selectedLens: Lens = "terrain";
let selectedInspector: InspectorKey = "element_size";
let terrainMode: TerrainMode = "count";
let terrainLayout: TerrainLayout | null = null;
let pendingTerrainFrame: number | null = null;
let pendingFeeAgeFrame: number | null = null;
let detailSequence = 0;
let selectedTransactionId: string | null = null;

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

const isRuleId = (key: TerrainRegionKey): key is RuleId =>
  TERRAIN_RULES.some(({ id }) => id === key);

const isInspectorKey = (key: TerrainRegionKey): key is InspectorKey =>
  key === "violating_unresolved" || isRuleId(key);

const inspectorKeys = (): InspectorKey[] => [
  ...TERRAIN_RULES.map(({ id }) => id),
  "violating_unresolved",
];

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

const clearDetail = (message: string): void => {
  detailSequence += 1;
  selectedTransactionId = null;
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
  );
  detailRules.replaceChildren(...detail.rules.map(renderRuleDetail));
};

const loadTransactionDetail = async (
  transaction: MempoolTransaction,
): Promise<void> => {
  const snapshot = currentSnapshot;
  if (snapshot === null) {
    return;
  }
  const sequence = ++detailSequence;
  selectedTransactionId = transaction.txid;
  detailStatus.textContent = "Loading rule evidence…";
  detailTransaction.replaceChildren(
    detailValue("txid", transaction.txid),
    detailValue("wtxid", transaction.wtxid),
  );
  detailRules.replaceChildren();
  renderSampleTable();
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

const renderSampleTable = (): void => {
  if (currentSnapshot === null) {
    ruleTransactions.replaceChildren();
    sampleSummary.textContent = "No snapshot";
    return;
  }
  const population =
    selectedInspector === "violating_unresolved"
      ? unresolvedPrimaryPopulation(currentSnapshot.transactions)
      : selectedRulePopulation(currentSnapshot.transactions, selectedInspector);
  const sample = population.transactions.slice(0, 8);
  sampleSummary.textContent =
    population.count === 0
      ? "No primary matches"
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

    const sizeCell = document.createElement("td");
    sizeCell.textContent = countFormat.format(transaction.vsize);
    const feeCell = document.createElement("td");
    feeCell.textContent = decimalFormat.format(
      transaction.fee_sats / transaction.vsize,
    );
    row.append(transactionCell, sizeCell, feeCell);
    return row;
  });
  ruleTransactions.replaceChildren(...rows);
};

const renderRuleNavigation = (): void => {
  if (currentSnapshot === null) {
    ruleList.replaceChildren();
    return;
  }
  const buttons = TERRAIN_RULES.map((rule) => {
    const population = selectedRulePopulation(
      currentSnapshot?.transactions ?? [],
      rule.id,
    );
    const button = document.createElement("button");
    button.type = "button";
    button.dataset.rule = rule.id;
    button.dataset.inspector = rule.id;
    button.setAttribute("aria-pressed", String(rule.id === selectedInspector));
    button.tabIndex = rule.id === selectedInspector ? 0 : -1;
    button.innerHTML = `<span>R${rule.number}</span><strong>${rule.shortLabel}</strong><small>${countFormat.format(population.count)}</small>`;
    button.addEventListener("click", () => {
      selectInspector(rule.id, true);
    });
    return button;
  });
  const unresolved = unresolvedPrimaryPopulation(currentSnapshot.transactions);
  const unresolvedButton = document.createElement("button");
  unresolvedButton.type = "button";
  unresolvedButton.dataset.inspector = "violating_unresolved";
  unresolvedButton.setAttribute(
    "aria-pressed",
    String(selectedInspector === "violating_unresolved"),
  );
  unresolvedButton.tabIndex =
    selectedInspector === "violating_unresolved" ? 0 : -1;
  unresolvedButton.innerHTML = `<span>?</span><strong>First rule unresolved</strong><small>${countFormat.format(unresolved.count)}</small>`;
  unresolvedButton.addEventListener("click", () => {
    selectInspector("violating_unresolved", true);
  });
  ruleList.replaceChildren(...buttons, unresolvedButton);
};

const renderInspector = (): void => {
  if (selectedInspector === "violating_unresolved") {
    ruleNumber.textContent = "Status";
    ruleName.textContent = "First rule unresolved";
    ruleDescription.textContent =
      "A later BIP-110 rule is definitely violated, but missing facts for an earlier input prevent Atlas from naming the first rejecting rule.";
  } else {
    const rule = terrainRule(selectedInspector);
    ruleNumber.textContent = `Rule ${rule.number}`;
    ruleName.textContent = rule.label;
    ruleDescription.textContent = rule.description;
  }
  if (currentSnapshot === null) {
    ruleCount.textContent = "0";
    ruleVsize.textContent = "0 vB";
    ruleShare.textContent = "0%";
  } else {
    const population =
      selectedInspector === "violating_unresolved"
        ? unresolvedPrimaryPopulation(currentSnapshot.transactions)
        : selectedRulePopulation(
            currentSnapshot.transactions,
            selectedInspector,
          );
    ruleCount.textContent = countFormat.format(population.count);
    ruleVsize.textContent = formatVsize(population.vsize);
    ruleShare.textContent = percentageFormat.format(population.totalShare);
  }
  renderRuleNavigation();
  renderSampleTable();
};

const regionLabel = (key: TerrainRegionKey): string => {
  if (key === "compatible") {
    return "Compatible";
  }
  if (key === "indeterminate") {
    return "Indeterminate";
  }
  if (key === "violating_unresolved") {
    return "Violating / first rule unresolved";
  }
  if (key === "unclassified") {
    return "Not classified";
  }
  return `R${terrainRule(key).number} · ${terrainRule(key).shortLabel}`;
};

const renderTerrainRegions = (layout: TerrainLayout): void => {
  const preserveFocus = terrainRegions.contains(document.activeElement);
  const labels = layout.regions.map((region) => {
    const selectable = isInspectorKey(region.key);
    const isRule = isRuleId(region.key);
    const label = document.createElement(selectable ? "button" : "div");
    label.className = `terrain-region-label ${selectable ? "rule-region" : "status-region"}`;
    label.style.left = `${(region.rect.x / layout.width) * 100}%`;
    label.style.top = `${(region.rect.y / layout.height) * 100}%`;
    label.style.width = `${(region.rect.width / layout.width) * 100}%`;
    label.style.height = `${Math.min(
      48,
      Math.max(18, region.rect.height * 0.28),
    )}px`;
    const compact = region.rect.width < 112 || region.rect.height < 72;
    const title = regionLabel(region.key);
    label.innerHTML = `<strong>${compact && isRule ? `R${terrainRule(region.key as RuleId).number}` : title}</strong><span>${countFormat.format(region.transactionCount)}</span>`;
    label.title = `${title}: ${countFormat.format(region.transactionCount)} transactions, ${formatVsize(region.totalVsize)}`;

    if (label instanceof HTMLButtonElement && selectable) {
      label.type = "button";
      label.dataset.inspector = region.key;
      label.setAttribute(
        "aria-pressed",
        String(region.key === selectedInspector),
      );
      label.tabIndex = region.key === selectedInspector ? 0 : -1;
      label.setAttribute(
        "aria-label",
        `${title}, ${countFormat.format(region.transactionCount)} transactions`,
      );
      label.addEventListener("click", () => {
        selectInspector(region.key as InspectorKey, true);
      });
    }
    return label;
  });
  terrainRegions.replaceChildren(...labels);
  if (preserveFocus) {
    terrainRegions
      .querySelector<HTMLButtonElement>(
        `[data-inspector="${selectedInspector}"]`,
      )
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
  );
  renderTerrainRegions(terrainLayout);
  terrainCanvas.setAttribute(
    "aria-label",
    `Classification terrain for ${countFormat.format(currentSnapshot.transaction_count)} transactions. Transaction area represents ${terrainMode === "count" ? "one equal membership" : "virtual size"}. Use the rule buttons to inspect a territory.`,
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
    const count = selectedRulePopulation(snapshot.transactions, rule.id).count;
    if (count > maximum) {
      chosen = rule.id;
      maximum = count;
    }
  }
  return chosen;
};

const selectInspector = (
  inspector: InspectorKey,
  loadSample: boolean,
): void => {
  const changed = selectedInspector !== inspector;
  selectedInspector = inspector;
  if (changed) {
    clearDetail("Choose a sample");
  }
  renderInspector();
  scheduleTerrainRender();
  if (loadSample && currentSnapshot !== null) {
    const first =
      selectedInspector === "violating_unresolved"
        ? unresolvedPrimaryPopulation(currentSnapshot.transactions)
            .transactions[0]
        : selectedRulePopulation(
            currentSnapshot.transactions,
            selectedInspector,
          ).transactions[0];
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
  terrainSummary.textContent = `${snapshot.bip110_summary.evaluator_id} ${snapshot.bip110_summary.evaluator_version} classified this source snapshot. Each transaction appears once, under its overall status or primary policy rule.`;
};

const renderResponse = (response: SourceSnapshotResponse): void => {
  const { source, snapshot } = response;
  sourceLabel.textContent = source.source_label;
  sourceId.textContent = source.source_id;
  pageStatus.dataset.state = source.availability;

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
    clearDetail("Choose a sample");
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
    return;
  }

  currentSnapshot = snapshot;
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
  selectedInspector = initialRule;
  clearDetail("Choose a sample");
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

  const first = selectedRulePopulation(snapshot.transactions, initialRule)
    .transactions[0];
  if (first !== undefined) {
    void loadTransactionDetail(first);
  }
};

const chooseSource = async (): Promise<string> => {
  const response = await fetchSources();
  if (response.sources.length === 0) {
    throw new Error("Atlas has no configured Bitcoin source");
  }
  const requested = new URLSearchParams(window.location.search).get("source");
  if (
    requested !== null &&
    response.sources.some((source) => source.source_id === requested)
  ) {
    return requested;
  }
  return response.sources[0]?.source_id ?? "";
};

const loadSnapshot = async (): Promise<void> => {
  refreshButton.disabled = true;
  refreshButton.textContent = "Loading…";
  try {
    selectedSourceId ??= await chooseSource();
    renderResponse(await fetchSourceSnapshot(selectedSourceId));
  } catch (error) {
    pageStatus.dataset.state = "error";
    statusTitle.textContent = "Atlas website unavailable";
    statusDetail.textContent =
      error instanceof Error ? error.message : "Unable to load snapshot";
  } finally {
    refreshButton.disabled = false;
    refreshButton.textContent = "Refresh";
  }
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
  modeCount.setAttribute("aria-pressed", "true");
  modeVsize.setAttribute("aria-pressed", "false");
  scheduleTerrainRender();
});

modeVsize.addEventListener("click", () => {
  terrainMode = "vsize";
  modeCount.setAttribute("aria-pressed", "false");
  modeVsize.setAttribute("aria-pressed", "true");
  scheduleTerrainRender();
});

const moveInspectorFocus = (
  event: KeyboardEvent,
  container: HTMLElement,
): void => {
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
  const keys = inspectorKeys();
  const current = keys.indexOf(selectedInspector);
  const nextIndex =
    event.key === "Home"
      ? 0
      : event.key === "End"
        ? keys.length - 1
        : (current +
            (event.key === "ArrowLeft" || event.key === "ArrowUp" ? -1 : 1) +
            keys.length) %
          keys.length;
  const inspector = keys[nextIndex];
  if (inspector !== undefined) {
    selectInspector(inspector, false);
    container
      .querySelector<HTMLButtonElement>(`[data-inspector="${inspector}"]`)
      ?.focus();
  }
};

ruleList.addEventListener("keydown", (event) => {
  moveInspectorFocus(event, ruleList);
});

terrainRegions.addEventListener("keydown", (event) => {
  moveInspectorFocus(event, terrainRegions);
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
  if (hit?.kind === "region" && isInspectorKey(hit.region.key)) {
    selectInspector(hit.region.key, true);
    return;
  }
  if (hit?.kind === "transaction") {
    if (isInspectorKey(hit.glyph.regionKey)) {
      selectInspector(hit.glyph.regionKey, false);
    }
    const transaction = transactionById.get(hit.glyph.txid);
    if (transaction !== undefined) {
      void loadTransactionDetail(transaction);
    }
  }
});

terrainCanvas.addEventListener("keydown", (event) => {
  if (event.key === "Enter" || event.key === " ") {
    event.preventDefault();
    terrainRegions
      .querySelector<HTMLButtonElement>(
        `[data-inspector="${selectedInspector}"]`,
      )
      ?.focus();
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

refreshButton.addEventListener("click", () => {
  void loadSnapshot();
});

new ResizeObserver(scheduleTerrainRender).observe(terrainCanvas);
new ResizeObserver(scheduleFeeAgeRender).observe(feeAgeCanvas);

selectLens(selectedLens);
renderInspector();
void loadSnapshot();
