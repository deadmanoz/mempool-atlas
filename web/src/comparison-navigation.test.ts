import { describe, expect, it } from "vitest";

import {
  cursorMoveForKey,
  moveComparisonCursor,
} from "./comparison-navigation";

describe("comparison transaction navigation", () => {
  it("can reach every transaction using bounded next moves", () => {
    const itemCount = 10_000;
    let index = 0;
    const reached = new Set([index]);
    for (let step = 1; step < itemCount; step += 1) {
      index = moveComparisonCursor(index, itemCount, "next") ?? -1;
      reached.add(index);
    }

    expect(reached.size).toBe(itemCount);
    expect(index).toBe(itemCount - 1);
  });

  it("supports region endpoints and bounded page movement", () => {
    expect(moveComparisonCursor(25, 1_000, "first")).toBe(0);
    expect(moveComparisonCursor(25, 1_000, "last")).toBe(999);
    expect(moveComparisonCursor(950, 1_000, "page_next")).toBe(999);
    expect(moveComparisonCursor(20, 1_000, "page_previous")).toBe(0);
    expect(moveComparisonCursor(0, 0, "next")).toBeNull();
  });

  it("maps conventional list navigation keys", () => {
    expect(cursorMoveForKey("ArrowDown")).toBe("next");
    expect(cursorMoveForKey("ArrowLeft")).toBe("previous");
    expect(cursorMoveForKey("Home")).toBe("first");
    expect(cursorMoveForKey("PageUp")).toBe("page_previous");
    expect(cursorMoveForKey("Enter")).toBeNull();
  });
});
