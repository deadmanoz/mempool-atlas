import { describe, expect, it } from "vitest";

import { atlasFailureBody, parseAtlasProblem } from "./atlas-problem";

const unavailable = {
  type: "v2_unavailable",
  title: "Current v2 publication unavailable",
  status: 503,
  detail:
    "Atlas has not published a current complete snapshot for this source.",
};

describe("Atlas problem responses", () => {
  it("parses an exact problem whose status matches the HTTP response", () => {
    expect(parseAtlasProblem(unavailable, 503)).toEqual(unavailable);
    expect(atlasFailureBody(unavailable, 503)).toEqual({
      detail: unavailable.title,
      problem: unavailable,
    });
  });

  it("rejects problem metadata whose status disagrees with HTTP", () => {
    expect(parseAtlasProblem(unavailable, 502)).toBeNull();
  });

  it("preserves the generic JSON error contract", () => {
    expect(atlasFailureBody({ error: "unknown source" }, 404)).toEqual({
      detail: "unknown source",
      problem: null,
    });
  });
});
