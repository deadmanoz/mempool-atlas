import { expect, test } from "@playwright/test";
import type { Page, Route, TestInfo } from "@playwright/test";

const GOOD_CLS_THRESHOLD = 0.1;

interface ManifestGate {
  release(): void;
  waitForRequests(count: number): Promise<void>;
}

interface CompletionStageGate {
  release(): void;
  waitForRequests(count: number): Promise<void>;
}

interface CandidateGate {
  release(): Promise<void>;
  wait(): Promise<void>;
}

type CandidateGateWindow = Window &
  typeof globalThis & {
    __atlasCandidateGateSeen?: number;
    __atlasCandidateGateRelease?: () => void;
    __atlasCandidateReadyHook?: () => void | Promise<void>;
  };

interface LayoutShiftMetric {
  observer: PerformanceObserver;
  score: number;
  shifts: Array<{ sources: string[]; value: number }>;
}

type MetricWindow = Window &
  typeof globalThis & {
    __atlasLayoutShiftMetric: LayoutShiftMetric;
  };

const installLayoutShiftObserver = async (page: Page): Promise<void> => {
  await page.addInitScript(() => {
    const metric: LayoutShiftMetric = {
      observer: null as unknown as PerformanceObserver,
      score: 0,
      shifts: [],
    };
    const observer = new PerformanceObserver((list) => {
      for (const entry of list.getEntries()) {
        const shift = entry as PerformanceEntry & {
          hadRecentInput: boolean;
          sources?: Array<{ node?: Node }>;
          value: number;
        };
        if (!shift.hadRecentInput) {
          metric.score += shift.value;
          metric.shifts.push({
            sources: (shift.sources ?? []).map(({ node }) => {
              if (!(node instanceof Element)) {
                return node?.nodeName ?? "unknown";
              }
              const identifier = node.id.length > 0 ? `#${node.id}` : "";
              const classes = [...node.classList]
                .slice(0, 2)
                .map((name) => `.${name}`)
                .join("");
              return `${node.tagName.toLowerCase()}${identifier}${classes}`;
            }),
            value: shift.value,
          });
        }
      }
    });
    metric.observer = observer;
    (window as MetricWindow).__atlasLayoutShiftMetric = metric;
    observer.observe({ type: "layout-shift", buffered: true });
  });
};

const waitForRendering = async (page: Page): Promise<void> => {
  await page.evaluate(
    () =>
      new Promise<void>((resolve) => {
        requestAnimationFrame(() => {
          requestAnimationFrame(() => resolve());
        });
      }),
  );
};

interface SelectedStrokePixelProfile {
  backingWidth: number;
  backingHeight: number;
  count: number;
  solidCount: number;
  redTotal: number;
  greenTotal: number;
  blueTotal: number;
}

const selectedTerrainStrokePixelProfile = async (
  page: Page,
): Promise<SelectedStrokePixelProfile> => {
  await page.goto(
    "/?source=vps-core-01&classifier=knots_bip110&rule=element_size",
  );
  await expect(page.locator("#page-status")).toHaveAttribute(
    "data-readiness",
    "complete-feature-ready",
  );
  await page.locator("#terrain-tab").click();
  await expect(
    page.locator('#rule-list button[data-rule="element_size"]'),
  ).toHaveAttribute("aria-pressed", "true");
  await waitForRendering(page);
  return page.locator("#terrain-canvas").evaluate((canvas) => {
    const context = (canvas as HTMLCanvasElement).getContext("2d");
    if (context === null) throw new Error("terrain canvas has no 2D context");
    const profile: SelectedStrokePixelProfile = {
      backingWidth: context.canvas.width,
      backingHeight: context.canvas.height,
      count: 0,
      solidCount: 0,
      redTotal: 0,
      greenTotal: 0,
      blueTotal: 0,
    };
    const pixels = context.getImageData(
      0,
      0,
      context.canvas.width,
      context.canvas.height,
    ).data;
    for (let index = 0; index < pixels.length; index += 4) {
      const red = pixels[index] ?? 0;
      const green = pixels[index + 1] ?? 0;
      const blue = pixels[index + 2] ?? 0;
      const alpha = pixels[index + 3] ?? 0;
      if (
        red >= 80 &&
        red <= 150 &&
        green >= 230 &&
        blue >= 225 &&
        Math.abs(green - blue) <= 12 &&
        alpha > 0
      ) {
        profile.count += 1;
        profile.redTotal += red;
        profile.greenTotal += green;
        profile.blueTotal += blue;
        if (red === 110 && green === 242 && blue === 240 && alpha === 255) {
          profile.solidCount += 1;
        }
      }
    }
    return profile;
  });
};

