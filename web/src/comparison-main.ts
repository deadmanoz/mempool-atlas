import {
  fetchSources,
  fetchSourceSnapshot,
  fetchTransactionDetail,
  transactionDetailMatchesSnapshot,
} from "./api";
import {
  classificationPresentation,
  unclassifiedLabel,
} from "./classification-progress";
import {
  ComparisonLifecycle,
  RequestLifecycle,
  type ComparisonRequestTicket,
} from "./comparison-lifecycle";
import { createComparisonDistributionsView } from "./comparison-distributions-view";
import {
  hitTestComparison,
  renderComparisonCanvas,
  type ComparisonGeometry,
  type ComparisonLayout,
} from "./comparison-layout";
import {
  cursorMoveForKey,
  moveComparisonCursor,
} from "./comparison-navigation";
import {
  comparisonPolicyFiltersMatch,
  executeComparisonViewTransition,
  resolveComparisonViewTransition,
  type ResolvedComparisonViewTransition,
} from "./comparison-view-transition";
import {
  buildComparisonPolicyView,
  comparisonPolicyMatrixTarget,
  comparisonPolicyMatrixRowPresentation,
  type ComparisonPolicyMatrixRow,
  type ComparisonPolicyMatrixSelection,
  type ComparisonPolicyMatrixStatus,
  type ComparisonPolicyPopulation,
  type ComparisonPolicyView,
} from "./comparison-policy-view";
import {
  compareCurrentSnapshots,
  comparisonRegionEntries,
  policySideForRegion,
  requireLoadedSnapshot,
  sourceEntry,
  type ComparedTransaction,
  type ComparisonPolicyFilter,
  type ComparisonRegionKey,
  type ComparisonSide,
  type CurrentComparison,
  type LoadedSourceSnapshot,
} from "./comparison-model";
import { formatMembershipAge } from "./membership-table";
import {
  countFormat,
  decimalFormat,
  formatVsize,
  percentageFormat,
} from "./format";
import { transactionFactSummary } from "./transaction-facts";
import {
  TERRAIN_RULES,
  signatureLabel,
  terrainRule,
  unknownRulesLabel,
  violationSignature,
} from "./terrain";
import {
  parseComparisonViewState,
  serializeComparisonViewState,
  type ComparisonViewState,
} from "./view-state";
import "./styles.css";
import "./comparison-styles.css";
import type {
  Bip110Assessment,
  ClassificationState,
  MempoolTransaction,
  RuleAssessment,
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

const refreshButton = requiredElement<HTMLButtonElement>("comparison-refresh");
const atlasVersion = requiredElement<HTMLElement>("atlas-version");
const pageStatus = requiredElement<HTMLElement>("comparison-status");
const statusTitle = requiredElement<HTMLElement>("comparison-status-title");
const statusDetail = requiredElement<HTMLElement>("comparison-status-detail");
const leftSelect = requiredElement<HTMLSelectElement>("left-source");
const rightSelect = requiredElement<HTMLSelectElement>("right-source");
const swapButton = requiredElement<HTMLButtonElement>("swap-sources");
const leftSourceCard = requiredElement<HTMLElement>("left-source-card");
const rightSourceCard = requiredElement<HTMLElement>("right-source-card");
const transactionSearch = requiredElement<HTMLFormElement>(
  "comparison-transaction-search",
);
const transactionSearchInput = requiredElement<HTMLInputElement>(
  "comparison-transaction-search-input",
);
const transactionSearchStatus = requiredElement<HTMLElement>(
  "comparison-transaction-search-status",
);
const samplingPanel = requiredElement<HTMLElement>("sampling-panel");
const samplingSummary = requiredElement<HTMLElement>("sampling-summary");
const chainSummary = requiredElement<HTMLElement>("chain-summary");
const samplingTimeline = requiredElement<HTMLElement>("sampling-timeline");
const samplingNote = requiredElement<HTMLElement>("sampling-note");
const policyMatrix = requiredElement<HTMLElement>("comparison-policy-matrix");
const policyMatrixBody = requiredElement<HTMLTableSectionElement>(
  "comparison-policy-matrix-body",
);
const regionControls = requiredElement<HTMLElement>("comparison-regions");
const comparisonStage = requiredElement<HTMLElement>("comparison-stage");
const comparisonCanvas =
  requiredElement<HTMLCanvasElement>("comparison-canvas");
const comparisonEmpty = requiredElement<HTMLElement>("comparison-empty");
const visualSummary = requiredElement<HTMLElement>("comparison-visual-summary");
const unionCount = requiredElement<HTMLElement>("union-count");
const transactionNavigator = requiredElement<HTMLElement>(
  "comparison-transaction-navigator",
);
const transactionListbox = requiredElement<HTMLElement>(
  "comparison-transaction-listbox",
);
const navigatorSummary = requiredElement<HTMLElement>(
  "comparison-navigator-summary",
);
const activeTransactionOption = requiredElement<HTMLElement>(
  "comparison-active-transaction",
);
const navigatorTxid = requiredElement<HTMLElement>("comparison-navigator-txid");
const navigatorVariant = requiredElement<HTMLElement>(
  "comparison-navigator-variant",
);
const regionEyebrow = requiredElement<HTMLElement>("region-eyebrow");
const regionName = requiredElement<HTMLElement>("region-name");
const regionDescription = requiredElement<HTMLElement>("region-description");
const policySourceRow = requiredElement<HTMLElement>("policy-source-row");
const policyLeft = requiredElement<HTMLButtonElement>("policy-left");
const policyRight = requiredElement<HTMLButtonElement>("policy-right");
const matchCount = requiredElement<HTMLElement>("comparison-match-count");
const matchVsize = requiredElement<HTMLElement>("comparison-match-vsize");
const matchShare = requiredElement<HTMLElement>("comparison-match-share");
const clearPolicyFilter = requiredElement<HTMLButtonElement>(
  "clear-policy-filter",
);
const comparisonRuleList = requiredElement<HTMLElement>("comparison-rule-list");
const policyBuckets = requiredElement<HTMLElement>("policy-buckets");
const policyBucketSummary = requiredElement<HTMLElement>(
  "policy-bucket-summary",
);
const sampleSummary = requiredElement<HTMLElement>("comparison-sample-summary");
const sampleTransactions = requiredElement<HTMLTableSectionElement>(
  "comparison-transactions",
);
const detailStatus = requiredElement<HTMLElement>("comparison-detail-status");
const detailContainer = requiredElement<HTMLElement>("comparison-detail");
const comparisonDistributions = createComparisonDistributionsView(
  requiredElement<HTMLElement>("comparison-distributions"),
);

const lifecycle = new ComparisonLifecycle();
const discoveryLifecycle = new RequestLifecycle();
let initialViewState: ComparisonViewState | null = parseComparisonViewState(
  window.location.search,
);
let configuredSources: SourceSummary[] = [];
let leftSourceId = "";
let rightSourceId = "";
let comparison: CurrentComparison | null = null;
let comparisonPolicyView: ComparisonPolicyView | null = null;
let selectedRegion: ComparisonRegionKey = "common";
let preferredPolicySide: ComparisonSide = "left";
let policyFilter: ComparisonPolicyFilter = { kind: "all" };
let comparisonLayout: ComparisonLayout | null = null;
let comparisonGeometry: ComparisonGeometry | null = null;
let pendingCanvasFrame: number | null = null;
let detailSequence = 0;
let detailController: AbortController | null = null;
let selectedTransactionId: string | null = null;
let keyboardTransactionIndex = 0;
let activeLoads = 0;

const TXID_PATTERN = /^[0-9a-f]{64}$/i;

const formatDuration = (milliseconds: number): string => {
  if (milliseconds < 1_000) {
    return `${countFormat.format(milliseconds)} ms`;
  }
  if (milliseconds < 60_000) {
    return `${decimalFormat.format(milliseconds / 1_000)} s`;
  }
  return formatMembershipAge(milliseconds);
};

const formatFreshness = (observedAtMs: number): string => {
  const age = Math.max(0, Date.now() - observedAtMs);
  return age < 60_000 ? "just now" : `${formatMembershipAge(age)} ago`;
};

const formatTime = (milliseconds: number): string =>
  new Date(milliseconds).toLocaleTimeString();

const compactTxid = (txid: string): string =>
  `${txid.slice(0, 10)}…${txid.slice(-8)}`;

const formatTxidCount = (count: number): string =>
  `${countFormat.format(count)} transaction ${count === 1 ? "ID" : "IDs"}`;

const isAbortError = (error: unknown): boolean =>
  typeof error === "object" &&
  error !== null &&
  "name" in error &&
  error.name === "AbortError";

const setStatus = (
  state: "waiting" | "ready" | "stale" | "error",
  title: string,
  detail: string,
): void => {
  pageStatus.dataset.state = state;
  statusTitle.textContent = title;
  statusDetail.textContent = detail;
};

const setTransactionSearchStatus = (
  state: "idle" | "waiting" | "found" | "absent" | "error",
  message: string,
): void => {
  transactionSearchStatus.dataset.state = state;
  transactionSearchStatus.textContent = message;
  transactionSearchInput.setAttribute(
    "aria-invalid",
    String(state === "error"),
  );
};

const clearDetailPanel = (message: string): void => {
  detailController?.abort();
  detailController = null;
  detailSequence += 1;
  detailStatus.textContent = message;
  detailContainer.replaceChildren();
};

const clearTransactionSelection = (message: string): void => {
  selectedTransactionId = null;
  transactionSearchInput.value = "";
  setTransactionSearchStatus("idle", "Search the current membership union.");
  clearDetailPanel(message);
};

const replaceSourceSummary = (next: SourceSummary): void => {
  const index = configuredSources.findIndex(
    ({ source_id: sourceId }) => sourceId === next.source_id,
  );
  if (index === -1) {
    configuredSources.push(next);
    configuredSources.sort((left, right) =>
      left.source_id.localeCompare(right.source_id),
    );
  } else {
    configuredSources[index] = next;
  }
};

const hasSnapshot = (source: SourceSummary): boolean =>
  source.snapshot_observed_at_ms !== null;

const optionLabel = (source: SourceSummary): string => {
  const state = source.availability === "stale" ? "stale" : source.availability;
  const count =
    source.transaction_count === null
      ? "no snapshot"
      : `${countFormat.format(source.transaction_count)} tx`;
  return `${source.source_label} · ${count} · ${state}`;
};

const populateSourceSelect = (
  select: HTMLSelectElement,
  selectedSourceId: string,
  unavailableSourceId: string,
): void => {
  const options = configuredSources.map((source) => {
    const option = document.createElement("option");
    option.value = source.source_id;
    option.textContent = optionLabel(source);
    option.disabled =
      !hasSnapshot(source) || source.source_id === unavailableSourceId;
    return option;
  });
  select.replaceChildren(...options);
  select.value = selectedSourceId;
};

const populateSourceSelectors = (): void => {
  populateSourceSelect(leftSelect, leftSourceId, rightSourceId);
  populateSourceSelect(rightSelect, rightSourceId, leftSourceId);
  swapButton.disabled = leftSourceId.length === 0 || rightSourceId.length === 0;
};

const activePolicySide = (): ComparisonSide =>
  policySideForRegion(selectedRegion, preferredPolicySide);

const currentViewState = (): ComparisonViewState => ({
  left: leftSourceId.length === 0 ? null : leftSourceId,
  right: rightSourceId.length === 0 ? null : rightSourceId,
  region: selectedRegion,
  side: activePolicySide(),
  filter: policyFilter,
  txid: selectedTransactionId,
});

const defaultViewState = (): ComparisonViewState => ({
  left: leftSourceId.length === 0 ? null : leftSourceId,
  right: rightSourceId.length === 0 ? null : rightSourceId,
  region: "common",
  side: "left",
  filter: { kind: "all" },
  txid: null,
});

const chooseInitialSources = (requested: ComparisonViewState | null): void => {
  const available = configuredSources.filter(hasSnapshot);
  const known = new Set(available.map(({ source_id: sourceId }) => sourceId));
  const requestedPairIsKnown =
    requested !== null &&
    requested.left !== null &&
    requested.right !== null &&
    known.has(requested.left) &&
    known.has(requested.right);
  if (requestedPairIsKnown) {
    leftSourceId = requested.left ?? "";
    rightSourceId = requested.right ?? "";
    return;
  }

  const retainedPairIsKnown =
    leftSourceId !== rightSourceId &&
    known.has(leftSourceId) &&
    known.has(rightSourceId);
  if (retainedPairIsKnown) {
    return;
  }

  leftSourceId = available[0]?.source_id ?? "";
  rightSourceId =
    available.find(({ source_id: sourceId }) => sourceId !== leftSourceId)
      ?.source_id ?? "";
};

const updateQuery = (): void => {
  const url = new URL(window.location.href);
  url.search = serializeComparisonViewState(currentViewState());
  window.history.replaceState(null, "", url);
};

const sourceFact = (label: string, value: string): HTMLElement => {
  const wrapper = document.createElement("div");
  const term = document.createElement("dt");
  term.textContent = label;
  const detail = document.createElement("dd");
  detail.textContent = value;
  detail.title = value;
  wrapper.append(term, detail);
  return wrapper;
};

const sourceClassificationPresentation = (
  loaded: LoadedSourceSnapshot,
): ReturnType<typeof classificationPresentation> & {
  state: ClassificationState;
} => {
  const progress = loaded.source.classification;
  if (progress === null) {
    throw new Error("Loaded source is missing classification progress");
  }
  return {
    ...classificationPresentation(
      progress,
      loaded.snapshot.transaction_count,
      (value) => countFormat.format(value),
    ),
    state: progress.state,
  };
};

const sourceUnclassifiedLabel = (loaded: LoadedSourceSnapshot): string => {
  const progress = loaded.source.classification;
  if (progress === null) {
    return "Assessment unavailable";
  }
  return unclassifiedLabel(progress);
};

const renderSourceCard = (
  card: HTMLElement,
  sideLabel: string,
  loaded: LoadedSourceSnapshot,
): void => {
  const { source, snapshot } = loaded;
  const classification = sourceClassificationPresentation(loaded);
  card.dataset.state = source.availability;
  const eyebrow = document.createElement("p");
  eyebrow.textContent = sideLabel;
  const heading = document.createElement("h3");
  heading.textContent = source.source_label;
  const identifier = document.createElement("code");
  identifier.textContent = source.source_id;
  const nodeLink = document.createElement("a");
  nodeLink.className = "source-card-link";
  nodeLink.href = `../?source=${encodeURIComponent(source.source_id)}`;
  nodeLink.textContent = "Explore this node";
  const facts = document.createElement("dl");
  facts.append(
    sourceFact(
      "Observed",
      `${formatFreshness(snapshot.observed_at_ms)} · ${formatTime(snapshot.observed_at_ms)}`,
    ),
    sourceFact(
      "Collection",
      `${formatTime(snapshot.collection_started_at_ms)}–${formatTime(snapshot.collection_completed_at_ms)} · ${formatDuration(snapshot.collection_duration_ms)}`,
    ),
    sourceFact(
      "Chain tip",
      `${countFormat.format(snapshot.chain_tip.height)} · ${snapshot.chain_tip.hash.slice(0, 10)}…`,
    ),
    sourceFact(
      "Membership",
      `${countFormat.format(snapshot.transaction_count)} tx · ${formatVsize(snapshot.total_vsize)}`,
    ),
    sourceFact(
      "Policy",
      `${classification.compact} · ${snapshot.bip110_summary.evaluator_id} ${snapshot.bip110_summary.evaluator_version} · revision ${countFormat.format(snapshot.classification_revision)}`,
    ),
  );
  const classificationNote = document.createElement("p");
  classificationNote.className = "source-card-classification";
  classificationNote.dataset.state = classification.state;
  classificationNote.textContent = classification.summary;
  card.replaceChildren(
    eyebrow,
    heading,
    identifier,
    nodeLink,
    facts,
    classificationNote,
  );
  if (source.availability === "stale") {
    const warning = document.createElement("p");
    warning.className = "source-card-warning";
    warning.textContent = `Last complete snapshot retained. Latest poll: ${source.last_error ?? "unavailable"}`;
    card.append(warning);
  }
};

const resetSourceCard = (
  card: HTMLElement,
  sideLabel: string,
  sourceId: string,
  message: string,
): void => {
  const source = configuredSources.find(
    ({ source_id: configuredId }) => configuredId === sourceId,
  );
  card.dataset.state = "waiting";
  const eyebrow = document.createElement("p");
  eyebrow.textContent = sideLabel;
  const heading = document.createElement("h3");
  heading.textContent = source?.source_label ?? "No source selected";
  const identifier = document.createElement("code");
  identifier.textContent = sourceId;
  const note = document.createElement("p");
  note.className = "source-card-placeholder";
  note.textContent = message;
  card.replaceChildren(
    eyebrow,
    heading,
    ...(sourceId.length === 0 ? [] : [identifier]),
    note,
  );
};

const timelineRow = (
  label: string,
  side: ComparisonSide,
  source: LoadedSourceSnapshot,
  minimum: number,
  span: number,
): HTMLElement => {
  const row = document.createElement("div");
  row.className = "sampling-row";
  const name = document.createElement("span");
  name.textContent = label;
  const track = document.createElement("div");
  track.className = "sampling-track";
  const bar = document.createElement("i");
  bar.dataset.side = side;
  const start = source.snapshot.collection_started_at_ms;
  const duration = Math.max(1, source.snapshot.collection_duration_ms);
  bar.style.left = `${((start - minimum) / span) * 100}%`;
  bar.style.width = `${Math.max(1.2, (duration / span) * 100)}%`;
  bar.title = `${formatTime(start)}–${formatTime(source.snapshot.collection_completed_at_ms)}`;
  track.append(bar);
  const time = document.createElement("time");
  time.dateTime = new Date(
    source.snapshot.collection_completed_at_ms,
  ).toISOString();
  time.textContent = formatTime(source.snapshot.collection_completed_at_ms);
  row.append(name, track, time);
  return row;
};

const renderSampling = (current: CurrentComparison): void => {
  samplingPanel.hidden = false;
  const { left, right } = current;
  const earlierLabel =
    current.earlier_side === null
      ? "completed together"
      : `${current[current.earlier_side].snapshot.source_label} completed earlier`;
  samplingSummary.textContent = `${formatDuration(current.observed_skew_ms)} observation skew · ${earlierLabel}`;
  const sameTip =
    left.snapshot.chain_tip.hash === right.snapshot.chain_tip.hash;
  chainSummary.dataset.state = sameTip ? "same" : "different";
  chainSummary.textContent = sameTip
    ? `Same chain tip · ${countFormat.format(left.snapshot.chain_tip.height)}`
    : `Different chain tips · ${countFormat.format(left.snapshot.chain_tip.height)} / ${countFormat.format(right.snapshot.chain_tip.height)}`;

  const minimum = Math.min(
    left.snapshot.collection_started_at_ms,
    right.snapshot.collection_started_at_ms,
  );
  const maximum = Math.max(
    left.snapshot.collection_completed_at_ms,
    right.snapshot.collection_completed_at_ms,
  );
  const span = Math.max(1, maximum - minimum);
  samplingTimeline.replaceChildren(
    timelineRow("A", "left", left, minimum, span),
    timelineRow("B", "right", right, minimum, span),
  );

  const overlap =
    Math.min(
      left.snapshot.collection_completed_at_ms,
      right.snapshot.collection_completed_at_ms,
    ) -
    Math.max(
      left.snapshot.collection_started_at_ms,
      right.snapshot.collection_started_at_ms,
    );
  if (overlap >= 0) {
    samplingNote.textContent = `The collection windows overlapped by ${formatDuration(overlap)}. Membership still comes from independent node observations.`;
  } else {
    samplingNote.textContent = `The collection windows were separated by ${formatDuration(Math.abs(overlap))}. Membership changes during that interval can contribute to regions observed in only one snapshot.`;
  }
};

const regionLabel = (
  current: CurrentComparison,
  key: ComparisonRegionKey,
): string => {
  if (key === "common") {
    return "Present in both sampled snapshots";
  }
  const source = key === "left_only" ? current.left : current.right;
  return `Observed only in ${source.snapshot.source_label} snapshot`;
};

const regionDescriptionText = (
  current: CurrentComparison,
  key: ComparisonRegionKey,
): string => {
  if (key === "common") {
    return "Transaction IDs observed in both independently sampled mempools. Each source keeps its own witness variant and policy assessment.";
  }
  const owner = key === "left_only" ? current.left : current.right;
  const other = key === "left_only" ? current.right : current.left;
  return `Transaction IDs present in ${owner.snapshot.source_label} at ${formatTime(owner.snapshot.observed_at_ms)} and not present in ${other.snapshot.source_label} at ${formatTime(other.snapshot.observed_at_ms)}.`;
};

const renderRegionControls = (current: CurrentComparison): void => {
  const values: Record<ComparisonRegionKey, { count: number; detail: string }> =
    {
      left_only: {
        count: current.totals.left_only_count,
        detail: formatVsize(current.totals.left_only_vsize),
      },
      common: {
        count: current.totals.common_count,
        detail: `${current.left.snapshot.source_label} ${formatVsize(current.totals.common_left_vsize)} · ${current.right.snapshot.source_label} ${formatVsize(current.totals.common_right_vsize)}`,
      },
      right_only: {
        count: current.totals.right_only_count,
        detail: formatVsize(current.totals.right_only_vsize),
      },
    };
  for (const button of regionControls.querySelectorAll<HTMLButtonElement>(
    "button[data-region]",
  )) {
    const key = button.dataset.region as ComparisonRegionKey;
    const value = values[key];
    if (value === undefined) {
      continue;
    }
    button.disabled = false;
    button.setAttribute("aria-pressed", String(key === selectedRegion));
    button.querySelector("span")!.textContent = regionLabel(current, key);
    button.querySelector("strong")!.textContent = countFormat.format(
      value.count,
    );
    button.querySelector("small")!.textContent = value.detail;
    button.setAttribute(
      "aria-label",
      `${regionLabel(current, key)}: ${formatTxidCount(value.count)}, ${value.detail}`,
    );
  }
};

const resetRegionControls = (): void => {
  const labels: Record<ComparisonRegionKey, string> = {
    left_only: "Observed only in source A snapshot",
    common: "Present in both snapshots",
    right_only: "Observed only in source B snapshot",
  };
  const details: Record<ComparisonRegionKey, string> = {
    left_only: "0 vB",
    common: "0 vB in each source",
    right_only: "0 vB",
  };
  for (const button of regionControls.querySelectorAll<HTMLButtonElement>(
    "button[data-region]",
  )) {
    const key = button.dataset.region as ComparisonRegionKey;
    button.disabled = true;
    button.setAttribute("aria-pressed", String(key === selectedRegion));
    button.querySelector("span")!.textContent = labels[key];
    button.querySelector("strong")!.textContent = "0";
    button.querySelector("small")!.textContent = details[key];
    button.setAttribute(
      "aria-label",
      `${labels[key]}: 0 transaction IDs, ${details[key]}`,
    );
  }
};

const invalidateComparisonGeometry = (): void => {
  comparisonGeometry = null;
  comparisonLayout = null;
};

const activeNavigatorEntry = (): ComparedTransaction | null => {
  if (comparison === null) {
    return null;
  }
  const entries = comparisonRegionEntries(comparison, selectedRegion);
  if (entries.length === 0) {
    return null;
  }
  keyboardTransactionIndex = Math.min(
    entries.length - 1,
    Math.max(0, keyboardTransactionIndex),
  );
  return entries[keyboardTransactionIndex] ?? null;
};

const renderTransactionNavigator = (): void => {
  const current = comparison;
  const entry = activeNavigatorEntry();
  if (current === null || entry === null) {
    transactionNavigator.hidden = true;
    navigatorSummary.textContent = "No transactions";
    navigatorTxid.textContent = "Waiting";
    navigatorVariant.textContent = "";
    activeTransactionOption.removeAttribute("aria-posinset");
    activeTransactionOption.removeAttribute("aria-setsize");
    return;
  }
  const entries = comparisonRegionEntries(current, selectedRegion);
  transactionNavigator.hidden = false;
  navigatorSummary.textContent = `${regionLabel(current, selectedRegion)} · transaction ${countFormat.format(keyboardTransactionIndex + 1)} of ${countFormat.format(entries.length)}`;
  navigatorTxid.textContent = entry.txid;
  navigatorTxid.title = entry.txid;
  navigatorVariant.textContent =
    entry.same_wtxid === false
      ? "Different witness variants"
      : entry.same_wtxid === true
        ? "Same witness variant"
        : "Observed in one snapshot";
  activeTransactionOption.setAttribute(
    "aria-posinset",
    String(keyboardTransactionIndex + 1),
  );
  activeTransactionOption.setAttribute("aria-setsize", String(entries.length));
  activeTransactionOption.setAttribute(
    "aria-label",
    `${entry.txid}. ${navigatorVariant.textContent}. Transaction ${keyboardTransactionIndex + 1} of ${entries.length} in ${regionLabel(current, selectedRegion)}.`,
  );
  transactionListbox.setAttribute(
    "aria-label",
    `Transactions in ${regionLabel(current, selectedRegion)}`,
  );
};

const scheduleCanvasRender = (): void => {
  if (pendingCanvasFrame !== null) {
    return;
  }
  pendingCanvasFrame = window.requestAnimationFrame(() => {
    pendingCanvasFrame = null;
    if (comparison === null || comparisonStage.hidden) {
      invalidateComparisonGeometry();
      return;
    }
    const result = renderComparisonCanvas(
      comparisonCanvas,
      comparison,
      selectedRegion,
      selectedTransactionId,
      comparisonGeometry,
    );
    comparisonGeometry = result.geometry;
    comparisonLayout = result.geometry.layout;
  });
};

const ruleChip = (
  label: string,
  className: string,
  color?: string,
): HTMLElement => {
  const chip = document.createElement("span");
  chip.className = `rule-chip ${className}`;
  chip.textContent = label;
  if (color !== undefined) {
    chip.style.setProperty("--rule-color", color);
  }
  return chip;
};

const assessmentChips = (
  transaction: MempoolTransaction | null,
  loaded: LoadedSourceSnapshot,
): HTMLElement => {
  const chips = document.createElement("span");
  chips.className = "rule-chips comparison-rule-chips";
  if (transaction === null) {
    chips.append(ruleChip("Not present", "absent"));
    return chips;
  }
  const assessment = transaction.bip110;
  if (assessment === null) {
    chips.append(ruleChip(sourceUnclassifiedLabel(loaded), "unclassified"));
    return chips;
  }
  if (assessment.status === "compatible") {
    chips.append(ruleChip("Compatible", "compatible"));
    return chips;
  }
  if (assessment.status === "indeterminate") {
    chips.append(ruleChip("Indeterminate", "unknown"));
  }
  for (const ruleId of assessment.violated_rules) {
    const rule = terrainRule(ruleId);
    chips.append(ruleChip(`R${rule.number}`, "violated", rule.color));
  }
  for (const ruleId of assessment.unknown_rules) {
    chips.append(ruleChip(`R${terrainRule(ruleId).number}?`, "unknown"));
  }
  return chips;
};

const preparePendingView = (requested: ComparisonViewState): void => {
  selectedRegion = requested.region ?? "common";
  preferredPolicySide = policySideForRegion(
    selectedRegion,
    requested.side ?? "left",
  );
  selectedTransactionId = requested.txid;
  policyFilter =
    selectedTransactionId === null ? requested.filter : { kind: "all" };
  keyboardTransactionIndex = 0;
  transactionSearchInput.value = selectedTransactionId ?? "";
  if (selectedTransactionId === null) {
    setTransactionSearchStatus("idle", "Search the current membership union.");
    clearDetailPanel("Choose a sample");
  } else {
    setTransactionSearchStatus(
      "waiting",
      "Waiting to search the current snapshots.",
    );
    clearDetailPanel("Waiting for current membership");
  }
};

const applyResolvedLoadedView = (
  current: CurrentComparison,
  resolved: ResolvedComparisonViewTransition,
): void => {
  selectedRegion = resolved.region;
  preferredPolicySide = resolved.side;
  policyFilter = resolved.filter;
  selectedTransactionId = resolved.txid;
  keyboardTransactionIndex = resolved.transactionIndex;

  transactionSearchInput.value = resolved.txid ?? "";
  if (resolved.txid === null) {
    setTransactionSearchStatus("idle", "Search the current membership union.");
    clearDetailPanel("Choose a sample");
  } else if (resolved.selectedEntry === null) {
    setTransactionSearchStatus(
      "absent",
      "Not present in either current snapshot.",
    );
    clearDetailPanel("Not present in the current snapshots");
  } else if (resolved.region === "common") {
    setTransactionSearchStatus("found", "Present in both current snapshots.");
    clearDetailPanel("Loading source detail…");
  } else {
    const source =
      resolved.region === "left_only" ? current.left : current.right;
    setTransactionSearchStatus(
      "found",
      `Observed only in the ${source.snapshot.source_label} snapshot.`,
    );
    clearDetailPanel("Loading source detail…");
  }
};

const matrixRowCopy = (
  current: CurrentComparison,
  row: ComparisonPolicyMatrixRow,
): ReturnType<typeof comparisonPolicyMatrixRowPresentation> =>
  comparisonPolicyMatrixRowPresentation(
    row,
    sourceClassificationPresentation(current[row.side]).label,
    formatTxidCount,
  );

const matrixStatusLabel = (
  status: ComparisonPolicyMatrixStatus,
  loaded: LoadedSourceSnapshot,
): string => {
  if (status === "compatible") {
    return "Compatible";
  }
  if (status === "violating") {
    return "Would violate deployed policy";
  }
  if (status === "indeterminate") {
    return "Indeterminate";
  }
  return sourceUnclassifiedLabel(loaded);
};

const setMatrixControlMetadata = (
  button: HTMLButtonElement,
  row: ComparisonPolicyMatrixRow,
  selection: ComparisonPolicyMatrixSelection,
): void => {
  button.dataset.matrixRegion = row.region;
  button.dataset.matrixSide = row.side;
  button.dataset.matrixFilterKind = selection.kind;
  button.dataset.matrixFilterValue =
    selection.kind === "status" ? selection.status : selection.signature;
  button.setAttribute("aria-pressed", "false");
  button.addEventListener("click", () => {
    transitionComparisonView({
      ...currentViewState(),
      ...comparisonPolicyMatrixTarget(row, selection),
    });
  });
};

const matrixStatusCell = (
  current: CurrentComparison,
  row: ComparisonPolicyMatrixRow,
  status: ComparisonPolicyMatrixStatus,
): HTMLTableCellElement => {
  const cell = document.createElement("td");
  cell.className = "comparison-policy-cell";
  cell.dataset.status = status;
  const count = row.statusCounts[status];
  const copy = matrixRowCopy(current, row);
  if (count === 0) {
    const zero = document.createElement("span");
    zero.className = "comparison-policy-zero";
    zero.textContent = "0";
    cell.append(zero);
  } else {
    const selection: ComparisonPolicyMatrixSelection = {
      kind: "status",
      status,
    };
    const button = document.createElement("button");
    button.type = "button";
    button.className = "comparison-policy-status-button";
    button.setAttribute(
      "aria-label",
      `${copy.ariaContext}, ${matrixStatusLabel(status, current[row.side])}: ${formatTxidCount(count)}`,
    );
    const total = document.createElement("strong");
    total.textContent = countFormat.format(count);
    const unit = document.createElement("span");
    unit.textContent = count === 1 ? "transaction" : "transactions";
    button.append(total, unit);
    setMatrixControlMetadata(button, row, selection);
    cell.append(button);
  }

  if (status !== "violating" || count === 0) {
    return cell;
  }

  const exactSummary = document.createElement("p");
  exactSummary.className = "comparison-policy-cell-note";
  exactSummary.textContent = `${countFormat.format(row.exactViolationCount)} exact · ${countFormat.format(row.partialViolationCount)} partly unresolved`;
  cell.append(exactSummary);

  if (row.dominantExactCombinations.length > 0) {
    const combinations = document.createElement("div");
    combinations.className = "comparison-policy-exact";
    for (const combination of row.dominantExactCombinations) {
      const selection: ComparisonPolicyMatrixSelection = {
        kind: "signature",
        signature: combination.signature.key,
      };
      const label = signatureLabel(combination.signature);
      const button = document.createElement("button");
      button.type = "button";
      button.className = "comparison-policy-signature";
      button.textContent = `${label} · ${countFormat.format(combination.count)}`;
      button.setAttribute(
        "aria-label",
        `${copy.ariaContext}, exact ${label}: ${formatTxidCount(combination.count)}`,
      );
      setMatrixControlMetadata(button, row, selection);
      combinations.append(button);
    }
    cell.append(combinations);
  }

  if (row.exactCombinationOverflow.combinationCount > 0) {
    const overflow = document.createElement("p");
    overflow.className = "comparison-policy-cell-note";
    overflow.textContent = `Plus ${formatTxidCount(row.exactCombinationOverflow.transactionCount)} across ${countFormat.format(row.exactCombinationOverflow.combinationCount)} more exact combinations`;
    cell.append(overflow);
  }
  return cell;
};

const syncPolicyMatrixSelection = (): void => {
  for (const button of policyMatrixBody.querySelectorAll<HTMLButtonElement>(
    "button[data-matrix-filter-kind]",
  )) {
    const targetMatches =
      button.dataset.matrixRegion === selectedRegion &&
      button.dataset.matrixSide === activePolicySide();
    const filterMatchesButton =
      (button.dataset.matrixFilterKind === "status" &&
        policyFilter.kind === "status" &&
        button.dataset.matrixFilterValue === policyFilter.status) ||
      (button.dataset.matrixFilterKind === "signature" &&
        policyFilter.kind === "signature" &&
        button.dataset.matrixFilterValue === policyFilter.signature);
    button.setAttribute(
      "aria-pressed",
      String(targetMatches && filterMatchesButton),
    );
  }
};

const renderPolicyMatrix = (
  current: CurrentComparison,
  view: ComparisonPolicyView,
): void => {
  const statuses: readonly ComparisonPolicyMatrixStatus[] = [
    "compatible",
    "violating",
    "indeterminate",
    "unclassified",
  ];
  const rows = view.rows.map((row) => {
    const element = document.createElement("tr");
    element.dataset.side = row.side;
    const heading = document.createElement("th");
    heading.scope = "row";
    const label = document.createElement("span");
    label.className = "comparison-policy-row-label";
    const title = document.createElement("strong");
    const detail = document.createElement("span");
    const copy = matrixRowCopy(current, row);
    title.textContent = copy.label;
    detail.textContent = copy.detail;
    label.append(title, detail);
    heading.append(label);
    element.append(
      heading,
      ...statuses.map((status) => matrixStatusCell(current, row, status)),
    );
    return element;
  });
  policyMatrixBody.replaceChildren(...rows);
  policyMatrix.hidden = false;
  syncPolicyMatrixSelection();
};

const choosePolicyFilter = (filter: ComparisonPolicyFilter): void => {
  transitionComparisonView({
    ...currentViewState(),
    filter,
    txid: null,
  });
};

const renderRuleFilters = (view: ComparisonPolicyView): void => {
  const slice = view.slice(selectedRegion, activePolicySide());
  const buttons = TERRAIN_RULES.map((rule) => {
    const filter: ComparisonPolicyFilter = { kind: "rule", rule: rule.id };
    const count = slice.ruleTotals[rule.id].count;
    const button = document.createElement("button");
    button.type = "button";
    button.setAttribute(
      "aria-pressed",
      String(comparisonPolicyFiltersMatch(policyFilter, filter)),
    );
    button.setAttribute(
      "aria-label",
      `${rule.label}: ${countFormat.format(count)} transactions have a proven violation. Rule filters overlap.`,
    );
    const number = document.createElement("span");
    number.textContent = `R${rule.number}`;
    const label = document.createElement("strong");
    label.textContent = rule.shortLabel;
    const total = document.createElement("small");
    total.textContent = countFormat.format(count);
    button.append(number, label, total);
    button.addEventListener("click", () => {
      choosePolicyFilter(filter);
    });
    return button;
  });
  comparisonRuleList.replaceChildren(...buttons);
};

const bucketButton = (
  label: string,
  detail: string,
  count: number,
  filter: ComparisonPolicyFilter,
  completeness: "status" | "exact" | "partial",
): HTMLButtonElement => {
  const button = document.createElement("button");
  button.type = "button";
  button.dataset.completeness = completeness;
  button.setAttribute(
    "aria-pressed",
    String(comparisonPolicyFiltersMatch(policyFilter, filter)),
  );
  const heading = document.createElement("strong");
  heading.textContent = label;
  const note = document.createElement("span");
  note.textContent = detail;
  const total = document.createElement("small");
  total.textContent = countFormat.format(count);
  button.append(heading, note, total);
  button.addEventListener("click", () => {
    choosePolicyFilter(filter);
  });
  return button;
};

const renderPolicyBuckets = (
  current: CurrentComparison,
  view: ComparisonPolicyView,
): void => {
  const buttons: HTMLButtonElement[] = [];
  const side = activePolicySide();
  const loaded = current[side];
  const slice = view.slice(selectedRegion, side);
  const statusLabels: Array<
    ["compatible" | "indeterminate" | "unclassified", string]
  > = [
    ["compatible", "Compatible"],
    ["indeterminate", "Indeterminate"],
    ["unclassified", sourceUnclassifiedLabel(loaded)],
  ];
  for (const [status, label] of statusLabels) {
    const totals = slice.statusTotals[status];
    if (totals.count === 0) {
      continue;
    }
    buttons.push(
      bucketButton(
        label,
        formatVsize(totals.vsize),
        totals.count,
        { kind: "status", status },
        "status",
      ),
    );
  }
  for (const bucket of [...slice.exactSignatures, ...slice.partialSignatures]) {
    const signature = bucket.signature;
    const detail =
      signature.completeness === "exact"
        ? formatVsize(bucket.vsize)
        : `${formatVsize(bucket.vsize)} · unresolved ${unknownRulesLabel(signature)}`;
    buttons.push(
      bucketButton(
        signatureLabel(signature),
        detail,
        bucket.count,
        { kind: "signature", signature: signature.key },
        signature.completeness,
      ),
    );
  }
  policyBucketSummary.textContent = `${countFormat.format(buttons.length)} observed buckets`;
  policyBuckets.replaceChildren(...buttons);
};

const renderSampleTable = (
  current: CurrentComparison,
  population: ComparisonPolicyPopulation,
): void => {
  const entries = population.sample;
  sampleSummary.textContent =
    population.count === 0
      ? "No matches"
      : `Largest ${countFormat.format(entries.length)} of ${countFormat.format(population.count)}`;
  const rows = entries.map((entry) => {
    const row = document.createElement("tr");
    row.dataset.txid = entry.txid;
    row.classList.toggle("selected", entry.txid === selectedTransactionId);
    const transactionCell = document.createElement("td");
    const select = document.createElement("button");
    select.type = "button";
    select.className = "tx-select";
    select.textContent = compactTxid(entry.txid);
    select.title = entry.txid;
    select.addEventListener("click", () => {
      transitionComparisonView({
        ...currentViewState(),
        txid: entry.txid,
      });
    });
    transactionCell.append(select);
    const leftCell = document.createElement("td");
    leftCell.append(assessmentChips(entry.left, current.left));
    const rightCell = document.createElement("td");
    rightCell.append(assessmentChips(entry.right, current.right));
    const variantCell = document.createElement("td");
    if (entry.same_wtxid === false) {
      const badge = document.createElement("span");
      badge.className = "variant-badge differing";
      badge.textContent = "Different wtxids";
      variantCell.append(badge);
    } else if (entry.same_wtxid === true) {
      variantCell.textContent = "Same wtxid";
    } else {
      const other = entry.left === null ? current.left : current.right;
      variantCell.textContent = `Not present in ${other.snapshot.source_label}`;
    }
    row.append(transactionCell, leftCell, rightCell, variantCell);
    return row;
  });
  sampleTransactions.replaceChildren(...rows);
};

const syncSampleTransactionSelection = (): void => {
  for (const row of sampleTransactions.querySelectorAll<HTMLTableRowElement>(
    "tr[data-txid]",
  )) {
    row.classList.toggle(
      "selected",
      row.dataset.txid === selectedTransactionId,
    );
  }
};

const renderInspector = (): void => {
  const current = comparison;
  const view = comparisonPolicyView;
  if (current === null || view === null) {
    regionEyebrow.textContent = "Membership region";
    regionName.textContent = "Waiting for two snapshots";
    regionDescription.textContent =
      "Choose two configured sources with complete current membership.";
    policyLeft.textContent = "Source A";
    policyRight.textContent = "Source B";
    policyLeft.disabled = true;
    policyRight.disabled = true;
    policyLeft.setAttribute("aria-pressed", "true");
    policyRight.setAttribute("aria-pressed", "false");
    policySourceRow.dataset.locked = "true";
    matchCount.textContent = "0";
    matchVsize.textContent = "0 vB";
    matchShare.textContent = "0%";
    clearPolicyFilter.disabled = true;
    comparisonRuleList.replaceChildren();
    policyBuckets.replaceChildren();
    policyBucketSummary.textContent = "No snapshot";
    sampleTransactions.replaceChildren();
    sampleSummary.textContent = "No matches";
    return;
  }
  const side = activePolicySide();
  const population = view.population(selectedRegion, side, policyFilter);
  regionEyebrow.textContent = "Membership region";
  regionName.textContent = regionLabel(current, selectedRegion);
  regionDescription.textContent = regionDescriptionText(
    current,
    selectedRegion,
  );
  policyLeft.textContent = current.left.snapshot.source_label;
  policyRight.textContent = current.right.snapshot.source_label;
  policyLeft.disabled = selectedRegion === "right_only";
  policyRight.disabled = selectedRegion === "left_only";
  policyLeft.setAttribute("aria-pressed", String(side === "left"));
  policyRight.setAttribute("aria-pressed", String(side === "right"));
  policySourceRow.dataset.locked = String(selectedRegion !== "common");
  matchCount.textContent = countFormat.format(population.count);
  matchVsize.textContent = formatVsize(population.vsize);
  matchShare.textContent = percentageFormat.format(
    current.totals.union_count === 0
      ? 0
      : population.count / current.totals.union_count,
  );
  clearPolicyFilter.disabled = policyFilter.kind === "all";
  renderRuleFilters(view);
  renderPolicyBuckets(current, view);
  renderSampleTable(current, population);
};

const detailAssessmentText = (assessment: Bip110Assessment): string => {
  if (assessment.status === "compatible") {
    return "Compatible with the deployed Knots mempool policy";
  }
  if (assessment.status === "indeterminate") {
    return "Indeterminate because one or more rule checks remain unresolved";
  }
  const signature = violationSignature(assessment);
  if (signature === null) {
    return "Policy assessment unavailable";
  }
  return signature.completeness === "exact"
    ? `Would violate exactly ${signatureLabel(signature)}`
    : `${signatureLabel(signature)}; unresolved ${unknownRulesLabel(signature)}`;
};

const compactEvidence = (values: unknown[]): string => {
  if (values.length === 0) {
    return "";
  }
  const encoded = JSON.stringify(values[0]) ?? "unavailable exemplar";
  return values.length === 1
    ? encoded
    : `${encoded} · exemplar of ${values.length}`;
};

const detailRuleElement = (rule: RuleAssessment): HTMLLIElement => {
  const item = document.createElement("li");
  item.className = `detail-rule ${rule.verdict}`;
  const heading = document.createElement("div");
  const name = document.createElement("strong");
  name.textContent = `R${rule.number} · ${terrainRule(rule.rule).label}`;
  const verdict = document.createElement("span");
  verdict.textContent = rule.verdict;
  heading.append(name, verdict);
  item.append(heading);
  if (rule.evidence_count > 0) {
    const evidence = document.createElement("p");
    evidence.textContent = `Evidence ${countFormat.format(rule.evidence_count)} · ${compactEvidence(rule.evidence)}`;
    item.append(evidence);
  }
  if (rule.missing_count > 0) {
    const missing = document.createElement("p");
    missing.textContent = `Unresolved ${countFormat.format(rule.missing_count)} · ${compactEvidence(rule.missing)}`;
    item.append(missing);
  }
  return item;
};

type DetailOutcome =
  | { side: ComparisonSide; state: "absent" }
  | { side: ComparisonSide; state: "unclassified" }
  | {
      side: ComparisonSide;
      state: "ready";
      detail: TransactionDetailResponse;
    }
  | { side: ComparisonSide; state: "error"; message: string };

const renderDetailOutcome = (
  current: CurrentComparison,
  entry: ComparedTransaction,
  outcome: DetailOutcome,
): HTMLElement => {
  const source = current[outcome.side];
  const transaction = sourceEntry(entry, outcome.side);
  const panel = document.createElement("section");
  panel.className = "comparison-detail-source";
  const heading = document.createElement("div");
  const name = document.createElement("h3");
  name.textContent = source.snapshot.source_label;
  const membership = document.createElement("span");
  membership.textContent =
    outcome.state === "absent"
      ? "Not present in snapshot"
      : "Present in snapshot";
  heading.append(name, membership);
  panel.append(heading);
  if (transaction !== null) {
    const variant = document.createElement("code");
    variant.textContent = `wtxid ${transaction.wtxid}`;
    variant.title = transaction.wtxid;
    panel.append(variant);
    const facts = document.createElement("p");
    facts.className = "comparison-detail-facts";
    facts.textContent = transactionFactSummary(transaction);
    panel.append(facts);
  }
  if (outcome.state === "absent") {
    const note = document.createElement("p");
    note.textContent = `This txid was not present in the ${source.snapshot.source_label} snapshot observed at ${formatTime(source.snapshot.observed_at_ms)}.`;
    panel.append(note);
  } else if (outcome.state === "unclassified") {
    const note = document.createElement("p");
    note.className = "detail-unclassified";
    note.textContent =
      sourceClassificationPresentation(source).unclassifiedDetail;
    panel.append(note);
  } else if (outcome.state === "error") {
    const note = document.createElement("p");
    note.className = "detail-error";
    note.textContent = outcome.message;
    panel.append(note);
  } else {
    const summary = document.createElement("p");
    summary.className = "comparison-detail-assessment";
    summary.textContent = detailAssessmentText(outcome.detail.assessment);
    const rules = document.createElement("ol");
    rules.className = "detail-rules";
    rules.append(...outcome.detail.rules.map(detailRuleElement));
    panel.append(summary, rules);
  }
  return panel;
};

const detailForSide = async (
  current: CurrentComparison,
  entry: ComparedTransaction,
  side: ComparisonSide,
  signal: AbortSignal,
): Promise<DetailOutcome> => {
  const transaction = sourceEntry(entry, side);
  if (transaction === null) {
    return { side, state: "absent" };
  }
  if (transaction.bip110 === null) {
    return { side, state: "unclassified" };
  }
  try {
    const detail = await fetchTransactionDetail(
      current[side].snapshot.source_id,
      transaction.txid,
      signal,
    );
    if (
      !transactionDetailMatchesSnapshot(
        current[side].snapshot,
        transaction,
        detail,
      )
    ) {
      throw new Error("Transaction detail does not match this source snapshot");
    }
    return { side, state: "ready", detail };
  } catch (error) {
    if (signal.aborted || isAbortError(error)) {
      throw error;
    }
    return {
      side,
      state: "error",
      message:
        error instanceof Error ? error.message : "Policy detail unavailable",
    };
  }
};

const loadSelectedTransactionDetail = async (
  entry: ComparedTransaction,
): Promise<void> => {
  const current = comparison;
  if (current === null || selectedTransactionId !== entry.txid) {
    return;
  }
  detailController?.abort();
  const controller = new AbortController();
  detailController = controller;
  const sequence = ++detailSequence;
  detailStatus.textContent = "Loading source detail…";
  let outcomes: DetailOutcome[];
  try {
    outcomes = await Promise.all([
      detailForSide(current, entry, "left", controller.signal),
      detailForSide(current, entry, "right", controller.signal),
    ]);
  } catch (error) {
    if (controller.signal.aborted || isAbortError(error)) {
      return;
    }
    throw error;
  } finally {
    if (detailController === controller) {
      detailController = null;
    }
  }
  if (
    sequence !== detailSequence ||
    comparison !== current ||
    selectedTransactionId !== entry.txid
  ) {
    return;
  }
  detailStatus.textContent = compactTxid(entry.txid);
  const identity = document.createElement("div");
  identity.className = "comparison-detail-identity";
  const transactionId = document.createElement("code");
  transactionId.textContent = entry.txid;
  transactionId.title = entry.txid;
  identity.append(transactionId);
  if (entry.same_wtxid === false) {
    const warning = document.createElement("p");
    warning.className = "variant-warning";
    warning.textContent =
      "The same txid carries different witness variants. Policy assessments remain source-local.";
    identity.append(warning);
  }
  detailContainer.replaceChildren(
    identity,
    ...outcomes.map((outcome) => renderDetailOutcome(current, entry, outcome)),
  );
};

const renderComparison = (
  current: CurrentComparison,
  view: ComparisonPolicyView,
): void => {
  invalidateComparisonGeometry();
  renderSourceCard(leftSourceCard, "Source A", current.left);
  renderSourceCard(rightSourceCard, "Source B", current.right);
  renderSampling(current);
  comparisonDistributions.render(current);
  renderPolicyMatrix(current, view);
  renderRegionControls(current);
  unionCount.textContent = `${countFormat.format(current.totals.union_count)} txids`;
  comparisonStage.hidden = current.totals.union_count === 0;
  comparisonEmpty.hidden = current.totals.union_count !== 0;
  comparisonEmpty.textContent = "Both sampled mempools are empty.";
  const differingVariants = current.common.filter(
    ({ same_wtxid: sameVariant }) => sameVariant === false,
  ).length;
  comparisonCanvas.setAttribute(
    "aria-label",
    `Current membership comparison with ${formatTxidCount(current.totals.common_count)} present in both snapshots, ${formatTxidCount(current.totals.left_only_count)} observed only in ${current.left.snapshot.source_label} snapshot, and ${formatTxidCount(current.totals.right_only_count)} observed only in ${current.right.snapshot.source_label} snapshot. Use the transaction navigator or arrow keys to reach every transaction.`,
  );
  const unionSentence =
    current.totals.union_count === 1
      ? "The one transaction ID appears once."
      : `Each of the ${formatTxidCount(current.totals.union_count)} appears once.`;
  const variantVerb = differingVariants === 1 ? "carries" : "carry";
  visualSummary.textContent = `${unionSentence} The snapshots were observed ${formatDuration(current.observed_skew_ms)} apart. ${formatTxidCount(differingVariants)} present in both ${variantVerb} different witness variants.`;
  renderTransactionNavigator();
  renderInspector();
  scheduleCanvasRender();

  const stale =
    current.left.source.availability === "stale" ||
    current.right.source.availability === "stale";
  const sameTip =
    current.left.snapshot.chain_tip.hash ===
    current.right.snapshot.chain_tip.hash;
  setStatus(
    stale ? "stale" : "ready",
    stale ? "Comparing a retained snapshot" : "Comparison ready",
    `${formatDuration(current.observed_skew_ms)} observation skew. ${sameTip ? "Both snapshots report the same chain tip." : "The snapshots report different chain tips."}`,
  );
};

const transitionComparisonView = (requested: ComparisonViewState): void => {
  const current = comparison;
  if (current === null) {
    preparePendingView(requested);
    updateQuery();
    return;
  }

  const previous = currentViewState();
  executeComparisonViewTransition(current, previous, requested, {
    applyResolvedView: (resolved) => {
      applyResolvedLoadedView(current, resolved);
    },
    renderPopulation: () => {
      renderRegionControls(current);
      syncPolicyMatrixSelection();
      renderInspector();
    },
    syncTransactionSelection: syncSampleTransactionSelection,
    renderTransactionNavigator,
    updateQuery,
    scheduleCanvasRender,
    loadTransactionDetail: (entry) => {
      void loadSelectedTransactionDetail(entry);
    },
  });
};

const resetComparisonView = (message: string): void => {
  comparison = null;
  comparisonPolicyView = null;
  selectedRegion = "common";
  preferredPolicySide = "left";
  policyFilter = { kind: "all" };
  keyboardTransactionIndex = 0;
  invalidateComparisonGeometry();
  comparisonStage.hidden = true;
  comparisonEmpty.hidden = false;
  comparisonEmpty.textContent = message;
  samplingPanel.hidden = true;
  samplingTimeline.replaceChildren();
  comparisonDistributions.reset();
  policyMatrix.hidden = true;
  policyMatrixBody.replaceChildren();
  resetSourceCard(leftSourceCard, "Source A", leftSourceId, message);
  resetSourceCard(rightSourceCard, "Source B", rightSourceId, message);
  resetRegionControls();
  unionCount.textContent = "0 txids";
  comparisonCanvas.setAttribute(
    "aria-label",
    "No current membership comparison is available.",
  );
  visualSummary.textContent =
    "Each transaction ID appears once: present in both snapshots, observed only in source A snapshot, or observed only in source B snapshot.";
  clearTransactionSelection("Choose a sample");
  renderTransactionNavigator();
  renderInspector();
};

const currentSelectionMatches = (ticket: ComparisonRequestTicket): boolean =>
  lifecycle.isCurrent(ticket, leftSelect.value, rightSelect.value);

const loadComparison = async (
  selectionChanged: boolean,
  requestedState: ComparisonViewState,
): Promise<void> => {
  if (
    leftSourceId.length === 0 ||
    rightSourceId.length === 0 ||
    leftSourceId === rightSourceId
  ) {
    return;
  }
  activeLoads += 1;
  refreshButton.disabled = true;
  refreshButton.textContent = "Loading…";
  const ticket = lifecycle.begin(leftSourceId, rightSourceId);
  if (selectionChanged) {
    resetComparisonView("Loading the selected source snapshots.");
    preparePendingView(requestedState);
    updateQuery();
  }
  setStatus(
    "waiting",
    "Loading current snapshots",
    "Reading each source independently from Atlas memory.",
  );
  try {
    const [leftResponse, rightResponse] = await Promise.all([
      fetchSourceSnapshot(leftSourceId, ticket.signal),
      fetchSourceSnapshot(rightSourceId, ticket.signal),
    ]);
    if (!currentSelectionMatches(ticket)) {
      return;
    }
    replaceSourceSummary(leftResponse.source);
    replaceSourceSummary(rightResponse.source);
    const nextComparison = compareCurrentSnapshots(
      requireLoadedSnapshot(leftResponse),
      requireLoadedSnapshot(rightResponse),
    );
    const nextPolicyView = buildComparisonPolicyView(nextComparison);
    comparison = nextComparison;
    comparisonPolicyView = nextPolicyView;
    const previous = currentViewState();
    const applied = resolveComparisonViewTransition(
      nextComparison,
      previous,
      previous,
    );
    applyResolvedLoadedView(nextComparison, applied);
    populateSourceSelectors();
    renderComparison(nextComparison, nextPolicyView);
    updateQuery();
    if (applied.selectedEntry !== null) {
      void loadSelectedTransactionDetail(applied.selectedEntry);
    }
  } catch (error) {
    if (
      ticket.signal.aborted ||
      isAbortError(error) ||
      !currentSelectionMatches(ticket)
    ) {
      return;
    }
    const message =
      error instanceof Error ? error.message : "Unable to load comparison";
    if (comparison === null) {
      const pendingState = currentViewState();
      resetComparisonView(message);
      preparePendingView(pendingState);
      updateQuery();
    }
    setStatus(
      "error",
      "Comparison unavailable",
      comparison === null
        ? message
        : `${message}. Showing the last browser copy for this source pair.`,
    );
  } finally {
    activeLoads -= 1;
    if (activeLoads === 0) {
      refreshButton.disabled = false;
      refreshButton.textContent = "Refresh";
    }
  }
};

const discoverSources = async (): Promise<void> => {
  activeLoads += 1;
  refreshButton.disabled = true;
  refreshButton.textContent = "Loading…";
  lifecycle.invalidate();
  const ticket = discoveryLifecycle.begin();
  try {
    const response = await fetchSources(ticket.signal);
    if (!discoveryLifecycle.isCurrent(ticket)) {
      return;
    }
    atlasVersion.textContent = `v${response.atlas_version}`;
    configuredSources = response.sources;
    const requestedInitialState = initialViewState;
    const previousLeftSourceId = leftSourceId;
    const previousRightSourceId = rightSourceId;
    chooseInitialSources(requestedInitialState);
    const discoveredPairChanged =
      previousLeftSourceId.length > 0 &&
      previousRightSourceId.length > 0 &&
      (previousLeftSourceId !== leftSourceId ||
        previousRightSourceId !== rightSourceId);
    populateSourceSelectors();
    if (leftSourceId.length === 0 || rightSourceId.length === 0) {
      initialViewState = null;
      lifecycle.invalidate();
      resetComparisonView(
        "Atlas needs two configured sources with complete snapshots before comparison is available.",
      );
      updateQuery();
      setStatus(
        "waiting",
        "Waiting for two snapshots",
        "At least two configured sources must have a complete current snapshot.",
      );
      return;
    }
    const selectionChanged =
      comparison === null ||
      comparison.left.snapshot.source_id !== leftSourceId ||
      comparison.right.snapshot.source_id !== rightSourceId;
    const requestedState = discoveredPairChanged
      ? defaultViewState()
      : currentViewState();
    initialViewState = null;
    await loadComparison(selectionChanged, requestedState);
  } catch (error) {
    if (
      ticket.signal.aborted ||
      isAbortError(error) ||
      !discoveryLifecycle.isCurrent(ticket)
    ) {
      return;
    }
    lifecycle.invalidate();
    const message =
      error instanceof Error ? error.message : "Unable to discover sources";
    if (comparison === null) {
      comparisonEmpty.hidden = false;
      comparisonEmpty.textContent = message;
    }
    setStatus("error", "Atlas website unavailable", message);
  } finally {
    activeLoads -= 1;
    if (activeLoads === 0) {
      refreshButton.disabled = false;
      refreshButton.textContent = "Refresh";
    }
  }
};

const selectRegion = (
  region: ComparisonRegionKey,
  loadFirst: boolean,
): void => {
  const current = comparison;
  if (current === null) {
    return;
  }
  const first = loadFirst
    ? comparisonRegionEntries(current, region)[0]
    : undefined;
  transitionComparisonView({
    ...currentViewState(),
    region,
    filter: { kind: "all" },
    txid: first?.txid ?? null,
  });
};

const selectionChanged = (): void => {
  const nextLeft = leftSelect.value;
  const nextRight = rightSelect.value;
  if (nextLeft === nextRight) {
    return;
  }
  discoveryLifecycle.invalidate();
  lifecycle.invalidate();
  leftSourceId = nextLeft;
  rightSourceId = nextRight;
  populateSourceSelectors();
  void loadComparison(true, defaultViewState());
};

leftSelect.addEventListener("change", selectionChanged);
rightSelect.addEventListener("change", selectionChanged);

swapButton.addEventListener("click", () => {
  discoveryLifecycle.invalidate();
  lifecycle.invalidate();
  [leftSourceId, rightSourceId] = [rightSourceId, leftSourceId];
  populateSourceSelectors();
  void loadComparison(true, defaultViewState());
});

refreshButton.addEventListener("click", () => {
  void discoverSources();
});

transactionSearch.addEventListener("submit", (event) => {
  event.preventDefault();
  const requestedTxid = transactionSearchInput.value.trim().toLowerCase();
  if (!TXID_PATTERN.test(requestedTxid)) {
    setTransactionSearchStatus(
      "error",
      "Enter a 64-character hexadecimal transaction ID.",
    );
    return;
  }
  transitionComparisonView({
    ...currentViewState(),
    txid: requestedTxid,
  });
});

regionControls.addEventListener("click", (event) => {
  const button = (event.target as HTMLElement).closest<HTMLButtonElement>(
    "button[data-region]",
  );
  const region = button?.dataset.region as ComparisonRegionKey | undefined;
  if (region !== undefined) {
    selectRegion(region, true);
  }
});

policyLeft.addEventListener("click", () => {
  if (selectedRegion !== "right_only") {
    transitionComparisonView({
      ...currentViewState(),
      side: "left",
      filter: { kind: "all" },
      txid: null,
    });
  }
});

policyRight.addEventListener("click", () => {
  if (selectedRegion !== "left_only") {
    transitionComparisonView({
      ...currentViewState(),
      side: "right",
      filter: { kind: "all" },
      txid: null,
    });
  }
});

clearPolicyFilter.addEventListener("click", () => {
  choosePolicyFilter({ kind: "all" });
});

comparisonCanvas.addEventListener("click", (event) => {
  const current = comparison;
  if (current === null || comparisonLayout === null) {
    return;
  }
  const bounds = comparisonCanvas.getBoundingClientRect();
  const hit = hitTestComparison(
    comparisonLayout,
    event.clientX - bounds.left,
    event.clientY - bounds.top,
  );
  if (hit?.kind === "region") {
    selectRegion(hit.region.key, true);
    return;
  }
  if (hit?.kind === "transaction") {
    const entry = comparisonRegionEntries(current, hit.glyph.regionKey).find(
      ({ txid }) => txid === hit.glyph.txid,
    );
    if (entry !== undefined) {
      transitionComparisonView({
        ...currentViewState(),
        region: hit.glyph.regionKey,
        txid: entry.txid,
      });
    }
  }
});

const handleTransactionNavigation = (event: KeyboardEvent): void => {
  const current = comparison;
  if (current === null) {
    return;
  }
  const entries = comparisonRegionEntries(current, selectedRegion);
  const move = cursorMoveForKey(event.key);
  if (move !== null) {
    event.preventDefault();
    const nextIndex = moveComparisonCursor(
      keyboardTransactionIndex,
      entries.length,
      move,
    );
    if (nextIndex !== null) {
      keyboardTransactionIndex = nextIndex;
      renderTransactionNavigator();
      scheduleCanvasRender();
    }
    return;
  }
  if (event.key === "Enter" || event.key === " ") {
    const entry = activeNavigatorEntry();
    if (entry !== null) {
      event.preventDefault();
      transitionComparisonView({
        ...currentViewState(),
        txid: entry.txid,
      });
    }
  }
};

comparisonCanvas.addEventListener("keydown", handleTransactionNavigation);
transactionListbox.addEventListener("keydown", handleTransactionNavigation);
transactionListbox.addEventListener("dblclick", () => {
  const entry = activeNavigatorEntry();
  if (entry !== null) {
    transitionComparisonView({
      ...currentViewState(),
      txid: entry.txid,
    });
  }
});

new ResizeObserver(() => {
  invalidateComparisonGeometry();
  scheduleCanvasRender();
}).observe(comparisonCanvas);

const startupViewState = initialViewState;
resetComparisonView("Choose two sources with complete snapshots.");
if (startupViewState !== null) {
  preparePendingView(startupViewState);
  resetRegionControls();
  renderInspector();
}
void discoverSources();
