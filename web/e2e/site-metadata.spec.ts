import { expect, test } from "@playwright/test";

const pageMetadata = [
  {
    label: "Node",
    path: "/?source=vps-core-01",
    canonical: "https://atlas.deadmanoz.xyz/",
    image: "https://atlas.deadmanoz.xyz/social/mempool-atlas-og.png",
  },
  {
    label: "Compare",
    path: "/compare/",
    canonical: "https://atlas.deadmanoz.xyz/compare/",
    image: "https://atlas.deadmanoz.xyz/social/mempool-atlas-compare-og.png",
  },
] as const;

test("the public entry opens the Core versus Knots comparison", async ({
  page,
}) => {
  const requestedPaths: string[] = [];
  page.on("request", (request) => {
    requestedPaths.push(new URL(request.url()).pathname);
  });
  await page.goto("/");
  await expect(page).toHaveURL(/\/compare\/\?[^#]*left=vps-core-01/);
  await expect(page).toHaveURL(/\/compare\/\?[^#]*right=vps-knots-01/);
  await expect(
    page.locator('.product-nav a[aria-current="page"]'),
  ).toContainText("Compare");
  expect(requestedPaths).not.toContain("/src/main.ts");

  await page.goto(
    "/?utm_source=social&utm_campaign=launch&left=vps-core-01&right=vps-knots-01#comparison",
  );
  await expect(page).toHaveURL(/\/compare\/\?[^#]*left=vps-core-01/);
  let comparisonUrl = new URL(page.url());
  expect(comparisonUrl.searchParams.get("utm_source")).toBe("social");
  expect(comparisonUrl.searchParams.get("utm_campaign")).toBe("launch");
  expect(comparisonUrl.hash).toBe("#comparison");

  await page
    .getByRole("button", { name: "Swap source A and source B" })
    .click();
  comparisonUrl = new URL(page.url());
  expect(comparisonUrl.searchParams.get("utm_source")).toBe("social");
  expect(comparisonUrl.searchParams.get("utm_campaign")).toBe("launch");
});

test("node interactions retain campaign attribution", async ({ page }) => {
  await page.goto("/?source=vps-core-01&utm_source=social");
  await expect(page.locator("#page-status")).toHaveAttribute(
    "data-state",
    /ready|stale/,
  );
  await page.locator("#mode-vsize").click();
  const nodeUrl = new URL(page.url());
  expect(nodeUrl.searchParams.get("source")).toBe("vps-core-01");
  expect(nodeUrl.searchParams.get("utm_source")).toBe("social");
});

for (const metadata of pageMetadata) {
  test(`${metadata.label} publishes canonical social metadata`, async ({
    page,
  }) => {
    await page.goto(metadata.path);
    await expect(page.locator('link[rel="canonical"]')).toHaveAttribute(
      "href",
      metadata.canonical,
    );
    await expect(page.locator('meta[property="og:url"]')).toHaveAttribute(
      "content",
      metadata.canonical,
    );
    await expect(page.locator('meta[property="og:image"]')).toHaveAttribute(
      "content",
      metadata.image,
    );
    await expect(
      page.locator('meta[property="og:image:width"]'),
    ).toHaveAttribute("content", "1200");
    await expect(
      page.locator('meta[property="og:image:height"]'),
    ).toHaveAttribute("content", "630");
    await expect(page.locator('meta[name="twitter:card"]')).toHaveAttribute(
      "content",
      "summary_large_image",
    );
    await expect(page.locator('meta[name="twitter:image"]')).toHaveAttribute(
      "content",
      metadata.image,
    );
    await expect(page.locator('meta[name="theme-color"]')).toHaveAttribute(
      "content",
      "#071018",
    );
    await expect(page.locator('link[rel="manifest"]')).toHaveAttribute(
      "href",
      "/site.webmanifest",
    );
    await expect(page.locator('link[rel="icon"]')).toHaveCount(3);
    await expect(page.locator('link[rel="apple-touch-icon"]')).toHaveAttribute(
      "href",
      "/apple-touch-icon.png",
    );
    await expect(page.locator('link[rel="me"]')).toHaveCount(2);
  });
}

test("public identity assets are served", async ({ request }) => {
  for (const path of [
    "/social/mempool-atlas-og.png",
    "/social/mempool-atlas-compare-og.png",
    "/favicon.ico",
    "/favicon-16x16.png",
    "/favicon-32x32.png",
    "/apple-touch-icon.png",
    "/icons/mempool-atlas-192x192.png",
    "/icons/mempool-atlas-512x512.png",
  ]) {
    const response = await request.get(path);
    expect(response.ok(), `${path} should be served`).toBe(true);
    expect(response.headers()["content-type"]).toMatch(/^image\//);
  }

  const manifestResponse = await request.get("/site.webmanifest");
  expect(manifestResponse.ok()).toBe(true);
  await expect(manifestResponse.json()).resolves.toMatchObject({
    name: "Mempool Atlas",
    short_name: "Atlas",
    theme_color: "#071018",
  });

  const robotsResponse = await request.get("/robots.txt");
  expect(await robotsResponse.text()).toContain(
    "Sitemap: https://atlas.deadmanoz.xyz/sitemap.xml",
  );

  const sitemapResponse = await request.get("/sitemap.xml");
  expect(await sitemapResponse.text()).toContain(
    "https://atlas.deadmanoz.xyz/compare/",
  );
});
