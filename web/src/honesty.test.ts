import { describe, expect, it } from "vitest";

import { honestyBanner } from "./honesty";
import type { ClassificationProgress } from "./types";

const progress = (
  state: ClassificationProgress["state"],
  classified: number,
  unclassified: number,
): ClassificationProgress => ({
  state,
  revision: 4,
  classified_count: classified,
  unclassified_count: unclassified,
});

describe("honestyBanner", () => {
  it("returns null for a fresh, fully assessed observation", () => {
    expect(
      honestyBanner("ready", null, progress("complete", 10, 0), 10, "just now"),
    ).toBeNull();
  });

  it("explains a retained observation with its poll failure", () => {
    const banner = honestyBanner(
      "stale",
      "connection refused",
      progress("complete", 10, 0),
      10,
      "3 minutes ago",
    );
    expect(banner?.tone).toBe("stale");
    expect(banner?.message).toBe(
      "Showing the retained observation from 3 minutes ago. " +
        "Latest poll failed: connection refused. " +
        "Membership may have changed since.",
    );
  });

  it("omits the failure clause when no error is reported", () => {
    const banner = honestyBanner("stale", null, null, 0, null);
    expect(banner?.message).toBe(
      "Showing the retained last-good observation. " +
        "Membership may have changed since.",
    );
  });

  it("reuses the paused classification summary copy", () => {
    const banner = honestyBanner(
      "ready",
      null,
      progress("paused", 8, 2),
      10,
      "just now",
    );
    expect(banner?.tone).toBe("paused");
    expect(banner?.message).toBe(
      "8 of 10 assessed. Classification paused after an operational failure " +
        "with assessments unavailable for 2 transactions. " +
        "It retries with the next snapshot.",
    );
  });

  it("returns null while classification is still in progress", () => {
    expect(
      honestyBanner("ready", null, progress("classifying", 5, 5), 10, null),
    ).toBeNull();
  });

  it("returns null for waiting and error availabilities", () => {
    expect(honestyBanner("waiting", null, null, 0, null)).toBeNull();
    expect(honestyBanner("error", "boom", null, 0, null)).toBeNull();
  });
});
