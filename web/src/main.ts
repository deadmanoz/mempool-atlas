import { fetchSources, fetchSourceSnapshot } from "./api";
import {
  DEFAULT_FILTERS,
  filterTransactions,
  type MempoolFilters,
} from "./filters";
import { formatMembershipAge, membershipPage } from "./membership-table";
import { renderSwimView } from "./swim-view";
import "./styles.css";
import type {
  MempoolSnapshot,
  MempoolTransaction,
  SourceSnapshotResponse,
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
const filtersForm = requiredElement<HTMLFormElement>("filters");
const minimumFeeRate = requiredElement<HTMLInputElement>("minimum-fee-rate");
const maximumAge = requiredElement<HTMLSelectElement>("maximum-age");
const minimumVsize = requiredElement<HTMLInputElement>("minimum-vsize");
const resetFilters = requiredElement<HTMLButtonElement>("reset-filters");
const filterSummary = requiredElement<HTMLElement>("filter-summary");
const visualWrap = requiredElement<HTMLElement>("visual-wrap");
const canvas = requiredElement<HTMLCanvasElement>("mempool-canvas");
const empty = requiredElement<HTMLElement>("empty");
const visualSummary = requiredElement<HTMLElement>("visual-summary");
const inspector = requiredElement<HTMLDetailsElement>("inspector");
const txidSearchForm = requiredElement<HTMLFormElement>("txid-search-form");
const txidSearch = requiredElement<HTMLInputElement>("txid-search");
const tableSummary = requiredElement<HTMLElement>("table-summary");
const tableBody = requiredElement<HTMLTableSectionElement>("transactions");
const previousPage = requiredElement<HTMLButtonElement>("previous-page");
const nextPage = requiredElement<HTMLButtonElement>("next-page");

const countFormat = new Intl.NumberFormat();
const decimalFormat = new Intl.NumberFormat(undefined, {
  maximumFractionDigits: 2,
});

let selectedSourceId: string | null = null;
let currentSnapshot: MempoolSnapshot | null = null;
let filteredTransactions: MempoolTransaction[] = [];
let tablePageIndex = 0;
let pendingRenderFrame: number | null = null;

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
    return "less than a minute ago";
  }
  return `${formatMembershipAge(ageMs)} ago`;
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

const renderTable = (): void => {
  if (currentSnapshot === null) {
    tableBody.replaceChildren();
    tableSummary.textContent = "No snapshot loaded.";
    previousPage.disabled = true;
    nextPage.disabled = true;
    return;
  }
  const snapshot = currentSnapshot;

  const page = membershipPage(
    filteredTransactions,
    txidSearch.value,
    tablePageIndex,
  );
  tablePageIndex = page.pageIndex;
  const rows = page.entries.map((transaction) => {
    const row = document.createElement("tr");
    const txidCell = document.createElement("td");
    const txid = document.createElement("code");
    txid.textContent = transaction.txid;
    txidCell.append(txid);
    row.append(txidCell);

    for (const value of [
      decimalFormat.format(transaction.fee_sats / transaction.vsize),
      countFormat.format(transaction.vsize),
      formatMembershipAge(snapshot.observed_at_ms - transaction.entered_at_ms),
    ]) {
      const cell = document.createElement("td");
      cell.textContent = value;
      row.append(cell);
    }
    return row;
  });
  tableBody.replaceChildren(...rows);
  previousPage.disabled = page.pageIndex === 0 || page.pageCount === 0;
  nextPage.disabled =
    page.pageCount === 0 || page.pageIndex >= page.pageCount - 1;
  tableSummary.textContent =
    page.matchCount === 0
      ? "No transaction IDs match that prefix."
      : `Showing ${countFormat.format(page.firstMatchNumber)}–${countFormat.format(page.lastMatchNumber)} of ${countFormat.format(page.matchCount)} matching transactions. Page ${countFormat.format(page.pageIndex + 1)} of ${countFormat.format(page.pageCount)}.`;
};

const renderVisual = (): void => {
  pendingRenderFrame = null;
  if (
    currentSnapshot === null ||
    filteredTransactions.length === 0 ||
    visualWrap.hidden
  ) {
    return;
  }
  const summary = renderSwimView(
    canvas,
    filteredTransactions,
    currentSnapshot.observed_at_ms,
  );
  visualSummary.textContent = `${countFormat.format(summary.transactionCount)} transactions, representing ${formatVsize(summary.totalVsize)}. Rows are base fee rate, columns and colour are age, and square area is virtual size.`;
  canvas.setAttribute(
    "aria-label",
    `Mempool snapshot containing ${countFormat.format(summary.transactionCount)} filtered transactions. Fee rate increases from bottom to top, age runs from oldest on the left to newest on the right, and square area represents virtual size.`,
  );
};

