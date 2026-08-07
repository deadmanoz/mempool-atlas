import { describe, expect, it } from "vitest";

import { AtlasRequestError } from "./api";
import { presentation } from "./first-publication-failure";
import type { SourceSummary } from "./types";

const source = (
  availability: SourceSummary["availability"],
): SourceSummary => ({
  source_id: "core",
  source_label: "Bitcoin Core",
  availability,
  poll_interval_seconds: 300,
  last_poll_started_at_ms: null,
  snapshot_observed_at_ms: null,
  chain_tip: null,
  transaction_count: null,
  total_vsize: null,
  classification: null,
  last_error: availability === "error" ? "Node RPC connection failed" : null,
});

const unavailable = () =>
  new AtlasRequestError(503, "Current v2 publication unavailable", {
    type: "v2_unavailable",
    title: "Current v2 publication unavailable",
    status: 503,
    detail:
      "Atlas has not published a current complete snapshot for this source.",
  });

describe("first publication failure presentation", () => {
  it("treats only v2 unavailable plus fresh waiting metadata as expected", () => {
    expect(presentation(unavailable(), source("waiting"))).toMatchObject({
      state: "waiting",
      title: "Waiting for first snapshot",
    });
  });

  it.each(["ready", "stale"] as const)(
    "treats v2 unavailable plus %s metadata as an outage",
    (availability) => {
      expect(presentation(unavailable(), source(availability))).toMatchObject({
        state: "error",
        title: "Atlas website unavailable",
      });
    },
  );

  it("treats a generic 503 as an outage even while discovery says waiting", () => {
    expect(
      presentation(
        new AtlasRequestError(503, "Upstream failed"),
        source("waiting"),
      ),
    ).toMatchObject({ state: "error", detail: expect.stringContaining("503") });
  });

  it("surfaces the fresh source error after a failed first poll", () => {
    expect(presentation(unavailable(), source("error"))).toMatchObject({
      state: "error",
      detail: "Node RPC connection failed",
    });
  });
});
