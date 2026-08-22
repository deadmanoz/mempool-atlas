export interface AtlasProblem {
  type: string;
  title: string;
  status: number;
  detail: string | null;
}

const isRecord = (value: unknown): value is Record<string, unknown> =>
  typeof value === "object" && value !== null;

export const parseAtlasProblem = (
  value: unknown,
  httpStatus: number,
): AtlasProblem | null => {
  if (
    !isRecord(value) ||
    typeof value.type !== "string" ||
    typeof value.title !== "string" ||
    value.status !== httpStatus ||
    (value.detail !== null && typeof value.detail !== "string")
  ) {
    return null;
  }
  return {
    type: value.type,
    title: value.title,
    status: value.status,
    detail: value.detail,
  };
};

export const atlasFailureBody = (
  value: unknown,
  httpStatus: number,
): { detail: string; problem: AtlasProblem | null } => {
  const problem = parseAtlasProblem(value, httpStatus);
  if (problem !== null) {
    return { detail: problem.title, problem };
  }
  return {
    detail:
      isRecord(value) && typeof value.error === "string" ? value.error : "",
    problem: null,
  };
};
