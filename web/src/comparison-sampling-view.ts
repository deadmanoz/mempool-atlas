import { countFormat, formatDuration, formatTime } from "./format";
import type {
  ComparisonSide,
  CurrentComparison,
  LoadedSourceSnapshot,
} from "./comparison-model";

interface ComparisonSamplingElements {
  panel: HTMLElement;
  summary: HTMLElement;
  chainSummary: HTMLElement;
  timeline: HTMLElement;
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
  timeline,
  note,
}: ComparisonSamplingElements): ComparisonSamplingView => {
  const timelineRow = (
    label: string,
    side: ComparisonSide,
    source: LoadedSourceSnapshot,
    minimum: number,
    span: number,
  ): HTMLElement => {
    const row = document.createElement("div");
    row.className = "sampling-row";
    const name = document.createElement("span");
    name.textContent = label;
    const track = document.createElement("div");
    track.className = "sampling-track";
    const bar = document.createElement("i");
    bar.dataset.side = side;
    const start = source.snapshot.collection_started_at_ms;
    const duration = Math.max(1, source.snapshot.collection_duration_ms);
    const startPercent = ((start - minimum) / span) * 100;
    const widthScale = Math.max(0.012, duration / span);
    bar.style.transform = `translateX(${startPercent}%) scaleX(${widthScale})`;
    bar.title = `${formatTime(start)}–${formatTime(source.snapshot.collection_completed_at_ms)}`;
    track.append(bar);
    const time = document.createElement("time");
    time.dateTime = new Date(
      source.snapshot.collection_completed_at_ms,
    ).toISOString();
    time.textContent = formatTime(source.snapshot.collection_completed_at_ms);
    row.append(name, track, time);
    return row;
  };

  return {
    render(current): void {
      panel.hidden = false;
      const { left, right } = current;
      const earlierLabel =
        current.earlier_side === null
          ? "completed together"
          : `${current[current.earlier_side].snapshot.source_label} completed earlier`;
      summary.textContent = `${formatDuration(current.observed_skew_ms)} observation skew · ${earlierLabel}`;
      const sameTip =
        left.snapshot.chain_tip.hash === right.snapshot.chain_tip.hash;
      chainSummary.dataset.state = sameTip ? "same" : "different";
      chainSummary.textContent = sameTip
        ? `Same chain tip · ${countFormat.format(left.snapshot.chain_tip.height)}`
        : `Different chain tips · ${countFormat.format(left.snapshot.chain_tip.height)} / ${countFormat.format(right.snapshot.chain_tip.height)}`;

      const minimum = Math.min(
        left.snapshot.collection_started_at_ms,
        right.snapshot.collection_started_at_ms,
      );
      const maximum = Math.max(
        left.snapshot.collection_completed_at_ms,
        right.snapshot.collection_completed_at_ms,
      );
      const span = Math.max(1, maximum - minimum);
      timeline.replaceChildren(
        timelineRow("A", "left", left, minimum, span),
        timelineRow("B", "right", right, minimum, span),
      );

      const overlap =
        Math.min(
          left.snapshot.collection_completed_at_ms,
          right.snapshot.collection_completed_at_ms,
        ) -
        Math.max(
          left.snapshot.collection_started_at_ms,
          right.snapshot.collection_started_at_ms,
        );
      if (overlap >= 0) {
        note.textContent = `The collection windows overlapped by ${formatDuration(overlap)}. Membership still comes from independent node observations.`;
      } else {
        note.textContent = `The collection windows were separated by ${formatDuration(Math.abs(overlap))}. Membership changes during that interval can contribute to regions observed in only one snapshot.`;
      }
    },
    reset(): void {
      panel.hidden = true;
      timeline.replaceChildren();
    },
  };
};
