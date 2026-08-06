import { countFormat, decimalFormat, formatSats, formatVsize } from "./format";
import type { MempoolTransaction } from "./types";

/** Effective ancestor fee rate: delta-adjusted package fees over package vsize. */
export const packageFeeRate = (transaction: MempoolTransaction): number =>
  transaction.ancestor_vsize > 0
    ? transaction.ancestor_fee_sats / transaction.ancestor_vsize
    : 0;

/**
 * Label and value pairs for one transaction's membership and structure facts,
 * shared by the node inspector and the comparison detail panels.
 */
export const transactionFactPairs = (
  transaction: MempoolTransaction,
): [string, string][] => {
  const pairs: [string, string][] = [
    ["Weight", `${countFormat.format(transaction.weight)} wu`],
    [
      "Package",
      `${countFormat.format(transaction.ancestor_count)} tx incl. self · ${formatVsize(transaction.ancestor_vsize)} · ${decimalFormat.format(packageFeeRate(transaction))} sat/vB`,
    ],
    [
      "Descendants",
      `${countFormat.format(transaction.descendant_count)} tx incl. self · ${formatVsize(transaction.descendant_vsize)}`,
    ],
    [
      "Replaceable",
      transaction.replaceable
        ? "Yes, as reported by the source"
        : "No, as reported by the source",
    ],
  ];
  const structure = transaction.structure;
  if (structure === null) {
    pairs.push(["Structure", "Facts arrive with classification"]);
    return pairs;
  }
  pairs.push(
    [
      "Shape",
      `${countFormat.format(structure.input_count)} input${structure.input_count === 1 ? "" : "s"} → ${countFormat.format(structure.output_count)} output${structure.output_count === 1 ? "" : "s"}`,
    ],
    ["Output value", formatSats(structure.output_sats)],
    ["Witness", `${countFormat.format(structure.witness_bytes)} bytes`],
  );
  if (structure.op_return_bytes > 0) {
    pairs.push([
      "OP_RETURN payload",
      `${countFormat.format(structure.op_return_bytes)} bytes`,
    ]);
  }
  return pairs;
};

/** Compact one-line variant of the same facts for dense detail panels. */
export const transactionFactSummary = (
  transaction: MempoolTransaction,
): string => {
  const parts = [
    `${countFormat.format(transaction.weight)} wu`,
    `package ${countFormat.format(transaction.ancestor_count)} tx · ${formatVsize(transaction.ancestor_vsize)} @ ${decimalFormat.format(packageFeeRate(transaction))} sat/vB`,
    `descendants ${countFormat.format(transaction.descendant_count)}`,
    transaction.replaceable ? "replaceable" : "not replaceable",
  ];
  const structure = transaction.structure;
  if (structure === null) {
    parts.push("structure facts pending");
  } else {
    parts.push(
      `${countFormat.format(structure.input_count)} in → ${countFormat.format(structure.output_count)} out`,
      `moves ${formatSats(structure.output_sats)}`,
    );
    if (structure.op_return_bytes > 0) {
      parts.push(
        `OP_RETURN ${countFormat.format(structure.op_return_bytes)} B`,
      );
    }
  }
  return parts.join(" · ");
};
