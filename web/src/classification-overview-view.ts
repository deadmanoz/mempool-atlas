import {
  DEFAULT_CLASSIFIER_ID,
  classifierDescriptor,
  classifierSummary,
  firstPopulatedLabel,
} from "./classification-view";
import { countFormat } from "./format";
import type { MempoolSnapshot } from "./types";

interface ClassificationOverviewElements {
  lensSelect: HTMLSelectElement;
  method: HTMLElement;
  empty: HTMLElement;
  labels: HTMLElement;
  summary: HTMLElement;
}

interface ClassificationOverviewSelection {
  classifierId: string;
  label: string | null;
}

interface ClassificationOverviewView {
  render(
    snapshot: MempoolSnapshot | null,
    selection: ClassificationOverviewSelection,
  ): ClassificationOverviewSelection;
}

const methodologyText = (
  methodology: "exact" | "heuristic" | "fingerprint" | "policy",
): string => {
  if (methodology === "exact") return "Exact observations";
  if (methodology === "heuristic") {
    return "Heuristic signals, not proof of intent";
  }
  if (methodology === "fingerprint") {
    return "Byte and script fingerprints, not protocol-state validation";
  }
  return "Source-local policy compatibility";
};

export const createClassificationOverviewView = (
  elements: ClassificationOverviewElements,
  onSelectLabel: (label: string) => void,
): ClassificationOverviewView => ({
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
      return { classifierId, label: selection.label };
    }

    const label = descriptor.labels.some(({ key }) => key === selection.label)
      ? selection.label
      : firstPopulatedLabel(descriptor, classifier);
    elements.empty.hidden = snapshot.transaction_count !== 0;
    elements.empty.textContent = "This snapshot contains an empty mempool.";
    const methodTitle = document.createElement("strong");
    methodTitle.textContent = `${methodologyText(descriptor.methodology)} · version ${descriptor.version}`;
    const methodDetail = document.createElement("span");
    methodDetail.textContent =
      descriptor.semantics === "multi_label"
        ? "Labels can overlap within this lens."
        : "This lens reports a specialized rule-set outcome.";
    elements.method.replaceChildren(methodTitle, methodDetail);

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
          onSelectLabel(descriptorLabel.key);
        });
      }
      button.setAttribute(
        "aria-pressed",
        String(descriptorLabel.key === label),
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
    elements.summary.textContent = `${countFormat.format(classifier.complete_count)} complete, ${countFormat.format(classifier.partial_count)} partial, and ${countFormat.format(classifier.unclassified_count)} unavailable results. Marginal label totals may overlap.`;
    return { classifierId, label };
  },
});
