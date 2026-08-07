import { classifierSummary } from "./classification-view";
import {
  KNOTS_BIP110_CLASSIFIER_ID,
  classifierUsesSummaryBuckets,
} from "./classifier-terrain";
import { countFormat } from "./format";
import type { ClassifierDescriptor, MempoolSnapshot } from "./types";

export type TerrainSummaryMetric = "count" | "vsize";

export interface TerrainSummaryView {
  render(
    snapshot: MempoolSnapshot,
    descriptor: ClassifierDescriptor | null,
    metric: TerrainSummaryMetric,
  ): void;
  reset(): void;
}

const requiredElement = <T extends HTMLElement>(id: string): T => {
  const element = document.getElementById(id);
  if (!(element instanceof HTMLElement)) {
    throw new Error(`Missing required terrain-summary element #${id}`);
  }
  return element as T;
};

const methodologyText = (descriptor: ClassifierDescriptor): string => {
  if (descriptor.methodology === "exact") return "Exact properties";
  if (descriptor.methodology === "heuristic") return "Heuristic shapes";
  if (descriptor.methodology === "fingerprint") return "Data fingerprints";
  return "Policy rule set";
};

export const createTerrainSummaryView = (): TerrainSummaryView => {
  const eyebrow = requiredElement<HTMLElement>("terrain-eyebrow");
  const title = requiredElement<HTMLElement>("terrain-title");
  const summary = requiredElement<HTMLElement>("terrain-summary");
  const totals = (["a", "b", "c"] as const).map((key) => ({
    wrapper: requiredElement<HTMLElement>(`terrain-total-${key}`),
    label: requiredElement<HTMLElement>(`terrain-total-${key}-label`),
    count: requiredElement<HTMLElement>(`terrain-total-${key}-count`),
  }));

  const setTotal = (
    index: number,
    label: string,
    count: number,
    tone: string,
  ): void => {
    const total = totals[index];
    if (total === undefined) return;
    total.wrapper.dataset.tone = tone;
    total.label.textContent = label;
    total.count.textContent = countFormat.format(count);
  };

  const reset = (): void => {
    for (const total of totals) total.count.textContent = "0";
  };

  return {
    reset,
    render: (snapshot, descriptor, metric) => {
      const metricLabel =
        metric === "count" ? "transaction count" : "virtual size";
      if (descriptor?.id === KNOTS_BIP110_CLASSIFIER_ID) {
        eyebrow.textContent = "Knots mempool policy lens";
        title.textContent = "BIP-110 rule buckets";
        setTotal(
          0,
          "Compatible",
          snapshot.bip110_summary.compatible_count,
          "compatible",
        );
        setTotal(
          1,
          "Indeterminate",
          snapshot.bip110_summary.indeterminate_count,
          "indeterminate",
        );
        setTotal(
          2,
          "Would violate",
          snapshot.bip110_summary.violating_count,
          "violating",
        );
        summary.textContent = `Policy assessments for this source snapshot use ${snapshot.bip110_summary.evaluator_id} ${snapshot.bip110_summary.evaluator_version}. Each transaction appears once. Complete violations use exact rule-combination buckets; proven violations with unresolved checks remain separate. Bucket area represents ${metricLabel}.`;
        return;
      }
      if (descriptor === null) {
        reset();
        return;
      }
      const totals = classifierSummary(snapshot, descriptor.id);
      const summaryBuckets = classifierUsesSummaryBuckets(descriptor);
      eyebrow.textContent = `${methodologyText(descriptor)} · version ${descriptor.version}`;
      title.textContent = `${descriptor.title} ${summaryBuckets ? "groups" : "buckets"}`;
      setTotal(0, "Complete", totals?.complete_count ?? 0, "complete");
      setTotal(1, "Partial", totals?.partial_count ?? 0, "partial");
      setTotal(
        2,
        "Unavailable",
        totals?.unclassified_count ?? 0,
        "unavailable",
      );
      summary.textContent = summaryBuckets
        ? `Broad script-profile groups keep this overview readable; exact version, RBF, witness, data-carrier, and script labels remain on each transaction. Group area represents ${metricLabel}. Complete, partial, and unavailable results remain separate.`
        : `Bucket area represents ${metricLabel}. Complete, partial, and unavailable results remain separate. Choose a bucket for its population; open Labels & rules only when you need a marginal filter.`;
    },
  };
};
