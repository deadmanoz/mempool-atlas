export interface AtlasCandidateReadyDetail {
  surface: "node" | "comparison";
  candidateKey: string;
  sourceIds: string[];
}

declare global {
  interface Window {
    __atlasCandidateReadyHook?: (
      detail: AtlasCandidateReadyDetail,
    ) => void | Promise<void>;
  }
}

const abortReason = (signal: AbortSignal): unknown =>
  signal.reason ?? new DOMException("The operation was aborted", "AbortError");

const markCandidate = (
  phase: "replacement-candidate-ready" | "replacement-committed",
  detail: AtlasCandidateReadyDetail,
): void => {
  performance.mark(`atlas:${detail.surface}:${phase}`, { detail });
};

export const awaitAtlasCandidateRelease = async (
  detail: AtlasCandidateReadyDetail,
  signal: AbortSignal,
): Promise<void> => {
  signal.throwIfAborted();
  markCandidate("replacement-candidate-ready", detail);
  const hook = window.__atlasCandidateReadyHook;
  if (hook === undefined) return;

  const hookResult = Promise.resolve().then(() => hook(detail));
  await new Promise<void>((resolve, reject) => {
    const abort = (): void => {
      signal.removeEventListener("abort", abort);
      reject(abortReason(signal));
    };
    signal.addEventListener("abort", abort, { once: true });
    hookResult.then(
      () => {
        signal.removeEventListener("abort", abort);
        if (signal.aborted) reject(abortReason(signal));
        else resolve();
      },
      (error) => {
        signal.removeEventListener("abort", abort);
        reject(error);
      },
    );
  });
};

export const recordAtlasCandidateCommitted = (
  detail: AtlasCandidateReadyDetail,
): void => {
  markCandidate("replacement-committed", detail);
};
