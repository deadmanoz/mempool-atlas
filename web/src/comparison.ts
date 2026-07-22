// Comparison controller: ordered source selection, comparison polling, and DOM
// rendering for the multi-source workspace. Everything stays aggregate: the
// staged deltas are region summaries, and the only per-transaction data is the
// bounded `recent` list inside each rejection panel, which is the rejection
// surface's own inspector, kept small on purpose.
//
// The source order the user builds is an expected permissiveness hypothesis,
// least to most permissive. The server does not enforce it; it reports staged
// membership differences and flags where the expected nesting does not hold.

import { fetchComparison } from "./comparison-api";
import {
  ComparisonLifecycle,
  type ComparisonRequestTicket,
} from "./comparison-lifecycle";
import {
  rejectionAvailabilityMessage,
  rejectionVerdictSegments,
  rejectionReasonLabel,
  sharedView,
  stageView,
} from "./comparison-model";
import { fetchRejections } from "./rejections-api";
import { fetchSources } from "./summary-api";
import type {
  ComparisonSourceTotal,
  SourceComparison,
  SourceDescriptor,
  SourceRejections,
  TaxonomyDescriptor,
} from "./types";
import type { CompositionRow } from "./workbench-model";
import { formatCompact, formatCount } from "./workbench-model";

const POLL_INTERVAL_MS = 5_000;
const MIN_SOURCES = 2;
const MAX_SOURCES = 4;
/** Bounds each rejection panel's `recent` list. The window aggregate is bounded
 * server-side regardless, so this only limits the per-tx detail. */
const REJECTION_LIMIT = 6;

interface ComparisonElements {
  liveDot: HTMLElement;
  status: HTMLParagraphElement;
  available: HTMLElement;
  order: HTMLElement;
  clear: HTMLButtonElement;
  note: HTMLParagraphElement;
  metricTabs: HTMLElement;
  totals: HTMLElement;
  stages: HTMLElement;
  rejections: HTMLElement;
}

export interface ComparisonHandle {
  setActive: (active: boolean) => void;
}

interface ComparisonState {
  available: SourceDescriptor[];
  order: string[];
  metric: "count" | "vsize";
}

const requiredElement = <T extends Element>(id: string): T => {
  const element = document.getElementById(id);
  if (element === null) {
    throw new Error(`Missing required element #${id}`);
  }
  return element as unknown as T;
};

const comparisonElements = (): ComparisonElements => ({
  liveDot: requiredElement("compare-live-dot"),
  status: requiredElement("compare-status"),
  available: requiredElement("compare-available"),
  order: requiredElement("compare-order"),
  clear: requiredElement("compare-clear"),
  note: requiredElement("compare-note"),
  metricTabs: requiredElement("compare-metric-tabs"),
  totals: requiredElement("compare-totals"),
  stages: requiredElement("compare-stages"),
  rejections: requiredElement("compare-rejections"),
});

