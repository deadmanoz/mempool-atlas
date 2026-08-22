// @vitest-environment happy-dom

import { beforeEach, describe, expect, it } from "vitest";

import { renderCompositionBars, renderMosaicChart } from "./detail-panels";

describe("passive distribution aggregates", () => {
  beforeEach(() => {
    document.body.innerHTML = '<div id="panel"></div>';
  });

  it("uses one keyboard stop to inspect composition segments", () => {
    const panel = document.querySelector<HTMLElement>("#panel")!;
    renderCompositionBars(
      panel,
      [
        {
          classifierId: "shape",
          title: "Shape",
          segments: [
            {
              key: "small",
              label: "Small",
              color: "#111111",
              count: 1,
              vsize: 100,
              share: 0.25,
              bucketCount: 1,
            },
            {
              key: "large",
              label: "Large",
              color: "#222222",
              count: 3,
              vsize: 300,
              share: 0.75,
              bucketCount: 1,
            },
          ],
        },
      ],
      { metricLabel: "count", emptyMessage: "Empty" },
    );

    const track = panel.querySelector<HTMLElement>(".composition-track")!;
    expect(track.tabIndex).toBe(0);
    expect(
      panel.querySelectorAll(".composition-segment[tabindex]"),
    ).toHaveLength(0);
    expect(track.dataset.distributionInspectionKey).toBe(
      "composition:shape:large",
    );

    track.focus();
    track.dispatchEvent(
      new KeyboardEvent("keydown", { key: "Home", bubbles: true }),
    );

    expect(track.dataset.distributionInspectionKey).toBe(
      "composition:shape:small",
    );
    expect(
      panel.querySelector<HTMLElement>('[data-segment="small"]')?.dataset
        .keyboardInspectionActive,
    ).toBe("true");
  });

  it("does not consume arrow keys from a nested bucket action", () => {
    const panel = document.querySelector<HTMLElement>("#panel")!;
    renderCompositionBars(
      panel,
      [
        {
          classifierId: "shape",
          title: "Shape",
          segments: [
            {
              key: "exact",
              label: "Exact",
              color: "#111111",
              count: 1,
              vsize: 100,
              share: 0.25,
              bucketCount: 1,
            },
            {
              key: "overflow",
              label: "Other",
              color: "#222222",
              count: 3,
              vsize: 300,
              share: 0.75,
              bucketCount: 2,
            },
          ],
        },
      ],
      {
        metricLabel: "count",
        emptyMessage: "Empty",
        onSegmentSelect: () => undefined,
      },
    );
    const button = panel.querySelector<HTMLButtonElement>(
      "button.composition-segment",
    )!;
    const event = new KeyboardEvent("keydown", {
      key: "ArrowRight",
      bubbles: true,
      cancelable: true,
    });

    button.dispatchEvent(event);

    expect(event.defaultPrevented).toBe(false);
  });

  it("allows each interactive composition segment to be selected", () => {
    const panel = document.querySelector<HTMLElement>("#panel")!;
    const selected: string[] = [];
    renderCompositionBars(
      panel,
      [
        {
          classifierId: "shape",
          title: "Shape",
          segments: [
            {
              key: "small",
              label: "Small",
              color: "#111111",
              count: 1,
              vsize: 100,
              share: 0.25,
              bucketCount: 1,
            },
            {
              key: "large",
              label: "Large",
              color: "#222222",
              count: 3,
              vsize: 300,
              share: 0.75,
              bucketCount: 1,
            },
          ],
        },
      ],
      {
        metricLabel: "count",
        emptyMessage: "Empty",
        onSegmentSelect: (_bar, segment) => selected.push(segment.key),
      },
    );

    panel.querySelector<HTMLButtonElement>('[data-segment="small"]')?.click();
    panel.querySelector<HTMLButtonElement>('[data-segment="large"]')?.click();

    expect(selected).toEqual(["small", "large"]);
  });

  it("uses one keyboard stop to inspect mosaic columns", () => {
    const panel = document.querySelector<HTMLElement>("#panel")!;
    renderMosaicChart(
      panel,
      {
        totalWeight: 4,
        columns: [
          {
            key: "small",
            label: "Small",
            color: "#111111",
            count: 1,
            weight: 1,
            share: 0.25,
            cells: [
              {
                bandKey: "under_10m",
                bandLabel: "< 10 min",
                bandColor: "#b9eff3",
                count: 1,
                weight: 1,
                share: 1,
              },
            ],
          },
          {
            key: "large",
            label: "Large",
            color: "#222222",
            count: 3,
            weight: 3,
            share: 0.75,
            cells: [
              {
                bandKey: "under_1h",
                bandLabel: "10–60 min",
                bandColor: "#72c8d6",
                count: 3,
                weight: 3,
                share: 1,
              },
            ],
          },
        ],
      },
      { metricFormat: String, emptyMessage: "Empty" },
    );

    const board = panel.querySelector<HTMLElement>(".mosaic-board")!;
    expect(board.tabIndex).toBe(0);
    expect(panel.querySelectorAll(".mosaic-column[tabindex]")).toHaveLength(0);
    expect(board.dataset.distributionInspectionKey).toBe("mosaic:large");
    expect(board.dataset.distributionInspectionDetail).toContain(
      "10–60 min: 3 tx",
    );

    board.focus();
    board.dispatchEvent(
      new KeyboardEvent("keydown", { key: "Home", bubbles: true }),
    );

    expect(board.dataset.distributionInspectionKey).toBe("mosaic:small");
    expect(
      panel.querySelector<HTMLElement>('[data-column="small"]')?.dataset
        .keyboardInspectionActive,
    ).toBe("true");
  });
});
