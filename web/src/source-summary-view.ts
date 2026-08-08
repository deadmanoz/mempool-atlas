import { classificationPresentation } from "./classification-progress";
import { countFormat, decimalFormat, formatVsize } from "./format";
import { honestyBanner } from "./honesty";
import { formatMembershipAge } from "./membership-table";
import type { MempoolSnapshot, SourceSummary } from "./types";

export type AtlasLoadPhase =
  | "discovering-sources"
  | "metadata-ready"
  | "loading-snapshot"
  | "deriving-view"
  | "interactive";

export const setAtlasLoadPhase = (
  status: HTMLElement,
  phase: AtlasLoadPhase,
): void => {
  status.dataset.phase = phase;
};

const formatSnapshotFreshness = (observedAtMs: number): string => {
  const ageMs = Math.max(0, Date.now() - observedAtMs);
  return ageMs < 60_000 ? "just now" : `${formatMembershipAge(ageMs)} ago`;
};

const formatTime = (milliseconds: number): string =>
  new Date(milliseconds).toLocaleTimeString();

const formatDuration = (milliseconds: number): string => {
  if (milliseconds < 1_000) {
    return `${countFormat.format(milliseconds)} ms`;
  }
  if (milliseconds < 60_000) {
    return `${decimalFormat.format(milliseconds / 1_000)} s`;
  }
  return formatMembershipAge(milliseconds);
};

const classificationCopy = (
  source: SourceSummary,
  transactionCount: number | null,
): {
  compact: string;
  summary: string;
  state: "waiting" | "classifying" | "complete" | "paused";
} => {
  if (source.classification === null || transactionCount === null) {
    return {
      compact: "Classification waiting",
      summary: "Classification begins after a complete snapshot is available.",
      state: "waiting",
    };
  }
  const presentation = classificationPresentation(
    source.classification,
    transactionCount,
    (value) => countFormat.format(value),
  );
  return {
    compact: presentation.compact,
    summary: presentation.summary,
    state: source.classification.state,
  };
};

const requiredElement = <T extends HTMLElement>(id: string): T => {
  const element = document.getElementById(id);
  if (!(element instanceof HTMLElement)) {
    throw new Error(`Missing required source-summary element #${id}`);
  }
  return element as T;
};

export interface NodeSourceSummaryView {
  renderDiscovering(): void;
  renderDiscoveryFailure(): void;
  renderMetadata(source: SourceSummary, busy?: boolean): void;
  renderSnapshot(source: SourceSummary, snapshot: MempoolSnapshot): void;
  renderUnavailable(source: SourceSummary): void;
  setBusy(busy: boolean): void;
}

