import { describe, expect, it } from "vitest";
import { ComparisonLifecycle } from "./comparison-lifecycle";

describe("ComparisonLifecycle", () => {
  it("invalidates a request when the selected order changes", () => {
    const lifecycle = new ComparisonLifecycle();
    const ticket = lifecycle.begin(["knots", "core"]);

    lifecycle.invalidate();

    expect(lifecycle.isCurrent(ticket, ["core", "knots"])).toBe(false);
  });

  it("rejects a late result from an older refresh of the same order", () => {
    const lifecycle = new ComparisonLifecycle();
    const first = lifecycle.begin(["knots", "core"]);
    const second = lifecycle.begin(["knots", "core"]);

    expect(lifecycle.isCurrent(first, ["knots", "core"])).toBe(false);
    expect(lifecycle.isCurrent(second, ["knots", "core"])).toBe(true);
  });

  it("copies the selected order into each request ticket", () => {
    const lifecycle = new ComparisonLifecycle();
    const sources = ["knots", "core"];
    const ticket = lifecycle.begin(sources);

    sources.reverse();

    expect(ticket.sources).toEqual(["knots", "core"]);
    expect(lifecycle.isCurrent(ticket, sources)).toBe(false);
  });
});
