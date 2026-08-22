import { isPublicationCommitRetryExhausted } from "./publication-commit-budget";

export const renderPrimaryComparisonPublication = async (
  render: () => Promise<unknown>,
): Promise<void> => {
  try {
    await render();
  } catch (error) {
    if (!isPublicationCommitRetryExhausted(error, "Comparison")) throw error;
  }
};
