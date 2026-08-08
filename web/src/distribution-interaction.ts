const INSPECTION_ATTRIBUTE = "data-distribution-inspection-key";
const INSPECTION_SELECTOR = `[${INSPECTION_ATTRIBUTE}]`;
const PINNED_ATTRIBUTE = "data-distribution-inspection-pinned";
const HELP_TEXT = "Hover/focus for details \u00b7 click charts to pin";
const CLEAR_TEXT = "Clear focus";
const TOOLTIP_OFFSET = 12;
const VIEWPORT_INSET = 8;
const MAX_TOOLTIP_WIDTH = 420;

let nextInspectorId = 1;

export interface DistributionInspectionMetadata {
  key: string;
  title: string;
  detail: string;
  accessibleLabel?: string;
  pinOnClick?: boolean;
}

export interface DistributionInspector {
  reset(): void;
  destroy(): void;
}

export const createSectionDistributionInspector = (
  root: HTMLElement,
  headingSelector: string,
): DistributionInspector => {
  const heading = root.querySelector<HTMLElement>(headingSelector);
  if (heading === null) {
    throw new Error(`Missing distribution heading ${headingSelector}`);
  }
  return createDistributionInspector(root, heading);
};

interface InspectionTarget {
  element: HTMLElement | SVGElement;
  metadata: DistributionInspectionMetadata;
}

interface Point {
  x: number;
  y: number;
}

const supportsPressedState = (element: HTMLElement | SVGElement): boolean =>
  element instanceof HTMLButtonElement ||
  element.getAttribute("role") === "button";

export const annotateDistributionInspectionTarget = <
  T extends HTMLElement | SVGElement,
>(
  element: T,
  metadata: DistributionInspectionMetadata,
): T => {
  element.removeAttribute("title");
  for (const child of [...element.children]) {
    if (child instanceof SVGTitleElement) child.remove();
  }
  element.setAttribute(INSPECTION_ATTRIBUTE, metadata.key);
  element.setAttribute("data-distribution-inspection-title", metadata.title);
  element.setAttribute("data-distribution-inspection-detail", metadata.detail);
  element.setAttribute(
    "data-distribution-inspection-pin",
    String(metadata.pinOnClick ?? true),
  );
  if (metadata.accessibleLabel !== undefined) {
    element.setAttribute("aria-label", metadata.accessibleLabel);
  }
  if (
    metadata.pinOnClick !== false &&
    supportsPressedState(element) &&
    !element.hasAttribute("aria-pressed")
  ) {
    element.setAttribute("aria-pressed", "false");
  }
  return element;
};

/** Refresh an inspector after a keyboard-controlled chart changes its region. */
export const notifyDistributionInspectionTargetChanged = (
  element: HTMLElement | SVGElement,
): void => {
  element.dispatchEvent(
    new CustomEvent("distributioninspectionchange", { bubbles: true }),
  );
};

const inspectionTarget = (
  root: HTMLElement,
  eventTarget: EventTarget | null,
): InspectionTarget | null => {
  if (!(eventTarget instanceof Element)) return null;
  const element = eventTarget.closest(INSPECTION_SELECTOR);
  if (
    (!(element instanceof HTMLElement) && !(element instanceof SVGElement)) ||
    !root.contains(element)
  ) {
    return null;
  }
  const key = element.getAttribute(INSPECTION_ATTRIBUTE);
  const title = element.getAttribute("data-distribution-inspection-title");
  const detail = element.getAttribute("data-distribution-inspection-detail");
  if (key === null || title === null || detail === null) return null;
  return {
    element,
    metadata: {
      key,
      title,
      detail,
      pinOnClick:
        element.getAttribute("data-distribution-inspection-pin") !== "false",
    },
  };
};

const sameTarget = (
  left: InspectionTarget | null,
  right: InspectionTarget | null,
): boolean =>
  left?.element === right?.element &&
  left?.metadata.key === right?.metadata.key;

const visuallyHide = (element: HTMLElement): void => {
  element.style.position = "absolute";
  element.style.width = "1px";
  element.style.height = "1px";
  element.style.padding = "0";
  element.style.margin = "-1px";
  element.style.overflow = "hidden";
  element.style.clip = "rect(0, 0, 0, 0)";
  element.style.whiteSpace = "nowrap";
  element.style.border = "0";
};

const clamp = (value: number, minimum: number, maximum: number): number =>
  Math.min(Math.max(value, minimum), Math.max(minimum, maximum));

