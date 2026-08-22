import { describe, expect, it, vi } from "vitest";

import { renderPrimaryComparisonPublication } from "./comparison-primary-publication";
import { PublicationCommitRetryExhaustedError } from "./publication-commit-budget";

describe("primary comparison publication", () => {
  it("recovers only comparison commit retry exhaustion", async () => {
    const render = vi.fn(async () => {
      throw new PublicationCommitRetryExhaustedError("Comparison");
    });

    await expect(
      renderPrimaryComparisonPublication(render),
    ).resolves.toBeUndefined();
    expect(render).toHaveBeenCalledOnce();
  });

  it.each([
    new PublicationCommitRetryExhaustedError("Node"),
    new Error("render failed"),
  ])("keeps unrelated render failures fail-closed", async (error) => {
    await expect(
      renderPrimaryComparisonPublication(async () => {
        throw error;
      }),
    ).rejects.toBe(error);
  });
});
