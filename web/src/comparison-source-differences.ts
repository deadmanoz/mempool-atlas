import {
  comparisonWitnessVariantDescription,
  type ComparedTransaction,
  type CurrentComparison,
} from "./comparison-model";
import { countFormat, decimalFormat, formatSats, formatVsize } from "./format";
import { ancestorFeeRate, baseFeeRate } from "./transaction-facts";

export interface SourceDifferenceSummaryElements {
  panel: HTMLElement;
  any: HTMLElement;
  witness: HTMLElement;
  ancestor: HTMLElement;
  replaceability: HTMLElement;
}

const differenceListFormat = new Intl.ListFormat("en", {
  style: "long",
  type: "conjunction",
});

const differenceKinds = (entry: ComparedTransaction): string[] => {
  const left = entry.left;
  const right = entry.right;
  if (left === null || right === null || entry.witness_relation === "loading") {
    return [];
  }
  const differences: string[] = [];
  if (entry.witness_relation === "different") {
    differences.push("witness variant");
  }
  if (
    left.ancestor_vsize !== right.ancestor_vsize ||
    left.ancestor_fee_sats !== right.ancestor_fee_sats
  ) {
    differences.push("ancestor package");
  }
  if (left.replaceable !== right.replaceable) {
    differences.push("replaceability");
  }
  return differences;
};

export const sourceDifferenceNavigatorDescription = (
  entry: ComparedTransaction,
): string => {
  if (
    entry.left === null ||
    entry.right === null ||
    entry.witness_relation === "loading"
  ) {
    return comparisonWitnessVariantDescription(entry.witness_relation);
  }
  const differences = differenceKinds(entry);
  return differences.length === 0
    ? "Same source-local facts"
    : `Different ${differenceListFormat.format(differences)}`;
};

export const resetSourceDifferenceSummary = (
  elements: SourceDifferenceSummaryElements,
): void => {
  elements.panel.hidden = true;
};

export const renderSourceDifferenceSummary = (
  elements: SourceDifferenceSummaryElements,
  current: CurrentComparison,
  complete: boolean,
): void => {
  elements.panel.hidden = !complete;
  if (!complete) return;
  elements.any.textContent = countFormat.format(
    current.totals.common_source_difference_count,
  );
  elements.witness.textContent = countFormat.format(
    current.totals.common_witness_variant_count,
  );
  elements.ancestor.textContent = countFormat.format(
    current.totals.common_ancestor_package_difference_count,
  );
  elements.replaceability.textContent = countFormat.format(
    current.totals.common_replaceability_difference_count,
  );
};

export const createSourceDifferenceDetail = (
  current: CurrentComparison,
  entry: ComparedTransaction,
): HTMLElement | null => {
  const left = entry.left;
  const right = entry.right;
  if (left === null || right === null) return null;

  const differences: string[] = [];
  if (entry.witness_relation === "different") {
    differences.push(
      `Witness variant: ${current.left.snapshot.source_label} ${formatVsize(left.vsize)} at ${decimalFormat.format(baseFeeRate(left))} sat/vB; ${current.right.snapshot.source_label} ${formatVsize(right.vsize)} at ${decimalFormat.format(baseFeeRate(right))} sat/vB.`,
    );
  }
  if (
    left.ancestor_vsize !== right.ancestor_vsize ||
    left.ancestor_fee_sats !== right.ancestor_fee_sats
  ) {
    differences.push(
      `Ancestor package: ${current.left.snapshot.source_label} ${formatVsize(left.ancestor_vsize)} and ${formatSats(left.ancestor_fee_sats)} at ${decimalFormat.format(ancestorFeeRate(left))} sat/vB; ${current.right.snapshot.source_label} ${formatVsize(right.ancestor_vsize)} and ${formatSats(right.ancestor_fee_sats)} at ${decimalFormat.format(ancestorFeeRate(right))} sat/vB.`,
    );
  }
  if (left.replaceable !== right.replaceable) {
    differences.push(
      `Effective replaceability: ${current.left.snapshot.source_label} ${left.replaceable ? "reported replaceable" : "not reported replaceable"}; ${current.right.snapshot.source_label} ${right.replaceable ? "reported replaceable" : "not reported replaceable"}.`,
    );
  }
  if (differences.length === 0) return null;

  const panel = document.createElement("section");
  panel.className = "comparison-detail-differences";
  const heading = document.createElement("h3");
  heading.textContent = "Different source-local facts";
  const list = document.createElement("ul");
  list.append(
    ...differences.map((difference) => {
      const item = document.createElement("li");
      item.textContent = difference;
      return item;
    }),
  );
  const note = document.createElement("p");
  note.textContent =
    "These are observations from two snapshots. They do not establish why either node reported different facts.";
  panel.append(heading, list, note);
  return panel;
};
