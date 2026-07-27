import { describe, expect, it } from "vitest";

import { ComparisonLifecycle, RequestLifecycle } from "./comparison-lifecycle";

describe("RequestLifecycle", () => {
  it("aborts and rejects an older source-discovery generation", () => {
    const lifecycle = new RequestLifecycle();
    const first = lifecycle.begin();
    const second = lifecycle.begin();

    expect(first.signal.aborted).toBe(true);
    expect(lifecycle.isCurrent(first)).toBe(false);
    expect(second.signal.aborted).toBe(false);
    expect(lifecycle.isCurrent(second)).toBe(true);
  });
});

describe("ComparisonLifecycle", () => {
  it("rejects a late response after the pair changes", () => {
    const lifecycle = new ComparisonLifecycle();
    const first = lifecycle.begin("core", "knots");
    const second = lifecycle.begin("core", "libre");

    expect(first.signal.aborted).toBe(true);
    expect(second.signal.aborted).toBe(false);
    expect(lifecycle.isCurrent(first, "core", "knots")).toBe(false);
    expect(lifecycle.isCurrent(second, "core", "libre")).toBe(true);
  });

  it("treats source order as presentation state", () => {
    const lifecycle = new ComparisonLifecycle();
    const ticket = lifecycle.begin("core", "knots");

    expect(lifecycle.isCurrent(ticket, "knots", "core")).toBe(false);
  });

  it("invalidates an in-flight refresh", () => {
    const lifecycle = new ComparisonLifecycle();
    const ticket = lifecycle.begin("core", "knots");
    lifecycle.invalidate();

    expect(ticket.signal.aborted).toBe(true);
    expect(lifecycle.isCurrent(ticket, "core", "knots")).toBe(false);
  });
});
