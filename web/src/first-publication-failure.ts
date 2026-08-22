import { AtlasRequestError, fetchSources } from "./api";
import { formatDuration } from "./format";
import type { SourceSummary, SourcesResponse } from "./types";

export interface FirstPublicationFailurePresentation {
  state: "waiting" | "error";
  title: string;
  detail: string;
}

export const refreshSources = async (
  error: unknown,
  hasCurrentSnapshot: boolean,
  signal: AbortSignal,
): Promise<SourcesResponse | null | undefined> => {
  if (
    hasCurrentSnapshot ||
    !(error instanceof AtlasRequestError) ||
    error.problemType !== "v2_unavailable"
  ) {
    return undefined;
  }
  try {
    return await fetchSources(signal);
  } catch {
    return null;
  }
};

export const presentation = (
  error: unknown,
  source: SourceSummary | undefined,
): FirstPublicationFailurePresentation => {
  if (
    error instanceof AtlasRequestError &&
    error.problemType === "v2_unavailable" &&
    source?.availability === "waiting"
  ) {
    return {
      state: "waiting",
      title: "Waiting for first snapshot",
      detail: `Atlas polls ${source.source_label} every ${formatDuration(source.poll_interval_seconds * 1_000)}. No complete observation has been published yet.`,
    };
  }
  return {
    state: "error",
    title: "Atlas website unavailable",
    detail:
      source?.availability === "error" && source.last_error !== null
        ? source.last_error
        : error instanceof Error
          ? error.message
          : "Unable to load snapshot",
  };
};