export const initComparison = (): ComparisonHandle => {
  const elements = comparisonElements();
  const state: ComparisonState = {
    available: [],
    order: [],
    metric: "vsize",
  };
  let active = false;
  let started = false;
  let latest: SourceComparison | null = null;
  let latestRejections = new Map<
    string,
    PromiseSettledResult<SourceRejections>
  >();
  let loading = false;
  let queued = false;
  const comparisonLifecycle = new ComparisonLifecycle();
  const rejectionLifecycle = new ComparisonLifecycle();
  let comparisonController: AbortController | null = null;
  let rejectionController: AbortController | null = null;

  const cancelComparisonLoad = (): void => {
    comparisonController?.abort();
    comparisonController = null;
  };

  const cancelRejectionLoads = (): void => {
    rejectionController?.abort();
    rejectionController = null;
  };

  const setStatus = (text: string, dataState: string): void => {
    elements.status.dataset.state = dataState;
    elements.status.textContent = text;
  };

  const tabButton = (
    label: string,
    isActive: boolean,
    onClick: () => void,
  ): HTMLButtonElement => {
    const button = document.createElement("button");
    button.type = "button";
    button.className = "tab";
    button.textContent = label;
    button.setAttribute("aria-pressed", String(isActive));
    if (isActive) {
      button.dataset.active = "true";
    }
    button.addEventListener("click", onClick);
    return button;
  };

  const iconButton = (
    label: string,
    title: string,
    disabled: boolean,
    onClick: () => void,
  ): HTMLButtonElement => {
    const button = document.createElement("button");
    button.type = "button";
    button.className = "order-control";
    button.textContent = label;
    button.title = title;
    button.setAttribute("aria-label", title);
    button.disabled = disabled;
    button.addEventListener("click", onClick);
    return button;
  };

  const addSource = (sourceId: string): void => {
    if (state.order.length >= MAX_SOURCES || state.order.includes(sourceId)) {
      return;
    }
    state.order.push(sourceId);
    selectionChanged();
  };

  const removeAt = (index: number): void => {
    state.order.splice(index, 1);
    selectionChanged();
  };

  const swap = (first: number, second: number): void => {
    const value = state.order[first];
    const other = state.order[second];
    if (value === undefined || other === undefined) {
      return;
    }
    state.order[first] = other;
    state.order[second] = value;
    selectionChanged();
  };

  const renderPicker = (): void => {
    const selected = new Set(state.order);
    const addable = state.available.filter(
      (source) => !selected.has(source.source_id),
    );
    const full = state.order.length >= MAX_SOURCES;

    if (addable.length === 0) {
      const note = document.createElement("span");
      note.className = "comp-unavailable";
      note.textContent =
        state.available.length === 0
          ? "No sources discovered yet."
          : "All discovered sources are in the comparison.";
      elements.available.replaceChildren(note);
    } else {
      elements.available.replaceChildren(
        ...addable.map((source) => {
          const chip = document.createElement("button");
          chip.type = "button";
          chip.className = "chip";
          chip.disabled = full;
          if (full) {
            chip.title = `A comparison holds at most ${MAX_SOURCES} sources`;
          }
          const label = document.createElement("span");
          label.textContent = source.source_id;
          const count = document.createElement("span");
          count.className = "chip-count mono";
          count.textContent = formatCompact(source.membership_count);
          chip.append(label, count);
          chip.addEventListener("click", () => {
            addSource(source.source_id);
          });
          return chip;
        }),
      );
    }

    elements.order.replaceChildren(
      ...state.order.map((sourceId, index) => {
        const chip = document.createElement("div");
        chip.className = "order-chip";
        const position = document.createElement("span");
        position.className = "order-index mono";
        position.textContent = String(index + 1);
        const name = document.createElement("span");
        name.className = "order-id";
        name.textContent = sourceId;
        const controls = document.createElement("span");
        controls.className = "order-controls";
        controls.append(
          iconButton("↑", `Move ${sourceId} earlier`, index === 0, () => {
            swap(index, index - 1);
          }),
          iconButton(
            "↓",
            `Move ${sourceId} later`,
            index === state.order.length - 1,
            () => {
              swap(index, index + 1);
            },
          ),
          iconButton("✕", `Remove ${sourceId}`, false, () => {
            removeAt(index);
          }),
        );
        chip.append(position, name, controls);
        return chip;
      }),
    );

    elements.clear.disabled = state.order.length === 0;
    elements.note.textContent =
      "Order is a least → most permissive hypothesis. Each stage shows transactions present on the right but absent on the left; that difference does not prove filtering, rejection, or relay causality.";
  };

  const renderMetricTabs = (): void => {
    elements.metricTabs.hidden = latest === null;
    if (latest === null) {
      elements.metricTabs.replaceChildren();
      return;
    }
    elements.metricTabs.replaceChildren(
      tabButton("by vsize", state.metric === "vsize", () => {
        state.metric = "vsize";
        render();
      }),
      tabButton("by count", state.metric === "count", () => {
        state.metric = "count";
        render();
      }),
    );
  };

  const awaitingPill = (count: number): HTMLElement | null => {
    if (count === 0) {
      return null;
    }
    const pill = document.createElement("span");
    pill.className = "awaiting-pill";
    const dot = document.createElement("i");
    dot.setAttribute("aria-hidden", "true");
    const label = document.createElement("span");
    label.textContent = `${formatCount(count)} awaiting RPC`;
    pill.append(dot, label);
    return pill;
  };

  const compositionRowElement = (row: CompositionRow): HTMLElement => {
    const container = document.createElement("div");
    container.className = "comp-row";
    const heading = document.createElement("div");
    heading.className = "comp-label";
    heading.textContent = row.label;
    container.append(heading);
    const bar = document.createElement("div");
    bar.className = "comp-bar";
    if (row.status === "unavailable") {
      bar.dataset.unavailable = "true";
      bar.setAttribute("role", "note");
      bar.setAttribute(
        "aria-label",
        `${row.label} composition unavailable: awaiting raw-transaction derivation`,
      );
      const note = document.createElement("span");
      note.className = "comp-unavailable";
      note.textContent = "awaiting raw-transaction derivation";
      bar.append(note);
    } else {
      bar.setAttribute("role", "list");
      bar.setAttribute(
        "aria-label",
        `${row.label} composition by ${state.metric === "count" ? "transaction count" : "virtual size"}`,
      );
      for (const segment of row.segments) {
        const piece = document.createElement("div");
        piece.style.width = `${(segment.fraction * 100).toFixed(2)}%`;
        piece.style.background = segment.color;
        const share = Math.round(segment.fraction * 100);
        const amount =
          state.metric === "count"
            ? `${formatCompact(segment.count)} tx`
            : `${formatCompact(segment.vsize)} vB`;
        piece.title =
          segment.key === "underived"
            ? `Underived: ${share}% · ${amount} (shape facts unavailable)`
            : `${segment.label}: ${share}% · ${amount}`;
        piece.setAttribute("role", "listitem");
        piece.setAttribute("aria-label", piece.title);
        piece.tabIndex = 0;
        bar.append(piece);
      }
    }
    container.append(bar);
    return container;
  };

  const compositionBlock = (rows: CompositionRow[]): HTMLElement => {
    const block = document.createElement("div");
    block.className = "composition stage-composition";
    block.append(...rows.map(compositionRowElement));
    return block;
  };

  const renderTotals = (comparison: SourceComparison): void => {
    const byId = new Map<string, ComparisonSourceTotal>(
      comparison.source_totals.map((total) => [total.source_id, total]),
    );
    const label = document.createElement("span");
    label.className = "totals-label";
    label.textContent = "Membership size";
    const pills = comparison.sources.flatMap((sourceId) => {
      const total = byId.get(sourceId);
      if (total === undefined) {
        return [];
      }
      const pill = document.createElement("span");
      pill.className = "total-pill";
      const name = document.createElement("strong");
      name.textContent = sourceId;
      const detail = document.createElement("span");
      detail.className = "mono";
      const awaiting =
        total.awaiting_rpc.count === 0
          ? ""
          : ` · ${formatCount(total.awaiting_rpc.count)} awaiting`;
      detail.textContent = `${formatCount(total.present.count)} tx · ${formatCompact(total.present.vsize)} vB${awaiting}`;
      pill.append(name, detail);
      return [pill];
    });
    elements.totals.replaceChildren(label, ...pills);
  };

  const stageHead = (title: string, magnitudeAwaiting: number): HTMLElement => {
    const head = document.createElement("div");
    head.className = "stage-head";
    const heading = document.createElement("h3");
    heading.textContent = title;
    head.append(heading);
    const pill = awaitingPill(magnitudeAwaiting);
    if (pill !== null) {
      head.append(pill);
    }
    return head;
  };

  const renderStages = (comparison: SourceComparison): void => {
    const cards: HTMLElement[] = [];

    const shared = sharedView(
      comparison.shared,
      comparison.sources.length,
      state.metric,
    );
    const sharedCard = document.createElement("div");
    sharedCard.className = "stage-card stage-shared";
    const sharedHeadline = document.createElement("p");
    sharedHeadline.className = "stage-headline";
    sharedHeadline.textContent = shared.headline;
    sharedCard.append(
      stageHead("Shared by all", shared.magnitude.awaitingCount),
      sharedHeadline,
      compositionBlock(shared.rows),
    );
    cards.push(sharedCard);

    for (const stage of comparison.stages) {
      const view = stageView(stage, state.metric);
      const card = document.createElement("div");
      card.className = "stage-card";
      const headline = document.createElement("p");
      headline.className = "stage-headline";
      headline.textContent = view.headline;

      const anomaly = document.createElement("p");
      anomaly.className = "stage-anomaly";
      anomaly.dataset.present = String(view.anomaly.present);
      const icon = document.createElement("span");
      icon.className = "anomaly-icon";
      icon.setAttribute("aria-hidden", "true");
      icon.textContent = view.anomaly.present ? "⚠" : "✓";
      const anomalyText = document.createElement("span");
      let note = view.anomaly.note;
      if (view.anomaly.present && view.anomaly.vsize > 0) {
        note += ` · ${formatCompact(view.anomaly.vsize)} vB`;
      }
      if (view.anomaly.present && view.anomaly.awaitingCount > 0) {
        note += ` · ${formatCount(view.anomaly.awaitingCount)} awaiting RPC`;
      }
      anomalyText.textContent = note;
      anomaly.append(icon, anomalyText);

      card.append(
        stageHead(`${stage.from} → ${stage.to}`, view.magnitude.awaitingCount),
        headline,
        compositionBlock(view.rows),
        anomaly,
      );
      cards.push(card);
    }

    elements.stages.replaceChildren(...cards);
  };

  const rejectionPanel = (
    sourceId: string,
    result: PromiseSettledResult<SourceRejections> | undefined,
    catalog: TaxonomyDescriptor[],
  ): HTMLElement => {
    const panel = document.createElement("div");
    panel.className = "rejection-panel";
    const head = document.createElement("div");
    head.className = "rejection-head";
    const name = document.createElement("h4");
    name.textContent = sourceId;
    head.append(name);
    panel.append(head);

    if (result === undefined) {
      const note = document.createElement("p");
      note.className = "rejection-note";
      note.textContent = "Loading rejections…";
      panel.append(note);
      return panel;
    }
    if (result.status === "rejected") {
      const note = document.createElement("p");
      note.className = "rejection-note rejection-error";
      note.textContent =
        result.reason instanceof Error
          ? result.reason.message
          : "Unable to load rejections";
      panel.append(note);
      return panel;
    }

    const rejections = result.value;
    const unavailableMessage = rejectionAvailabilityMessage(
      rejections.availability,
    );
    if (unavailableMessage !== null) {
      const note = document.createElement("p");
      note.className = "rejection-note";
      note.textContent = unavailableMessage;
      panel.append(note);
      return panel;
    }
    const count = document.createElement("span");
    count.className = "rejection-count mono";
    count.textContent = formatCount(rejections.window.count);
    head.append(count);

    const summary = document.createElement("p");
    summary.className = "rejection-note";
    if (rejections.window.count === 0) {
      summary.textContent =
        "No rejections recorded in the window. Absence is not acceptance.";
      panel.append(summary);
      return panel;
    }
    const oldest = rejections.window.oldest_at_ms;
    const newest = rejections.window.newest_at_ms;
    summary.textContent =
      oldest !== undefined && newest !== undefined
        ? `${formatCount(rejections.window.count)} refused, ${new Date(oldest).toLocaleTimeString()}–${new Date(newest).toLocaleTimeString()}.`
        : `${formatCount(rejections.window.count)} refused in the window.`;
    panel.append(summary);

    // Classified / unclassified attribution split.
    const attribution = rejections.attribution;
    const attributed =
      attribution.classified_count + attribution.unclassified_count;
    const split = document.createElement("div");
    split.className = "attribution";
    const splitBar = document.createElement("div");
    splitBar.className = "attribution-bar";
    const classifiedShare =
      attributed === 0 ? 0 : attribution.classified_count / attributed;
    const classifiedPiece = document.createElement("div");
    classifiedPiece.className = "attribution-classified";
    classifiedPiece.style.width = `${(classifiedShare * 100).toFixed(1)}%`;
    const unclassifiedPiece = document.createElement("div");
    unclassifiedPiece.className = "attribution-unclassified";
    unclassifiedPiece.style.width = `${((1 - classifiedShare) * 100).toFixed(1)}%`;
    splitBar.append(classifiedPiece, unclassifiedPiece);
    const splitLabel = document.createElement("p");
    splitLabel.className = "attribution-label";
    splitLabel.textContent = `${formatCount(attribution.classified_count)} classified · ${formatCount(attribution.unclassified_count)} unclassified (no derived verdicts)`;
    split.append(splitBar, splitLabel);
    panel.append(split);

    // Top reasons.
    if (rejections.by_reason.length > 0) {
      const reasons = document.createElement("ul");
      reasons.className = "rejection-reasons";
      for (const reason of rejections.by_reason) {
        const item = document.createElement("li");
        const text = document.createElement("span");
        text.className = "reason-text";
        text.textContent = rejectionReasonLabel(reason);
        const value = document.createElement("span");
        value.className = "reason-count mono";
        value.textContent = formatCount(reason.count);
        item.append(text, value);
        reasons.append(item);
      }
      panel.append(reasons);
    }

    // Per-taxonomy verdict breakdown over classified rejections.
    for (const breakdown of attribution.taxonomies) {
      const segments = rejectionVerdictSegments(breakdown, catalog);
      if (segments.length === 0) {
        continue;
      }
      const group = document.createElement("div");
      group.className = "verdict-group";
      const heading = document.createElement("span");
      heading.className = "verdict-heading";
      heading.textContent = breakdown.label;
      const chips = document.createElement("div");
      chips.className = "verdict-chips";
      for (const segment of segments) {
        const chip = document.createElement("span");
        chip.className = "verdict-chip";
        const dot = document.createElement("i");
        dot.style.background = segment.color;
        const label = document.createElement("span");
        label.textContent = segment.label;
        const value = document.createElement("span");
        value.className = "verdict-count mono";
        value.textContent = formatCount(segment.count);
        chip.append(dot, label, value);
        chips.append(chip);
      }
      group.append(heading, chips);
      panel.append(group);
    }

    // Bounded recent detail; the rejection surface is the per-tx inspector.
    if (rejections.recent.length > 0) {
      const detail = document.createElement("details");
      detail.className = "rejection-recent";
      const summaryEl = document.createElement("summary");
      summaryEl.textContent = `Recent (${rejections.recent.length})`;
      detail.append(summaryEl);
      const list = document.createElement("ul");
      for (const record of rejections.recent.slice(0, REJECTION_LIMIT)) {
        const item = document.createElement("li");
        const txid = document.createElement("code");
        txid.textContent = record.txid;
        txid.title = record.txid;
        const reason = document.createElement("span");
        reason.className = "recent-reason";
        reason.textContent = record.reason;
        const observed = document.createElement("time");
        const observedDate = new Date(record.observed_at_ms);
        observed.className = "recent-observed";
        observed.dateTime = observedDate.toISOString();
        observed.textContent = observedDate.toLocaleString();
        item.append(txid, reason, observed);
        list.append(item);
      }
      detail.append(list);
      panel.append(detail);
    }

    return panel;
  };

  const renderRejections = (comparison: SourceComparison): void => {
    const catalog = comparison.shared.bins.taxonomies;
    elements.rejections.replaceChildren(
      ...comparison.sources.map((sourceId) =>
        rejectionPanel(sourceId, latestRejections.get(sourceId), catalog),
      ),
    );
  };

  const renderIdle = (): void => {
    elements.liveDot.dataset.state = "idle";
    setStatus(
      state.order.length === 0
        ? "Pick 2–4 sources to compare. The order is an expected least-to-most permissive hypothesis."
        : `Pick ${MIN_SOURCES - state.order.length} more source to compare.`,
      "ready",
    );
    renderMetricTabs();
    elements.totals.replaceChildren();
    elements.stages.replaceChildren();
    elements.rejections.replaceChildren();
  };

  const renderPending = (): void => {
    elements.liveDot.dataset.state = "idle";
    setStatus("Loading comparison…", "loading");
    renderMetricTabs();
    elements.totals.replaceChildren();
    elements.stages.replaceChildren();
    elements.rejections.replaceChildren();
  };

  const selectionChanged = (): void => {
    comparisonLifecycle.invalidate();
    rejectionLifecycle.invalidate();
    cancelComparisonLoad();
    cancelRejectionLoads();
    latest = null;
    latestRejections = new Map();
    renderPicker();
    if (state.order.length >= MIN_SOURCES) {
      renderPending();
      void load();
    } else {
      renderIdle();
    }
  };

  const render = (): void => {
    if (latest === null) {
      renderIdle();
      return;
    }
    const comparison = latest;
    renderMetricTabs();
    setStatus(
      `Computed at ${new Date(comparison.as_of_ms).toLocaleTimeString()} · auto ${POLL_INTERVAL_MS / 1_000}s · ${comparison.sources.length} sources in caller order.`,
      "ready",
    );
    renderTotals(comparison);
    renderStages(comparison);
    renderRejections(comparison);
  };

  const loadRejections = (
    ticket: ComparisonRequestTicket,
    comparison: SourceComparison,
  ): void => {
    cancelRejectionLoads();
    const controller = new AbortController();
    rejectionController = controller;
    let pending = ticket.sources.length;
    for (const sourceId of ticket.sources) {
      void fetchRejections(sourceId, REJECTION_LIMIT, controller.signal)
        .then((value) => {
          if (
            !rejectionLifecycle.isCurrent(ticket, state.order) ||
            latest !== comparison
          ) {
            return;
          }
          latestRejections.set(sourceId, { status: "fulfilled", value });
          renderRejections(comparison);
        })
        .catch((reason: unknown) => {
          if (
            !rejectionLifecycle.isCurrent(ticket, state.order) ||
            latest !== comparison
          ) {
            return;
          }
          latestRejections.set(sourceId, { status: "rejected", reason });
          renderRejections(comparison);
        })
        .finally(() => {
          pending -= 1;
          if (pending === 0 && rejectionController === controller) {
            rejectionController = null;
          }
        });
    }
  };

  const load = async (): Promise<void> => {
    if (!active || state.order.length < MIN_SOURCES) {
      renderIdle();
      return;
    }
    if (loading) {
      queued = true;
      return;
    }
    loading = true;
    const requested = [...state.order];
    const ticket = comparisonLifecycle.begin(requested);
    const controller = new AbortController();
    comparisonController = controller;
    try {
      const comparison = await fetchComparison(requested, controller.signal);
      if (!comparisonLifecycle.isCurrent(ticket, state.order)) {
        return;
      }
      latest = comparison;
      latestRejections = new Map();
      elements.liveDot.dataset.state = "live";
      render();
      const rejectionTicket = rejectionLifecycle.begin(requested);
      loadRejections(rejectionTicket, comparison);
    } catch (error) {
      if (!comparisonLifecycle.isCurrent(ticket, state.order)) {
        return;
      }
      elements.liveDot.dataset.state = "error";
      const detail =
        error instanceof Error ? error.message : "Unable to load comparison";
      setStatus(
        latest === null
          ? detail
          : `${detail}. Showing the last successful comparison from ${new Date(latest.as_of_ms).toLocaleTimeString()}.`,
        "error",
      );
    } finally {
      if (comparisonController === controller) {
        comparisonController = null;
      }
      loading = false;
      if (queued) {
        queued = false;
        void load();
      }
    }
  };

  const applyPreset = (sources: SourceDescriptor[]): void => {
    const known = new Set(sources.map((source) => source.source_id));
    const fromParam = new URL(window.location.href).searchParams.get("compare");
    const fromEnv: unknown = import.meta.env.VITE_ATLAS_FORK_PRESET;
    const preset =
      typeof fromParam === "string"
        ? fromParam
        : typeof fromEnv === "string"
          ? fromEnv
          : "";
    const raw = preset
      .split(",")
      .map((value) => value.trim())
      .filter((value) => value.length > 0);
    const unique = [...new Set(raw)];
    if (
      unique.length >= MIN_SOURCES &&
      unique.length <= MAX_SOURCES &&
      unique.every((sourceId) => known.has(sourceId))
    ) {
      state.order = unique;
    }
  };

  const start = async (): Promise<void> => {
    setStatus("Discovering sources…", "loading");
    try {
      const sources = await fetchSources();
      state.available = sources;
      if (sources.length === 0) {
        setStatus(
          "No sources have delivered evidence yet. Start an agent or run `just seed-dev` against the dev server.",
          "error",
        );
        renderPicker();
        return;
      }
      applyPreset(sources);
      renderPicker();
      if (state.order.length >= MIN_SOURCES) {
        await load();
      } else {
        renderIdle();
      }
    } catch (error) {
      setStatus(
        error instanceof Error ? error.message : "Unable to discover sources",
        "error",
      );
    }
  };

  elements.clear.addEventListener("click", () => {
    state.order = [];
    selectionChanged();
  });

  window.setInterval(() => {
    if (active && !document.hidden && state.order.length >= MIN_SOURCES) {
      void load();
    }
  }, POLL_INTERVAL_MS);

  renderPicker();
  renderIdle();

  return {
    setActive: (next: boolean): void => {
      active = next;
      if (!active) {
        comparisonLifecycle.invalidate();
        rejectionLifecycle.invalidate();
        cancelComparisonLoad();
        cancelRejectionLoads();
        return;
      }
      if (active && !started) {
        started = true;
        void start();
      } else if (state.order.length >= MIN_SOURCES) {
        void load();
      }
    },
  };
};
