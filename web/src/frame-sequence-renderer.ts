import { yieldCooperatively } from "./cooperative-work";

export interface FrameSequenceRenderer {
  schedule(): void;
  cancel(): void;
}

/**
 * Give the browser one animation frame, then run at most one rendering step
 * after yielding back to the task scheduler. Canvas/DOM work cannot inflate
 * the animation-frame callback itself, and every step gets a paint opportunity
 * before the next one is scheduled.
 */
export const createFrameSequenceRenderer = (
  steps: readonly (() => void)[],
): FrameSequenceRenderer => {
  let pendingFrame: number | null = null;
  let revision = 0;

  const requestFrame = (expectedRevision: number, index: number): void => {
    const callback = function sequenceRenderFrame() {
      void renderFrame(expectedRevision, index);
    };
    Object.assign(callback, { __atlasPerfLabel: "frame-sequence" });
    pendingFrame = window.requestAnimationFrame(callback);
  };

  const renderFrame = async (
    expectedRevision: number,
    index: number,
  ): Promise<void> => {
    await yieldCooperatively();
    if (expectedRevision !== revision) return;
    if (index >= 0) steps[index]?.();
    if (index + 1 < steps.length) {
      requestFrame(expectedRevision, index + 1);
    } else {
      pendingFrame = null;
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
