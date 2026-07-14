import type { Membership } from "./types";

export interface MembershipKey {
  source_id: string;
  txid: string;
}

export interface Checkpoint {
  sequence: number;
  memberships: readonly Membership[];
}

export interface Delta {
  sequence: number;
  upserts: readonly Membership[];
  removals: readonly MembershipKey[];
}

export type MempoolSyncState = "empty" | "ready" | "reset_required";

export interface MempoolState {
  sequence: number | null;
  sync: MempoolSyncState;
  memberships: ReadonlyMap<string, Membership>;
}

export type MempoolUpdate =
  | { type: "checkpoint"; checkpoint: Checkpoint }
  | { type: "delta"; delta: Delta };

export const membershipKey = ({ source_id, txid }: MembershipKey): string =>
  `${source_id}\u0000${txid}`;

export const createMempoolState = (): MempoolState => ({
  sequence: null,
  sync: "empty",
  memberships: new Map(),
});

const assertSequence = (sequence: number): void => {
  if (!Number.isSafeInteger(sequence) || sequence < 0) {
    throw new RangeError(
      "Mempool sequence must be a non-negative safe integer",
    );
  }
};

const applyCheckpoint = (
  state: MempoolState,
  checkpoint: Checkpoint,
): MempoolState => {
  assertSequence(checkpoint.sequence);

  if (state.sequence !== null && checkpoint.sequence < state.sequence) {
    return state;
  }

  return {
    sequence: checkpoint.sequence,
    sync: "ready",
    memberships: new Map(
      checkpoint.memberships.map((membership) => [
        membershipKey(membership),
        membership,
      ]),
    ),
  };
};

const applyDelta = (state: MempoolState, delta: Delta): MempoolState => {
  assertSequence(delta.sequence);

  if (state.sync === "reset_required") {
    return state;
  }

  if (state.sequence === null) {
    return { ...state, sync: "reset_required" };
  }

  if (delta.sequence <= state.sequence) {
    return state;
  }

  if (delta.sequence !== state.sequence + 1) {
    return { ...state, sync: "reset_required" };
  }

  const memberships = new Map(state.memberships);
  for (const removal of delta.removals) {
    memberships.delete(membershipKey(removal));
  }
  for (const membership of delta.upserts) {
    memberships.set(membershipKey(membership), membership);
  }

  return {
    sequence: delta.sequence,
    sync: "ready",
    memberships,
  };
};

export const reduceMempoolState = (
  state: MempoolState,
  update: MempoolUpdate,
): MempoolState => {
  switch (update.type) {
    case "checkpoint":
      return applyCheckpoint(state, update.checkpoint);
    case "delta":
      return applyDelta(state, update.delta);
  }
};