const resetLayoutShiftScore = async (page: Page): Promise<void> => {
  await waitForRendering(page);
  await page.evaluate(() => {
    const metric = (window as MetricWindow).__atlasLayoutShiftMetric;
    metric.observer.takeRecords();
    metric.score = 0;
    metric.shifts = [];
  });
};

const layoutShiftMetric = async (
  page: Page,
): Promise<{ score: number; shifts: LayoutShiftMetric["shifts"] }> => {
  await waitForRendering(page);
  return page.evaluate(() => {
    const { score, shifts } = (window as MetricWindow).__atlasLayoutShiftMetric;
    return { score, shifts };
  });
};

const expectStableLayout = async (page: Page): Promise<void> => {
  const metric = await layoutShiftMetric(page);
  expect(
    metric.score,
    `unexpected layout shifts: ${JSON.stringify(metric.shifts)}`,
  ).toBeLessThanOrEqual(GOOD_CLS_THRESHOLD);
};

const installManifestGate = async (page: Page): Promise<ManifestGate> => {
  let releaseGate: (() => void) | null = null;
  let requestCount = 0;
  const held = new Promise<void>((resolve) => {
    releaseGate = resolve;
  });

  await page.route(
    /\/api\/v2\/sources\/[^/]+\/mempool(?:\?.*)?$/,
    async (route: Route) => {
      requestCount += 1;
      await held;
      await route.continue();
    },
  );

  return {
    release: () => releaseGate?.(),
    waitForRequests: async (count) => {
      await expect
        .poll(() => requestCount, {
          message: `expected ${count} held manifest request(s)`,
        })
        .toBe(count);
    },
  };
};

const installCompletionStageGate = async (
  page: Page,
): Promise<CompletionStageGate> => {
  let releaseGate: (() => void) | null = null;
  let requestCount = 0;
  const held = new Promise<void>((resolve) => {
    releaseGate = resolve;
  });

  await page.route(
    /\/api\/v2\/sources\/[^/]+\/mempool\/stages\/(?:membership|structure)\//,
    async (route: Route) => {
      requestCount += 1;
      await held;
      await route.continue();
    },
  );

  return {
    release: () => releaseGate?.(),
    waitForRequests: async (count) => {
      await expect
        .poll(() => requestCount, {
          message: `expected at least ${count} held completion-stage request(s)`,
        })
        .toBeGreaterThanOrEqual(count);
    },
  };
};

const installCandidateGate = async (page: Page): Promise<CandidateGate> => {
  await page.evaluate(() => {
    const target = window as CandidateGateWindow;
    let held = true;
    target.__atlasCandidateGateSeen = 0;
    target.__atlasCandidateReadyHook = async () => {
      target.__atlasCandidateGateSeen =
        (target.__atlasCandidateGateSeen ?? 0) + 1;
      if (!held) return;
      await new Promise<void>((resolve) => {
        target.__atlasCandidateGateRelease = () => {
          held = false;
          resolve();
        };
      });
    };
  });
  return {
    wait: async () => {
      await expect
        .poll(() =>
          page.evaluate(
            () => (window as CandidateGateWindow).__atlasCandidateGateSeen ?? 0,
          ),
        )
        .toBeGreaterThan(0);
    },
    release: () =>
      page.evaluate(() => {
        (window as CandidateGateWindow).__atlasCandidateGateRelease?.();
      }),
  };
};

const waitForCandidateCommit = async (
  page: Page,
  surface: "node" | "comparison",
): Promise<void> => {
  await expect
    .poll(() =>
      page.evaluate(
        (name) => performance.getEntriesByName(name).length,
        `atlas:${surface}:replacement-committed`,
      ),
    )
    .toBeGreaterThan(0);
};

const useMinimumSupportedWidth = async (
  page: Page,
  testInfo: TestInfo,
): Promise<void> => {
  if (testInfo.project.name === "mobile") {
    await page.setViewportSize({ width: 320, height: 844 });
  }
};

