// @vitest-environment happy-dom

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import {
  annotateDistributionInspectionTarget,
  createDistributionInspector,
  notifyDistributionInspectionTargetChanged,
  type DistributionInspector,
} from "./distribution-interaction";

const rect = (
  left: number,
  top: number,
  width: number,
  height: number,
): DOMRect =>
  ({
    x: left,
    y: top,
    left,
    top,
    width,
    height,
    right: left + width,
    bottom: top + height,
    toJSON: () => ({}),
  }) as DOMRect;

const dispatchPointer = (
  target: Element,
  type: "pointerover" | "pointermove" | "pointerout",
  options: { x?: number; y?: number; relatedTarget?: EventTarget | null } = {},
): void => {
  const event = new Event(type, { bubbles: true });
  Object.defineProperties(event, {
    clientX: { value: options.x ?? 0 },
    clientY: { value: options.y ?? 0 },
    relatedTarget: { value: options.relatedTarget ?? null },
  });
  target.dispatchEvent(event);
};

describe("distribution inspection annotations", () => {
  it("annotates HTML and SVG marks with semantic metadata", () => {
    const html = document.createElement("button");
    html.title = "native HTML title";
    const svg = document.createElementNS("http://www.w3.org/2000/svg", "rect");
    const nativeSvgTitle = document.createElementNS(
      "http://www.w3.org/2000/svg",
      "title",
    );
    nativeSvgTitle.textContent = "native SVG title";
    svg.append(nativeSvgTitle);

    expect(
      annotateDistributionInspectionTarget(html, {
        key: "composition:alpha",
        title: "Alpha",
        detail: "12 transactions",
        accessibleLabel: "Inspect Alpha",
      }),
    ).toBe(html);
    annotateDistributionInspectionTarget(svg, {
      key: "fee:3:alpha",
      title: "Alpha at 4 to 8 sat/vB",
      detail: "24.6 kvB",
    });

    expect(html.dataset.distributionInspectionKey).toBe("composition:alpha");
    expect(html.dataset.distributionInspectionTitle).toBe("Alpha");
    expect(html.dataset.distributionInspectionDetail).toBe("12 transactions");
    expect(html.getAttribute("aria-label")).toBe("Inspect Alpha");
    expect(html.getAttribute("aria-pressed")).toBe("false");
    expect(html.hasAttribute("title")).toBe(false);
    expect(svg.getAttribute("data-distribution-inspection-key")).toBe(
      "fee:3:alpha",
    );
    expect(svg.hasAttribute("title")).toBe(false);
    expect(svg.querySelector("title")).toBe(null);
  });
});