export const createDistributionInspector = (
  root: HTMLElement,
  heading: HTMLElement,
): DistributionInspector => {
  const inspectorId = nextInspectorId;
  nextInspectorId += 1;

  const control = document.createElement("button");
  control.type = "button";
  control.className = "distribution-inspection-control";
  control.setAttribute("data-distribution-inspection-control", "");
  control.textContent = HELP_TEXT;
  control.disabled = true;

  const tooltip = document.createElement("div");
  tooltip.id = `distribution-inspection-tooltip-${inspectorId}`;
  tooltip.className = "distribution-inspection-tooltip";
  tooltip.setAttribute("role", "tooltip");
  tooltip.hidden = true;
  tooltip.style.position = "fixed";
  tooltip.style.pointerEvents = "none";
  tooltip.style.zIndex = "1000";
  const tooltipTitle = document.createElement("strong");
  tooltipTitle.className = "distribution-inspection-tooltip-title";
  const tooltipDetail = document.createElement("span");
  tooltipDetail.className = "distribution-inspection-tooltip-detail";
  tooltip.append(tooltipTitle, tooltipDetail);

  const liveStatus = document.createElement("span");
  liveStatus.className = "distribution-inspection-status";
  liveStatus.setAttribute("aria-live", "polite");
  liveStatus.setAttribute("aria-atomic", "true");
  visuallyHide(liveStatus);

  const controlHost =
    heading.querySelector<HTMLElement>(":scope > div:first-child") ?? heading;
  controlHost.append(control);
  root.append(tooltip, liveStatus);

  let hovered: InspectionTarget | null = null;
  let focused: InspectionTarget | null = null;
  let pinned: InspectionTarget | null = null;
  let rendered: InspectionTarget | null = null;
  let lastPointer: Point | null = null;
  let destroyed = false;

  const activeTarget = (): InspectionTarget | null => {
    for (const candidate of [pinned, hovered, focused]) {
      if (candidate !== null && root.contains(candidate.element)) {
        return candidate;
      }
    }
    return null;
  };

  const targetAnchor = (target: InspectionTarget): Point => {
    if (pinned === null && focused === null && lastPointer !== null) {
      return {
        x: lastPointer.x + TOOLTIP_OFFSET,
        y: lastPointer.y + TOOLTIP_OFFSET,
      };
    }
    const bounds = target.element.getBoundingClientRect();
    return { x: bounds.left, y: bounds.bottom + TOOLTIP_OFFSET };
  };

  const positionTooltip = (target: InspectionTarget): void => {
    const anchor = targetAnchor(target);
    const rootBounds = root.getBoundingClientRect();
    const viewportRight = Math.max(
      VIEWPORT_INSET,
      window.innerWidth - VIEWPORT_INSET,
    );
    const viewportBottom = Math.max(
      VIEWPORT_INSET,
      window.innerHeight - VIEWPORT_INSET,
    );
    const rootHasArea = rootBounds.width > 0 && rootBounds.height > 0;
    const minimumX = rootHasArea
      ? Math.max(VIEWPORT_INSET, rootBounds.left + VIEWPORT_INSET)
      : VIEWPORT_INSET;
    const maximumX = rootHasArea
      ? Math.min(viewportRight, rootBounds.right - VIEWPORT_INSET)
      : viewportRight;
    const minimumY = rootHasArea
      ? Math.max(VIEWPORT_INSET, rootBounds.top + VIEWPORT_INSET)
      : VIEWPORT_INSET;
    const maximumY = rootHasArea
      ? Math.min(viewportBottom, rootBounds.bottom - VIEWPORT_INSET)
      : viewportBottom;
    const availableWidth = Math.max(0, maximumX - minimumX);
    tooltip.style.maxWidth = `${Math.min(MAX_TOOLTIP_WIDTH, availableWidth)}px`;
    const tooltipBounds = tooltip.getBoundingClientRect();
    tooltip.style.left = `${clamp(
      anchor.x,
      minimumX,
      maximumX - tooltipBounds.width,
    )}px`;
    tooltip.style.top = `${clamp(
      anchor.y,
      minimumY,
      maximumY - tooltipBounds.height,
    )}px`;
  };

  const render = (announcementPrefix = ""): void => {
    const target = activeTarget();
    if (target === null) {
      rendered = null;
      tooltip.hidden = true;
      return;
    }
    tooltipTitle.textContent = target.metadata.title;
    tooltipDetail.textContent = target.metadata.detail;
    tooltip.hidden = false;
    positionTooltip(target);
    if (!sameTarget(rendered, target) || announcementPrefix !== "") {
      liveStatus.textContent = `${announcementPrefix}${target.metadata.title}. ${target.metadata.detail}`;
    }
    rendered = target;
  };

  const clearPinnedAttribute = (): void => {
    pinned?.element.removeAttribute(PINNED_ATTRIBUTE);
    if (pinned !== null && supportsPressedState(pinned.element)) {
      pinned.element.setAttribute("aria-pressed", "false");
    }
  };

  const syncControl = (): void => {
    const hasPinnedTarget = pinned !== null && root.contains(pinned.element);
    control.disabled = !hasPinnedTarget;
    control.textContent = hasPinnedTarget ? CLEAR_TEXT : HELP_TEXT;
  };

  const clear = (announce: boolean): void => {
    clearPinnedAttribute();
    hovered = null;
    focused = null;
    pinned = null;
    rendered = null;
    lastPointer = null;
    tooltip.hidden = true;
    syncControl();
    liveStatus.textContent = announce ? "Distribution focus cleared." : "";
  };

  const handlePointerOver = (event: PointerEvent): void => {
    const target = inspectionTarget(root, event.target);
    if (!sameTarget(hovered, target)) hovered = target;
    if (focused?.element === target?.element) focused = target;
    lastPointer = { x: event.clientX, y: event.clientY };
    render();
  };

  const handlePointerMove = (event: PointerEvent): void => {
    const target = inspectionTarget(root, event.target);
    if (!sameTarget(hovered, target)) hovered = target;
    if (focused?.element === target?.element) focused = target;
    lastPointer = { x: event.clientX, y: event.clientY };
    render();
  };

  const handlePointerOut = (event: PointerEvent): void => {
    const leaving = inspectionTarget(root, event.target);
    const entering = inspectionTarget(root, event.relatedTarget);
    if (sameTarget(leaving, entering)) return;
    if (sameTarget(hovered, leaving)) hovered = null;
    lastPointer = null;
    render();
  };

  const handleFocusIn = (event: FocusEvent): void => {
    focused = inspectionTarget(root, event.target);
    render();
  };

  const handleFocusOut = (event: FocusEvent): void => {
    const leaving = inspectionTarget(root, event.target);
    const entering = inspectionTarget(root, event.relatedTarget);
    if (sameTarget(leaving, entering)) return;
    if (sameTarget(focused, leaving)) focused = null;
    render();
  };

  const handleClick = (event: MouseEvent): void => {
    if (
      event.target instanceof Element &&
      event.target.closest("[data-distribution-inspection-control]") === control
    ) {
      clear(true);
      return;
    }
    const target = inspectionTarget(root, event.target);
    if (target === null) return;
    if (target.metadata.pinOnClick === false) return;
    if (sameTarget(pinned, target)) {
      clearPinnedAttribute();
      pinned = null;
      syncControl();
      render();
      return;
    }
    clearPinnedAttribute();
    pinned = target;
    pinned.element.setAttribute(PINNED_ATTRIBUTE, "true");
    if (supportsPressedState(pinned.element)) {
      pinned.element.setAttribute("aria-pressed", "true");
    }
    syncControl();
    render("Pinned. ");
  };

  const handleKeyDown = (event: KeyboardEvent): void => {
    if (event.key === "Escape") clear(true);
  };

  const handleInspectionChange = (event: Event): void => {
    const target = inspectionTarget(root, event.target);
    if (target === null) return;
    if (
      focused?.element === target.element ||
      document.activeElement === target.element
    ) {
      focused = target;
    }
    if (hovered?.element === target.element) hovered = target;
    if (pinned?.element === target.element) pinned = target;
    render();
  };

  const reposition = (): void => {
    const target = activeTarget();
    if (target !== null && !tooltip.hidden) positionTooltip(target);
  };

  root.addEventListener("pointerover", handlePointerOver);
  root.addEventListener("pointermove", handlePointerMove);
  root.addEventListener("pointerout", handlePointerOut);
  root.addEventListener("focusin", handleFocusIn);
  root.addEventListener("focusout", handleFocusOut);
  root.addEventListener("click", handleClick, true);
  root.addEventListener("keydown", handleKeyDown);
  root.addEventListener("distributioninspectionchange", handleInspectionChange);
  window.addEventListener("resize", reposition);
  window.addEventListener("scroll", reposition, true);

  const reset = (): void => {
    if (!destroyed) clear(false);
  };

  const destroy = (): void => {
    if (destroyed) return;
    clear(false);
    destroyed = true;
    root.removeEventListener("pointerover", handlePointerOver);
    root.removeEventListener("pointermove", handlePointerMove);
    root.removeEventListener("pointerout", handlePointerOut);
    root.removeEventListener("focusin", handleFocusIn);
    root.removeEventListener("focusout", handleFocusOut);
    root.removeEventListener("click", handleClick, true);
    root.removeEventListener("keydown", handleKeyDown);
    root.removeEventListener(
      "distributioninspectionchange",
      handleInspectionChange,
    );
    window.removeEventListener("resize", reposition);
    window.removeEventListener("scroll", reposition, true);
    control.remove();
    tooltip.remove();
    liveStatus.remove();
  };

  return { reset, destroy };
};