const documentOverflow = async (page: Page): Promise<number> =>
  page.evaluate(() => {
    const root = document.documentElement;
    return (
      Math.max(root.scrollWidth, document.body.scrollWidth) - root.clientWidth
    );
  });

test.describe("early source metadata", () => {
  test.beforeEach(async ({ page }, testInfo) => {
    await useMinimumSupportedWidth(page, testInfo);
    await installLayoutShiftObserver(page);
  });

  test("presents an expected first publication wait without an outage", async ({
    page,
  }) => {
    await page.route(/\/api\/v2\/sources(?:\?.*)?$/, async (route) => {
      const upstream = await route.fetch();
      const body = (await upstream.json()) as {
        sources: Array<Record<string, unknown>>;
      };
      body.sources[0] = {
        ...body.sources[0],
        availability: "waiting",
        last_poll_started_at_ms: null,
        snapshot_observed_at_ms: null,
        chain_tip: null,
        transaction_count: null,
        total_vsize: null,
        classification: null,
        last_error: null,
      };
      await route.fulfill({ json: body });
    });
    await page.route(
      /\/api\/v2\/sources\/vps-core-01\/mempool(?:\?.*)?$/,
      async (route) => {
        await route.fulfill({
          status: 503,
          contentType: "application/problem+json",
          json: {
            type: "v2_unavailable",
            title: "Current v2 publication unavailable",
            status: 503,
            detail:
              "Atlas has not published a current complete snapshot for this source.",
          },
        });
      },
    );

    await page.goto("/?source=vps-core-01");

    const status = page.locator("#page-status");
    await expect(status).toHaveAttribute("data-state", "waiting");
    await expect(page.locator("#status-title")).toHaveText(
      "Waiting for first snapshot",
    );
    await expect(page.locator("#status-detail")).toContainText(
      "No complete observation has been published yet",
    );
    await expect(page.getByText("Atlas website unavailable")).toHaveCount(0);
    await expect(page.locator("#source-summary")).toHaveAttribute(
      "data-state",
      "waiting",
    );
    await expect(page.locator("#source-summary")).toContainText(
      "Classification waiting",
    );
    await expect(page.locator("#refresh")).toBeEnabled();
  });

  test("presents a failed first poll as an outage", async ({ page }) => {
    let discoveries = 0;
    await page.route(/\/api\/v2\/sources(?:\?.*)?$/, async (route) => {
      const upstream = await route.fetch();
      const body = (await upstream.json()) as {
        sources: Array<Record<string, unknown>>;
      };
      discoveries += 1;
      body.sources[0] = {
        ...body.sources[0],
        availability: discoveries === 1 ? "waiting" : "error",
        last_poll_started_at_ms: null,
        snapshot_observed_at_ms: null,
        chain_tip: null,
        transaction_count: null,
        total_vsize: null,
        classification: null,
        last_error: discoveries === 1 ? null : "Node RPC connection failed",
      };
      await route.fulfill({ json: body });
    });
    await page.route(
      /\/api\/v2\/sources\/vps-core-01\/mempool(?:\?.*)?$/,
      async (route) => {
        await route.fulfill({
          status: 503,
          contentType: "application/problem+json",
          json: {
            type: "v2_unavailable",
            title: "Current v2 publication unavailable",
            status: 503,
            detail:
              "Atlas has not published a current complete snapshot for this source.",
          },
        });
      },
    );

    await page.goto("/?source=vps-core-01");

    await expect(page.locator("#page-status")).toHaveAttribute(
      "data-state",
      "error",
    );
    await expect(page.locator("#status-title")).toHaveText(
      "Atlas website unavailable",
    );
    await expect(page.locator("#status-detail")).toHaveText(
      "Node RPC connection failed",
    );
  });

  test("clears the busy summary when first-publication rediscovery fails", async ({
    page,
  }) => {
    let discoveries = 0;
    await page.route(/\/api\/v2\/sources(?:\?.*)?$/, async (route) => {
      discoveries += 1;
      if (discoveries === 1) {
        await route.continue();
        return;
      }
      await route.fulfill({
        status: 503,
        contentType: "application/problem+json",
        json: {
          type: "sources_unavailable",
          title: "Source discovery unavailable",
          status: 503,
          detail: "Unable to refresh source metadata.",
        },
      });
    });
    await page.route(
      /\/api\/v2\/sources\/vps-core-01\/mempool(?:\?.*)?$/,
      async (route) => {
        await route.fulfill({
          status: 503,
          contentType: "application/problem+json",
          json: {
            type: "v2_unavailable",
            title: "Current v2 publication unavailable",
            status: 503,
            detail:
              "Atlas has not published a current complete snapshot for this source.",
          },
        });
      },
    );

    await page.goto("/?source=vps-core-01");

    const summary = page.locator("#source-summary");
    await expect(page.locator("#page-status")).toHaveAttribute(
      "data-phase",
      "discovering-sources",
    );
    await expect(page.locator("#page-status")).toHaveAttribute(
      "data-state",
      "error",
    );
    await expect(summary).toHaveAttribute("data-phase", "discovering-sources");
    await expect(summary).toHaveAttribute("data-state", "error");
    await expect(summary).toHaveAttribute("aria-busy", "false");
    await expect(page.locator("#status-title")).toHaveText(
      "Atlas website unavailable",
    );
    await expect(page.locator("#refresh")).toBeEnabled();
  });

  test("keeps the node summary useful while the snapshot is loading", async ({
    page,
  }) => {
    const gate = await installManifestGate(page);
    try {
      await page.goto("/");
      await gate.waitForRequests(1);

      const summary = page.locator("#source-summary");
      await expect(summary).toHaveAttribute("data-phase", "metadata-ready");
      await expect(summary).toHaveAttribute("aria-busy", "true");
      await expect(page.locator("#source-id")).toHaveText("vps-core-01");
      await expect(page.locator("#transaction-count")).toHaveText("700");
      expect(await documentOverflow(page)).toBeLessThanOrEqual(0);

      await resetLayoutShiftScore(page);
      gate.release();

      await expect(page.locator("#page-status")).toHaveAttribute(
        "data-state",
        /ready|stale/,
      );
      await expect(summary).toHaveAttribute("data-phase", "interactive");
      await expect(summary).toHaveAttribute("aria-busy", "false");
      await expectStableLayout(page);
      expect(await documentOverflow(page)).toBeLessThanOrEqual(0);
    } finally {
      gate.release();
    }
  });

  test("keeps both comparison summaries useful while snapshots load", async ({
    page,
  }) => {
    const gate = await installManifestGate(page);
    try {
      await page.goto("/compare/");
      await gate.waitForRequests(2);

      const cards = page.locator(".source-card");
      await expect(cards).toHaveCount(2);
      for (const card of await cards.all()) {
        await expect(card).toHaveAttribute("data-phase", "metadata-ready");
        await expect(card).toHaveAttribute("aria-busy", "true");
        await expect(card.locator("dl")).toContainText("700 tx");
      }
      expect(await documentOverflow(page)).toBeLessThanOrEqual(0);

      await resetLayoutShiftScore(page);
      gate.release();

      await expect(page.locator("#comparison-status")).toHaveAttribute(
        "data-state",
        /ready|stale/,
      );
      for (const card of await cards.all()) {
        await expect(card).toHaveAttribute("data-phase", "interactive");
        await expect(card).toHaveAttribute("aria-busy", "false");
      }
      await expectStableLayout(page);
      expect(await documentOverflow(page)).toBeLessThanOrEqual(0);
    } finally {
      gate.release();
    }
  });
});

