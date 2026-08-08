import { expect, test } from "@playwright/test";
import type { Locator, Page } from "@playwright/test";

// These assertions encode a real regression: the responsive layer for shared
// shell components once lived in `comparison-styles.css`, which only the
// comparison page imports. The node page therefore clipped its source facts
// and collapsed the txid search box to a few dozen pixels at phone widths,
// while the comparison page looked fine. Anything asserted here must hold on
// both pages.

// A control a finger has to hit. Below this, a target is a miss risk on a
// touch screen.
const MINIMUM_TAP_HEIGHT = 32;

const isMobile = (viewportWidth: number): boolean => viewportWidth <= 620;

const viewportWidth = (page: Page): number => {
  const size = page.viewportSize();
  if (!size) {
    throw new Error("viewport size is required for layout assertions");
  }
  return size.width;
};

const waitForSnapshot = async (page: Page, statusId: string): Promise<void> => {
  await expect(page.locator(`#${statusId}`)).toHaveAttribute(
    "data-state",
    /ready|stale|different/,
    { timeout: 20_000 },
  );
};

/** Widest right edge reached by any element, ignoring its own scroll offset. */
const documentOverflow = async (page: Page): Promise<number> =>
  page.evaluate(() => {
    const root = document.documentElement;
    return (
      Math.max(root.scrollWidth, document.body.scrollWidth) - root.clientWidth
    );
  });

const boxOf = async (
  locator: Locator,
): Promise<{
  width: number;
  height: number;
  right: number;
  top: number;
  bottom: number;
}> => {
  const box = await locator.boundingBox();
  if (!box) {
    throw new Error("expected the element to be laid out and visible");
  }
  return {
    width: box.width,
    height: box.height,
    right: box.x + box.width,
    top: box.y,
    bottom: box.y + box.height,
  };
};

const expectProminentViewSwitch = async (page: Page): Promise<void> => {
  const nav = page.locator(".product-nav");
  const links = nav.locator("a");
  await expect(nav).toBeVisible();
  await expect(links).toHaveCount(2);
  await expect(nav.locator('a[aria-current="page"]')).toHaveCount(1);

  const navBox = await boxOf(nav);
  for (const link of await links.all()) {
    const linkBox = await boxOf(link);
    expect(linkBox.width).toBeGreaterThanOrEqual(navBox.width * 0.45);
    expect(linkBox.height).toBeGreaterThanOrEqual(40);
  }
};

const expectPublicFooter = async (page: Page): Promise<void> => {
  const footer = page.locator(".atlas-footer");
  await footer.scrollIntoViewIfNeeded();
  await expect(footer).toBeVisible();
  await expect(footer.locator("p")).toHaveText("Mempool Atlas · open source");
  const links = footer.locator("nav a");
  await expect(links).toHaveCount(3);
  await expect(links.nth(0)).toHaveAttribute(
    "href",
    "https://github.com/deadmanoz/mempool-atlas",
  );
  await expect(links.nth(1)).toHaveAttribute("href", "https://x.com/ozdeadman");
  await expect(links.nth(2)).toHaveAttribute(
    "href",
    "https://primal.net/deadmanoz",
  );
  for (const link of await links.all()) {
    await expect(link).toHaveAttribute("target", "_blank");
    await expect(link).toHaveAttribute("rel", "noopener noreferrer");
  }
  expect((await boxOf(footer)).right).toBeLessThanOrEqual(
    viewportWidth(page) + 1,
  );
};

