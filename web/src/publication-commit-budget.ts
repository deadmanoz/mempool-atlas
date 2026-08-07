export const MAX_PUBLICATION_COMMIT_ATTEMPTS = 4;

export const requirePublicationCommitRetry = (
  surface: "Node" | "Comparison",
  completedAttempts: number,
): void => {
  if (completedAttempts >= MAX_PUBLICATION_COMMIT_ATTEMPTS) {
    throw new Error(
      `${surface} publication candidate changed during ${MAX_PUBLICATION_COMMIT_ATTEMPTS} consecutive commit attempts`,
    );
  }
};
