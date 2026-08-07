import { describe, expect, it, vi } from "vitest";

import { combineAbortSignals, forEachCooperatively } from "./cooperative-work";

describe("combineAbortSignals", () => {
  it("propagates an already-aborted signal and its reason", () => {
    const first = new AbortController();
    const second = new AbortController();
    const reason = new Error("cancelled before composition");
    second.abort(reason);

    const combined = combineAbortSignals([first.signal, second.signal]);

    expect(combined.signal.aborted).toBe(true);
    expect(combined.signal.reason).toBe(reason);
    combined.dispose();
  });

  it("propagates a later abort and ignores signals after disposal", () => {
    const first = new AbortController();
    const second = new AbortController();
    const combined = combineAbortSignals([first.signal, second.signal]);
    const reason = new Error("cancelled after composition");

    second.abort(reason);

    expect(combined.signal.aborted).toBe(true);
    expect(combined.signal.reason).toBe(reason);

    const disposed = combineAbortSignals([
      first.signal,
      new AbortController().signal,
    ]);
    disposed.dispose();
    first.abort();
    expect(disposed.signal.aborted).toBe(false);
  });

  it("rejects an empty signal set", () => {
    expect(() => combineAbortSignals([])).toThrow("At least one abort signal");
  });
});

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

  it("uses a view's retained iterator instead of repeated numeric lookup", async () => {
    const values = new Proxy([1, 2, 3], {
      get(target, property, receiver) {
        if (property === Symbol.iterator) {
          return function* (): IterableIterator<number> {
            yield* target;
          };
        }
        if (typeof property === "string" && /^[0-9]+$/.test(property)) {
          throw new Error("numeric lookup should not be used");
        }
        return Reflect.get(target, property, receiver);
      },
    });
    const visited: Array<readonly [number, number]> = [];

    await forEachCooperatively(
      values,
      (value, index) => visited.push([value, index]),
      { batchSize: 2 },
    );

    expect(visited).toEqual([
      [1, 0],
      [2, 1],
      [3, 2],
    ]);
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
