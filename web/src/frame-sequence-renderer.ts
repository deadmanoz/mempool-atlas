export interface FrameSequenceRenderer {
  schedule(): void;
  cancel(): void;
}

/** Run at most one rendering step in each animation frame. */
export const createFrameSequenceRenderer = (
  steps: readonly (() => void)[],
): FrameSequenceRenderer => {
  let pendingFrame: number | null = null;
  let revision = 0;

  const requestFrame = (expectedRevision: number, index: number): void => {
    const callback = function sequenceRenderFrame() {
      renderFrame(expectedRevision, index);
    };
    Object.assign(callback, { __atlasPerfLabel: "frame-sequence" });
    pendingFrame = window.requestAnimationFrame(callback);
  };

  const renderFrame = (expectedRevision: number, index: number): void => {
    pendingFrame = null;
    if (expectedRevision !== revision) return;
    if (index >= 0) steps[index]?.();
    if (index + 1 < steps.length) {
      requestFrame(expectedRevision, index + 1);
    }
  };

  const cancel = (): void => {
    revision += 1;
    if (pendingFrame !== null) window.cancelAnimationFrame(pendingFrame);
    pendingFrame = null;
  };

  return {
    cancel,
    schedule: () => {
      if (pendingFrame !== null || steps.length === 0) return;
      const expectedRevision = revision;
      requestFrame(expectedRevision, -1);
    },
  };
};
