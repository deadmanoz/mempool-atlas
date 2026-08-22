const mempoolSpaceTransactionUrl = (txid: string): string =>
  `https://mempool.space/tx/${encodeURIComponent(txid)}`;

export const createMempoolSpaceTransactionLink = (
  txid: string,
): HTMLAnchorElement => {
  const link = document.createElement("a");
  link.className = "transaction-explorer-link";
  link.href = mempoolSpaceTransactionUrl(txid);
  link.target = "_blank";
  link.rel = "noopener noreferrer";
  link.title = `View ${txid} on mempool.space`;
  link.setAttribute(
    "aria-label",
    `View transaction ${txid} on mempool.space (opens in a new tab)`,
  );
  const code = document.createElement("code");
  code.textContent = txid;
  code.title = txid;
  link.append(code);
  return link;
};

export const createTransactionDetailValue = (
  label: string,
  value: string,
): HTMLElement => {
  const wrapper = document.createElement("div");
  const term = document.createElement("span");
  term.textContent = label;
  if (label === "txid") {
    wrapper.append(term, createMempoolSpaceTransactionLink(value));
  } else {
    const code = document.createElement("code");
    code.textContent = value;
    code.title = value;
    wrapper.append(term, code);
  }
  return wrapper;
};
