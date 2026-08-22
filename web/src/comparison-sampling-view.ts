import { countFormat, formatDuration } from "./format";
import type { CurrentComparison } from "./comparison-model";

interface ComparisonSamplingElements {
  panel: HTMLElement;
  summary: HTMLElement;
  chainSummary: HTMLElement;
  note: HTMLElement;
}

export interface ComparisonSamplingView {
  render(current: CurrentComparison): void;
  reset(): void;
}

export const createComparisonSamplingView = ({
  panel,
  summary,
  chainSummary,
  note,
}: ComparisonSamplingElements): ComparisonSamplingView => {
  return {
    render(current): void {
      panel.hidden = false;
      const { left, right } = current;
      if (current.earlier_side === null) {
        summary.textContent = "Both snapshots were observed at the same time.";
      } else {
        const earlierName = current.earlier_side === "left" ? "A" : "B";
        const laterName = current.earlier_side === "left" ? "B" : "A";
        summary.textContent = `Source ${laterName} was observed ${formatDuration(current.observed_skew_ms)} after Source ${earlierName}.`;
      }
      const sameTip =
        left.snapshot.chain_tip.hash === right.snapshot.chain_tip.hash;
      const leftTip = left.snapshot.chain_tip;
      const rightTip = right.snapshot.chain_tip;
      const sameHeight = leftTip.height === rightTip.height;
      const compactHash = (hash: string): string => `${hash.slice(0, 10)}…`;
      panel.dataset.chainState = sameTip ? "same" : "different";
      chainSummary.dataset.state = sameTip ? "same" : "different";
      chainSummary.title = sameTip
        ? `Source A and Source B: height ${countFormat.format(leftTip.height)}, block ${leftTip.hash}`
        : `Source A: height ${countFormat.format(leftTip.height)}, block ${leftTip.hash}\nSource B: height ${countFormat.format(rightTip.height)}, block ${rightTip.hash}`;
      if (sameTip) {
        chainSummary.textContent = `Same chain tip · ${countFormat.format(leftTip.height)} · ${compactHash(leftTip.hash)}`;
      } else if (sameHeight) {
        chainSummary.textContent = `Different chain tips at height ${countFormat.format(leftTip.height)} · A ${compactHash(leftTip.hash)} / B ${compactHash(rightTip.hash)}`;
      } else {
        chainSummary.textContent = `Different chain tips · A ${countFormat.format(leftTip.height)} · ${compactHash(leftTip.hash)} / B ${countFormat.format(rightTip.height)} · ${compactHash(rightTip.hash)}`;
      }

      const overlap =
        Math.min(
          left.snapshot.collection_completed_at_ms,
          right.snapshot.collection_completed_at_ms,
        ) -
        Math.max(
          left.snapshot.collection_started_at_ms,
          right.snapshot.collection_started_at_ms,
        );
      const chainContext = sameTip
        ? ""
        : sameHeight
          ? "The sources reported different blocks at the same height. Transactions observed in both snapshots were present across both reported chain tips. "
          : "The sources reported different blocks at different heights. This can reflect node lag or chain divergence; Atlas does not infer which. ";
      const timingContext =
        overlap >= 0
          ? `The collection windows overlapped by ${formatDuration(overlap)}. Membership still comes from independent node observations.`
          : `The collection windows were separated by ${formatDuration(Math.abs(overlap))}. Membership changes during that interval can contribute to regions observed in only one snapshot.`;
      note.textContent = `${chainContext}${timingContext}`;
    },
    reset(): void {
      panel.hidden = true;
      delete panel.dataset.chainState;
    },
  };
};
