import { describe, expect, it, vi } from "vitest";

import { forEachCooperatively } from "./cooperative-work";

describe("forEachCooperatively", () => {
  it("visits values in order and yields between bounded batches", async () => {
    const visited: number[] = [];
    const yieldBetweenBatches = vi.fn(async () => Promise.resolve());

    await forEachCooperatively(
      [0, 1, 2, 3, 4],
      (value) => visited.push(value),
      { batchSize: 2, yieldBetweenBatches },
    );

    expect(visited).toEqual([0, 1, 2, 3, 4]);
    expect(yieldBetweenBatches).toHaveBeenCalledTimes(2);
  });

  it("stops before the next batch when aborted during a yield", async () => {
    const controller = new AbortController();
    const visited: number[] = [];

    await expect(
      forEachCooperatively([0, 1, 2, 3], (value) => visited.push(value), {
        batchSize: 2,
        signal: controller.signal,
        yieldBetweenBatches: () => controller.abort(),
      }),
    ).rejects.toMatchObject({ name: "AbortError" });
    expect(visited).toEqual([0, 1]);
  });

  it("rejects invalid batch sizes before visiting values", async () => {
    const visit = vi.fn();
    await expect(
      forEachCooperatively([1], visit, { batchSize: 0 }),
    ).rejects.toThrow("positive integer");
    expect(visit).not.toHaveBeenCalled();
  });
});
