import { fetchMempool } from "./api";
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
const tableWrap = requiredElement<HTMLDivElement>("table-wrap");
const tableBody = requiredElement<HTMLTableSectionElement>("memberships");
const refresh = requiredElement<HTMLButtonElement>("refresh");
const membershipHeading =
  requiredElement<HTMLHeadingElement>("membership-heading");

const querySource = new URL(window.location.href).searchParams
  .get("source")
  ?.trim();
const configuredSource = import.meta.env.VITE_ATLAS_SOURCE_ID?.trim();
const selectedSource = querySource || configuredSource || null;

const appendCell = (row: HTMLTableRowElement, value: string): void => {
  const cell = document.createElement("td");
  cell.textContent = value;
  row.append(cell);
};

const renderMembership = (membership: MempoolEntry): HTMLTableRowElement => {
  const row = document.createElement("tr");

  const txidCell = document.createElement("td");
  const txid = document.createElement("code");
  txid.textContent = membership.txid;
  txidCell.append(txid);
  row.append(txidCell);

  appendCell(row, new Date(membership.updated_at_ms).toLocaleString());
  return row;
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

  if (selectedSource === null) {
    tableBody.replaceChildren();
    tableWrap.hidden = true;
    empty.hidden = true;
    captureStatus.hidden = true;
    status.dataset.state = "error";
    status.textContent =
      "Choose a source with ?source=<source-id> or configure VITE_ATLAS_SOURCE_ID.";
    refresh.disabled = true;
    return;
  }

  try {
    const response = await fetchMempool(selectedSource);
    const memberships = [...response.memberships].sort((left, right) =>
      left.txid.localeCompare(right.txid),
    );

    tableBody.replaceChildren(...memberships.map(renderMembership));
    membershipHeading.textContent = `${response.source_id} mempool`;
    renderCaptureStatus(response.source_id, response.health.capture);
    empty.textContent = `No transactions are currently present in ${response.source_id}.`;
    empty.hidden = memberships.length !== 0;
    tableWrap.hidden = memberships.length === 0;
    status.dataset.state = "ready";
    status.textContent = `${memberships.length} transaction${memberships.length === 1 ? "" : "s"} currently present. Latest source evidence arrived ${new Date(response.health.last_seen_at_ms).toLocaleString()}.`;
  } catch (error) {
    tableBody.replaceChildren();
    tableWrap.hidden = true;
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

void loadMemberships();