test.describe("progressive v2 publications", () => {
  test("makes the primary node view interactive before membership completes", async ({
    page,
  }) => {
    const gate = await installCompletionStageGate(page);
    const pageErrors: Error[] = [];
    page.on("pageerror", (error) => pageErrors.push(error));
    try {
      await page.goto("/");
      await gate.waitForRequests(2);

      const status = page.locator("#page-status");
      await expect(status).toHaveAttribute(
        "data-readiness",
        "primary-interactive",
      );
      await expect(status).toHaveAttribute("data-state", /ready|stale/);
      await expect(page.locator("#classification-lens-select")).toBeDisabled();
      await expect(page.locator("#fee-age-tab")).toBeDisabled();
      const classifierControls = page.locator("#classification-labels button");
      await expect(classifierControls.first()).toBeEnabled();
      const selectedControl = page.locator(
        '#classification-labels button[data-label="version_2"]',
      );
      await selectedControl.click();
      await expect(selectedControl).toHaveAttribute("aria-pressed", "true");
      await expect(classifierControls.first()).toHaveAttribute(
        "aria-pressed",
        "false",
      );
      await selectedControl.focus();
      await expect(selectedControl).toBeFocused();
      const metricToggle = page.locator("#mode-vsize");
      await metricToggle.click();
      await expect(metricToggle).toHaveAttribute("aria-pressed", "true");
      await expect(page.locator("#snapshot-distributions")).toHaveAttribute(
        "aria-busy",
        "false",
      );
      await expect(status).toHaveAttribute("data-state", /ready|stale/);
      await expect(page.locator("#status-title")).toHaveText(
        "Snapshot classifications ready",
      );
      expect(pageErrors).toEqual([]);
      await selectedControl.focus();
      await expect(selectedControl).toBeFocused();

      gate.release();
      await expect(status).toHaveAttribute(
        "data-readiness",
        "complete-feature-ready",
      );
      await expect(page.locator("#classification-lens-select")).toBeEnabled();
      await expect(page.locator("#fee-age-tab")).toBeEnabled();
      await expect(selectedControl).toHaveAttribute("aria-pressed", "true");
      await expect(classifierControls.first()).toHaveAttribute(
        "aria-pressed",
        "false",
      );
      await expect(metricToggle).toHaveAttribute("aria-pressed", "true");
      await expect(page.locator("#distribution-grid")).toBeVisible();
      await expect(selectedControl).toBeFocused();
      expect(pageErrors).toEqual([]);
    } finally {
      gate.release();
    }
  });

  test("makes policy comparison interactive before membership details complete", async ({
    page,
  }) => {
    const gate = await installCompletionStageGate(page);
    let detailRequests = 0;
    await page.route(
      /\/api\/v2\/sources\/[^/]+\/transactions\//,
      async (route: Route) => {
        detailRequests += 1;
        await route.continue();
      },
    );
    try {
      await page.goto("/compare/");
      await gate.waitForRequests(4);

      const status = page.locator("#comparison-status");
      await expect(status).toHaveAttribute(
        "data-readiness",
        "primary-interactive",
      );
      await expect(status).toHaveAttribute("data-state", /ready|stale/);
      await expect(page.locator("#comparison-policy-matrix")).toBeVisible();
      const commonRegion = page.locator(
        '#comparison-regions button[data-region="common"]',
      );
      await expect(commonRegion).toBeEnabled();
      await commonRegion.click();
      await expect(commonRegion).toHaveAttribute("aria-pressed", "true");
      const sample = page
        .locator("#comparison-transactions .tx-select")
        .first();
      await expect(sample).toBeEnabled();
      await sample.click();
      await expect(page.locator("#comparison-detail-status")).toContainText(
        "Membership details are still loading",
      );
      expect(detailRequests).toBe(0);

      gate.release();
      await expect(status).toHaveAttribute(
        "data-readiness",
        "complete-feature-ready",
      );
      await expect.poll(() => detailRequests).toBeGreaterThan(0);
    } finally {
      gate.release();
    }
  });

  test("keeps a comparison source link focused when completion stages arrive", async ({
    page,
  }) => {
    const gate = await installCompletionStageGate(page);
    try {
      await page.goto("/compare/");
      await gate.waitForRequests(4);

      const status = page.locator("#comparison-status");
      await expect(status).toHaveAttribute(
        "data-readiness",
        "primary-interactive",
      );
      const sourceLink = page.locator(".source-card .source-card-link").first();
      await sourceLink.focus();
      await expect(sourceLink).toBeFocused();

      gate.release();
      await expect(status).toHaveAttribute(
        "data-readiness",
        "complete-feature-ready",
      );
      await expect(sourceLink).toBeFocused();
    } finally {
      gate.release();
    }
  });
});