export const createNodeSourceSummaryView = (): NodeSourceSummaryView => {
  const root = requiredElement<HTMLElement>("source-summary");
  const sourceLabel = requiredElement<HTMLElement>("source-label");
  const sourceId = requiredElement<HTMLElement>("source-id");
  const observedValue = requiredElement<HTMLElement>("observed-value");
  const tipValue = requiredElement<HTMLElement>("tip-value");
  const transactionCount = requiredElement<HTMLElement>("transaction-count");
  const totalVsize = requiredElement<HTMLElement>("total-vsize");
  let classificationSummary = requiredElement<HTMLElement>(
    "source-classification-summary",
  );
  const honestyBannerElement = requiredElement<HTMLElement>("honesty-banner");
  const honestyMessage = requiredElement<HTMLElement>("honesty-message");

  const render = (
    source: SourceSummary,
    snapshot: MempoolSnapshot | null,
    phase: "metadata-ready" | "interactive",
    busy: boolean,
  ): void => {
    const observedAt =
      snapshot?.observed_at_ms ?? source.snapshot_observed_at_ms;
    const chainTip = snapshot?.chain_tip ?? source.chain_tip;
    const count = snapshot?.transaction_count ?? source.transaction_count;
    const vsize = snapshot?.total_vsize ?? source.total_vsize;
    const classification = classificationCopy(source, count);
    root.dataset.phase = phase;
    root.dataset.state = source.availability;
    root.setAttribute("aria-busy", String(busy));
    sourceLabel.textContent = source.source_label;
    sourceId.textContent = source.source_id;
    observedValue.textContent =
      observedAt === null ? "No snapshot" : formatSnapshotFreshness(observedAt);
    observedValue.title =
      observedAt === null
        ? "No complete observation"
        : new Date(observedAt).toLocaleString();
    tipValue.textContent =
      chainTip === null ? "Unknown" : countFormat.format(chainTip.height);
    tipValue.title = chainTip?.hash ?? "No chain tip available";
    transactionCount.textContent =
      count === null ? "No snapshot" : countFormat.format(count);
    totalVsize.textContent = vsize === null ? "Waiting" : formatVsize(vsize);
    const classificationText = `${classification.compact}. ${classification.summary}`;
    if (classificationSummary.textContent !== classificationText) {
      const replacement = document.createElement("p");
      replacement.id = "source-classification-summary";
      classificationSummary.replaceWith(replacement);
      classificationSummary = replacement;
    }
    classificationSummary.textContent = classificationText;
    classificationSummary.title = classification.compact;
    classificationSummary.dataset.state = classification.state;
    const banner = honestyBanner(
      source.availability,
      source.last_error,
      source.classification,
      count ?? 0,
      observedAt === null ? null : formatSnapshotFreshness(observedAt),
      (value) => countFormat.format(value),
    );
    honestyBannerElement.hidden = banner === null;
    if (banner !== null) {
      honestyBannerElement.dataset.tone = banner.tone;
      honestyMessage.textContent = banner.message;
    }
  };

  const renderDiscovering = (): void => {
    root.dataset.phase = "discovering-sources";
    root.dataset.state = "waiting";
    root.setAttribute("aria-busy", "true");
    sourceLabel.textContent = "Discovering";
    sourceId.textContent = "unknown";
    observedValue.textContent = "Waiting";
    observedValue.title = "Waiting for source discovery";
    tipValue.textContent = "Unknown";
    tipValue.title = "Waiting for source discovery";
    transactionCount.textContent = "Waiting";
    totalVsize.textContent = "Waiting";
    classificationSummary.textContent =
      "Classification waiting for source metadata.";
    classificationSummary.title = "Waiting for source discovery";
    classificationSummary.dataset.state = "waiting";
    honestyBannerElement.hidden = true;
  };

  return {
    renderDiscovering,
    renderDiscoveryFailure: () => {
      renderDiscovering();
      root.dataset.state = "error";
      root.setAttribute("aria-busy", "false");
    },
    renderMetadata: (
      source,
      busy = source.availability !== "error" ||
        source.transaction_count !== null,
    ) => render(source, null, "metadata-ready", busy),
    renderSnapshot: (source, snapshot) =>
      render(source, snapshot, "interactive", false),
    renderUnavailable: (source) => render(source, null, "interactive", false),
    setBusy: (busy) => root.setAttribute("aria-busy", String(busy)),
  };
};

interface SourceFactView {
  element: HTMLElement;
  value: HTMLElement;
}

const sourceFact = (label: string): SourceFactView => {
  const wrapper = document.createElement("div");
  const term = document.createElement("dt");
  term.textContent = label;
  const detail = document.createElement("dd");
  wrapper.append(term, detail);
  return { element: wrapper, value: detail };
};

const setSourceFact = (fact: SourceFactView, value: string): void => {
  fact.value.textContent = value;
  fact.value.title = value;
};

export interface SourceCardView {
  renderPlaceholder(message: string): void;
  renderMetadata(source: SourceSummary, message?: string): void;
  renderSnapshot(source: SourceSummary, snapshot: MempoolSnapshot): void;
  setBusy(busy: boolean): void;
}

