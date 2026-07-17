import { fetchMempool } from "./api";
import { formatMembershipAge, membershipPage } from "./membership-table";
import { renderSwimView } from "./swim-view";
import { initWorkbench } from "./workbench";
import "./styles.css";
import type { CaptureStatus, MempoolEntry } from "./types";

const requiredElement = <T extends HTMLElement>(id: string): T => {
  const element = document.getElementById(id);
  if (!(element instanceof HTMLElement)) {
    throw new Error(`Missing required element #${id}`);
  }
  return element as T;
};

const status = requiredElement<HTMLParagraphElement>("status");
const captureStatus = requiredElement<HTMLParagraphElement>("capture-status");
const empty = requiredElement<HTMLParagraphElement>("empty");
const visualWrap = requiredElement<HTMLElement>("visual-wrap");
const canvas = requiredElement<HTMLCanvasElement>("mempool-canvas");
const visualSummary = requiredElement<HTMLParagraphElement>("visual-summary");
const inspector = requiredElement<HTMLDetailsElement>("inspector");
const txidSearch = requiredElement<HTMLInputElement>("txid-search");
const tableSummary = requiredElement<HTMLParagraphElement>("table-summary");
const tableBody = requiredElement<HTMLTableSectionElement>("memberships");
const previousPage = requiredElement<HTMLButtonElement>("previous-page");
const nextPage = requiredElement<HTMLButtonElement>("next-page");
const refresh = requiredElement<HTMLButtonElement>("refresh");
const membershipHeading = requiredElement<HTMLElement>("membership-heading");
const membershipPanel = requiredElement<HTMLDetailsElement>("membership-panel");

const workbench = initWorkbench();

const countFormat = new Intl.NumberFormat();
const decimalFormat = new Intl.NumberFormat(undefined, {
  maximumFractionDigits: 2,
});

let currentMemberships: MempoolEntry[] = [];
let tablePageIndex = 0;
let renderedAtMs = Date.now();
let pendingRenderFrame: number | null = null;

const appendCell = (
  row: HTMLTableRowElement,
  value: string,
  className?: string,
): void => {
  const cell = document.createElement("td");
  cell.textContent = value;
  if (className !== undefined) {
    cell.className = className;
  }
  row.append(cell);
};

const renderMembership = (membership: MempoolEntry): HTMLTableRowElement => {
  const row = document.createElement("tr");

  const txidCell = document.createElement("td");
  const txid = document.createElement("code");
  txid.textContent = membership.txid;
  txidCell.append(txid);
  row.append(txidCell);

  if (membership.facts.status === "awaiting_rpc") {
    appendCell(row, "Awaiting RPC", "pending-value");
    appendCell(row, "Awaiting RPC", "pending-value");
    appendCell(row, "Awaiting RPC", "pending-value");
  } else {
    appendCell(
      row,
      decimalFormat.format(membership.facts.fee_sats / membership.facts.vsize),
    );
    appendCell(row, countFormat.format(membership.facts.vsize));
    appendCell(
      row,
      formatMembershipAge(renderedAtMs - membership.facts.entered_at_ms),
    );
  }

  appendCell(row, new Date(membership.updated_at_ms).toLocaleString());
  return row;
};

const renderTable = (): void => {
  const page = membershipPage(
    currentMemberships,
    txidSearch.value,
    tablePageIndex,
  );
  tablePageIndex = page.pageIndex;
  tableBody.replaceChildren(...page.entries.map(renderMembership));
  previousPage.disabled = page.pageIndex === 0 || page.pageCount === 0;
  nextPage.disabled =
    page.pageCount === 0 || page.pageIndex >= page.pageCount - 1;

  tableSummary.textContent =
    page.matchCount === 0
      ? "No transaction IDs match this prefix."
      : `Showing ${countFormat.format(page.firstMatchNumber)}–${countFormat.format(page.lastMatchNumber)} of ${countFormat.format(page.matchCount)} matching transactions. Page ${countFormat.format(page.pageIndex + 1)} of ${countFormat.format(page.pageCount)}.`;
};

