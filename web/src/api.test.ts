import { afterEach, describe, expect, it, vi } from "vitest";

import {
  fetchSourceSnapshot,
  parseSourceSnapshotResponse,
  parseSourcesResponse,
} from "./api";

const TXID = "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f";
const BLOCK_HASH = "00".repeat(32);

const source = () => ({
  source_id: "core",
  source_label: "Bitcoin Core",
  availability: "ready",
  poll_interval_seconds: 300,
  last_poll_started_at_ms: 1_700_000_000_000,
  snapshot_observed_at_ms: 1_700_000_001_000,
  chain_tip: { height: 900_000, hash: BLOCK_HASH },
  transaction_count: 1,
  total_vsize: 141,
  last_error: null,
});

const snapshot = () => ({
  source_id: "core",
  source_label: "Bitcoin Core",
  observed_at_ms: 1_700_000_001_000,
  chain_tip: { height: 900_000, hash: BLOCK_HASH },
  transaction_count: 1,
  total_vsize: 141,
  transactions: [
    {
      txid: TXID,
      vsize: 141,
      fee_sats: 423,
      entered_at_ms: 1_699_999_000_000,
    },
  ],
});

afterEach(() => {
  vi.unstubAllGlobals();
});

describe("parseSourceSnapshotResponse", () => {
  it("validates and retains the API object without rebuilding it", () => {
    const value = { source: source(), snapshot: snapshot() };
    const response = parseSourceSnapshotResponse(value);

    expect(response).toBe(value);
    expect(response.source.availability).toBe("ready");
    expect(response.snapshot?.transactions[0]?.fee_sats).toBe(423);
  });

  it("accepts waiting state before the first snapshot", () => {
    const waiting = {
      ...source(),
      availability: "waiting",
      last_poll_started_at_ms: null,
      snapshot_observed_at_ms: null,
      chain_tip: null,
      transaction_count: null,
      total_vsize: null,
    };

    expect(
      parseSourceSnapshotResponse({ source: waiting, snapshot: null }),
    ).toMatchObject({
      source: { availability: "waiting" },
      snapshot: null,
    });
  });

  it("accepts stale state only with a last error and retained snapshot", () => {
    const stale = {
      ...source(),
      availability: "stale",
      last_error: "node unavailable",
    };

    expect(
      parseSourceSnapshotResponse({ source: stale, snapshot: snapshot() }),
    ).toMatchObject({
      source: { availability: "stale", last_error: "node unavailable" },
    });
  });

  it("rejects inconsistent totals and source metadata", () => {
    expect(() =>
      parseSourceSnapshotResponse({
        source: source(),
        snapshot: { ...snapshot(), total_vsize: 142 },
      }),
    ).toThrow("Snapshot total vsize does not match payload");

    expect(() =>
      parseSourceSnapshotResponse({
        source: source(),
        snapshot: { ...snapshot(), source_id: "knots" },
      }),
    ).toThrow("Source summary does not match its snapshot");
  });

  it("rejects malformed or unordered transactions", () => {
    expect(() =>
      parseSourceSnapshotResponse({
        source: source(),
        snapshot: {
          ...snapshot(),
          transaction_count: 2,
          total_vsize: 282,
          transactions: [
            snapshot().transactions[0],
            snapshot().transactions[0],
          ],
        },
      }),
    ).toThrow("Transactions are not strictly ordered by txid");

    expect(() =>
      parseSourceSnapshotResponse({
        source: source(),
        snapshot: {
          ...snapshot(),
          transactions: [{ ...snapshot().transactions[0], vsize: 0 }],
        },
      }),
    ).toThrow("Invalid transaction at index 0");
  });
});

describe("parseSourcesResponse", () => {
  it("parses source discovery", () => {
    expect(
      parseSourcesResponse({ sources: [source()] }).sources[0]?.source_id,
    ).toBe("core");
  });

  it("rejects source IDs that normalize as URL dot segments", () => {
    for (const sourceId of [".", ".."]) {
      expect(() =>
        parseSourcesResponse({
          sources: [{ ...source(), source_id: sourceId }],
        }),
      ).toThrow("Invalid source summary");
    }
  });
});

describe("fetchSourceSnapshot", () => {
  it("requests the source-scoped current snapshot", async () => {
    const fetchMock = vi.fn().mockResolvedValue({
      ok: true,
      json: async () => ({ source: source(), snapshot: snapshot() }),
    });
    vi.stubGlobal("fetch", fetchMock);

    await fetchSourceSnapshot("core");

    expect(fetchMock).toHaveBeenCalledWith("/api/v1/sources/core/mempool", {
      headers: { Accept: "application/json" },
    });
  });

  it("surfaces an API error body", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn().mockResolvedValue({
        ok: false,
        status: 404,
        json: async () => ({ error: 'unknown source "missing"' }),
      }),
    );

    await expect(fetchSourceSnapshot("missing")).rejects.toThrow(
      'Atlas request failed (404): unknown source "missing"',
    );
  });
});
