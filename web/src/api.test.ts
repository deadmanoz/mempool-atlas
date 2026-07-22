import { afterEach, describe, expect, it, vi } from "vitest";

import { fetchMempool, parseMempoolResponse } from "./api";

const snapshot = (sourceId = "core") => ({
  source_id: sourceId,
  health: {
    state_cursor: { epoch_id: "epoch-a", revision: 7 },
    state_observed_at_ms: 1_700_000_000_000,
    capture: { status: "not_collected" },
  },
  memberships: [
    {
      txid: "abc",
      facts: {
        status: "available",
        vsize: 141,
        fee_sats: 423,
        entered_at_ms: 1_699_999_000_000,
      },
    },
  ],
});

afterEach(() => {
  vi.unstubAllGlobals();
});

describe("parseMempoolResponse", () => {
  it("parses the read API response", () => {
    const response = parseMempoolResponse(snapshot());

    expect(response.source_id).toBe("core");
    expect(response.health.capture.status).toBe("not_collected");
    expect(response.health.state_cursor).toEqual({
      epoch_id: "epoch-a",
      revision: 7,
    });
    expect(response.memberships[0]?.txid).toBe("abc");
    expect(response.memberships[0]?.facts).toEqual({
      status: "available",
      vsize: 141,
      fee_sats: 423,
      entered_at_ms: 1_699_999_000_000,
    });
  });

  it("parses a membership awaiting RPC facts", () => {
    const response = parseMempoolResponse({
      ...snapshot(),
      memberships: [
        {
          txid: "def",
          facts: { status: "awaiting_rpc" },
        },
      ],
    });

    expect(response.memberships[0]?.facts).toEqual({
      status: "awaiting_rpc",
    });
  });

  it("parses a known capture gap", () => {
    const response = parseMempoolResponse({
      ...snapshot(),
      health: {
        state_cursor: { epoch_id: "epoch-a", revision: 7 },
        state_observed_at_ms: 1_700_000_000_000,
        capture: {
          status: "contains_gaps",
          first_gap_at_ms: 1_699_999_000_000,
          latest_gap_at_ms: 1_700_000_000_000,
          marker_count: 2,
          strongest_certainty: "known_loss",
          latest_input: "peer_observer_nats",
          latest_reason: "slow_consumer",
        },
      },
    });

    expect(response.health.capture).toMatchObject({
      status: "contains_gaps",
      marker_count: 2,
      strongest_certainty: "known_loss",
    });
  });

  it("rejects a malformed membership", () => {
    expect(() =>
      parseMempoolResponse({
        source_id: "core",
        health: snapshot().health,
        memberships: [{ txid: "abc" }],
      }),
    ).toThrow("Invalid membership at index 0");
  });

  it("rejects legacy per-membership event provenance", () => {
    expect(() =>
      parseMempoolResponse({
        ...snapshot(),
        memberships: [
          {
            ...snapshot().memberships[0],
            updated_at_ms: 1_700_000_000_000,
            evidence_event_id: "event-1",
          },
        ],
      }),
    ).toThrow("Invalid membership at index 0");
  });

  it.each([
    undefined,
    { status: "unknown" },
    { status: "awaiting_rpc", vsize: 141 },
    {
      status: "available",
      vsize: 0,
      fee_sats: 423,
      entered_at_ms: 1_699_999_000_000,
    },
    {
      status: "available",
      vsize: 141,
      fee_sats: -1,
      entered_at_ms: 1_699_999_000_000,
    },
    {
      status: "available",
      vsize: 141,
      fee_sats: 423,
    },
  ])("rejects malformed membership facts %#", (facts) => {
    expect(() =>
      parseMempoolResponse({
        ...snapshot(),
        memberships: [
          {
            txid: "abc",
            facts,
          },
        ],
      }),
    ).toThrow("Invalid membership facts at index 0");
  });

  it("rejects a malformed capture status", () => {
    expect(() =>
      parseMempoolResponse({
        ...snapshot(),
        health: {
          state_cursor: { epoch_id: "epoch-a", revision: 7 },
          state_observed_at_ms: 1_700_000_000_000,
          capture: {
            status: "contains_gaps",
            marker_count: 0,
          },
        },
      }),
    ).toThrow("Invalid capture status");
  });

  it("rejects an unestablished state cursor", () => {
    expect(() =>
      parseMempoolResponse({
        ...snapshot(),
        health: {
          ...snapshot().health,
          state_cursor: { epoch_id: "epoch-a", revision: 0 },
        },
      }),
    ).toThrow("Invalid source state cursor");
  });

  it("rejects a state cursor with an invalid epoch identifier", () => {
    expect(() =>
      parseMempoolResponse({
        ...snapshot(),
        health: {
          ...snapshot().health,
          state_cursor: { epoch_id: "epoch a", revision: 7 },
        },
      }),
    ).toThrow("Invalid source state cursor");
  });
});

describe("fetchMempool", () => {
  it("requests the canonical source path", async () => {
    const fetchMock = vi.fn().mockResolvedValue({
      ok: true,
      json: async () => snapshot("core-v31.1"),
    });
    vi.stubGlobal("fetch", fetchMock);

    await fetchMempool("core-v31.1");

    expect(fetchMock).toHaveBeenCalledWith(
      "/api/v1/sources/core-v31.1/mempool",
      { headers: { Accept: "application/json" } },
    );
  });

  it("rejects a response for another source", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn().mockResolvedValue({
        ok: true,
        json: async () => snapshot("knots"),
      }),
    );

    await expect(fetchMempool("core")).rejects.toThrow(
      "Mempool response source knots does not match core",
    );
  });
});
