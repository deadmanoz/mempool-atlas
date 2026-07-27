import {
  fetchSources,
  fetchSourceSnapshot,
  fetchTransactionDetail,
  transactionDetailMatchesSnapshot,
} from "./api";
import {
  ComparisonLifecycle,
  RequestLifecycle,
  type ComparisonRequestTicket,
} from "./comparison-lifecycle";
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
  compareCurrentSnapshots,
  comparisonPolicyPopulation,
  comparisonRegionEntries,
  policyFilterCount,
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
  TERRAIN_RULES,
  signatureLabel,
  signaturePopulations,
  statusPopulation,
  terrainRule,
  unknownRulesLabel,
  violationSignature,
  type StatusRegionKey,
  type ViolationSignatureKey,
} from "./terrain";
import "./styles.css";
import type {
  Bip110Assessment,
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
const pageStatus = requiredElement<HTMLElement>("comparison-status");
const statusTitle = requiredElement<HTMLElement>("comparison-status-title");
const statusDetail = requiredElement<HTMLElement>("comparison-status-detail");
const leftSelect = requiredElement<HTMLSelectElement>("left-source");
const rightSelect = requiredElement<HTMLSelectElement>("right-source");
const swapButton = requiredElement<HTMLButtonElement>("swap-sources");
const leftSourceCard = requiredElement<HTMLElement>("left-source-card");
const rightSourceCard = requiredElement<HTMLElement>("right-source-card");
const samplingPanel = requiredElement<HTMLElement>("sampling-panel");
const samplingSummary = requiredElement<HTMLElement>("sampling-summary");
const chainSummary = requiredElement<HTMLElement>("chain-summary");
const samplingTimeline = requiredElement<HTMLElement>("sampling-timeline");
const samplingNote = requiredElement<HTMLElement>("sampling-note");
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

const countFormat = new Intl.NumberFormat();
const decimalFormat = new Intl.NumberFormat(undefined, {
  maximumFractionDigits: 2,
});
const percentageFormat = new Intl.NumberFormat(undefined, {
  maximumFractionDigits: 2,
  style: "percent",
});

const lifecycle = new ComparisonLifecycle();
const discoveryLifecycle = new RequestLifecycle();
let configuredSources: SourceSummary[] = [];
let leftSourceId = "";
let rightSourceId = "";
let comparison: CurrentComparison | null = null;
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

const clearDetail = (message: string): void => {
  detailController?.abort();
  detailController = null;
  detailSequence += 1;
  selectedTransactionId = null;
  detailStatus.textContent = message;
  detailContainer.replaceChildren();
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

const chooseInitialSources = (): void => {
  const available = configuredSources.filter(hasSnapshot);
  const known = new Set(available.map(({ source_id: sourceId }) => sourceId));
  const query = new URLSearchParams(window.location.search);
  const requestedLeft = query.get("left");
  const requestedRight = query.get("right");
  const retainedLeft = known.has(leftSourceId) ? leftSourceId : null;
  const retainedRight = known.has(rightSourceId) ? rightSourceId : null;
  leftSourceId =
    (requestedLeft !== null && known.has(requestedLeft)
      ? requestedLeft
      : retainedLeft) ??
    available[0]?.source_id ??
    "";
  rightSourceId =
    (requestedRight !== null &&
    requestedRight !== leftSourceId &&
    known.has(requestedRight)
      ? requestedRight
      : retainedRight !== leftSourceId
        ? retainedRight
        : null) ??
    available.find(({ source_id: sourceId }) => sourceId !== leftSourceId)
      ?.source_id ??
    "";
};

const updateQuery = (): void => {
  const url = new URL(window.location.href);
  if (leftSourceId.length === 0 || rightSourceId.length === 0) {
    url.searchParams.delete("left");
    url.searchParams.delete("right");
  } else {
    url.searchParams.set("left", leftSourceId);
    url.searchParams.set("right", rightSourceId);
  }
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

const renderSourceCard = (
  card: HTMLElement,
  sideLabel: string,
  loaded: LoadedSourceSnapshot,
): void => {
  const { source, snapshot } = loaded;
  const classifiedCount =
    snapshot.bip110_summary.compatible_count +
    snapshot.bip110_summary.violating_count +
    snapshot.bip110_summary.indeterminate_count;
  card.dataset.state = source.availability;
  const eyebrow = document.createElement("p");
  eyebrow.textContent = sideLabel;
  const heading = document.createElement("h3");
  heading.textContent = source.source_label;
  const identifier = document.createElement("code");
  identifier.textContent = source.source_id;
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
      `${snapshot.bip110_summary.evaluator_id} ${snapshot.bip110_summary.evaluator_version} · revision ${countFormat.format(snapshot.classification_revision)} · ${countFormat.format(classifiedCount)} classified`,
    ),
  );
  card.replaceChildren(eyebrow, heading, identifier, facts);
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
      activeNavigatorEntry()?.txid ?? null,
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
): HTMLElement => {
  const chips = document.createElement("span");
  chips.className = "rule-chips comparison-rule-chips";
  if (transaction === null) {
    chips.append(ruleChip("Not present", "absent"));
    return chips;
  }
  const assessment = transaction.bip110;
  if (assessment === null) {
    chips.append(ruleChip("Not classified", "unclassified"));
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

const activePolicySide = (): ComparisonSide =>
  policySideForRegion(selectedRegion, preferredPolicySide);

const regionTransactionsForPolicy = (
  current: CurrentComparison,
): MempoolTransaction[] => {
  const side = activePolicySide();
  return comparisonRegionEntries(current, selectedRegion).flatMap((entry) => {
    const transaction = sourceEntry(entry, side);
    return transaction === null ? [] : [transaction];
  });
};

const filtersMatch = (
  left: ComparisonPolicyFilter,
  right: ComparisonPolicyFilter,
): boolean => {
  if (left.kind !== right.kind) {
    return false;
  }
  if (left.kind === "all") {
    return true;
  }
  if (left.kind === "rule") {
    return left.rule === (right as { kind: "rule"; rule: string }).rule;
  }
  if (left.kind === "status") {
    return (
      left.status ===
      (right as { kind: "status"; status: StatusRegionKey }).status
    );
  }
  return (
    left.signature ===
    (right as { kind: "signature"; signature: ViolationSignatureKey }).signature
  );
};

const choosePolicyFilter = (filter: ComparisonPolicyFilter): void => {
  policyFilter = filter;
  clearDetail("Choose a sample");
  renderInspector();
};

const renderRuleFilters = (current: CurrentComparison): void => {
  const buttons = TERRAIN_RULES.map((rule) => {
    const filter: ComparisonPolicyFilter = { kind: "rule", rule: rule.id };
    const count = policyFilterCount(
      current,
      selectedRegion,
      activePolicySide(),
      filter,
    );
    const button = document.createElement("button");
    button.type = "button";
    button.setAttribute(
      "aria-pressed",
      String(filtersMatch(policyFilter, filter)),
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
    String(filtersMatch(policyFilter, filter)),
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

const renderPolicyBuckets = (current: CurrentComparison): void => {
  const transactions = regionTransactionsForPolicy(current);
  const buttons: HTMLButtonElement[] = [];
  const statusLabels: Array<[StatusRegionKey, string]> = [
    ["compatible", "Compatible"],
    ["indeterminate", "Indeterminate"],
    ["unclassified", "Not classified"],
  ];
  for (const [status, label] of statusLabels) {
    const population = statusPopulation(transactions, status);
    if (population.count === 0) {
      continue;
    }
    buttons.push(
      bucketButton(
        label,
        formatVsize(population.vsize),
        population.count,
        { kind: "status", status },
        "status",
      ),
    );
  }
  for (const population of signaturePopulations(transactions)) {
    const signature = population.signature;
    const detail =
      signature.completeness === "exact"
        ? formatVsize(population.vsize)
        : `${formatVsize(population.vsize)} · unresolved ${unknownRulesLabel(signature)}`;
    buttons.push(
      bucketButton(
        signatureLabel(signature),
        detail,
        population.count,
        { kind: "signature", signature: signature.key },
        signature.completeness,
      ),
    );
  }
  policyBucketSummary.textContent = `${countFormat.format(buttons.length)} observed buckets`;
  policyBuckets.replaceChildren(...buttons);
};

const renderSampleTable = (current: CurrentComparison): void => {
  const side = activePolicySide();
  const population = comparisonPolicyPopulation(
    current,
    selectedRegion,
    side,
    policyFilter,
  );
  const entries = [...population.entries]
    .sort((left, right) => {
      const leftTransaction = sourceEntry(left, side);
      const rightTransaction = sourceEntry(right, side);
      return (
        (rightTransaction?.vsize ?? 0) - (leftTransaction?.vsize ?? 0) ||
        left.txid.localeCompare(right.txid)
      );
    })
    .slice(0, 12);
  sampleSummary.textContent =
    population.count === 0
      ? "No matches"
      : `Largest ${countFormat.format(entries.length)} of ${countFormat.format(population.count)}`;
  const rows = entries.map((entry) => {
    const row = document.createElement("tr");
    row.classList.toggle("selected", entry.txid === selectedTransactionId);
    const transactionCell = document.createElement("td");
    const select = document.createElement("button");
    select.type = "button";
    select.className = "tx-select";
    select.textContent = compactTxid(entry.txid);
    select.title = entry.txid;
    select.addEventListener("click", () => {
      void loadTransactionDetail(entry);
    });
    transactionCell.append(select);
    const leftCell = document.createElement("td");
    leftCell.append(assessmentChips(entry.left));
    const rightCell = document.createElement("td");
    rightCell.append(assessmentChips(entry.right));
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

const renderInspector = (): void => {
  const current = comparison;
  if (current === null) {
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
  const population = comparisonPolicyPopulation(
    current,
    selectedRegion,
    side,
    policyFilter,
  );
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
  renderRuleFilters(current);
  renderPolicyBuckets(current);
  renderSampleTable(current);
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
  }
  if (outcome.state === "absent") {
    const note = document.createElement("p");
    note.textContent = `This txid was not present in the ${source.snapshot.source_label} snapshot observed at ${formatTime(source.snapshot.observed_at_ms)}.`;
    panel.append(note);
  } else if (outcome.state === "unclassified") {
    const note = document.createElement("p");
    note.className = "detail-unclassified";
    note.textContent =
      "The transaction is present, but no complete policy assessment is available yet.";
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

const loadTransactionDetail = async (
  entry: ComparedTransaction,
): Promise<void> => {
  const current = comparison;
  if (current === null) {
    return;
  }
  detailController?.abort();
  const controller = new AbortController();
  detailController = controller;
  const sequence = ++detailSequence;
  selectedTransactionId = entry.txid;
  const entries = comparisonRegionEntries(current, selectedRegion);
  const entryIndex = entries.findIndex(({ txid }) => txid === entry.txid);
  if (entryIndex !== -1) {
    keyboardTransactionIndex = entryIndex;
  }
  detailStatus.textContent = "Loading source detail…";
  renderTransactionNavigator();
  renderSampleTable(current);
  scheduleCanvasRender();
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
  if (sequence !== detailSequence || comparison !== current) {
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
  scheduleCanvasRender();
};

const renderComparison = (current: CurrentComparison): void => {
  invalidateComparisonGeometry();
  renderSourceCard(leftSourceCard, "Source A", current.left);
  renderSourceCard(rightSourceCard, "Source B", current.right);
  renderSampling(current);
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

const resetComparisonView = (message: string): void => {
  comparison = null;
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
  clearDetail("Choose a sample");
  renderTransactionNavigator();
  renderInspector();
};

const currentSelectionMatches = (ticket: ComparisonRequestTicket): boolean =>
  lifecycle.isCurrent(ticket, leftSelect.value, rightSelect.value);

const loadComparison = async (selectionChanged: boolean): Promise<void> => {
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
    comparison = compareCurrentSnapshots(
      requireLoadedSnapshot(leftResponse),
      requireLoadedSnapshot(rightResponse),
    );
    selectedRegion = "common";
    preferredPolicySide = "left";
    policyFilter = { kind: "all" };
    keyboardTransactionIndex = 0;
    clearDetail("Choose a sample");
    populateSourceSelectors();
    renderComparison(comparison);
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
      resetComparisonView(message);
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
    configuredSources = response.sources;
    chooseInitialSources();
    populateSourceSelectors();
    updateQuery();
    if (leftSourceId.length === 0 || rightSourceId.length === 0) {
      lifecycle.invalidate();
      resetComparisonView(
        "Atlas needs two configured sources with complete snapshots before comparison is available.",
      );
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
    await loadComparison(selectionChanged);
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
    resetComparisonView(message);
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
  if (comparison === null) {
    return;
  }
  selectedRegion = region;
  keyboardTransactionIndex = 0;
  policyFilter = { kind: "all" };
  clearDetail("Choose a sample");
  renderRegionControls(comparison);
  renderTransactionNavigator();
  renderInspector();
  scheduleCanvasRender();
  if (loadFirst) {
    const first = comparisonRegionEntries(comparison, region)[0];
    if (first !== undefined) {
      void loadTransactionDetail(first);
    }
  }
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
  updateQuery();
  populateSourceSelectors();
  void loadComparison(true);
};

leftSelect.addEventListener("change", selectionChanged);
rightSelect.addEventListener("change", selectionChanged);

swapButton.addEventListener("click", () => {
  discoveryLifecycle.invalidate();
  lifecycle.invalidate();
  [leftSourceId, rightSourceId] = [rightSourceId, leftSourceId];
  updateQuery();
  populateSourceSelectors();
  void loadComparison(true);
});

refreshButton.addEventListener("click", () => {
  void discoverSources();
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
    preferredPolicySide = "left";
    policyFilter = { kind: "all" };
    clearDetail("Choose a sample");
    renderInspector();
  }
});

policyRight.addEventListener("click", () => {
  if (selectedRegion !== "left_only") {
    preferredPolicySide = "right";
    policyFilter = { kind: "all" };
    clearDetail("Choose a sample");
    renderInspector();
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
    if (hit.glyph.regionKey !== selectedRegion) {
      selectRegion(hit.glyph.regionKey, false);
    }
    const entry = comparisonRegionEntries(current, hit.glyph.regionKey).find(
      ({ txid }) => txid === hit.glyph.txid,
    );
    if (entry !== undefined) {
      const entries = comparisonRegionEntries(current, hit.glyph.regionKey);
      keyboardTransactionIndex = entries.findIndex(
        ({ txid }) => txid === entry.txid,
      );
      renderTransactionNavigator();
      void loadTransactionDetail(entry);
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
      void loadTransactionDetail(entry);
    }
  }
};

comparisonCanvas.addEventListener("keydown", handleTransactionNavigation);
transactionListbox.addEventListener("keydown", handleTransactionNavigation);
transactionListbox.addEventListener("dblclick", () => {
  const entry = activeNavigatorEntry();
  if (entry !== null) {
    void loadTransactionDetail(entry);
  }
});

new ResizeObserver(() => {
  invalidateComparisonGeometry();
  scheduleCanvasRender();
}).observe(comparisonCanvas);

resetComparisonView("Choose two sources with complete snapshots.");
void discoverSources();
