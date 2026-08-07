import { describe, expect, it } from "vitest";

import { prepareComparisonPublication } from "./comparison-publication-candidate";
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
    publication: loaded.snapshot,
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
    expect(candidate.candidateKey).toContain("left:");
    expect(candidate.candidateKey).toContain("right:");
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
});
