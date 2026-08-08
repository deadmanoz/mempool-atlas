import {
  hitTestBucketTerrain,
  renderBucketTerrain,
  type BucketTerrainLayout,
  type BucketTerrainSectionGroup,
} from "./bucket-terrain";
import {
  classifierLabelQueryPopulation,
  type ClassifierLabelMatchMode,
  type ClassifierLabelQueryPopulation,
} from "./classifier-terrain";
import { countFormat, formatVsize, percentageFormat } from "./format";
import { TerrainSelectionView } from "./terrain-selection-view";
import type {
  ClassifierDescriptor,
  MempoolSnapshot,
  MempoolTransaction,
} from "./types";

type QuerySectionKey = "complete" | "partial";
type QueryRegionKey = "complete:matching" | "partial:matching";

interface QuerySignature {
  state: QuerySectionKey;
}

type QueryLayout = BucketTerrainLayout<
  QuerySectionKey,
  QueryRegionKey,
  QuerySignature
>;

interface ClassificationQueryElements {
  root: HTMLElement;
  stage: HTMLElement;
  canvas: HTMLCanvasElement;
  selectionMarker: HTMLElement;
  regions: HTMLElement;
  summary: HTMLElement;
  empty: HTMLElement;
  hint: HTMLElement;
  activeOption: HTMLElement;
  navigationStatus: HTMLElement;
}

interface ClassificationQueryRenderInput {
  snapshot: MempoolSnapshot;
  descriptor: ClassifierDescriptor;
  labelKeys: readonly string[];
  matchMode: ClassifierLabelMatchMode;
  metric: "count" | "vsize";
  selectedTxid: string | null;
}

interface ClassificationQueryView {
  render(input: ClassificationQueryRenderInput): void;
  reset(message: string): void;
  paintSelection(txid: string | null): boolean;
}

const QUERY_COLORS: Record<QuerySectionKey, string> = {
  complete: "#53d9d4",
  partial: "#e1aa4b",
};

const queryGroups = (
  population: ClassifierLabelQueryPopulation,
): BucketTerrainSectionGroup<
  QuerySectionKey,
  QueryRegionKey,
  QuerySignature
>[] => {
  const groups: BucketTerrainSectionGroup<
    QuerySectionKey,
    QueryRegionKey,
    QuerySignature
  >[] = [];
  const append = (
    state: QuerySectionKey,
    transactions: MempoolTransaction[],
  ): void => {
    if (transactions.length === 0) return;
    groups.push({
      key: state,
      transactions,
      nested: false,
      regions: [
        {
          key: `${state}:matching`,
          sectionKey: state,
          signature: { state },
          transactions,
        },
      ],
    });
  };
  append("complete", population.completeTransactions);
  append("partial", population.partialTransactions);
  return groups;
};

const queryPopulationKey = (input: ClassificationQueryRenderInput): string =>
  [
    input.snapshot.source_id,
    input.snapshot.observed_at_ms,
    input.snapshot.classification_revision,
    input.descriptor.id,
    input.matchMode,
    input.labelKeys.join(","),
  ].join(":");

const queryLayoutKey = (input: ClassificationQueryRenderInput): string =>
  `${queryPopulationKey(input)}:${input.metric}`;

const labelNames = (
  descriptor: ClassifierDescriptor,
  labelKeys: readonly string[],
): string[] => {
  const selected = new Set(labelKeys);
  return descriptor.labels.flatMap(({ key, label }) =>
    selected.has(key) ? [label] : [],
  );
};

const queryDescription = (
  descriptor: ClassifierDescriptor,
  population: ClassifierLabelQueryPopulation,
): string => {
  const names = labelNames(descriptor, population.labelKeys);
  if (names.length === 0) return "No classifier labels selected.";
  const connective = population.matchMode === "all" ? " and " : " or ";
  return `${countFormat.format(population.count)} transactions match ${names.join(connective)}. ${countFormat.format(population.completeTransactions.length)} complete and ${countFormat.format(population.partialTransactions.length)} partial results. ${formatVsize(population.vsize)}, ${percentageFormat.format(population.totalShare)} of the snapshot.`;
};

const cursorMove = (
  key: string,
  current: number,
  length: number,
): number | null => {
  if (length === 0) return null;
  const last = length - 1;
  const safe = Math.min(last, Math.max(0, current));
  switch (key) {
    case "ArrowRight":
    case "ArrowDown":
      return Math.min(last, safe + 1);
    case "ArrowLeft":
    case "ArrowUp":
      return Math.max(0, safe - 1);
    case "Home":
      return 0;
    case "End":
      return last;
    case "PageDown":
      return Math.min(last, safe + 100);
    case "PageUp":
      return Math.max(0, safe - 100);
    default:
      return null;
  }
};