const scheduleVisualRender = (): void => {
  if (
    pendingRenderFrame !== null ||
    currentSnapshot === null ||
    filteredTransactions.length === 0
  ) {
    return;
  }
  pendingRenderFrame = window.requestAnimationFrame(renderVisual);
};

const applyFilters = (): void => {
  if (currentSnapshot === null) {
    filteredTransactions = [];
    return;
  }
  filteredTransactions = filterTransactions(
    currentSnapshot.transactions,
    readFilters(),
    currentSnapshot.observed_at_ms,
  );
  tablePageIndex = 0;
  txidSearch.value = "";
  filterSummary.textContent = `Showing ${countFormat.format(filteredTransactions.length)} of ${countFormat.format(currentSnapshot.transaction_count)} transactions.`;
  visualWrap.hidden = filteredTransactions.length === 0;
  empty.hidden = filteredTransactions.length !== 0;
  empty.textContent =
    currentSnapshot.transaction_count === 0
      ? "This snapshot contains an empty mempool."
      : "No transactions match the current filters.";
  inspector.hidden = currentSnapshot.transaction_count === 0;
  renderTable();
  scheduleVisualRender();
};

const renderResponse = (response: SourceSnapshotResponse): void => {
  const { source, snapshot } = response;
  sourceLabel.textContent = source.source_label;
  sourceId.textContent = source.source_id;
  pageStatus.dataset.state = source.availability;

  if (snapshot === null) {
    currentSnapshot = null;
    filteredTransactions = [];
    visualWrap.hidden = true;
    inspector.hidden = true;
    empty.hidden = false;
    transactionCount.textContent = "Waiting";
    totalVsize.textContent = "Waiting";
    observedValue.textContent = "No snapshot";
    tipValue.textContent = "Unknown";
    filterSummary.textContent = "No snapshot loaded.";
    if (source.availability === "error") {
      statusTitle.textContent = "Node snapshot unavailable";
      statusDetail.textContent = `Latest poll failed: ${source.last_error ?? "unknown error"}`;
      empty.textContent = "Atlas has not received a valid snapshot yet.";
    } else {
      statusTitle.textContent = "Waiting for the first snapshot";
      statusDetail.textContent = `Atlas polls this node every ${formatPollInterval(source.poll_interval_seconds)}.`;
      empty.textContent =
        "The first complete mempool snapshot is being collected.";
    }
    renderTable();
    return;
  }

  currentSnapshot = snapshot;
  transactionCount.textContent = countFormat.format(snapshot.transaction_count);
  totalVsize.textContent = formatVsize(snapshot.total_vsize);
  observedValue.textContent = formatSnapshotFreshness(snapshot.observed_at_ms);
  observedValue.title = new Date(snapshot.observed_at_ms).toLocaleString();
  tipValue.textContent = `${countFormat.format(snapshot.chain_tip.height)} · ${snapshot.chain_tip.hash.slice(0, 10)}…`;
  tipValue.title = snapshot.chain_tip.hash;

  if (source.availability === "stale") {
    statusTitle.textContent = "Showing the last good snapshot";
    statusDetail.textContent = `Observed ${new Date(snapshot.observed_at_ms).toLocaleString()}. Latest poll failed: ${source.last_error ?? "unknown error"}`;
  } else {
    statusTitle.textContent = "Snapshot healthy";
    statusDetail.textContent = `Observed ${new Date(snapshot.observed_at_ms).toLocaleString()}. Atlas checks the node every ${formatPollInterval(source.poll_interval_seconds)}.`;
  }
  applyFilters();
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
    refreshButton.textContent = "Refresh snapshot";
  }
};

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

txidSearchForm.addEventListener("submit", (event) => {
  event.preventDefault();
  tablePageIndex = 0;
  renderTable();
});

previousPage.addEventListener("click", () => {
  tablePageIndex -= 1;
  renderTable();
});

nextPage.addEventListener("click", () => {
  tablePageIndex += 1;
  renderTable();
});

refreshButton.addEventListener("click", () => {
  void loadSnapshot();
});

new ResizeObserver(scheduleVisualRender).observe(canvas);

void loadSnapshot();
