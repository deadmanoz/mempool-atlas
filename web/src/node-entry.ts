import "./styles.css";
import "./source-summary-styles.css";

const NODE_STATE_KEYS = new Set([
  "source",
  "classifier",
  "label",
  "match",
  "rule",
  "region",
  "txid",
]);
const hasExplicitNodeState = [
  ...new URLSearchParams(window.location.search).keys(),
].some((key) => NODE_STATE_KEYS.has(key));

if (hasExplicitNodeState) {
  void import("./main");
} else {
  const comparisonUrl = new URL("/compare/", window.location.origin);
  comparisonUrl.search = window.location.search;
  comparisonUrl.hash = window.location.hash;
  window.location.replace(
    `${comparisonUrl.pathname}${comparisonUrl.search}${comparisonUrl.hash}`,
  );
}