test.describe("node page", () => {
  test.beforeEach(async ({ page }) => {
    await page.goto("/?source=vps-core-01");
    await waitForSnapshot(page, "page-status");
  });

  test("never scrolls horizontally", async ({ page }) => {
    await expect(page.locator("#atlas-version")).toHaveText("v1.0.0");
    expect(await documentOverflow(page)).toBeLessThanOrEqual(0);
  });

  test("presents Node and Compare as the primary view switch", async ({
    page,
  }) => {
    await expectProminentViewSwitch(page);
    await expect(
      page.locator('.product-nav a[aria-current="page"]'),
    ).toContainText("Node");
  });

  test("links to the public project and author profiles", async ({ page }) => {
    await expectPublicFooter(page);
  });

  test("keeps node facts in the source controls panel", async ({ page }) => {
    const header = page.locator(".atlas-header");
    const controls = page.locator(".node-controls");
    const picker = controls.locator(".node-source-picker");
    const summary = controls.locator("#source-summary");
    const search = controls.locator("#transaction-search");
    const facts = summary.locator(".source-summary-facts > div");

    await expect(header.locator("#source-summary")).toHaveCount(0);
    await expect(summary).toBeVisible();
    await expect(facts).toHaveCount(4);

    const limit = viewportWidth(page);
    for (const fact of await facts.all()) {
      const box = await boxOf(fact);
      expect(box.width).toBeGreaterThan(0);
      expect(box.height).toBeGreaterThan(0);
      expect(box.right).toBeLessThanOrEqual(limit + 1);
    }

    const pickerBox = await boxOf(picker);
    const summaryBox = await boxOf(summary);
    const searchBox = await boxOf(search);
    expect(pickerBox.bottom).toBeLessThanOrEqual(summaryBox.top + 1);
    expect(summaryBox.bottom).toBeLessThanOrEqual(searchBox.top + 1);
  });

  test("keeps a useful snapshot summary below the view switch", async ({
    page,
  }) => {
    const status = page.locator("#page-status");
    await expect(status).toBeVisible();
    await expect(status).toHaveAttribute("data-state", /ready|stale/);
    await expect(page.locator("#status-title")).toHaveText(/ snapshot$/);
    await expect(page.locator("#status-detail")).toHaveText(
      /transactions .* observed .* chain tip/i,
    );
  });

  test("uses shared button treatments for classification query actions", async ({
    page,
  }) => {
    await expect(
      page.locator(".classification-match-control > .segmented.compact"),
    ).toHaveCount(1);
    await expect(page.locator("#classification-clear")).toHaveClass(
      /action-button/,
    );
  });

  test("leaves the txid search box wide enough to read a txid prefix", async ({
    page,
  }) => {
    const box = await boxOf(page.locator("#transaction-search-input"));
    expect(box.width).toBeGreaterThanOrEqual(200);
  });

  test("keeps the classifier select at its natural height", async ({
    page,
  }) => {
    // `.toolbar-classifier` carries `flex: 1 1 15rem`. Once the toolbar stacks,
    // that basis applies to the main axis, which is now height.
    const box = await boxOf(page.locator(".toolbar-classifier"));
    expect(box.height).toBeLessThan(120);
  });

  test("gives primary controls a usable tap height", async ({ page }) => {
    test.skip(
      !isMobile(viewportWidth(page)),
      "touch sizing applies below 620px",
    );

    for (const locator of [
      page.locator(".product-nav a"),
      page.locator(".lens-toolbar .segmented button"),
    ]) {
      for (const control of await locator.all()) {
        const box = await boxOf(control);
        expect(box.height).toBeGreaterThanOrEqual(MINIMUM_TAP_HEIGHT);
      }
    }
  });

  test("uses automatic membership filters without an Apply control", async ({
    page,
  }) => {
    await page.locator("#fee-age-tab").click();
    const filters = page.locator("#filters");
    await expect(
      filters.locator(
        'button:not([type]), button[type="submit"], input[type="submit"], input[type="image"]',
      ),
    ).toHaveCount(0);
    await expect(filters.getByRole("button", { name: /^apply$/i })).toHaveCount(
      0,
    );
    await expect(filters.getByRole("button")).toHaveCount(1);
    await expect(filters.getByRole("button")).toHaveText("Reset");
  });

  test("keeps the Classifications query usable at 390px", async ({ page }) => {
    test.skip(
      viewportWidth(page) !== 390,
      "the Classifications mobile contract is checked at 390px",
    );

    await page
      .locator('#classification-labels button[data-label="version_2"]')
      .click();
    await page
      .locator('#classification-labels button[data-label="p2wsh"]')
      .click();
    await page.locator("#classification-match-all").click();
    await expect(page.locator("#classification-query-summary")).toContainText(
      "114 transactions match Version 2 and P2WSH.",
    );

    const limit = viewportWidth(page);
    for (const control of [
      page.locator("#classification-match-any"),
      page.locator("#classification-match-all"),
      page.locator("#classification-clear"),
    ]) {
      const box = await boxOf(control);
      expect(box.height).toBeGreaterThanOrEqual(MINIMUM_TAP_HEIGHT);
      expect(box.right).toBeLessThanOrEqual(limit + 1);
    }

    const queryStage = page.locator("#classification-query-stage");
    await expect(queryStage).toBeVisible();
    expect((await boxOf(queryStage)).right).toBeLessThanOrEqual(limit + 1);
    const terrain = await boxOf(page.locator(".terrain-panel"));
    const inspector = await boxOf(page.locator(".inspector-panel"));
    expect(inspector.top).toBeGreaterThanOrEqual(terrain.bottom - 1);
    expect(await documentOverflow(page)).toBeLessThanOrEqual(0);
  });

  test("fits all three bucket totals beside each other", async ({ page }) => {
    await page.locator("#terrain-tab").click();
    const totals = page.locator("#terrain-view .terrain-totals > div");
    await expect(totals).toHaveCount(3);

    const limit = viewportWidth(page);
    for (const total of await totals.all()) {
      expect((await boxOf(total)).right).toBeLessThanOrEqual(limit + 1);
    }
    expect(await documentOverflow(page)).toBeLessThanOrEqual(0);
  });

  test("inspects and pins distribution regions without changing the population", async ({
    page,
  }) => {
    const spectrum = page.locator("#spectrum-chart .spectrum-chart");
    await spectrum.scrollIntoViewIfNeeded();
    await expect(spectrum).toHaveAttribute("role", "button");
    await expect(spectrum).toHaveAttribute(
      "data-distribution-inspection-key",
      /snapshot:fee-rate:bin:/,
    );
    await spectrum.focus();
    await page.keyboard.press("Home");
    await expect(spectrum).toHaveAttribute(
      "data-distribution-inspection-key",
      "snapshot:fee-rate:bin:0",
    );
    await page.keyboard.press("End");
    await expect(spectrum).not.toHaveAttribute(
      "data-distribution-inspection-key",
      "snapshot:fee-rate:bin:0",
    );
    await expect(
      page.locator(".distribution-inspection-tooltip"),
    ).toContainText("Fee rate:");

    await page.keyboard.press("Enter");
    await expect(spectrum).toHaveAttribute(
      "data-distribution-inspection-pinned",
      "true",
    );
    await expect(spectrum).toHaveAttribute("aria-pressed", "true");
    await page.keyboard.press("Enter");
    await expect(spectrum).toHaveAttribute("aria-pressed", "false");
    await page.keyboard.press("Enter");
    await expect(spectrum).toHaveAttribute("aria-pressed", "true");
    const clear = page.locator(".distribution-inspection-control");
    await expect(clear).toBeEnabled();
    await expect(clear).toHaveText("Clear focus");
    await clear.click();
    await expect(spectrum).not.toHaveAttribute(
      "data-distribution-inspection-pinned",
      "true",
    );

    const spectrumBox = await spectrum.boundingBox();
    if (!spectrumBox) throw new Error("spectrum must be laid out");
    await spectrum.click({
      position: { x: spectrumBox.width * 0.75, y: spectrumBox.height * 0.5 },
    });
    await expect(spectrum).toHaveAttribute("aria-pressed", "true");
    await clear.click();

    const references = page.locator(
      "#data-chart .panel-axis [data-reference='true']",
    );
    await expect(references).toHaveText([
      "40 B",
      "80 B",
      "255 B",
      "1,020 B",
      "1,530 B",
      "6,334 B",
    ]);
    await expect(references.nth(1)).toHaveAttribute(
      "data-distribution-inspection-detail",
      /83-byte OP_RETURN script/,
    );
    await expect(references.nth(4)).toHaveAttribute(
      "data-distribution-inspection-detail",
      /JXL-n-hide/,
    );
    await expect(
      page.locator("#value-chart .spectrum-y-axis-title"),
    ).toHaveText(/transaction count|virtual size/i);
    await expect(page.locator("#value-chart .panel-axis-title")).toHaveText(
      "Total output value",
    );
    await expect(
      page.locator("#complexity-y-axis .joint-y-axis-title"),
    ).toHaveText("Outputs");
    await expect(
      page.locator("#complexity-chart .panel-axis-title"),
    ).toHaveText("Inputs");

    const density = page.locator("#joint-canvas");
    await density.scrollIntoViewIfNeeded();
    await expect(density).toHaveAttribute(
      "data-distribution-inspection-key",
      /snapshot:fee-size:cell:/,
    );
    await expect(page.locator("#joint-y-axis")).toContainText("1 KivB");
  });
});