describe("createDistributionInspector", () => {
  let root: HTMLElement;
  let heading: HTMLElement;
  let mark: HTMLButtonElement;
  let inspector: DistributionInspector;

  beforeEach(() => {
    document.body.innerHTML = `
      <section id="root">
        <header id="heading"><h2>Distributions</h2></header>
        <button id="mark"><span id="mark-child">mark</span></button>
      </section>
    `;
    root = document.querySelector<HTMLElement>("#root")!;
    heading = document.querySelector<HTMLElement>("#heading")!;
    mark = document.querySelector<HTMLButtonElement>("#mark")!;
    annotateDistributionInspectionTarget(mark, {
      key: "fee:0:alpha",
      title: "Alpha",
      detail: "1 to 2 sat/vB · 12 transactions",
      accessibleLabel: "Inspect Alpha fee bin",
    });
    root.getBoundingClientRect = () => rect(10, 20, 300, 200);
    mark.getBoundingClientRect = () => rect(40, 60, 30, 20);
    inspector = createDistributionInspector(root, heading);
    const tooltip = root.querySelector<HTMLElement>("[role=tooltip]")!;
    tooltip.getBoundingClientRect = () => rect(0, 0, 100, 60);
    vi.spyOn(window, "innerWidth", "get").mockReturnValue(320);
    vi.spyOn(window, "innerHeight", "get").mockReturnValue(240);
  });

  afterEach(() => {
    inspector.destroy();
    vi.restoreAllMocks();
    document.body.replaceChildren();
  });

  it("creates one heading control, tooltip, and polite live status", () => {
    const control = heading.querySelector<HTMLButtonElement>(
      "[data-distribution-inspection-control]",
    );
    const tooltip = root.querySelector<HTMLElement>("[role=tooltip]");
    const status = root.querySelector<HTMLElement>("[aria-live=polite]");

    expect(control?.textContent).toBe(
      "Hover/focus for details · click charts to pin",
    );
    expect(control?.disabled).toBe(true);
    expect(tooltip?.hidden).toBe(true);
    expect(status?.getAttribute("aria-atomic")).toBe("true");
  });

  it("uses delegated pointer hover and clamps the tooltip to root and viewport", () => {
    const child = document.querySelector<HTMLElement>("#mark-child")!;
    const tooltip = root.querySelector<HTMLElement>("[role=tooltip]")!;

    dispatchPointer(child, "pointerover", { x: 300, y: 220 });

    expect(tooltip.hidden).toBe(false);
    expect(tooltip.textContent).toBe("Alpha1 to 2 sat/vB · 12 transactions");
    expect(tooltip.style.left).toBe("202px");
    expect(tooltip.style.top).toBe("152px");
    expect(root.querySelector("[aria-live=polite]")?.textContent).toBe(
      "Alpha. 1 to 2 sat/vB · 12 transactions",
    );

    dispatchPointer(child, "pointerout");
    expect(tooltip.hidden).toBe(true);
  });

  it("gives focus the same tooltip and live detail as pointer hover", () => {
    const tooltip = root.querySelector<HTMLElement>("[role=tooltip]")!;

    mark.focus();

    expect(tooltip.hidden).toBe(false);
    expect(tooltip.textContent).toContain("1 to 2 sat/vB");
    expect(root.querySelector("[aria-live=polite]")?.textContent).toBe(
      "Alpha. 1 to 2 sat/vB · 12 transactions",
    );

    mark.blur();
    expect(tooltip.hidden).toBe(true);
  });

  it("refreshes pointer detail when the same dynamic chart still has focus", () => {
    const tooltip = root.querySelector<HTMLElement>("[role=tooltip]")!;
    mark.focus();
    annotateDistributionInspectionTarget(mark, {
      key: "fee:1:beta",
      title: "Beta",
      detail: "2 to 4 sat/vB · 8 transactions",
      accessibleLabel: "Inspect Beta fee bin",
    });

    dispatchPointer(mark, "pointermove", { x: 80, y: 90 });

    expect(tooltip.textContent).toContain("Beta");
    expect(root.querySelector("[aria-live=polite]")?.textContent).toContain(
      "2 to 4 sat/vB",
    );
  });

  it("lets a hovered child temporarily override its focused aggregate", () => {
    const child = annotateDistributionInspectionTarget(
      document.createElement("span"),
      {
        key: "mosaic:alpha:young",
        title: "Alpha · < 10 min",
        detail: "3 transactions",
        pinOnClick: false,
      },
    );
    mark.append(child);
    const tooltip = root.querySelector<HTMLElement>("[role=tooltip]")!;
    mark.focus();

    dispatchPointer(child, "pointerover", { x: 80, y: 90 });

    expect(tooltip.textContent).toContain("Alpha · < 10 min");
    dispatchPointer(child, "pointerout");
    expect(tooltip.textContent).toContain("1 to 2 sat/vB");
  });

  it("pins without suppressing an existing button action and clears by toggle", () => {
    const action = vi.fn();
    mark.addEventListener("click", action);
    const tooltip = root.querySelector<HTMLElement>("[role=tooltip]")!;
    const control = heading.querySelector<HTMLButtonElement>(
      "[data-distribution-inspection-control]",
    )!;

    mark.click();

    expect(action).toHaveBeenCalledOnce();
    expect(mark.dataset.distributionInspectionPinned).toBe("true");
    expect(mark.getAttribute("aria-pressed")).toBe("true");
    expect(control.textContent).toBe("Clear focus");
    expect(control.disabled).toBe(false);
    expect(tooltip.hidden).toBe(false);
    dispatchPointer(mark, "pointerout");
    expect(tooltip.hidden).toBe(false);

    mark.click();
    expect(action).toHaveBeenCalledTimes(2);
    expect(mark.hasAttribute("data-distribution-inspection-pinned")).toBe(
      false,
    );
    expect(mark.getAttribute("aria-pressed")).toBe("false");
    expect(control.textContent).toBe(
      "Hover/focus for details · click charts to pin",
    );
    expect(control.disabled).toBe(true);
  });

  it("can expose detail without hijacking an existing activation action", () => {
    const action = vi.fn();
    const actionMark = annotateDistributionInspectionTarget(
      document.createElement("button"),
      {
        key: "composition:exact",
        title: "Exact bucket",
        detail: "Opens the Buckets view.",
        pinOnClick: false,
      },
    );
    actionMark.addEventListener("click", action);
    root.append(actionMark);

    actionMark.click();

    expect(action).toHaveBeenCalledOnce();
    expect(actionMark.hasAttribute("data-distribution-inspection-pinned")).toBe(
      false,
    );
  });

  it("clears pinned focus through the heading control or Escape", () => {
    const control = heading.querySelector<HTMLButtonElement>(
      "[data-distribution-inspection-control]",
    )!;
    const tooltip = root.querySelector<HTMLElement>("[role=tooltip]")!;
    const status = root.querySelector<HTMLElement>("[aria-live=polite]")!;

    mark.click();
    control.click();
    expect(mark.hasAttribute("data-distribution-inspection-pinned")).toBe(
      false,
    );
    expect(tooltip.hidden).toBe(true);
    expect(status.textContent).toBe("Distribution focus cleared.");

    mark.click();
    mark.dispatchEvent(
      new KeyboardEvent("keydown", { key: "Escape", bubbles: true }),
    );
    expect(mark.hasAttribute("data-distribution-inspection-pinned")).toBe(
      false,
    );
    expect(tooltip.hidden).toBe(true);
  });

  it("reconnects a focused dynamic chart after Escape clears its pin", () => {
    const tooltip = root.querySelector<HTMLElement>("[role=tooltip]")!;
    mark.focus();
    mark.click();
    mark.dispatchEvent(
      new KeyboardEvent("keydown", { key: "Escape", bubbles: true }),
    );

    annotateDistributionInspectionTarget(mark, {
      key: "fee:1:beta",
      title: "Beta",
      detail: "2 to 4 sat/vB · 8 transactions",
      accessibleLabel: "Inspect Beta fee bin",
    });
    notifyDistributionInspectionTargetChanged(mark);

    expect(tooltip.hidden).toBe(false);
    expect(tooltip.textContent).toContain("Beta");
    expect(root.querySelector("[aria-live=polite]")?.textContent).toContain(
      "2 to 4 sat/vB",
    );
  });

  it("resets reusable state and destroys every owned node and listener", () => {
    mark.click();
    inspector.reset();

    expect(mark.hasAttribute("data-distribution-inspection-pinned")).toBe(
      false,
    );
    expect(root.querySelector<HTMLElement>("[role=tooltip]")?.hidden).toBe(
      true,
    );

    inspector.destroy();
    expect(
      heading.querySelector("[data-distribution-inspection-control]"),
    ).toBe(null);
    expect(root.querySelector("[role=tooltip]")).toBe(null);
    expect(root.querySelector("[aria-live=polite]")).toBe(null);

    mark.click();
    expect(mark.hasAttribute("data-distribution-inspection-pinned")).toBe(
      false,
    );
  });
});
