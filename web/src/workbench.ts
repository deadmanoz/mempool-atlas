// Workbench controller: source discovery, summary polling, filter state, and
// DOM rendering for the aggregate views. All data arrives pre-binned from the
// summary endpoint; nothing here ever holds per-transaction rows.

import { fetchSources, fetchSummary } from "./summary-api";
import type {
  CaptureStatus,
  ClassificationKey,
  MempoolSummary,
  ScriptTypeKey,
  SummaryDimension,
  SummaryFilter,
} from "./types";
import {
  CLASSIFICATION_META,
  SCRIPT_META,
  compositionRows,
  dimensionBinMeta,
  ecdfPaths,
  formatCompact,
  formatCount,
  jointViewModel,
  ramp,
  squarify,
} from "./workbench-model";

const POLL_INTERVAL_MS = 5_000;
const TREEMAP_GROUPS: readonly SummaryDimension[] = [
  "classification",
  "script",
  "value",
];

interface WorkbenchElements {
  sourceSelect: HTMLSelectElement;
  liveDot: HTMLElement;
  status: HTMLParagraphElement;
  banner: HTMLParagraphElement;
  metricTotal: HTMLElement;
  metricMatching: HTMLElement;
  metricShare: HTMLElement;
  metricAwaiting: HTMLElement;
  reset: HTMLButtonElement;
  classChips: HTMLElement;
  scriptChips: HTMLElement;
  sidebarNote: HTMLParagraphElement;
  primaryTabs: HTMLElement;
  groupTabs: HTMLElement;
  metricTabs: HTMLElement;
  treemap: HTMLElement;
  composition: HTMLElement;
  ecdf: SVGSVGElement;
  ecdfAxis: HTMLElement;
  jointTop: HTMLElement;
  jointCells: HTMLElement;
  jointRight: HTMLElement;
  jointAxis: HTMLElement;
}

export interface WorkbenchHandle {
  getSelectedSource: () => string | null;
  onSourceChange: (listener: (sourceId: string) => void) => void;
}

interface WorkbenchState {
  source: string | null;
  classes: Set<ClassificationKey>;
  scripts: Set<ScriptTypeKey>;
  primaryView: "composition" | "treemap";
  treeGroup: SummaryDimension;
  compMetric: "count" | "vsize";
}

const requiredElement = <T extends Element>(id: string): T => {
  const element = document.getElementById(id);
  if (element === null) {
    throw new Error(`Missing required element #${id}`);
  }
  return element as unknown as T;
};

const workbenchElements = (): WorkbenchElements => ({
  sourceSelect: requiredElement("source-select"),
  liveDot: requiredElement("live-dot"),
  status: requiredElement("workbench-status"),
  banner: requiredElement("workbench-banner"),
  metricTotal: requiredElement("metric-total"),
  metricMatching: requiredElement("metric-matching"),
  metricShare: requiredElement("metric-share"),
  metricAwaiting: requiredElement("metric-awaiting"),
  reset: requiredElement("workbench-reset"),
  classChips: requiredElement("class-chips"),
  scriptChips: requiredElement("script-chips"),
  sidebarNote: requiredElement("sidebar-note"),
  primaryTabs: requiredElement("primary-tabs"),
  groupTabs: requiredElement("group-tabs"),
  metricTabs: requiredElement("metric-tabs"),
  treemap: requiredElement("treemap"),
  composition: requiredElement("composition"),
  ecdf: requiredElement("ecdf"),
  ecdfAxis: requiredElement("ecdf-axis"),
  jointTop: requiredElement("joint-top"),
  jointCells: requiredElement("joint-cells"),
  jointRight: requiredElement("joint-right"),
  jointAxis: requiredElement("joint-axis"),
});

const stateFilter = (state: WorkbenchState): SummaryFilter => {
  const filter: SummaryFilter = {};
  if (state.classes.size > 0) {
    filter.classes = [...state.classes];
  }
  if (state.scripts.size > 0) {
    filter.scripts = [...state.scripts];
  }
  return filter;
};

