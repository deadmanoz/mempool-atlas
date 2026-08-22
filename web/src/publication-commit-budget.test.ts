import { describe, expect, it } from "vitest";

import {
  isPublicationCommitRetryExhausted,
  PublicationCommitRetryExhaustedError,
  requirePublicationCommitRetry,
} from "./publication-commit-budget";

describe("publication commit budget", () => {
  it("throws a typed surface-specific error when the budget is exhausted", () => {
    let thrown: unknown;
    try {
      requirePublicationCommitRetry("Node", 4);
    } catch (error) {
      thrown = error;
    }

    expect(thrown).toBeInstanceOf(PublicationCommitRetryExhaustedError);
    expect(isPublicationCommitRetryExhausted(thrown, "Node")).toBe(true);
    expect(isPublicationCommitRetryExhausted(thrown, "Comparison")).toBe(false);
  });

  it("does not identify unrelated errors by message", () => {
    const matchingMessage = new Error(
      "Node publication candidate changed during 4 consecutive commit attempts",
    );

    expect(isPublicationCommitRetryExhausted(matchingMessage, "Node")).toBe(
      false,
    );
  });
});
