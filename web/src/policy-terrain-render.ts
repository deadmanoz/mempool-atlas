import {
  renderTerrainCooperatively,
  type TerrainLayout,
  type TerrainMode,
  type TerrainSelection,
} from "./terrain";
import type { MempoolTransaction } from "./types";

interface PolicyTerrainRenderInput {
  canvas: HTMLCanvasElement;
  transactions: readonly MempoolTransaction[];
  mode: TerrainMode;
  selection: TerrainSelection;
  previousLayout: TerrainLayout | null;
  signal: AbortSignal;
  isCurrent: () => boolean;
  commit: (layout: TerrainLayout, ariaLabel: string) => void;
}

/** Run one cancellable policy-terrain render and publish only current work. */
export const startPolicyTerrainRender = (
  input: PolicyTerrainRenderInput,
): void => {
  void renderTerrainCooperatively(
    input.canvas,
    input.transactions,
    input.mode,
    input.selection,
    input.previousLayout,
    null,
    { batchSize: 500, signal: input.signal },
  )
    .then((layout) => {
      if (!input.signal.aborted && input.isCurrent()) {
        input.commit(
          layout,
          `BIP-110 rule-combination buckets for ${input.transactions.length.toLocaleString("en")} transactions. Complete violations appear once in their exact rule-set bucket; incomplete violations are separate. Bucket area represents ${input.mode === "count" ? "transaction count" : "virtual size"}.`,
        );
      }
    })
    .catch((error: unknown) => {
      if (!input.signal.aborted) throw error;
    });
};