const renderBanner = (
  banner: HTMLParagraphElement,
  sourceId: string,
  capture: CaptureStatus,
): void => {
  if (capture.status === "no_reported_gaps") {
    banner.hidden = true;
    return;
  }
  const since = new Date(capture.first_gap_at_ms).toLocaleString();
  banner.hidden = false;
  banner.dataset.certainty = capture.strongest_certainty;
  banner.textContent =
    `${capture.strongest_certainty === "known_loss" ? "Known" : "Possible"} evidence gap for ${sourceId} since ${since} ` +
    `(${capture.latest_input}: ${capture.latest_reason}). One node's view — absence is not rejection.`;
};

export const initWorkbench = (): WorkbenchHandle => {
  const elements = workbenchElements();
  const state: WorkbenchState = {
    source: null,
    classes: new Set(),
    scripts: new Set(),
    primaryView: "composition",
    treeGroup: "classification",
    compMetric: "vsize",
  };
  const sourceListeners: ((sourceId: string) => void)[] = [];
  let latest: MempoolSummary | null = null;
  let loading = false;
  let queued = false;

  const setSource = (sourceId: string): void => {
    if (state.source === sourceId) {
      return;
    }
    state.source = sourceId;
    elements.sourceSelect.value = sourceId;
    for (const listener of sourceListeners) {
      listener(sourceId);
    }
    void load();
  };

  const tabButton = (
    label: string,
    active: boolean,
    onClick: () => void,
    disabled = false,
    title?: string,
  ): HTMLButtonElement => {
    const button = document.createElement("button");
    button.type = "button";
    button.className = "tab";
    button.textContent = label;
    button.disabled = disabled;
    if (title !== undefined) {
      button.title = title;
    }
    if (active) {
      button.dataset.active = "true";
    }
    button.addEventListener("click", onClick);
    return button;
  };

  const renderTabs = (summary: MempoolSummary): void => {
    elements.primaryTabs.replaceChildren(
      tabButton("Composition", state.primaryView === "composition", () => {
        state.primaryView = "composition";
        render();
      }),
      tabButton("Treemap", state.primaryView === "treemap", () => {
        state.primaryView = "treemap";
        render();
      }),
    );

    elements.groupTabs.hidden = state.primaryView !== "treemap";
    elements.groupTabs.replaceChildren(
      ...TREEMAP_GROUPS.map((dimension) => {
        const available = summary.histograms[dimension].status === "available";
        return tabButton(
          dimension === "classification"
            ? "Class"
            : dimension === "script"
              ? "Script"
              : "Value",
          state.treeGroup === dimension,
          () => {
            state.treeGroup = dimension;
            render();
          },
          !available,
          available ? undefined : "Awaiting raw-transaction derivation",
        );
      }),
    );

    elements.metricTabs.hidden = state.primaryView !== "composition";
    elements.metricTabs.replaceChildren(
      tabButton("by vsize", state.compMetric === "vsize", () => {
        state.compMetric = "vsize";
        render();
      }),
      tabButton("by count", state.compMetric === "count", () => {
        state.compMetric = "count";
        render();
      }),
    );
  };

  const renderChips = (summary: MempoolSummary): void => {
    const classification = summary.histograms.classification;
    const counts = new Map<string, number>();
    if (classification.status === "available") {
      summary.bins.classification_keys.forEach((key, index) => {
        counts.set(key, classification.bins[index]?.count ?? 0);
      });
    }
    elements.classChips.replaceChildren(
      ...summary.bins.classification_keys.map((key) => {
        const meta = CLASSIFICATION_META[key];
        const active = state.classes.has(key);
        const chip = document.createElement("button");
        chip.type = "button";
        chip.className = "chip chip-row";
        chip.dataset.active = String(active);
        chip.style.setProperty("--chip-color", meta.color);
        const dot = document.createElement("i");
        const label = document.createElement("span");
        label.textContent = meta.label;
        const count = document.createElement("span");
        count.className = "chip-count mono";
        count.textContent = formatCompact(counts.get(key) ?? 0);
        chip.append(dot, label, count);
        chip.addEventListener("click", () => {
          if (active) {
            state.classes.delete(key);
          } else {
            state.classes.add(key);
          }
          void load();
        });
        return chip;
      }),
    );

    const scriptsAvailable = summary.histograms.script.status === "available";
    elements.scriptChips.replaceChildren(
      ...summary.bins.script_keys.map((key) => {
        const meta = SCRIPT_META[key];
        const active = state.scripts.has(key);
        const chip = document.createElement("button");
        chip.type = "button";
        chip.className = "chip";
        chip.dataset.active = String(active);
        chip.style.setProperty("--chip-color", meta.color);
        chip.disabled = !scriptsAvailable;
        if (!scriptsAvailable) {
          chip.title = "Awaiting raw-transaction derivation";
        }
        const dot = document.createElement("i");
        const label = document.createElement("span");
        label.textContent = meta.label;
        chip.append(dot, label);
        chip.addEventListener("click", () => {
          if (active) {
            state.scripts.delete(key);
          } else {
            state.scripts.add(key);
          }
          void load();
        });
        return chip;
      }),
    );
    elements.sidebarNote.textContent = scriptsAvailable
      ? "Filters apply server-side before binning."
      : "Script, value, and input/output facets unlock once raw-transaction derivation lands. Filters apply server-side before binning.";
  };

  const renderTreemap = (summary: MempoolSummary): void => {
    const histogram = summary.histograms[state.treeGroup];
    if (histogram.status !== "available") {
      elements.treemap.replaceChildren();
      return;
    }
    const meta = dimensionBinMeta(state.treeGroup, summary.bins);
    const nodes = histogram.bins.flatMap((bin, index) =>
      bin.vsize === 0 || index >= meta.length
        ? []
        : [{ ...meta[index]!, count: bin.count, vsize: bin.vsize }],
    );
    if (histogram.underived.vsize > 0) {
      nodes.push({
        key: "underived",
        label: "Underived",
        color: "#38434f",
        count: histogram.underived.count,
        vsize: histogram.underived.vsize,
      });
    }
    const tiles = squarify(nodes, 1_000, 620);
    const total = nodes.reduce((sum, node) => sum + node.vsize, 0);
    elements.treemap.replaceChildren(
      ...tiles.map((tile) => {
        const element = document.createElement("div");
        element.className = "treemap-tile";
        element.style.left = `${(tile.x / 1_000) * 100}%`;
        element.style.top = `${(tile.y / 620) * 100}%`;
        element.style.width = `${(tile.width / 1_000) * 100}%`;
        element.style.height = `${(tile.height / 620) * 100}%`;
        element.style.background = tile.color;
        const share = total === 0 ? 0 : Math.round((tile.vsize / total) * 100);
        element.title = `${tile.label}: ${formatCount(tile.count)} tx · ${formatCompact(tile.vsize)} vB · ${share}%`;
        if (tile.width / 1_000 > 0.14 && tile.height / 620 > 0.09) {
          const label = document.createElement("span");
          label.className = "treemap-label";
          label.textContent = tile.label;
          const sub = document.createElement("span");
          sub.className = "treemap-sub mono";
          sub.textContent = `${formatCompact(tile.count)} · ${share}%`;
          element.append(label, sub);
        }
        return element;
      }),
    );
  };

  const renderComposition = (summary: MempoolSummary): void => {
    elements.composition.replaceChildren(
      ...compositionRows(summary, state.compMetric).map((row) => {
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
          const note = document.createElement("span");
          note.className = "comp-unavailable";
          note.textContent = "awaiting raw-transaction derivation";
          bar.append(note);
        } else {
          for (const segment of row.segments) {
            const piece = document.createElement("div");
            piece.style.width = `${(segment.fraction * 100).toFixed(2)}%`;
            piece.style.background = segment.color;
            const share = Math.round(segment.fraction * 100);
            const amount =
              state.compMetric === "count"
                ? `${formatCompact(segment.count)} tx`
                : `${formatCompact(segment.vsize)} vB`;
            piece.title =
              segment.key === "underived"
                ? `Underived: ${share}% · ${amount} (no raw-transaction evidence)`
                : `${segment.label}: ${share}% · ${amount}`;
            bar.append(piece);
          }
        }
        container.append(bar);
        return container;
      }),
    );
  };

  const renderEcdf = (summary: MempoolSummary): void => {
    const svgNamespace = "http://www.w3.org/2000/svg";
    const frame = [
      { x1: 32, y1: 8, x2: 464, y2: 8, strong: false },
      { x1: 32, y1: 83, x2: 464, y2: 83, strong: false },
      { x1: 32, y1: 158, x2: 464, y2: 158, strong: true },
    ].map((line) => {
      const element = document.createElementNS(svgNamespace, "line");
      element.setAttribute("x1", String(line.x1));
      element.setAttribute("y1", String(line.y1));
      element.setAttribute("x2", String(line.x2));
      element.setAttribute("y2", String(line.y2));
      element.setAttribute("stroke", line.strong ? "#324255" : "#1e2a38");
      return element;
    });
    const labels = [
      { x: 27, y: 11, text: "100" },
      { x: 27, y: 161, text: "0" },
    ].map((label) => {
      const element = document.createElementNS(svgNamespace, "text");
      element.setAttribute("x", String(label.x));
      element.setAttribute("y", String(label.y));
      element.setAttribute("text-anchor", "end");
      element.setAttribute("fill", "#8996a6");
      element.setAttribute("class", "ecdf-tick");
      element.textContent = label.text;
      return element;
    });
    const paths =
      summary.ecdf === undefined
        ? []
        : ecdfPaths(summary.ecdf, {
            left: 32,
            top: 8,
            width: 432,
            height: 150,
          }).map((path) => {
            const element = document.createElementNS(svgNamespace, "path");
            element.setAttribute("d", path.d);
            element.setAttribute("stroke", path.color);
            element.setAttribute("stroke-width", "2");
            element.setAttribute("fill", "none");
            element.setAttribute("stroke-linejoin", "round");
            return element;
          });
    elements.ecdf.replaceChildren(...frame, ...labels, ...paths);

    if (summary.ecdf !== undefined && summary.ecdf.fee_edges.length >= 2) {
      const edges = summary.ecdf.fee_edges;
      const middle = edges[Math.floor((edges.length - 1) / 2)]!;
      elements.ecdfAxis.replaceChildren(
        ...[
          `${edges[0]}`,
          `${Math.round(middle)}`,
          `${edges[edges.length - 1]}+`,
        ].map((text) => {
          const span = document.createElement("span");
          span.textContent = text;
          return span;
        }),
      );
    }
  };

  const renderJoint = (summary: MempoolSummary): void => {
    if (summary.joint_fee_size === undefined) {
      elements.jointCells.replaceChildren();
      elements.jointTop.replaceChildren();
      elements.jointRight.replaceChildren();
      return;
    }
    const view = jointViewModel(summary.joint_fee_size);
    elements.jointCells.style.gridTemplateColumns = `repeat(${view.feeBinCount}, 1fr)`;
    elements.jointCells.style.gridTemplateRows = `repeat(${view.sizeBinCount}, 1fr)`;
    elements.jointCells.style.aspectRatio = `${view.feeBinCount} / ${view.sizeBinCount}`;
    elements.jointCells.replaceChildren(
      ...view.rows.flat().map((intensity) => {
        const cell = document.createElement("div");
        cell.style.background =
          intensity === 0 ? "#0c141d" : ramp(0.08 + 0.92 * intensity);
        return cell;
      }),
    );
    elements.jointTop.replaceChildren(
      ...view.top.map((fraction) => {
        const bar = document.createElement("div");
        bar.style.height = `${(fraction * 100).toFixed(1)}%`;
        return bar;
      }),
    );
    elements.jointRight.replaceChildren(
      ...view.right.map((fraction) => {
        const bar = document.createElement("div");
        bar.style.width = `${(fraction * 100).toFixed(1)}%`;
        return bar;
      }),
    );
    const edges = summary.joint_fee_size.fee_edges;
    if (edges.length >= 2) {
      const middle = edges[Math.floor((edges.length - 1) / 2)]!;
      elements.jointAxis.replaceChildren(
        ...[
          `${edges[0]}`,
          `${Math.round(middle)}`,
          `${edges[edges.length - 1]}+`,
        ].map((text) => {
          const span = document.createElement("span");
          span.textContent = text;
          return span;
        }),
      );
    }
  };

  const render = (): void => {
    if (latest === null) {
      return;
    }
    const summary = latest;
    const total = summary.totals.all.count + summary.totals.awaiting_rpc.count;
    elements.metricTotal.textContent = formatCount(total);
    elements.metricMatching.textContent = formatCount(
      summary.totals.matching.count,
    );
    const share =
      summary.totals.all.vsize === 0
        ? 0
        : Math.round(
            (summary.totals.matching.vsize / summary.totals.all.vsize) * 100,
          );
    elements.metricShare.textContent = `${share}% of vsize`;
    elements.metricAwaiting.textContent = formatCount(
      summary.totals.awaiting_rpc.count,
    );
    renderBanner(elements.banner, summary.source_id, summary.health.capture);
    renderTabs(summary);
    renderChips(summary);
    elements.treemap.hidden = state.primaryView !== "treemap";
    elements.composition.hidden = state.primaryView !== "composition";
    if (state.primaryView === "treemap") {
      renderTreemap(summary);
    } else {
      renderComposition(summary);
    }
    renderEcdf(summary);
    renderJoint(summary);
    elements.status.dataset.state = "ready";
    elements.status.textContent = `Updated ${new Date(summary.as_of_ms).toLocaleTimeString()} · auto ${POLL_INTERVAL_MS / 1_000}s · source evidence last seen ${new Date(summary.health.last_seen_at_ms).toLocaleString()}.`;
  };

  const load = async (): Promise<void> => {
    if (state.source === null) {
      return;
    }
    if (loading) {
      queued = true;
      return;
    }
    loading = true;
    try {
      latest = await fetchSummary(state.source, stateFilter(state));
      elements.liveDot.dataset.state = "live";
      render();
    } catch (error) {
      elements.liveDot.dataset.state = "error";
      elements.status.dataset.state = "error";
      elements.status.textContent =
        error instanceof Error ? error.message : "Unable to load summary";
    } finally {
      loading = false;
      if (queued) {
        queued = false;
        void load();
      }
    }
  };

  elements.reset.addEventListener("click", () => {
    state.classes.clear();
    state.scripts.clear();
    void load();
  });
  elements.sourceSelect.addEventListener("change", () => {
    setSource(elements.sourceSelect.value);
  });
  window.setInterval(() => {
    if (!document.hidden) {
      void load();
    }
  }, POLL_INTERVAL_MS);

  const start = async (): Promise<void> => {
    elements.status.dataset.state = "loading";
    elements.status.textContent = "Discovering sources…";
    try {
      const sources = await fetchSources();
      if (sources.length === 0) {
        elements.status.dataset.state = "error";
        elements.status.textContent =
          "No sources have delivered evidence yet. Start an agent or run `just seed-dev` against the dev server.";
        return;
      }
      elements.sourceSelect.replaceChildren(
        ...sources.map((source) => {
          const option = document.createElement("option");
          option.value = source.source_id;
          option.textContent = `${source.source_id} · ${formatCompact(source.membership_count)} tx`;
          return option;
        }),
      );
      const querySource = new URL(window.location.href).searchParams
        .get("source")
        ?.trim();
      const configuredSource = import.meta.env.VITE_ATLAS_SOURCE_ID?.trim();
      const known = new Set(sources.map((source) => source.source_id));
      const initial =
        querySource !== undefined && known.has(querySource)
          ? querySource
          : configuredSource !== undefined && known.has(configuredSource)
            ? configuredSource
            : sources[0]!.source_id;
      setSource(initial);
    } catch (error) {
      elements.status.dataset.state = "error";
      elements.status.textContent =
        error instanceof Error ? error.message : "Unable to discover sources";
    }
  };
  void start();

  return {
    getSelectedSource: () => state.source,
    onSourceChange: (listener) => {
      sourceListeners.push(listener);
    },
  };
};