export const createSourceCardView = (
  card: HTMLElement,
  sideLabel: string,
): SourceCardView => {
  const eyebrow = document.createElement("p");
  eyebrow.textContent = sideLabel;
  const heading = document.createElement("h3");
  const identifier = document.createElement("code");
  const nodeLink = document.createElement("a");
  nodeLink.className = "source-card-link";
  nodeLink.textContent = "Explore this node";
  const observedFact = sourceFact("Observed");
  const collectionFact = sourceFact("Collection");
  const chainTipFact = sourceFact("Chain tip");
  const membershipFact = sourceFact("Membership");
  const classificationFact = sourceFact("Classification");
  const facts = document.createElement("dl");
  facts.append(
    observedFact.element,
    collectionFact.element,
    chainTipFact.element,
    membershipFact.element,
    classificationFact.element,
  );
  const classificationNote = document.createElement("p");
  classificationNote.className = "source-card-classification";
  const placeholderNote = document.createElement("p");
  placeholderNote.className = "source-card-placeholder";
  const warning = document.createElement("p");
  warning.className = "source-card-warning";
  card.replaceChildren(
    eyebrow,
    heading,
    identifier,
    nodeLink,
    facts,
    classificationNote,
    placeholderNote,
    warning,
  );

  const metadataMessage = (source: SourceSummary): string => {
    if (source.transaction_count !== null) {
      return "Loading the complete snapshot…";
    }
    if (source.availability === "error") {
      return `No complete snapshot is available. Latest poll: ${source.last_error ?? "unavailable"}`;
    }
    return "Waiting for the first complete snapshot…";
  };

  const render = (
    source: SourceSummary,
    snapshot: MempoolSnapshot | null,
    phase: "metadata-ready" | "interactive",
    message: string | null,
  ): void => {
    const observedAt =
      snapshot?.observed_at_ms ?? source.snapshot_observed_at_ms;
    const chainTip = snapshot?.chain_tip ?? source.chain_tip;
    const count = snapshot?.transaction_count ?? source.transaction_count;
    const vsize = snapshot?.total_vsize ?? source.total_vsize;
    const classification = classificationCopy(source, count);
    card.dataset.state = source.availability;
    card.dataset.phase = phase;
    card.setAttribute(
      "aria-busy",
      String(
        snapshot === null &&
          (source.availability !== "error" ||
            source.transaction_count !== null),
      ),
    );
    heading.textContent = source.source_label;
    identifier.textContent = source.source_id;
    nodeLink.href = `../?source=${encodeURIComponent(source.source_id)}`;
    identifier.hidden = false;
    nodeLink.hidden = false;
    setSourceFact(
      observedFact,
      observedAt === null
        ? "No complete observation"
        : `${formatSnapshotFreshness(observedAt)} · ${formatTime(observedAt)}`,
    );
    setSourceFact(
      collectionFact,
      snapshot === null
        ? observedAt === null
          ? "Waiting for first snapshot"
          : "Complete observation published"
        : `${formatTime(snapshot.collection_started_at_ms)}–${formatTime(snapshot.collection_completed_at_ms)} · ${formatDuration(snapshot.collection_duration_ms)}`,
    );
    setSourceFact(
      chainTipFact,
      chainTip === null
        ? "Unknown"
        : `${countFormat.format(chainTip.height)} · ${chainTip.hash.slice(0, 10)}…`,
    );
    setSourceFact(
      membershipFact,
      count === null || vsize === null
        ? "No complete snapshot"
        : `${countFormat.format(count)} tx · ${formatVsize(vsize)}`,
    );
    setSourceFact(
      classificationFact,
      snapshot === null
        ? classification.compact
        : `${classification.compact} · revision ${countFormat.format(snapshot.classification_revision)}`,
    );
    classificationNote.hidden = false;
    classificationNote.dataset.state = classification.state;
    classificationNote.textContent = `${classification.compact}. ${classification.summary}`;
    placeholderNote.hidden = message === null;
    placeholderNote.textContent = message ?? "";
    warning.hidden = source.availability !== "stale";
    warning.textContent =
      source.availability === "stale"
        ? `Last complete snapshot retained. Latest poll: ${source.last_error ?? "unavailable"}`
        : "";
  };

  const renderPlaceholder = (message: string): void => {
    card.dataset.state = "waiting";
    card.dataset.phase = "discovering-sources";
    card.setAttribute("aria-busy", "true");
    heading.textContent = "Discovering source";
    identifier.hidden = true;
    nodeLink.hidden = true;
    setSourceFact(observedFact, "Waiting");
    setSourceFact(collectionFact, "Waiting");
    setSourceFact(chainTipFact, "Waiting");
    setSourceFact(membershipFact, "Waiting");
    setSourceFact(classificationFact, "Waiting");
    classificationNote.hidden = true;
    placeholderNote.hidden = false;
    placeholderNote.textContent = message;
    warning.hidden = true;
    warning.textContent = "";
  };

  return {
    renderPlaceholder,
    renderMetadata: (source, message) =>
      render(
        source,
        null,
        "metadata-ready",
        message ?? metadataMessage(source),
      ),
    renderSnapshot: (source, snapshot) =>
      render(source, snapshot, "interactive", null),
    setBusy: (busy) => card.setAttribute("aria-busy", String(busy)),
  };
};