const renderVisual = (): void => {
  pendingRenderFrame = null;
  if (currentMemberships.length === 0 || visualWrap.hidden) {
    return;
  }
  const summary = renderSwimView(canvas, currentMemberships, renderedAtMs);
  const enriched = countFormat.format(summary.availableCount);
  const awaiting = countFormat.format(summary.awaitingCount);
  visualSummary.textContent = `${enriched} transactions have RPC fee, size, and entry-time facts, representing ${countFormat.format(summary.totalVsize)} vB. ${awaiting} transactions are awaiting RPC facts.`;
  canvas.setAttribute(
    "aria-label",
    `Static mempool plot containing ${countFormat.format(currentMemberships.length)} transactions. ${enriched} have RPC facts and ${awaiting} are awaiting RPC facts. Individual base fee rate increases from bottom to top, age runs from oldest on the left to newest on the right, and glyph area represents virtual size.`,
  );
};

const scheduleVisualRender = (): void => {
  if (pendingRenderFrame !== null || currentMemberships.length === 0) {
    return;
  }
  pendingRenderFrame = window.requestAnimationFrame(renderVisual);
};

const clearMemberships = (): void => {
  currentMemberships = [];
  tablePageIndex = 0;
  tableBody.replaceChildren();
  visualWrap.hidden = true;
  inspector.hidden = true;
  if (pendingRenderFrame !== null) {
    window.cancelAnimationFrame(pendingRenderFrame);
    pendingRenderFrame = null;
  }
};

const renderCaptureStatus = (
  sourceId: string,
  capture: CaptureStatus,
): void => {
  captureStatus.hidden = false;
  if (capture.status === "no_reported_gaps") {
    captureStatus.dataset.certainty = "none";
    captureStatus.textContent = `No capture gaps have been reported for ${sourceId}. This is not proof of complete forensic coverage.`;
    return;
  }

  const since = new Date(capture.first_gap_at_ms).toLocaleString();
  const details = `${capture.latest_input}: ${capture.latest_reason}`;
  captureStatus.dataset.certainty = capture.strongest_certainty;
  captureStatus.textContent =
    capture.strongest_certainty === "known_loss"
      ? `${sourceId}'s peer-observer evidence history contains a known gap since ${since} (${details}). RPC may still have reconciled current membership.`
      : `${sourceId}'s peer-observer evidence history may contain a gap since ${since} (${details}). RPC may still have reconciled current membership.`;
};

const loadMemberships = async (): Promise<void> => {
  refresh.disabled = true;
  status.dataset.state = "loading";
  status.textContent = "Loading memberships…";

  const selectedSource = workbench.getSelectedSource();
  if (selectedSource === null) {
    clearMemberships();
    empty.hidden = true;
    captureStatus.hidden = true;
    status.dataset.state = "error";
    status.textContent = "Waiting for a source to be selected.";
    refresh.disabled = false;
    return;
  }

  try {
    const response = await fetchMempool(selectedSource);
    currentMemberships = response.memberships;
    renderedAtMs = Date.now();
    tablePageIndex = 0;
    membershipHeading.textContent = `Per-transaction detail · ${response.source_id}`;
    renderCaptureStatus(response.source_id, response.health.capture);
    empty.textContent = `No transactions are currently present in ${response.source_id}.`;
    empty.hidden = currentMemberships.length !== 0;
    visualWrap.hidden = currentMemberships.length === 0;
    inspector.hidden = currentMemberships.length === 0;
    if (currentMemberships.length === 0) {
      clearMemberships();
    } else {
      renderTable();
      scheduleVisualRender();
    }
    status.dataset.state = "ready";
    status.textContent = `${countFormat.format(currentMemberships.length)} transaction${currentMemberships.length === 1 ? "" : "s"} currently present. Latest source evidence arrived ${new Date(response.health.last_seen_at_ms).toLocaleString()}.`;
  } catch (error) {
    clearMemberships();
    empty.hidden = true;
    captureStatus.hidden = true;
    status.dataset.state = "error";
    status.textContent =
      error instanceof Error ? error.message : "Unable to load memberships";
  } finally {
    refresh.disabled = false;
  }
};

refresh.addEventListener("click", () => {
  void loadMemberships();
});

txidSearch.addEventListener("input", () => {
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

new ResizeObserver(scheduleVisualRender).observe(canvas);

// Per-transaction detail is on-demand: the workbench above is aggregate-only,
// so the full membership snapshot is only fetched when the panel is opened.
membershipPanel.addEventListener("toggle", () => {
  if (membershipPanel.open && currentMemberships.length === 0) {
    void loadMemberships();
  }
});

workbench.onSourceChange(() => {
  clearMemberships();
  status.dataset.state = "ready";
  status.textContent = "Open to load.";
  if (membershipPanel.open) {
    void loadMemberships();
  }
});
