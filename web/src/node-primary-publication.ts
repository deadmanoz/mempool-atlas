import { isPublicationCommitRetryExhausted } from "./publication-commit-budget";

export const renderPrimaryNodePublication = async (
  render: () => Promise<void>,
): Promise<void> => {
  try {
    await render();
  } catch (error) {
    if (!isPublicationCommitRetryExhausted(error, "Node")) throw error;
  }
};