test.describe("atomic publication replacement", () => {
  test("keeps the complete node view interactive until one coherent refresh commits", async ({
    page,
  }) => {
    await page.goto("/");
    await expect(page.locator("#page-status")).toHaveAttribute(
      "data-readiness",
      "complete-feature-ready",
    );
    const transactionCount = await page
      .locator("#transaction-count")
      .textContent();
    const gate = await installCandidateGate(page);

    await page.locator("#refresh").click();
    await gate.wait();
    await expect(page.locator("#source-summary")).toHaveAttribute(
      "data-phase",
      "interactive",
    );
    await expect(page.locator("#transaction-count")).toHaveText(
      transactionCount ?? "",
    );
    await page.locator("#mode-vsize").click();
    await expect(page.locator("#mode-vsize")).toHaveAttribute(
      "aria-pressed",
      "true",
    );

    await gate.release();
    await waitForCandidateCommit(page, "node");
    await expect(page.locator("#mode-vsize")).toHaveAttribute(
      "aria-pressed",
      "true",
    );
    await expect(page.locator("#refresh")).toBeEnabled();
  });

  test("keeps the complete comparison and its controls active until refresh commit", async ({
    page,
  }) => {
    await page.goto("/compare/");
    await expect(page.locator("#comparison-status")).toHaveAttribute(
      "data-readiness",
      "complete-feature-ready",
    );
    const sourceIds = await page
      .locator(".source-card > code")
      .allTextContents();
    const gate = await installCandidateGate(page);

    await page.locator("#comparison-refresh").click();
    await gate.wait();
    await expect(page.locator(".source-card > code")).toHaveText(sourceIds);
    await page.locator("#dist-scope-common").click();
    await expect(page.locator("#dist-scope-common")).toHaveAttribute(
      "aria-pressed",
      "true",
    );

    await gate.release();
    await waitForCandidateCommit(page, "comparison");
    await expect(page.locator("#dist-scope-common")).toHaveAttribute(
      "aria-pressed",
      "true",
    );
    await expect(page.locator("#comparison-refresh")).toBeEnabled();
  });
});

