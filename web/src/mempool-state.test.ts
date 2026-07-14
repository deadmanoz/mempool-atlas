import { describe, expect, it } from "vitest";

import {
  createMempoolState,
  membershipKey,
  reduceMempoolState,
} from "./mempool-state";
import type { Membership } from "./types";

const membership = (
  source_id: string,
  txid: string,
  present = true,
): Membership => ({
  source_id,
  txid,
  present,
  updated_at_ms: 1_700_000_000_000,
  evidence_event_id: `${source_id}-${txid}`,
});

describe("reduceMempoolState", () => {
  it("replaces state atomically from a checkpoint", () => {
    const oldMembership = membership("core", "old");
    const initial = reduceMempoolState(createMempoolState(), {
      type: "checkpoint",
      checkpoint: { sequence: 2, memberships: [oldMembership] },
    });
    const nextMembership = membership("knots", "next");

    const state = reduceMempoolState(initial, {
      type: "checkpoint",
      checkpoint: { sequence: 4, memberships: [nextMembership] },
    });

    expect(state.sequence).toBe(4);
    expect(state.sync).toBe("ready");
    expect([...state.memberships.values()]).toEqual([nextMembership]);
  });

  it("applies a contiguous delta", () => {
    const removed = membership("core", "removed");
    const retained = membership("core", "retained");
    const initial = reduceMempoolState(createMempoolState(), {
      type: "checkpoint",
      checkpoint: { sequence: 7, memberships: [removed, retained] },
    });
    const added = membership("knots", "added");

    const state = reduceMempoolState(initial, {
      type: "delta",
      delta: {
        sequence: 8,
        upserts: [added],
        removals: [{ source_id: removed.source_id, txid: removed.txid }],
      },
    });

    expect(state.sequence).toBe(8);
    expect(state.sync).toBe("ready");
    expect(state.memberships.has(membershipKey(removed))).toBe(false);
    expect(state.memberships.get(membershipKey(retained))).toBe(retained);
    expect(state.memberships.get(membershipKey(added))).toBe(added);
  });

  it("ignores duplicate and stale deltas", () => {
    const initial = reduceMempoolState(createMempoolState(), {
      type: "checkpoint",
      checkpoint: { sequence: 5, memberships: [] },
    });

    const duplicate = reduceMempoolState(initial, {
      type: "delta",
      delta: { sequence: 5, upserts: [], removals: [] },
    });
    const stale = reduceMempoolState(initial, {
      type: "delta",
      delta: { sequence: 4, upserts: [], removals: [] },
    });

    expect(duplicate).toBe(initial);
    expect(stale).toBe(initial);
  });

  it("requires a reset after a sequence gap and preserves last-good data", () => {
    const current = membership("core", "current");
    const initial = reduceMempoolState(createMempoolState(), {
      type: "checkpoint",
      checkpoint: { sequence: 10, memberships: [current] },
    });

    const state = reduceMempoolState(initial, {
      type: "delta",
      delta: {
        sequence: 12,
        upserts: [membership("knots", "missed-gap")],
        removals: [],
      },
    });

    expect(state.sequence).toBe(10);
    expect(state.sync).toBe("reset_required");
    expect([...state.memberships.values()]).toEqual([current]);
  });

  it("does not resume deltas until a fresh checkpoint arrives", () => {
    const initial = reduceMempoolState(createMempoolState(), {
      type: "checkpoint",
      checkpoint: { sequence: 3, memberships: [] },
    });
    const resetRequired = reduceMempoolState(initial, {
      type: "delta",
      delta: { sequence: 5, upserts: [], removals: [] },
    });
    const ignored = reduceMempoolState(resetRequired, {
      type: "delta",
      delta: {
        sequence: 4,
        upserts: [membership("core", "late")],
        removals: [],
      },
    });
    const recovered = reduceMempoolState(ignored, {
      type: "checkpoint",
      checkpoint: {
        sequence: 5,
        memberships: [membership("core", "recovered")],
      },
    });

    expect(ignored).toBe(resetRequired);
    expect(recovered.sync).toBe("ready");
    expect(recovered.sequence).toBe(5);
    expect([...recovered.memberships.values()].map(({ txid }) => txid)).toEqual(
      ["recovered"],
    );
  });

  it("requires a checkpoint before applying any delta", () => {
    const state = reduceMempoolState(createMempoolState(), {
      type: "delta",
      delta: { sequence: 1, upserts: [], removals: [] },
    });

    expect(state.sync).toBe("reset_required");
    expect(state.sequence).toBeNull();
  });

  it("does not roll back to a stale checkpoint", () => {
    const initial = reduceMempoolState(createMempoolState(), {
      type: "checkpoint",
      checkpoint: { sequence: 6, memberships: [] },
    });

    const state = reduceMempoolState(initial, {
      type: "checkpoint",
      checkpoint: {
        sequence: 5,
        memberships: [membership("core", "stale")],
      },
    });

    expect(state).toBe(initial);
  });
});