export const createClassificationQueryView = (
  elements: ClassificationQueryElements,
  onSelectTransaction: (transaction: MempoolTransaction) => void,
): ClassificationQueryView => {
  const selectionView = new TerrainSelectionView(
    elements.canvas,
    elements.selectionMarker,
  );
  let input: ClassificationQueryRenderInput | null = null;
  let population: ClassifierLabelQueryPopulation | null = null;
  let layout: QueryLayout | null = null;
  let populationKey: string | null = null;
  let layoutKey: string | null = null;
  let frame: number | null = null;
  let pendingClick: { x: number; y: number } | null = null;
  let keyboardIndex = 0;

  const focusedTxid = (): string | null =>
    document.activeElement === elements.stage
      ? (population?.transactions[keyboardIndex]?.txid ?? null)
      : (input?.selectedTxid ?? null);

  const paintSelection = (txid: string | null): boolean =>
    layout !== null && selectionView.paint(layout, txid);

  const updateActiveOption = (): void => {
    const transactions = population?.transactions ?? [];
    if (transactions.length === 0) {
      elements.activeOption.textContent = "No matching transactions";
      elements.activeOption.removeAttribute("aria-posinset");
      elements.activeOption.removeAttribute("aria-setsize");
      elements.navigationStatus.textContent = "No matching transactions.";
      return;
    }
    keyboardIndex = Math.min(
      transactions.length - 1,
      Math.max(0, keyboardIndex),
    );
    const transaction = transactions[keyboardIndex]!;
    const position = keyboardIndex + 1;
    elements.activeOption.textContent = transaction.txid;
    elements.activeOption.setAttribute("aria-posinset", String(position));
    elements.activeOption.setAttribute(
      "aria-setsize",
      String(transactions.length),
    );
    elements.activeOption.setAttribute(
      "aria-label",
      `${transaction.txid}, transaction ${position} of ${transactions.length}, ${formatVsize(transaction.vsize)}.`,
    );
    elements.activeOption.setAttribute(
      "aria-selected",
      String(transaction.txid === input?.selectedTxid),
    );
    elements.navigationStatus.textContent = `Transaction ${countFormat.format(position)} of ${countFormat.format(transactions.length)}. ${transaction.txid}.`;
  };

  const renderRegions = (nextLayout: QueryLayout): void => {
    elements.regions.replaceChildren(
      ...nextLayout.sections.map((section) => {
        const label = document.createElement("div");
        label.className = `terrain-section-label query-section-label ${section.key}`;
        label.style.left = `${(section.rect.x / nextLayout.width) * 100}%`;
        label.style.top = `${(section.rect.y / nextLayout.height) * 100}%`;
        label.style.width = `${(section.rect.width / nextLayout.width) * 100}%`;
        label.style.height = `${section.labelHeight}px`;
        const name = document.createElement("strong");
        name.textContent =
          section.key === "complete" ? "Complete matches" : "Partial matches";
        const count = document.createElement("span");
        count.textContent = countFormat.format(section.transactionCount);
        label.append(name, count);
        label.title = `${name.textContent}: ${countFormat.format(section.transactionCount)} transactions, ${formatVsize(section.totalVsize)}`;
        return label;
      }),
    );
  };

  const renderFrame = (): void => {
    frame = null;
    if (input === null || population === null || population.count === 0) {
      layout = null;
      selectionView.reset();
      elements.regions.replaceChildren();
      return;
    }
    layout = renderBucketTerrain(
      elements.canvas,
      queryGroups(population),
      input.metric,
      {
        color: ({ signature }) => QUERY_COLORS[signature.state],
        selected: () => true,
        partial: ({ signature }) => signature.state === "partial",
        glyphOpacity: () => 1,
        glyphOpacityByRegion: () => 1,
        rasterStyleKey: `classification-query:${input.descriptor.id}`,
      },
      layout,
      null,
    );
    selectionView.capture(layout);
    paintSelection(focusedTxid());
    renderRegions(layout);
    if (pendingClick !== null) {
      const click = pendingClick;
      pendingClick = null;
      selectTransactionAt(click.x, click.y);
    }
  };

  const schedule = (): void => {
    if (frame !== null) window.cancelAnimationFrame(frame);
    frame = window.requestAnimationFrame(renderFrame);
  };

  const reset = (message: string): void => {
    input = null;
    population = null;
    layout = null;
    populationKey = null;
    layoutKey = null;
    keyboardIndex = 0;
    pendingClick = null;
    selectionView.reset();
    elements.stage.hidden = true;
    elements.hint.hidden = true;
    elements.empty.hidden = false;
    elements.empty.textContent = message;
    elements.summary.textContent = "No label query selected.";
    elements.regions.replaceChildren();
    if (frame !== null) {
      window.cancelAnimationFrame(frame);
      frame = null;
    }
  };

  const render = (next: ClassificationQueryRenderInput): void => {
    const nextPopulationKey = queryPopulationKey(next);
    const nextLayoutKey = queryLayoutKey(next);
    if (populationKey !== nextPopulationKey) {
      populationKey = nextPopulationKey;
      population = classifierLabelQueryPopulation(
        next.snapshot.transactions,
        next.descriptor,
        next.labelKeys,
        next.matchMode,
      );
    }
    if (layoutKey !== nextLayoutKey) {
      layoutKey = nextLayoutKey;
      layout = null;
      selectionView.reset();
    }
    input = next;
    if (population === null) {
      throw new Error("Classification query population was not prepared");
    }
    const selectedIndex = population.transactions.findIndex(
      ({ txid }) => txid === next.selectedTxid,
    );
    keyboardIndex = selectedIndex >= 0 ? selectedIndex : 0;

    elements.summary.textContent = queryDescription(
      next.descriptor,
      population,
    );
    elements.root.dataset.state =
      population.labelKeys.length === 0
        ? "idle"
        : population.count === 0
          ? "empty"
          : "ready";
    if (population.labelKeys.length === 0) {
      elements.stage.hidden = true;
      elements.hint.hidden = true;
      elements.empty.hidden = false;
      elements.empty.textContent =
        "Choose one or more labels above to reveal their transactions.";
      layout = null;
      selectionView.reset();
      elements.regions.replaceChildren();
    } else if (population.count === 0) {
      elements.stage.hidden = true;
      elements.hint.hidden = true;
      elements.empty.hidden = false;
      elements.empty.textContent =
        next.matchMode === "all"
          ? "No transaction carries every selected label. Remove a label or switch to ANY."
          : "No transactions carry any of the selected labels.";
      layout = null;
      selectionView.reset();
      elements.regions.replaceChildren();
    } else {
      elements.stage.hidden = false;
      elements.hint.hidden = false;
      elements.empty.hidden = true;
      elements.stage.setAttribute(
        "aria-label",
        `${queryDescription(next.descriptor, population)} Use the arrow keys to move through matching transactions and press Enter to inspect one.`,
      );
      schedule();
    }
    updateActiveOption();
  };

  const selectTransactionAt = (x: number, y: number): void => {
    if (layout === null || population === null) return;
    const hit = hitTestBucketTerrain(layout, x, y);
    if (hit?.kind !== "transaction") return;
    const index = population.transactions.findIndex(
      ({ txid }) => txid === hit.glyph.txid,
    );
    if (index < 0) return;
    keyboardIndex = index;
    updateActiveOption();
    const transaction = population.transactions[index];
    if (transaction !== undefined) onSelectTransaction(transaction);
  };

  elements.stage.addEventListener("click", (event) => {
    const bounds = elements.canvas.getBoundingClientRect();
    const click = {
      x: event.clientX - bounds.left,
      y: event.clientY - bounds.top,
    };
    if (layout === null) {
      pendingClick = click;
      return;
    }
    selectTransactionAt(click.x, click.y);
  });

  elements.stage.addEventListener("pointermove", (event) => {
    if (layout === null) return;
    const bounds = elements.canvas.getBoundingClientRect();
    const hit = hitTestBucketTerrain(
      layout,
      event.clientX - bounds.left,
      event.clientY - bounds.top,
    );
    if (hit?.kind !== "transaction") {
      elements.stage.removeAttribute("title");
      return;
    }
    elements.stage.title = `${hit.glyph.txid} · ${formatVsize(hit.glyph.vsize)}`;
  });

  elements.stage.addEventListener("pointerleave", () => {
    elements.stage.removeAttribute("title");
  });

  elements.stage.addEventListener("focus", () => {
    updateActiveOption();
    paintSelection(focusedTxid());
  });

  elements.stage.addEventListener("blur", () => {
    paintSelection(input?.selectedTxid ?? null);
  });

  elements.stage.addEventListener("keydown", (event) => {
    const transactions = population?.transactions ?? [];
    const nextIndex = cursorMove(event.key, keyboardIndex, transactions.length);
    if (nextIndex !== null) {
      event.preventDefault();
      keyboardIndex = nextIndex;
      updateActiveOption();
      paintSelection(focusedTxid());
      return;
    }
    if (event.key !== "Enter" && event.key !== " ") return;
    const transaction = transactions[keyboardIndex];
    if (transaction === undefined) return;
    event.preventDefault();
    onSelectTransaction(transaction);
  });

  new ResizeObserver(() => {
    layout = null;
    selectionView.reset();
    if (!elements.stage.hidden) schedule();
  }).observe(elements.canvas);

  return { render, reset, paintSelection };
};
