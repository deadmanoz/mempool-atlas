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

test.describe("node page", () => {
  test.beforeEach(async ({ page }) => {
    await page.goto("/");
    await waitForSnapshot(page, "page-status");
  });

  test("never scrolls horizontally", async ({ page }) => {
    await expect(page.locator("#atlas-version")).toHaveText("v1.0.0");
    expect(await documentOverflow(page)).toBeLessThanOrEqual(0);
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
