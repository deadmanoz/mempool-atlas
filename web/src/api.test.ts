import { afterEach, describe, expect, it, vi } from "vitest";

import { fetchMempool, parseMempoolResponse } from "./api";

const snapshot = (sourceId = "core") => ({
  source_id: sourceId,
  health: {
    last_seen_at_ms: 1_700_000_000_000,
    capture: { status: "no_reported_gaps" },
  },
  memberships: [
    {
      txid: "abc",
      updated_at_ms: 1_700_000_000_000,
      evidence_event_id: "event-1",
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
    expect(response.health.capture.status).toBe("no_reported_gaps");
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
          updated_at_ms: 1_700_000_000_000,
          evidence_event_id: "event-2",
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
        last_seen_at_ms: 1_700_000_000_000,
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
            updated_at_ms: 1_700_000_000_000,
            evidence_event_id: "event-1",
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
          last_seen_at_ms: 1_700_000_000_000,
          capture: {
            status: "contains_gaps",
            marker_count: 0,
          },
        },
      }),
    ).toThrow("Invalid capture status");
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
