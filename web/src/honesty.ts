import { classificationPresentation } from "./classification-progress";
import type { ClassificationProgress, SourceAvailability } from "./types";

export type HonestyTone = "stale" | "paused";

export interface HonestyBannerModel {
  tone: HonestyTone;
  message: string;
}

type FormatCount = (value: number) => string;

/**
 * Evidence-gap banner for a retained or partially assessed observation.
 * Returns null when the current view needs no caveat beyond the status strip.
 */
export const honestyBanner = (
  availability: SourceAvailability,
  lastError: string | null,
  progress: ClassificationProgress | null,
  transactionCount: number,
  freshnessLabel: string | null,
  formatCount?: FormatCount,
): HonestyBannerModel | null => {
  if (availability === "stale") {
    const shown =
      freshnessLabel === null
        ? "Showing the retained last-good observation."
        : `Showing the retained observation from ${freshnessLabel}.`;
    const cause =
      lastError === null ? "" : ` Latest poll failed: ${lastError}.`;
    return {
      tone: "stale",
      message: `${shown}${cause} Membership may have changed since.`,
    };
  }
  if (
    availability === "ready" &&
    progress !== null &&
    progress.state === "paused"
  ) {
    return {
      tone: "paused",
      message: classificationPresentation(
        progress,
        transactionCount,
        formatCount,
      ).summary,
    };
  }
  return null;
};
