import { describe, expect, it, vi } from "vitest";

import { renderPrimaryNodePublication } from "./node-primary-publication";
import { PublicationCommitRetryExhaustedError } from "./publication-commit-budget";

describe("primary node publication", () => {
  it("recovers only node commit retry exhaustion", async () => {
    const render = vi.fn(async () => {
      throw new PublicationCommitRetryExhaustedError("Node");
    });

    await expect(renderPrimaryNodePublication(render)).resolves.toBeUndefined();
    expect(render).toHaveBeenCalledOnce();
  });

  it.each([
    new PublicationCommitRetryExhaustedError("Comparison"),
    new Error("render failed"),
  ])("keeps unrelated render failures fail-closed", async (error) => {
    await expect(
      renderPrimaryNodePublication(async () => {
        throw error;
      }),
    ).rejects.toBe(error);
  });
});
