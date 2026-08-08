import { describe, expect, it, vi } from "vitest";

import {
  comparisonCanvasRenderFailureHandler,
  handleComparisonCanvasRenderFailure,
} from "./comparison-canvas-failure";

describe("comparison canvas render failure handling", () => {
  it("reports a current non-abort rendering failure", () => {
    const showFailure = vi.fn();

    expect(
      handleComparisonCanvasRenderFailure(
        new Error("Canvas context lost"),
        () => true,
        showFailure,
      ),
    ).toBe(true);
    expect(showFailure).toHaveBeenCalledWith("Canvas context lost");
  });

  it("ignores aborts and failures from superseded comparisons", () => {
    const showFailure = vi.fn();

    expect(
      handleComparisonCanvasRenderFailure(
        new DOMException("Aborted", "AbortError"),
        () => true,
        showFailure,
      ),
    ).toBe(false);
    expect(
      handleComparisonCanvasRenderFailure(
        new Error("Obsolete render failed"),
        () => false,
        showFailure,
      ),
    ).toBe(false);
    expect(showFailure).not.toHaveBeenCalled();
  });

  it("uses a stable fallback for non-error rejections", () => {
    const showFailure = vi.fn();

    handleComparisonCanvasRenderFailure("failed", () => true, showFailure);

    expect(showFailure).toHaveBeenCalledWith(
      "Unable to render comparison canvas",
    );
  });

  it("maps a current failure into the visible comparison status", () => {
    const setStatus = vi.fn();
    const handleFailure = comparisonCanvasRenderFailureHandler(
      () => true,
      setStatus,
    );

    handleFailure(new Error("Canvas context lost"));

    expect(setStatus).toHaveBeenCalledWith(
      "error",
      "Comparison rendering unavailable",
      "Canvas context lost",
    );
  });
});
