// @vitest-environment happy-dom

import { afterEach, describe, expect, it, vi } from "vitest";

import { markAtlasReadinessAfterPaint } from "./readiness";

afterEach(() => {
  vi.restoreAllMocks();
});

describe("paint readiness", () => {
  it("marks readiness after two visible animation frames", async () => {
    const frames: FrameRequestCallback[] = [];
    vi.spyOn(window, "requestAnimationFrame").mockImplementation((callback) => {
      frames.push(callback);
      return frames.length;
    });
    vi.spyOn(performance, "mark").mockImplementation(
      () => ({}) as PerformanceMark,
    );
    const status = document.createElement("div");
    const readiness = markAtlasReadinessAfterPaint(
      status,
      "node",
      "primary-interactive",
      () => true,
    );

    frames.shift()?.(1);
    expect(status.dataset.readiness).toBeUndefined();
    frames.shift()?.(2);
    await readiness;

    expect(status.dataset.readiness).toBe("primary-interactive");
  });

  it("does not wait for suspended frames after the document becomes hidden", async () => {
    let visibility: DocumentVisibilityState = "visible";
    vi.spyOn(document, "visibilityState", "get").mockImplementation(
      () => visibility,
    );
    vi.spyOn(window, "requestAnimationFrame").mockImplementation(() => 1);
    vi.spyOn(performance, "mark").mockImplementation(
      () => ({}) as PerformanceMark,
    );
    const status = document.createElement("div");
    const readiness = markAtlasReadinessAfterPaint(
      status,
      "node",
      "complete-feature-ready",
      () => true,
    );

    visibility = "hidden";
    document.dispatchEvent(new Event("visibilitychange"));
    await readiness;

    expect(status.dataset.readiness).toBe("complete-feature-ready");
  });
});
