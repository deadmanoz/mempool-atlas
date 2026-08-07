import type { AtlasProblem } from "./atlas-problem";

export class SupersededStageError extends Error {}

export class WorkerHttpError extends Error {
  constructor(
    readonly status: number,
    message: string,
    readonly problem: AtlasProblem | null = null,
  ) {
    super(message);
  }
}
