import { describe, expect, it } from "vitest";

import { pinSelectedInBoundedSample } from "./bounded-sample";

const entry = (index: number) => ({ txid: `tx-${index}`, index });

describe("pinSelectedInBoundedSample", () => {
  it("pins an out-of-sample selection without exceeding the row limit", () => {
    const largest = Array.from({ length: 8 }, (_, index) => entry(index));
    const selected = entry(20);

    const sample = pinSelectedInBoundedSample(largest, selected, 8);

    expect(sample).toEqual({
      entries: [selected, ...largest.slice(0, 7)],
      pinsSelected: true,
    });
  });

  it("keeps an in-sample selection without duplicating it", () => {
    const largest = Array.from({ length: 8 }, (_, index) => entry(index));

    const sample = pinSelectedInBoundedSample(largest, largest[5], 8);

    expect(sample).toEqual({ entries: largest, pinsSelected: false });
  });

  it("keeps the sample bounded when there is no selected entry", () => {
    const largest = Array.from({ length: 10 }, (_, index) => entry(index));

    const sample = pinSelectedInBoundedSample(largest, undefined, 8);

    expect(sample).toEqual({
      entries: largest.slice(0, 8),
      pinsSelected: false,
    });
  });

  it("does not pin an out-of-population selection", () => {
    const largest = Array.from({ length: 8 }, (_, index) => entry(index));

    expect(
      pinSelectedInBoundedSample(largest, entry(20), 8, () => false),
    ).toEqual({ entries: largest, pinsSelected: false });
  });
});
