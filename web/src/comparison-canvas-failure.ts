import { isAbortError } from "./comparison-lifecycle";

export const handleComparisonCanvasRenderFailure = (
  error: unknown,
  ownsRender: () => boolean,
  showFailure: (message: string) => void,
): boolean => {
  if (isAbortError(error) || !ownsRender()) return false;
  showFailure(
    error instanceof Error
      ? error.message
      : "Unable to render comparison canvas",
  );
  return true;
};

export const comparisonCanvasRenderFailureHandler =
  (
    ownsRender: () => boolean,
    setStatus: (state: "error", title: string, detail: string) => void,
  ): ((error: unknown) => void) =>
  (error) => {
    handleComparisonCanvasRenderFailure(error, ownsRender, (message) =>
      setStatus("error", "Comparison rendering unavailable", message),
    );
  };