test.describe("policy terrain raster", () => {
  test("matches direct selected-region stroke pixels", async ({
    context,
    page,
  }) => {
    const rasterProfile = await selectedTerrainStrokePixelProfile(page);
    const directPage = await context.newPage();
    await directPage.addInitScript(() => {
      Reflect.deleteProperty(globalThis, "Path2D");
    });
    try {
      const directProfile = await selectedTerrainStrokePixelProfile(directPage);
      expect(rasterProfile.count).toBeGreaterThan(0);
      expect(directProfile.count).toBeGreaterThan(0);
      const countTolerance = Math.max(4, Math.ceil(directProfile.count * 0.01));
      expect(
        Math.abs(rasterProfile.count - directProfile.count),
      ).toBeLessThanOrEqual(countTolerance);
      expect(
        Math.abs(rasterProfile.solidCount - directProfile.solidCount),
      ).toBeLessThanOrEqual(countTolerance);
      const channelTolerance = 320 + countTolerance * 255;
      expect(
        Math.abs(rasterProfile.redTotal - directProfile.redTotal),
      ).toBeLessThanOrEqual(channelTolerance);
      expect(
        Math.abs(rasterProfile.greenTotal - directProfile.greenTotal),
      ).toBeLessThanOrEqual(channelTolerance);
      expect(
        Math.abs(rasterProfile.blueTotal - directProfile.blueTotal),
      ).toBeLessThanOrEqual(channelTolerance);
    } finally {
      await directPage.close();
    }
  });

  test("uses exact direct paint above the retained-raster cap", async ({
    browser,
  }, testInfo) => {
    test.skip(testInfo.project.name !== "desktop", "high-DPI case runs once");
    const baseURL = testInfo.project.use.baseURL;
    if (typeof baseURL !== "string") {
      throw new Error("Playwright project must configure a baseURL");
    }
    const context = await browser.newContext({
      baseURL,
      viewport: { width: 1_440, height: 900 },
      deviceScaleFactor: 4,
    });
    const path2dPage = await context.newPage();
    const directPage = await context.newPage();
    await directPage.addInitScript(() => {
      Reflect.deleteProperty(globalThis, "Path2D");
    });
    try {
      const path2dProfile = await selectedTerrainStrokePixelProfile(path2dPage);
      const directProfile = await selectedTerrainStrokePixelProfile(directPage);
      expect(
        path2dProfile.backingWidth * path2dProfile.backingHeight,
      ).toBeGreaterThan(4_194_304);
      expect(path2dProfile).toEqual(directProfile);
    } finally {
      await context.close();
    }
  });
});
