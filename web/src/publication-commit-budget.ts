export const MAX_PUBLICATION_COMMIT_ATTEMPTS = 4;

export type PublicationCommitSurface = "Node" | "Comparison";

export class PublicationCommitRetryExhaustedError extends Error {
  readonly surface: PublicationCommitSurface;

  constructor(surface: PublicationCommitSurface) {
    super(
      `${surface} publication candidate changed during ${MAX_PUBLICATION_COMMIT_ATTEMPTS} consecutive commit attempts`,
    );
    this.name = "PublicationCommitRetryExhaustedError";
    this.surface = surface;
  }
}

export const isPublicationCommitRetryExhausted = (
  error: unknown,
  surface?: PublicationCommitSurface,
): error is PublicationCommitRetryExhaustedError =>
  error instanceof PublicationCommitRetryExhaustedError &&
  (surface === undefined || error.surface === surface);

export const requirePublicationCommitRetry = (
  surface: PublicationCommitSurface,
  completedAttempts: number,
): void => {
  if (completedAttempts >= MAX_PUBLICATION_COMMIT_ATTEMPTS) {
    throw new PublicationCommitRetryExhaustedError(surface);
  }
};
