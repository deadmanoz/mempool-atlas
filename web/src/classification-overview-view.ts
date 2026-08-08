import {
  DEFAULT_CLASSIFIER_ID,
  classifierDescriptor,
  classifierSummary,
} from "./classification-view";
import type { ClassifierLabelMatchMode } from "./classifier-terrain";
import { countFormat } from "./format";
import type { MempoolSnapshot } from "./types";

interface ClassificationOverviewElements {
  lensSelect: HTMLSelectElement;
  method: HTMLElement;
  empty: HTMLElement;
  labels: HTMLElement;
  summary: HTMLElement;
  selectedSummary: HTMLElement;
  matchAny: HTMLButtonElement;
  matchAll: HTMLButtonElement;
  clear: HTMLButtonElement;
}

interface ClassificationOverviewSelection {
  classifierId: string;
  labels: readonly string[];
  matchMode: ClassifierLabelMatchMode;
}

interface ClassificationOverviewView {
  render(
    snapshot: MempoolSnapshot | null,
    selection: ClassificationOverviewSelection,
  ): ClassificationOverviewSelection;
}

export const createClassificationOverviewView = (
  elements: ClassificationOverviewElements,
  onToggleLabel: (label: string) => void,
  onSetMatchMode: (mode: ClassifierLabelMatchMode) => void,
  onClear: () => void,
): ClassificationOverviewView => {
  elements.matchAny.addEventListener("click", () => onSetMatchMode("any"));
  elements.matchAll.addEventListener("click", () => onSetMatchMode("all"));
  elements.clear.addEventListener("click", onClear);

  return {
    render: (snapshot, selection) => {
      if (snapshot === null) {
        elements.lensSelect.replaceChildren();
        elements.labels.replaceChildren();
        elements.empty.hidden = false;
        const waiting = document.createElement("strong");
        waiting.textContent = "Waiting for classifier catalog";
        const detail = document.createElement("span");
        detail.textContent =
          "Atlas publishes the available lenses with each snapshot.";
        elements.method.replaceChildren(waiting, detail);
        elements.summary.textContent =
          "Independent classifiers remain separate rather than forming one combined taxonomy.";
        elements.selectedSummary.textContent =
          "Choose labels to reveal their transactions.";
        elements.matchAny.disabled = true;
        elements.matchAll.disabled = true;
        elements.clear.disabled = true;
        return selection;
      }

      const existingOptions = [...elements.lensSelect.options];
      const optionsMatch =
        existingOptions.length === snapshot.classifier_catalog.length &&
        existingOptions.every(
          (option, index) =>
            option.value === snapshot.classifier_catalog[index]?.id,
        );
      if (optionsMatch) {
        existingOptions.forEach((option, index) => {
          option.textContent = snapshot.classifier_catalog[index]?.title ?? "";
        });
      } else {
        elements.lensSelect.replaceChildren(
          ...snapshot.classifier_catalog.map((descriptor) => {
            const option = document.createElement("option");
            option.value = descriptor.id;
            option.textContent = descriptor.title;
            return option;
          }),
        );
      }

      const classifierId = snapshot.classifier_catalog.some(
        ({ id }) => id === selection.classifierId,
      )
        ? selection.classifierId
        : (snapshot.classifier_catalog[0]?.id ?? DEFAULT_CLASSIFIER_ID);
      elements.lensSelect.value = classifierId;

      const descriptor = classifierDescriptor(snapshot, classifierId);
      const classifier = classifierSummary(snapshot, classifierId);
      if (descriptor === null || classifier === null) {
        elements.labels.replaceChildren();
        elements.empty.hidden = false;
        elements.empty.textContent = "This classifier is unavailable.";
        return {
          classifierId,
          labels: selection.labels,
          matchMode: selection.matchMode,
        };
      }

      const requestedLabels = new Set(selection.labels);
      const labels = descriptor.labels.flatMap(({ key }) =>
        requestedLabels.has(key) ? [key] : [],
      );
      const matchMode =
        descriptor.semantics === "multi_label" ? selection.matchMode : "any";
      const selectedLabels = new Set(labels);
      elements.empty.hidden = snapshot.transaction_count !== 0;
      elements.empty.textContent = "This snapshot contains an empty mempool.";
      const methodDetail = document.createElement("span");
      methodDetail.textContent =
        descriptor.semantics === "multi_label"
          ? "Labels can overlap within this lens."
          : "This lens reports a specialized rule-set outcome.";
      elements.method.replaceChildren(methodDetail);

      const existingButtons = new Map(
        [...elements.labels.querySelectorAll<HTMLButtonElement>("button")]
          .filter(({ dataset }) => dataset.label !== undefined)
          .map((button) => [button.dataset.label ?? "", button]),
      );
      const labelButtons = descriptor.labels.map((descriptorLabel) => {
        const count = classifier.label_counts[descriptorLabel.key] ?? 0;
        const share =
          snapshot.transaction_count === 0
            ? 0
            : count / snapshot.transaction_count;
        let button = existingButtons.get(descriptorLabel.key);
        if (button === undefined) {
          button = document.createElement("button");
          button.type = "button";
          button.className = "classification-label";
          button.dataset.label = descriptorLabel.key;
          button.append(
            document.createElement("span"),
            document.createElement("strong"),
            document.createElement("small"),
          );
          button.addEventListener("click", () => {
            onToggleLabel(descriptorLabel.key);
          });
        }
        button.setAttribute(
          "aria-pressed",
          String(selectedLabels.has(descriptorLabel.key)),
        );
        button.setAttribute(
          "aria-label",
          `${descriptorLabel.label}: ${countFormat.format(count)} transactions. Toggle this label in the transaction query.`,
        );
        button.style.setProperty("--label-share", `${share * 100}%`);
        const heading = button.querySelector("span");
        const total = button.querySelector("strong");
        const description = button.querySelector("small");
        if (heading !== null) heading.textContent = descriptorLabel.label;
        if (total !== null) total.textContent = countFormat.format(count);
        if (description !== null) {
          description.textContent = descriptorLabel.description;
        }
        return button;
      });
      const labelOrderMatches =
        elements.labels.children.length === labelButtons.length &&
        labelButtons.every(
          (button, index) => elements.labels.children[index] === button,
        );
      if (!labelOrderMatches) {
        elements.labels.replaceChildren(...labelButtons);
      }
      const selectedNames = descriptor.labels.flatMap(({ key, label }) =>
        selectedLabels.has(key) ? [label] : [],
      );
      elements.selectedSummary.textContent =
        selectedNames.length === 0
          ? "Choose labels to reveal their transactions."
          : selectedNames.length === 1
            ? `${selectedNames[0]} selected.`
            : `${selectedNames.length} labels selected: ${selectedNames.join(", ")}.`;
      const canCombine =
        descriptor.semantics === "multi_label" && selectedNames.length >= 2;
      elements.matchAny.disabled = !canCombine;
      elements.matchAll.disabled = !canCombine;
      elements.matchAny.setAttribute(
        "aria-pressed",
        String(matchMode === "any"),
      );
      elements.matchAll.setAttribute(
        "aria-pressed",
        String(matchMode === "all"),
      );
      elements.matchAny.title = canCombine
        ? "Match transactions carrying at least one selected label."
        : "Select at least two overlapping labels to choose how they combine.";
      elements.matchAll.title = canCombine
        ? "Match transactions carrying every selected label."
        : elements.matchAny.title;
      elements.clear.disabled = selectedNames.length === 0;
      elements.summary.textContent = `${countFormat.format(classifier.complete_count)} complete, ${countFormat.format(classifier.partial_count)} partial, and ${countFormat.format(classifier.unclassified_count)} unavailable results. Marginal label totals may overlap.`;
      return { classifierId, labels, matchMode };
    },
  };
};
