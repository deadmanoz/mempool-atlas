export type AtlasReadinessMilestone =
  "metadata-usable" | "primary-interactive" | "complete-feature-ready";

export const markAtlasReadiness = (
  status: HTMLElement,
  surface: "node" | "comparison",
  milestone: AtlasReadinessMilestone,
): void => {
  status.dataset.readiness = milestone;
  performance.mark(`atlas:${surface}:${milestone}`);
};

export const markAtlasReadinessAfterPaint = (
  status: HTMLElement,
  surface: "node" | "comparison",
  milestone: Exclude<AtlasReadinessMilestone, "metadata-usable">,
  isCurrent: () => boolean,
): Promise<void> =>
  new Promise((resolve) => {
    let settled = false;
    const finish = (): void => {
      if (settled) return;
      settled = true;
      document.removeEventListener("visibilitychange", finishWhenHidden);
      if (isCurrent()) {
        if (
          milestone === "complete-feature-ready" &&
          status.dataset.readiness !== "primary-interactive" &&
          status.dataset.readiness !== "complete-feature-ready"
        ) {
          markAtlasReadiness(status, surface, "primary-interactive");
        }
        markAtlasReadiness(status, surface, milestone);
      }
      resolve();
    };
    const finishWhenHidden = (): void => {
      if (document.visibilityState === "hidden") finish();
    };
    document.addEventListener("visibilitychange", finishWhenHidden);
    if (document.visibilityState === "hidden") {
      finish();
      return;
    }
    window.requestAnimationFrame(() => {
      if (settled) return;
      window.requestAnimationFrame(finish);
    });
  });
