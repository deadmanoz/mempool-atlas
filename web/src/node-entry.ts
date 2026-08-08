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
  window.location.replace("/compare/");
}
