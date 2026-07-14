import { fetchMempool } from "./api";
import "./styles.css";
import type { Membership } from "./types";

const requiredElement = <T extends HTMLElement>(id: string): T => {
  const element = document.getElementById(id);
  if (!(element instanceof HTMLElement)) {
    throw new Error(`Missing required element #${id}`);
  }
  return element as T;
};

const status = requiredElement<HTMLParagraphElement>("status");
const empty = requiredElement<HTMLParagraphElement>("empty");
const tableWrap = requiredElement<HTMLDivElement>("table-wrap");
const tableBody = requiredElement<HTMLTableSectionElement>("memberships");
const refresh = requiredElement<HTMLButtonElement>("refresh");

const appendCell = (row: HTMLTableRowElement, value: string): void => {
  const cell = document.createElement("td");
  cell.textContent = value;
  row.append(cell);
};

const renderMembership = (membership: Membership): HTMLTableRowElement => {
  const row = document.createElement("tr");
  appendCell(row, membership.source_id);

  const txidCell = document.createElement("td");
  const txid = document.createElement("code");
  txid.textContent = membership.txid;
  txidCell.append(txid);
  row.append(txidCell);

  appendCell(row, membership.present ? "yes" : "no");
  return row;
};

const loadMemberships = async (): Promise<void> => {
  refresh.disabled = true;
  status.dataset.state = "loading";
  status.textContent = "Loading memberships…";

  try {
    const response = await fetchMempool();
    const memberships = [...response.memberships].sort(
      (left, right) =>
        left.source_id.localeCompare(right.source_id) ||
        left.txid.localeCompare(right.txid),
    );

    tableBody.replaceChildren(...memberships.map(renderMembership));
    empty.hidden = memberships.length !== 0;
    tableWrap.hidden = memberships.length === 0;
    status.dataset.state = "ready";
    status.textContent = `${memberships.length} membership${memberships.length === 1 ? "" : "s"} loaded${response.source_id === null ? "" : ` for ${response.source_id}`}.`;
  } catch (error) {
    tableBody.replaceChildren();
    tableWrap.hidden = true;
    empty.hidden = true;
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