test.describe("comparison page", () => {
  test.beforeEach(async ({ page }) => {
    await page.goto("/compare/");
    await waitForSnapshot(page, "comparison-status");
  });

  test("never scrolls horizontally", async ({ page }) => {
    await expect(page.locator("#atlas-version")).toHaveText("v1.0.0");
    expect(await documentOverflow(page)).toBeLessThanOrEqual(0);
  });

  test("states snapshot timing direction without a duplicate skew graphic", async ({
    page,
  }) => {
    const sourcePanel = page.locator(".comparison-controls");
    const transactionPanel = page.locator(".comparison-transaction-panel");
    const timing = page.locator("#sampling-panel");
    const search = page.locator("#comparison-transaction-search");
    const regions = page.locator("#comparison-regions");
    await expect(page.locator("#sampling-summary")).toHaveText(
      /Source B was observed .* after Source A\./,
    );
    await expect(page.locator("#sampling-note")).toContainText(
      /collection windows were (separated|overlapped)/i,
    );
    await expect(page.locator("#sampling-timeline")).toHaveCount(0);
    await expect(page.locator("#comparison-status")).toBeVisible();
    await expect(page.locator("#comparison-status")).toHaveAttribute(
      "data-state",
      /ready|stale/,
    );
    await expect(page.locator("#comparison-status-title")).toContainText("↔");
    await expect(page.locator("#comparison-status-detail")).toHaveText(
      /shared .* only on .* chain tip/i,
    );
    await expect(search.locator("input")).toHaveAttribute(
      "placeholder",
      "Paste a 64-character txid",
    );
    await expect(search.getByRole("button", { name: "Inspect" })).toBeVisible();
    await expect(
      sourcePanel.locator("#comparison-transaction-search"),
    ).toHaveCount(0);
    await expect(
      transactionPanel.locator("#comparison-transaction-search"),
    ).toHaveCount(1);
    await expect(
      transactionPanel.locator("#comparison-regions button"),
    ).toHaveCount(3);
    await expect(
      transactionPanel.getByRole("heading", { level: 2 }),
    ).toHaveText("Transactions");
    const sourcePanelBox = await boxOf(sourcePanel);
    const transactionPanelBox = await boxOf(transactionPanel);
    const timingBox = await boxOf(timing);
    const searchBox = await boxOf(search);
    const regionsBox = await boxOf(regions);
    const inputBox = await boxOf(search.locator("input"));
    expect(sourcePanelBox.bottom).toBeLessThanOrEqual(
      transactionPanelBox.top + 1,
    );
    expect(timingBox.bottom).toBeLessThanOrEqual(searchBox.top + 1);
    expect(searchBox.bottom).toBeLessThanOrEqual(regionsBox.top + 1);
    expect(inputBox.height).toBeGreaterThanOrEqual(40);
  });

  test("promotes different chain tips and reframes the shared population", async ({
    page,
  }) => {
    await page.goto(
      "/compare/?left=vps-core-01&right=fixture-stale-01&region=common",
    );
    await waitForSnapshot(page, "comparison-status");

    await expect(page.locator("#comparison-status")).toHaveAttribute(
      "data-state",
      "different",
    );
    await expect(page.locator("#comparison-status-title")).toContainText(
      "Different chain tips at heights",
    );
    await expect(page.locator("#comparison-status-detail")).toContainText(
      "may reflect lag or chain divergence",
    );
    await expect(page.locator("#comparison-status-detail")).toContainText(
      "retained their last complete snapshot",
    );
    await expect(page.locator("#sampling-panel")).toHaveAttribute(
      "data-chain-state",
      "different",
    );
    await expect(page.locator("#sampling-note")).toContainText(
      "can reflect node lag or chain divergence",
    );
    await expect(
      page.locator('#comparison-regions button[data-region="common"] span'),
    ).toHaveText("Observed on both reported chain tips");
  });

  test("presents Node and Compare as the primary view switch", async ({
    page,
  }) => {
    await expectProminentViewSwitch(page);
    await expect(
      page.locator('.product-nav a[aria-current="page"]'),
    ).toContainText("Compare");
  });

  test("links to the public project and author profiles", async ({ page }) => {
    await expectPublicFooter(page);
  });

  test("keeps the source-pair summary useful after Swap", async ({ page }) => {
    const title = page.locator("#comparison-status-title");
    const detail = page.locator("#comparison-status-detail");
    const initialTitle = await title.textContent();
    await page.locator("#swap-sources").click();
    await expect(page.locator("#comparison-status")).toBeVisible();
    await expect(page.locator("#comparison-status")).toHaveAttribute(
      "data-state",
      /ready|stale/,
    );
    await expect(title).not.toHaveText(initialTitle ?? "");
    await expect(title).toContainText("↔");
    await expect(detail).toHaveText(/shared .* only on .* chain tip/i);
  });

  test("keeps policy controls prominent and transaction details selection-driven", async ({
    page,
  }) => {
    const toolbar = page.locator(".comparison-policy-toolbar");
    const workspace = page.locator(".comparison-workspace");
    await expect(toolbar).toBeVisible();
    await expect(toolbar.locator("#comparison-rule-list button")).toHaveCount(
      7,
    );
    const policyBuckets = toolbar.locator("#policy-buckets");
    const bucketOverflow = await policyBuckets.evaluate((element) => ({
      clientHeight: element.clientHeight,
      overflowY: getComputedStyle(element).overflowY,
      scrollHeight: element.scrollHeight,
    }));
    expect(bucketOverflow.overflowY).not.toMatch(/auto|scroll/);
    expect(bucketOverflow.scrollHeight).toBeLessThanOrEqual(
      bucketOverflow.clientHeight + 1,
    );
    await expect(
      page.getByText("Sample transactions", { exact: true }),
    ).toHaveCount(0);
    await expect(page.locator(".comparison-policy-matrix-note")).toContainText(
      "not verdicts reported by the nodes",
    );
    await expect(page.locator("#comparison-distributions")).toHaveAttribute(
      "aria-label",
      "Node-by-node distributions",
    );

    const toolbarBox = await boxOf(toolbar);
    const workspaceBox = await boxOf(workspace);
    expect(toolbarBox.bottom).toBeLessThanOrEqual(workspaceBox.top + 1);

    const rule = toolbar.locator("#comparison-rule-list button").first();
    const fullPopulation =
      (await page.locator("#comparison-match-count").textContent()) ?? "";
    await rule.click();
    await expect(rule).toHaveAttribute("aria-pressed", "true");
    await expect(page.locator("#comparison-match-count")).not.toHaveText(
      fullPopulation,
    );
    const navigator = page.locator("#comparison-transaction-listbox");
    await navigator.focus();
    await page.keyboard.press("Enter");
    await expect(rule).toHaveAttribute("aria-pressed", "true");
    await expect(page.locator("#comparison-detail-status")).not.toHaveText(
      "Select a transaction",
    );
    expect(await documentOverflow(page)).toBeLessThanOrEqual(0);
  });

  test("keeps distribution lens and metric controls with the charts and permits repeated segment selection", async ({
    page,
  }) => {
    const distributions = page.locator("#comparison-distributions");
    const lens = distributions.locator("#dist-classifier");
    const countMetric = distributions.locator("#dist-metric-count");
    const vsizeMetric = distributions.locator("#dist-metric-vsize");

    await expect(distributions).toBeVisible();
    await expect(lens).toBeEnabled();
    await expect(vsizeMetric).toHaveAttribute("aria-pressed", "true");
    await countMetric.click();
    await expect(countMetric).toHaveAttribute("aria-pressed", "true");
    await expect(vsizeMetric).toHaveAttribute("aria-pressed", "false");
    await expect(
      distributions.locator("#dist-spectrum-left-note"),
    ).toContainText("transaction count");
    await expect(
      distributions.locator("#comparison-distribution-summary"),
    ).toContainText("weighted by transaction count");

    const propertySegments = distributions.locator(
      '#composition-left button.composition-segment[data-classifier="transaction_properties"]',
    );
    expect(await propertySegments.count()).toBeGreaterThan(1);
    const firstKey = await propertySegments.nth(0).getAttribute("data-segment");
    const secondKey = await propertySegments
      .nth(1)
      .getAttribute("data-segment");
    expect(firstKey).not.toBeNull();
    expect(secondKey).not.toBeNull();
    expect(secondKey).not.toBe(firstKey);

    const segment = (key: string) =>
      distributions.locator(
        `#composition-left button.composition-segment[data-classifier="transaction_properties"][data-segment="${key}"]`,
      );
    await segment(firstKey!).click();
    await expect(segment(firstKey!)).toHaveAttribute("aria-pressed", "true");
    await segment(secondKey!).click();
    await expect(segment(secondKey!)).toHaveAttribute("aria-pressed", "true");
    await expect(segment(firstKey!)).toHaveAttribute("aria-pressed", "false");
    await expect(lens).toHaveValue("transaction_properties");
  });

  test("keeps keyboard navigation, selection, and the canvas highlight in sync", async ({
    page,
  }) => {
    const navigator = page.locator("#comparison-transaction-listbox");
    const navigatorTxid = page.locator("#comparison-navigator-txid");
    const canvas = page.locator("#comparison-canvas");
    const selection = page.locator("#comparison-selection");

    await navigator.focus();
    const initialTxid = (await navigatorTxid.textContent()) ?? "";
    await page.keyboard.press("ArrowRight");

    await expect(navigatorTxid).not.toHaveText(initialTxid);
    const selectedTxid = (await navigatorTxid.textContent()) ?? "";
    await expect(canvas).toHaveAttribute(
      "data-rendered-transaction",
      selectedTxid,
    );
    await expect(selection).toBeVisible();
    const firstMarkerPosition = await selection.getAttribute("style");
    await expect
      .poll(() => new URL(page.url()).searchParams.get("txid"))
      .toBe(selectedTxid);
    const explorerLink = page.locator(
      "#comparison-detail .transaction-explorer-link",
    );
    await expect(explorerLink).toHaveAttribute(
      "href",
      `https://mempool.space/tx/${selectedTxid}`,
    );
    await expect(explorerLink).toHaveAttribute("target", "_blank");
    await expect(explorerLink).toHaveAttribute("rel", "noopener noreferrer");
    await expect(page.locator("#comparison-detail-status")).not.toHaveText(
      "Select a transaction",
    );
    const transactionPanel = page.locator(".comparison-transaction-panel");
    await expect(transactionPanel.locator("#comparison-detail")).toHaveCount(1);
    await expect(
      page.locator(".comparison-inspector #comparison-detail"),
    ).toHaveCount(0);
    const sourceDetails = transactionPanel.locator(
      ".comparison-detail-source .detail-transaction",
    );
    await expect(sourceDetails).toHaveCount(2);
    for (const label of [
      "wtxid",
      "Virtual size",
      "Base fee",
      "Weight",
      "Ancestors",
      "Descendants",
      "Replaceable",
      "Shape",
      "Output value",
      "Witness",
    ]) {
      await expect(sourceDetails.first()).toContainText(label);
    }

    await page.keyboard.press("ArrowRight");
    await expect(navigatorTxid).not.toHaveText(selectedTxid);
    const adjacentTxid = (await navigatorTxid.textContent()) ?? "";
    await expect(canvas).toHaveAttribute(
      "data-rendered-transaction",
      adjacentTxid,
    );
    await expect(selection).toBeVisible();
    await expect
      .poll(() => selection.getAttribute("style"))
      .not.toBe(firstMarkerPosition);
  });

  test("scrolls the policy matrix inside its own container", async ({
    page,
  }) => {
    const scroller = page.locator(".comparison-policy-matrix-scroll");
    await expect(scroller).toBeVisible();

    const { clientWidth, scrollWidth } = await scroller.evaluate((element) => ({
      clientWidth: element.clientWidth,
      scrollWidth: element.scrollWidth,
    }));

    // The matrix is deliberately wider than a phone. It has to stay reachable
    // by scrolling the container rather than being clipped or pushing the page.
    expect(scrollWidth).toBeGreaterThanOrEqual(clientWidth);
    if (scrollWidth > clientWidth) {
      expect(await documentOverflow(page)).toBeLessThanOrEqual(0);
    }
  });

  test("separates each source id from its explore link", async ({ page }) => {
    // Both are inline-level by default, so they render as one run-together
    // string: "vps-core-01Explore this node". The link has to own its line.
    const card = page.locator(".source-card").first();
    const identifier = await boxOf(card.locator("> code"));
    const link = await boxOf(card.locator(".source-card-link"));

    expect(link.top).toBeGreaterThanOrEqual(identifier.bottom);
  });

  test("stacks the source cards to full width on a phone", async ({ page }) => {
    test.skip(
      !isMobile(viewportWidth(page)),
      "cards sit side by side above 620px",
    );

    const cards = page.locator(".source-card");
    await expect(cards).toHaveCount(2);

    const limit = viewportWidth(page);
    for (const card of await cards.all()) {
      const box = await boxOf(card);
      expect(box.width).toBeGreaterThan(limit * 0.8);
      expect(box.right).toBeLessThanOrEqual(limit + 1);
    }
  });
});
