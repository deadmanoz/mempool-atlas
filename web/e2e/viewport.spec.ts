import { expect, test } from "@playwright/test";
import type { Locator, Page } from "@playwright/test";

// These assertions encode a real regression: the responsive layer for shared
// shell components once lived in `comparison-styles.css`, which only the
// comparison page imports. The node page therefore clipped its whole
// header-facts row and collapsed the txid search box to a few dozen pixels at
// phone widths, while the comparison page looked fine. Anything asserted here
// must hold on both pages.

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
    /ready|stale/,
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

test.describe("node page", () => {
  test.beforeEach(async ({ page }) => {
    await page.goto("/");
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

  test("keeps every header fact on screen", async ({ page }) => {
    const facts = page.locator(".header-facts > div");
    await expect(facts).toHaveCount(4);

    const limit = viewportWidth(page);
    for (const fact of await facts.all()) {
      const box = await boxOf(fact);
      expect(box.width).toBeGreaterThan(0);
      expect(box.height).toBeGreaterThan(0);
      // `.atlas-header` sets `overflow: hidden`, so a fact that runs past the
      // viewport is silently cut off rather than wrapped.
      expect(box.right).toBeLessThanOrEqual(limit + 1);
    }
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
    await expect(references).toHaveCount(2);
    await expect(references.nth(0)).toHaveText("40 B");
    await expect(references.nth(1)).toHaveText("80 B");
    await expect(references.nth(1)).toHaveAttribute(
      "data-distribution-inspection-detail",
      /83-byte OP_RETURN script/,
    );

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

  test("presents Node and Compare as the primary view switch", async ({
    page,
  }) => {
    await expectProminentViewSwitch(page);
    await expect(
      page.locator('.product-nav a[aria-current="page"]'),
    ).toContainText("Compare");
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
    await expect(
      page.getByText("Sample transactions", { exact: true }),
    ).toHaveCount(0);
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
