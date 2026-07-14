import { describe, expect, it } from "vitest";

import { parseMempoolResponse } from "./api";

describe("parseMempoolResponse", () => {
  it("parses the read API response", () => {
    const response = parseMempoolResponse({
      source_id: null,
      memberships: [
        {
          source_id: "core",
          txid: "abc",
          present: true,
          updated_at_ms: 1_700_000_000_000,
          evidence_event_id: "event-1",
        },
      ],
    });

    expect(response.memberships[0]?.source_id).toBe("core");
  });

  it("rejects a malformed membership", () => {
    expect(() =>
      parseMempoolResponse({
        source_id: "core",
        memberships: [{ source_id: "core", txid: "abc" }],
      }),
    ).toThrow("Invalid membership at index 0");
  });
});
