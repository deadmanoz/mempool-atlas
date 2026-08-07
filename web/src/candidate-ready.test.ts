// @vitest-environment happy-dom

import { afterEach, describe, expect, it, vi } from "vitest";

import {
  awaitAtlasCandidateRelease,
  recordAtlasCandidateCommitted,
  type AtlasCandidateReadyDetail,
} from "./candidate-ready";

const detail: AtlasCandidateReadyDetail = {
  surface: "comparison",
  candidateKey: "left:12:right:34",
  sourceIds: ["left", "right"],
};

afterEach(() => {
  delete window.__atlasCandidateReadyHook;
  vi.restoreAllMocks();
});

describe("candidate-ready coordination", () => {
  it("marks and resolves immediately when no measurement hook is installed", async () => {
    const mark = vi.spyOn(performance, "mark");

    await awaitAtlasCandidateRelease(detail, new AbortController().signal);

    expect(mark).toHaveBeenCalledWith(
      "atlas:comparison:replacement-candidate-ready",
      { detail },
    );
  });

  it("holds candidate commit until the injected hook releases it", async () => {
    let release = (): void => undefined;
    window.__atlasCandidateReadyHook = vi.fn(
      () =>
        new Promise<void>((resolve) => {
          release = resolve;
        }),
    );
    let settled = false;
    const waiting = awaitAtlasCandidateRelease(
      detail,
      new AbortController().signal,
    ).then(() => {
      settled = true;
    });
    await Promise.resolve();

    expect(window.__atlasCandidateReadyHook).toHaveBeenCalledWith(detail);
    expect(settled).toBe(false);
    release();
    await waiting;
    expect(settled).toBe(true);
  });

  it("rejects an unreleased candidate when its request is superseded", async () => {
    window.__atlasCandidateReadyHook = () => new Promise<void>(() => undefined);
    const controller = new AbortController();
    const waiting = awaitAtlasCandidateRelease(detail, controller.signal);
    await Promise.resolve();

    controller.abort();

    await expect(waiting).rejects.toMatchObject({ name: "AbortError" });
  });

  it("records the committed candidate with the same identity", () => {
    const mark = vi.spyOn(performance, "mark");

    recordAtlasCandidateCommitted(detail);

    expect(mark).toHaveBeenCalledWith(
      "atlas:comparison:replacement-committed",
      { detail },
    );
  });
});
