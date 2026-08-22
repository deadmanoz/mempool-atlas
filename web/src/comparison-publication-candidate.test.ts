import { describe, expect, it, vi } from "vitest";

import type { ComparisonCanvasView } from "./comparison-canvas-view";
import type {
  ComparisonDistributionsView,
  PreparedComparisonDistributions,
} from "./comparison-distributions-view";
import {
  commitPreparedComparisonDistributions,
  prepareComparisonCommitCandidate,
  prepareComparisonPublication,
} from "./comparison-publication-candidate";
import { loadedSource } from "./comparison-test-fixtures";
import { mempoolTransaction } from "./test-fixtures";

const publication = (sourceId: string, values: number[]) => {
  const loaded = loadedSource(
    sourceId,
    values.map((value) => mempoolTransaction(value)),
  );
  return {
    source: loaded.source,
    publication_id:
      `${sourceId.charCodeAt(0).toString(16).padStart(2, "0")}`.repeat(32),
    population_id: "10".repeat(32),
    structure_id: "11".repeat(32),
    snapshot_identity: `snapshot-${sourceId}`,
    publication: loaded.snapshot,
  };
};

const unusedDistributionsView = {} as ComparisonDistributionsView;

const canvasView = (canCommit: () => boolean) => {
  const prepareCandidate = vi.fn((comparison) => ({ comparison }));
  const canCommitCandidate = vi.fn(canCommit);
  return {
    view: {
      prepareCandidate,
      canCommitCandidate,
    } as unknown as ComparisonCanvasView,
    prepareCandidate,
    canCommitCandidate,
  };
};

const distributionsView = (canCommit: () => boolean) => {
  const prepare = vi.fn(async (current) => {
    return { current } as unknown as PreparedComparisonDistributions;
  });
  const canCommitPrepared = vi.fn(canCommit);
  return {
    view: {
      prepare,
      canCommit: canCommitPrepared,
    } as unknown as ComparisonDistributionsView,
    prepare,
    canCommit: canCommitPrepared,
  };
};

describe("comparison publication candidate", () => {
  it("prepares one coherent comparison and policy view without changing inputs", async () => {
    const left = publication("left", [1, 2, 3]);
    const right = publication("right", [2, 3, 4]);

    const candidate = await prepareComparisonPublication(
      left,
      right,
      true,
      new AbortController().signal,
    );

    expect(candidate.comparison.left.snapshot).toBe(left.publication);
    expect(candidate.comparison.right.snapshot).toBe(right.publication);
    expect(
      candidate.policyView.rows.map(({ populationCount }) => populationCount),
    ).toEqual([1, 2, 2, 1]);
    expect(candidate.sourceIds).toEqual(["left", "right"]);
    expect(candidate.candidateKey).toBe("snapshot-left|snapshot-right");
  });

  it("does not begin preparation for a superseded request", async () => {
    const controller = new AbortController();
    controller.abort();

    await expect(
      prepareComparisonPublication(
        publication("left", [1]),
        publication("right", [1]),
        true,
        controller.signal,
      ),
    ).rejects.toMatchObject({ name: "AbortError" });
  });

  it("reprepares comparison geometry after one commit invalidation", async () => {
    let checks = 0;
    const canvas = canvasView(() => {
      checks += 1;
      return checks > 1;
    });

    const prepared = await prepareComparisonCommitCandidate(
      publication("left", [1, 2, 3]),
      publication("right", [2, 3, 4]),
      false,
      false,
      new AbortController().signal,
      () => true,
      unusedDistributionsView,
      canvas.view,
    );

    expect(prepared.publication.candidateKey).toBe(
      "snapshot-left|snapshot-right",
    );
    expect(canvas.prepareCandidate).toHaveBeenCalledTimes(2);
    expect(canvas.canCommitCandidate).toHaveBeenCalledTimes(3);
  });

  it("reprepares the complete candidate after one distribution invalidation", async () => {
    let checks = 0;
    const distributions = distributionsView(() => {
      checks += 1;
      return checks > 1;
    });
    const canvas = canvasView(() => true);
    const isCurrent = vi.fn(() => true);

    const prepared = await prepareComparisonCommitCandidate(
      publication("left", [1, 2, 3]),
      publication("right", [2, 3, 4]),
      true,
      false,
      new AbortController().signal,
      isCurrent,
      distributions.view,
      canvas.view,
    );

    expect(prepared.distributions).not.toBeNull();
    expect(distributions.prepare).toHaveBeenCalledTimes(2);
    expect(distributions.canCommit).toHaveBeenCalledTimes(3);
    expect(canvas.prepareCandidate).toHaveBeenCalledTimes(2);
    expect(canvas.canCommitCandidate).toHaveBeenCalledTimes(3);
    expect(isCurrent).toHaveBeenCalledTimes(3);
  });

  it("rejects a stale distribution commit before publishing comparison state", async () => {
    const prepared = await prepareComparisonPublication(
      publication("left", [1, 2, 3]),
      publication("right", [2, 3, 4]),
      true,
      new AbortController().signal,
    );
    const commit = vi.fn(() => false);
    const retained = { comparison: "retained" };
    let published = retained;

    expect(() => {
      commitPreparedComparisonDistributions(
        true,
        { current: prepared.comparison } as PreparedComparisonDistributions,
        prepared.comparison,
        { commit } as unknown as ComparisonDistributionsView,
      );
      published = { comparison: "candidate" };
    }).toThrowError(
      expect.objectContaining({
        name: "AbortError",
        message: "Comparison distribution candidate was superseded",
      }),
    );

    expect(commit).toHaveBeenCalledOnce();
    expect(published).toBe(retained);
  });

  it("exits after four consecutive comparison-canvas invalidations", async () => {
    const canvas = canvasView(() => false);
    const isCurrent = vi.fn(() => true);

    await expect(
      prepareComparisonCommitCandidate(
        publication("left", [1, 2, 3]),
        publication("right", [2, 3, 4]),
        false,
        false,
        new AbortController().signal,
        isCurrent,
        unusedDistributionsView,
        canvas.view,
      ),
    ).rejects.toThrow(
      "Comparison publication candidate changed during 4 consecutive commit attempts",
    );

    expect(canvas.prepareCandidate).toHaveBeenCalledTimes(4);
    expect(canvas.canCommitCandidate).toHaveBeenCalledTimes(4);
    expect(isCurrent).toHaveBeenCalledTimes(4);
  });
});
