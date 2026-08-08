import { expect, test } from "@playwright/test";
import type { Page, Route, TestInfo } from "@playwright/test";

import type { AtlasWorkerRequest } from "../src/atlas-worker-protocol";

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

interface PublicationAttemptGate {
  release(): Promise<void>;
  waitForAttempt(count: number): Promise<void>;
}

interface ComparisonPaintGate {
  release(): Promise<void>;
  waitForRequests(count: number): Promise<void>;
}

type CandidateGateWindow = Window &
  typeof globalThis & {
    __atlasCandidateGateSeen?: number;
    __atlasCandidateGateRelease?: () => void;
    __atlasCandidateReadyHook?: () => void | Promise<void>;
  };

type PublicationAttemptGateWindow = Window &
  typeof globalThis & {
    __atlasPublicationAttemptSeen?: number;
    __atlasPublicationAttemptRelease?: () => void;
    __atlasComparisonCandidateMetricOffset?: number;
    __atlasPublicationAttemptHook?: (detail: {
      surface: "node" | "comparison";
      complete: boolean;
    }) => void | Promise<void>;
  };

type ComparisonPaintGateWindow = Window &
  typeof globalThis & {
    __atlasComparisonPaintGateSeen?: number;
    __atlasComparisonPaintGateRelease?: () => void;
  };

type WorkerRequestWindow = Window &
  typeof globalThis & {
    __atlasWorkerRequests?: AtlasWorkerRequest[];
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

const installPublicationAttemptGate = async (
  page: Page,
  surface: "node" | "comparison",
): Promise<PublicationAttemptGate> => {
  await page.addInitScript((targetSurface) => {
    const target = window as PublicationAttemptGateWindow;
    target.__atlasPublicationAttemptSeen = 0;
    target.__atlasPublicationAttemptHook = async ({ surface, complete }) => {
      if (surface !== targetSurface || complete) return;
      target.__atlasPublicationAttemptSeen =
        (target.__atlasPublicationAttemptSeen ?? 0) + 1;
      await new Promise<void>((resolve) => {
        target.__atlasPublicationAttemptRelease = resolve;
      });
    };
  }, surface);
  return {
    waitForAttempt: async (count) => {
      await expect
        .poll(() =>
          page.evaluate(
            () =>
              (window as PublicationAttemptGateWindow)
                .__atlasPublicationAttemptSeen ?? 0,
          ),
        )
        .toBe(count);
    },
    release: () =>
      page.evaluate(() => {
        const target = window as PublicationAttemptGateWindow;
        const release = target.__atlasPublicationAttemptRelease;
        delete target.__atlasPublicationAttemptRelease;
        release?.();
      }),
  };
};

const installComparisonPaintGate = async (
  page: Page,
): Promise<ComparisonPaintGate> => {
  await page.addInitScript(() => {
    const target = window as ComparisonPaintGateWindow;
    const requestFrame = window.requestAnimationFrame.bind(window);
    const cancelFrame = window.cancelAnimationFrame.bind(window);
    const held = new Map<number, FrameRequestCallback>();
    let nextHandle = -1;
    target.__atlasComparisonPaintGateSeen = 0;
    window.requestAnimationFrame = (callback) => {
      if (
        (callback as FrameRequestCallback & { __atlasPerfLabel?: string })
          .__atlasPerfLabel !== "comparison-paint"
      ) {
        return requestFrame(callback);
      }
      const handle = nextHandle;
      nextHandle -= 1;
      held.set(handle, callback);
      target.__atlasComparisonPaintGateSeen =
        (target.__atlasComparisonPaintGateSeen ?? 0) + 1;
      return handle;
    };
    window.cancelAnimationFrame = (handle) => {
      if (!held.delete(handle)) cancelFrame(handle);
    };
    target.__atlasComparisonPaintGateRelease = () => {
      window.requestAnimationFrame = requestFrame;
      window.cancelAnimationFrame = cancelFrame;
      const callbacks = [...held.values()];
      held.clear();
      for (const callback of callbacks) requestFrame(callback);
    };
  });
  return {
    waitForRequests: async (count) => {
      await expect
        .poll(() =>
          page.evaluate(
            () =>
              (window as ComparisonPaintGateWindow)
                .__atlasComparisonPaintGateSeen ?? 0,
          ),
        )
        .toBeGreaterThanOrEqual(count);
    },
    release: () =>
      page.evaluate(() => {
        (
          window as ComparisonPaintGateWindow
        ).__atlasComparisonPaintGateRelease?.();
      }),
  };
};

const installWorkerRequestObserver = async (page: Page): Promise<void> => {
  await page.addInitScript(() => {
    const target = window as WorkerRequestWindow;
    target.__atlasWorkerRequests = [];
    const postMessage = Worker.prototype.postMessage;
    Worker.prototype.postMessage = function (
      message: unknown,
      transferOrOptions?: StructuredSerializeOptions | Transferable[],
    ): void {
      if (
        typeof message === "object" &&
        message !== null &&
        "type" in message &&
        (message.type === "load" || message.type === "cancel")
      ) {
        target.__atlasWorkerRequests?.push(
          structuredClone(message) as AtlasWorkerRequest,
        );
      }
      Reflect.apply(
        postMessage,
        this,
        transferOrOptions === undefined
          ? [message]
          : [message, transferOrOptions],
      );
    };
  });
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

  test("replaces pending panel copy when a switched source has no first publication", async ({
    page,
  }) => {
    const pageErrors: Error[] = [];
    page.on("pageerror", (error) => pageErrors.push(error));
    await page.goto("/?source=vps-core-01");
    await expect(page.locator("#page-status")).toHaveAttribute(
      "data-readiness",
      "complete-feature-ready",
    );
    await page.route(
      /\/api\/v2\/sources\/vps-knots-01\/mempool(?:\?.*)?$/,
      async (route) => {
        await route.fulfill({
          status: 503,
          contentType: "application/problem+json",
          json: {
            type: "v2_unavailable",
            title: "Current v2 publication unavailable",
            status: 503,
            detail: "The selected source has no current v2 publication.",
          },
        });
      },
    );

    await page.locator("#source-select").selectOption("vps-knots-01");
    await expect(page.locator("#page-status")).toHaveAttribute(
      "data-state",
      "error",
    );
    await expect(page.locator("#status-title")).toHaveText(
      "Atlas website unavailable",
    );
    const detail = (await page.locator("#status-detail").textContent()) ?? "";
    expect(detail).not.toContain("Loading");
    for (const selector of [
      "#terrain-empty",
      "#fee-age-empty",
      "#distribution-empty",
    ]) {
      await expect(page.locator(selector)).toHaveText(detail);
    }
    await expect(page.locator("#source-summary")).toHaveAttribute(
      "aria-busy",
      "false",
    );
    expect(pageErrors).toEqual([]);
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
      await page.goto("/?source=vps-core-01");
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
  test("keeps an absent exact transaction search safe while membership loads", async ({
    page,
  }) => {
    const gate = await installCompletionStageGate(page);
    const pageErrors: Error[] = [];
    page.on("pageerror", (error) => pageErrors.push(error));
    const absentTxid = "0".repeat(64);
    try {
      await page.goto("/?source=vps-core-01");
      await gate.waitForRequests(2);
      const status = page.locator("#page-status");
      await expect(status).toHaveAttribute(
        "data-readiness",
        "primary-interactive",
      );
      await page.locator("#transaction-search-input").fill(absentTxid);
      await page.locator("#transaction-search").evaluate((form) => {
        (form as HTMLFormElement).requestSubmit();
      });

      await expect(page.locator("#transaction-search-status")).toHaveAttribute(
        "data-state",
        "absent",
      );
      await expect(page.locator("#detail-status")).toHaveText(
        "Not present in this snapshot",
      );
      await expect
        .poll(() => new URL(page.url()).searchParams.get("txid"))
        .toBe(absentTxid);
      expect(pageErrors).toEqual([]);

      gate.release();
      await expect(status).toHaveAttribute(
        "data-readiness",
        "complete-feature-ready",
      );
      await expect(status).toHaveAttribute("data-state", /ready|stale/);
      await expect(page.locator("#transaction-search-status")).toHaveAttribute(
        "data-state",
        "absent",
      );
      await expect
        .poll(() => new URL(page.url()).searchParams.get("txid"))
        .toBe(absentTxid);
      expect(pageErrors).toEqual([]);
    } finally {
      gate.release();
    }
  });

  test("finishes loading after primary metric churn exhausts its commit budget", async ({
    page,
  }) => {
    const gate = await installPublicationAttemptGate(page, "node");
    const pageErrors: Error[] = [];
    page.on("pageerror", (error) => pageErrors.push(error));
    await page.goto("/?source=vps-core-01");

    const metrics = ["vsize", "count", "vsize", "count"] as const;
    for (let index = 0; index < metrics.length; index += 1) {
      await gate.waitForAttempt(index + 1);
      const metric = metrics[index]!;
      await page.locator(`#mode-${metric}`).evaluate((button) => {
        (button as HTMLButtonElement).click();
      });
      await expect(page.locator(`#mode-${metric}`)).toHaveAttribute(
        "aria-pressed",
        "true",
      );
      await gate.release();
    }

    const status = page.locator("#page-status");
    await expect(status).toHaveAttribute(
      "data-readiness",
      "complete-feature-ready",
    );
    await expect(status).toHaveAttribute("data-state", /ready|stale/);
    await expect(page.getByText("Atlas website unavailable")).toHaveCount(0);
    expect(pageErrors).toEqual([]);
  });

  test("finishes comparison loading after primary canvas metric churn exhausts its commit budget", async ({
    page,
  }) => {
    const gate = await installPublicationAttemptGate(page, "comparison");
    await page.addInitScript(() => {
      const target = window as PublicationAttemptGateWindow;
      target.__atlasComparisonCandidateMetricOffset = 0;
      const getBounds = HTMLCanvasElement.prototype.getBoundingClientRect;
      HTMLCanvasElement.prototype.getBoundingClientRect = function () {
        const bounds = getBounds.call(this);
        if (this.id !== "comparison-canvas") return bounds;
        return new DOMRect(
          bounds.x,
          bounds.y,
          bounds.width + (target.__atlasComparisonCandidateMetricOffset ?? 0),
          bounds.height,
        );
      };
    });
    const pageErrors: Error[] = [];
    page.on("pageerror", (error) => pageErrors.push(error));
    await page.goto("/compare/");

    for (let index = 0; index < 4; index += 1) {
      await gate.waitForAttempt(index + 1);
      await page.evaluate(
        (offset) => {
          (
            window as PublicationAttemptGateWindow
          ).__atlasComparisonCandidateMetricOffset = offset;
        },
        (index + 1) * 24,
      );
      await gate.release();
    }

    const status = page.locator("#comparison-status");
    await expect(status).toHaveAttribute(
      "data-readiness",
      "complete-feature-ready",
    );
    await expect(status).toHaveAttribute("data-state", /ready|stale/);
    await expect(page.locator("#comparison-status-title")).not.toContainText(
      "Comparison unavailable",
    );
    expect(pageErrors).toEqual([]);
  });

  test("makes the primary node view interactive before membership completes", async ({
    page,
  }) => {
    const gate = await installCompletionStageGate(page);
    const pageErrors: Error[] = [];
    page.on("pageerror", (error) => pageErrors.push(error));
    try {
      await page.goto("/?source=vps-core-01");
      await gate.waitForRequests(2);

      const status = page.locator("#page-status");
      await expect(status).toHaveAttribute(
        "data-readiness",
        "primary-interactive",
      );
      await expect(status).toHaveAttribute("data-state", /waiting|stale/);
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
      await expect(status).toHaveAttribute("data-state", /waiting|stale/);
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

  test("queries classifier labels and inspects the full matching population", async ({
    page,
  }) => {
    await page.goto("/?source=vps-core-01");
    await expect(page.locator("#page-status")).toHaveAttribute(
      "data-readiness",
      "complete-feature-ready",
    );

    const versionTwo = page.locator(
      '#classification-labels button[data-label="version_2"]',
    );
    const p2wsh = page.locator(
      '#classification-labels button[data-label="p2wsh"]',
    );
    const versionOne = page.locator(
      '#classification-labels button[data-label="version_1"]',
    );
    const matchAny = page.locator("#classification-match-any");
    const matchAll = page.locator("#classification-match-all");
    const querySummary = page.locator("#classification-query-summary");
    const queryStage = page.locator("#classification-query-stage");

    await versionTwo.click();
    await p2wsh.click();
    await expect(versionTwo).toHaveAttribute("aria-pressed", "true");
    await expect(p2wsh).toHaveAttribute("aria-pressed", "true");
    await expect(matchAny).toBeEnabled();
    await expect(matchAll).toBeEnabled();
    await expect(querySummary).toContainText(
      "622 transactions match Version 2 or P2WSH.",
    );
    expect(new URL(page.url()).searchParams.getAll("label")).toEqual([
      "p2wsh",
      "version_2",
    ]);
    expect(new URL(page.url()).searchParams.has("match")).toBe(false);

    await matchAll.click();
    await expect(matchAll).toHaveAttribute("aria-pressed", "true");
    await expect(querySummary).toContainText(
      "114 transactions match Version 2 and P2WSH.",
    );
    await expect(versionOne).toBeDisabled();
    await expect(versionOne).toHaveAttribute(
      "aria-label",
      /Unavailable with the current ALL selection/,
    );
    await expect(versionTwo).toBeEnabled();
    await expect(p2wsh).toBeEnabled();
    expect(new URL(page.url()).searchParams.get("match")).toBe("all");
    await expect(queryStage).toBeVisible();
    const sectionCounts = await page
      .locator("#classification-query-regions .query-section-label span")
      .allTextContents();
    expect(
      sectionCounts.reduce(
        (total, count) => total + Number(count.replaceAll(",", "")),
        0,
      ),
    ).toBe(114);

    const completeSection = page.locator(
      "#classification-query-regions .query-section-label.complete",
    );
    await expect(completeSection).toBeVisible();
    const [stageBox, completeBox] = await Promise.all([
      queryStage.boundingBox(),
      completeSection.boundingBox(),
    ]);
    if (stageBox === null || completeBox === null) {
      throw new Error("classification query terrain must be laid out");
    }
    await queryStage.click({
      position: {
        x: completeBox.x - stageBox.x + 8,
        y: completeBox.y + completeBox.height - stageBox.y + 8,
      },
    });
    await expect
      .poll(() => new URL(page.url()).searchParams.get("txid"))
      .toMatch(/^[0-9a-f]{64}$/);
    const selectedTxid = new URL(page.url()).searchParams.get("txid");
    if (selectedTxid === null) throw new Error("transaction was not selected");
    await expect(page.locator("#detail-transaction")).toContainText(
      selectedTxid,
    );
    const explorerLink = page.locator(
      "#detail-transaction .transaction-explorer-link",
    );
    await expect(explorerLink).toHaveAttribute(
      "href",
      `https://mempool.space/tx/${selectedTxid}`,
    );
    await expect(explorerLink).toHaveAttribute("target", "_blank");
    await expect(explorerLink).toHaveAttribute("rel", "noopener noreferrer");
    await expect(page.locator("#detail-status")).toHaveText(
      /Complete result|Partial result/,
    );
    await expect(versionTwo).toHaveAttribute("aria-pressed", "true");
    await expect(p2wsh).toHaveAttribute("aria-pressed", "true");
    await expect(matchAll).toHaveAttribute("aria-pressed", "true");

    await page.reload();
    await expect(page.locator("#page-status")).toHaveAttribute(
      "data-readiness",
      "complete-feature-ready",
    );
    await expect(versionTwo).toHaveAttribute("aria-pressed", "true");
    await expect(p2wsh).toHaveAttribute("aria-pressed", "true");
    await expect(matchAll).toHaveAttribute("aria-pressed", "true");
    await expect(querySummary).toContainText(
      "114 transactions match Version 2 and P2WSH.",
    );
    await expect(page.locator("#detail-transaction")).toContainText(
      selectedTxid,
    );

    await page.locator("#terrain-tab").click();
    await expect(page.locator("#inspector-filters")).toBeVisible();
    await expect(page.locator("#inspector-outcome")).toBeVisible();
    await page.locator('#rule-list button[data-label="p2tr"]').click();
    expect(new URL(page.url()).searchParams.getAll("label")).toEqual([
      "p2wsh",
      "version_2",
    ]);
    expect(new URL(page.url()).searchParams.get("match")).toBe("all");

    await page.locator("#overview-tab").click();
    await expect(versionTwo).toHaveAttribute("aria-pressed", "true");
    await expect(p2wsh).toHaveAttribute("aria-pressed", "true");
    await expect(matchAll).toHaveAttribute("aria-pressed", "true");
    await expect(querySummary).toContainText(
      "114 transactions match Version 2 and P2WSH.",
    );
    await expect(queryStage).toBeVisible();
    await expect(page.locator("#inspector-filters")).toBeHidden();
    await expect(page.locator("#inspector-outcome")).toBeHidden();
  });

  test("keeps snapshot-wide controls prominent and applies fee-age filters automatically", async ({
    page,
  }) => {
    await page.goto("/?source=vps-core-01");
    await expect(page.locator("#page-status")).toHaveAttribute(
      "data-readiness",
      "complete-feature-ready",
    );
    await expect(page.locator(".lens-toolbar #mode-count")).toBeVisible();
    const filters = page.locator("#filters");
    await expect(
      filters.locator(
        'button:not([type]), button[type="submit"], input[type="submit"], input[type="image"]',
      ),
    ).toHaveCount(0);
    await expect(
      filters.getByRole("button", { name: /^apply$/i, includeHidden: true }),
    ).toHaveCount(0);
    await expect(filters.locator("button")).toHaveCount(1);
    await expect(filters.locator("button")).toHaveText("Reset");
    await expect(page.locator("#sample-disclosure")).toHaveCount(0);
    await expect(page.locator("#transaction-disclosure")).toBeVisible();
    await expect(page.locator("#detail-classifiers")).toHaveCount(0);
    await expect(page.locator("#inspector-filters")).toBeHidden();
    await expect(page.locator("#inspector-outcome")).toBeHidden();
    await expect(page.locator("#transaction-disclosure")).toBeVisible();
    const inspectorOrder = await page
      .locator(".inspector-panel")
      .evaluate((inspector) =>
        [...inspector.children].map((element) =>
          element.classList.contains("filter-disclosure")
            ? "filters"
            : element.classList.contains("inspector-outcome")
              ? "outcome"
              : element.id === "transaction-disclosure"
                ? "transaction"
                : "other",
        ),
      );
    expect(inspectorOrder).toEqual(["filters", "outcome", "transaction"]);
    await expect(page.locator("#detail-status")).toHaveText(
      "Select a transaction",
    );
    const columnControl = page.locator(".distribution-column-control");
    const distributionGrid = page.locator("#distribution-grid");
    if ((page.viewportSize()?.width ?? 0) > 900) {
      await expect(columnControl).toBeVisible();
      const oneColumn = page.locator("#distribution-columns-1");
      await oneColumn.click();
      await expect(oneColumn).toHaveAttribute("aria-pressed", "true");
      await expect(distributionGrid).toHaveAttribute("data-columns", "1");
      expect(
        await distributionGrid.evaluate(
          (grid) =>
            getComputedStyle(grid).gridTemplateColumns.split(" ").length,
        ),
      ).toBe(1);
    } else {
      await expect(columnControl).toBeHidden();
      expect(
        await distributionGrid.evaluate(
          (grid) =>
            getComputedStyle(grid).gridTemplateColumns.split(" ").length,
        ),
      ).toBe(1);
    }

    await page.locator("#fee-age-tab").click();
    await page.locator("#minimum-fee-rate").fill("1000000");
    await expect(page.locator("#filter-summary")).toHaveText(
      "Showing 0 of 700 transactions.",
    );

    await page.locator("#reset-filters").click();
    await expect(page.locator("#filter-summary")).toHaveText(
      "Showing 700 of 700 transactions.",
    );
  });

  test("keeps membership filters disabled and inert during a source-switch primary view", async ({
    page,
  }) => {
    const pageErrors: Error[] = [];
    page.on("pageerror", (error) => pageErrors.push(error));
    await page.goto("/?source=vps-core-01");
    const status = page.locator("#page-status");
    await expect(status).toHaveAttribute(
      "data-readiness",
      "complete-feature-ready",
    );
    await page.locator("#fee-age-tab").click();
    await page.locator("#minimum-fee-rate").fill("7.5");
    await page.locator("#maximum-age").selectOption("3600000");
    await page.locator("#minimum-vsize").fill("300");
    await expect(page.locator("#fee-age-tab")).toHaveAttribute(
      "aria-selected",
      "true",
    );
    await expect(page.locator("#fee-age-view")).toBeVisible();

    const gate = await installCompletionStageGate(page);
    try {
      await page.locator("#source-select").selectOption("vps-knots-01");
      await gate.waitForRequests(2);
      await expect(status).toHaveAttribute(
        "data-readiness",
        "primary-interactive",
      );
      const membershipControls = page.locator(
        "#fee-age-tab, #classification-lens-select, #minimum-fee-rate, #maximum-age, #minimum-vsize, #reset-filters",
      );
      for (const control of await membershipControls.all()) {
        await expect(control).toBeDisabled();
      }
      await expect(page.locator("#overview-tab")).toHaveAttribute(
        "aria-selected",
        "true",
      );
      await expect(page.locator("#fee-age-tab")).toHaveAttribute(
        "aria-selected",
        "false",
      );
      await expect(page.locator("#overview-view")).toBeVisible();
      await expect(page.locator("#fee-age-view")).toBeHidden();
      await expect(page.locator("#minimum-fee-rate")).toHaveValue("7.5");
      await expect(page.locator("#maximum-age")).toHaveValue("3600000");
      await expect(page.locator("#minimum-vsize")).toHaveValue("300");

      const retainedStatus = await page.locator("#status-title").textContent();
      const retainedSummary = await page
        .locator("#filter-summary")
        .textContent();
      await page.locator("#maximum-age").evaluate((control) => {
        control.dispatchEvent(new Event("change", { bubbles: true }));
      });
      await expect(page.locator("#status-title")).toHaveText(
        retainedStatus ?? "",
      );
      await expect(page.locator("#filter-summary")).toHaveText(
        retainedSummary ?? "",
      );
      expect(pageErrors).toEqual([]);

      gate.release();
      await expect(status).toHaveAttribute(
        "data-readiness",
        "complete-feature-ready",
      );
      for (const control of await membershipControls.all()) {
        await expect(control).toBeEnabled();
      }
      await expect(page.locator("#overview-tab")).toHaveAttribute(
        "aria-selected",
        "true",
      );
      await expect(page.locator("#fee-age-tab")).toHaveAttribute(
        "aria-selected",
        "false",
      );
      await expect(page.locator("#overview-view")).toBeVisible();
      await expect(page.locator("#fee-age-view")).toBeHidden();
      await expect(page.locator("#minimum-fee-rate")).toHaveValue("7.5");
      await expect(page.locator("#maximum-age")).toHaveValue("3600000");
      await expect(page.locator("#minimum-vsize")).toHaveValue("300");
      await expect(page.locator("#filter-summary")).toContainText("Showing");
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
      await expect(status).toHaveAttribute("data-state", /waiting|stale/);
      await expect(page.locator("#comparison-policy-matrix")).toBeVisible();
      const commonRegion = page.locator(
        '#comparison-regions button[data-region="common"]',
      );
      await expect(commonRegion).toBeEnabled();
      await commonRegion.click();
      await expect(commonRegion).toHaveAttribute("aria-pressed", "true");
      const navigator = page.locator("#comparison-transaction-listbox");
      await expect(navigator).toBeVisible();
      await navigator.focus();
      await page.keyboard.press("Enter");
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

test.describe("comparison canvas selection churn", () => {
  test("renders the latest region after repeated changes during painting", async ({
    page,
  }) => {
    const gate = await installComparisonPaintGate(page);
    const pageErrors: Error[] = [];
    page.on("pageerror", (error) => pageErrors.push(error));
    try {
      await page.goto("/compare/");
      await gate.waitForRequests(1);
      const changes = [
        "left_only",
        "common",
        "right_only",
        "left_only",
        "common",
      ] as const;
      for (let index = 0; index < changes.length; index += 1) {
        const region = changes[index]!;
        const control = page.locator(
          `#comparison-regions button[data-region="${region}"]`,
        );
        await control.click();
        await expect(control).toHaveAttribute("aria-pressed", "true");
        await gate.waitForRequests(index + 2);
      }

      await gate.release();
      const status = page.locator("#comparison-status");
      await expect(status).toHaveAttribute(
        "data-readiness",
        "complete-feature-ready",
      );
      await expect(status).toHaveAttribute("data-state", /ready|stale/);
      await expect(page.locator("#comparison-canvas")).toHaveAttribute(
        "data-rendered-region",
        "common",
      );
      await expect(page.locator("#comparison-status-title")).not.toContainText(
        "unavailable",
      );
      expect(pageErrors).toEqual([]);
    } finally {
      await gate.release();
    }
  });
});

test.describe("atomic publication replacement", () => {
  test("keeps the complete node view interactive until one coherent refresh commits", async ({
    page,
  }) => {
    await page.goto("/?source=vps-core-01");
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

  test("retains node controls and filtered state when a superseding refresh fails", async ({
    page,
  }) => {
    await page.goto("/?source=vps-core-01");
    await expect(page.locator("#page-status")).toHaveAttribute(
      "data-readiness",
      "complete-feature-ready",
    );
    await page.locator("#fee-age-tab").click();
    await page.locator("#minimum-fee-rate").fill("1000000");
    await expect(page.locator("#filter-summary")).toHaveText(
      "Showing 0 of 700 transactions.",
    );
    const retainedFilterSummary =
      (await page.locator("#filter-summary").textContent()) ?? "";

    let refreshRequests = 0;
    await page.route(
      /\/api\/v2\/sources\/[^/]+\/mempool(?:\?.*)?$/,
      async (route) => {
        refreshRequests += 1;
        if (refreshRequests === 1) {
          await route.continue();
          return;
        }
        await route.fulfill({
          status: 503,
          contentType: "application/problem+json",
          json: {
            type: "v2_unavailable",
            title: "Current v2 publication unavailable",
            status: 503,
            detail: "Injected superseding refresh failure.",
          },
        });
      },
    );

    await page.locator("#spectrum-chart").evaluate((element) => {
      const originalReplaceChildren = element.replaceChildren.bind(element);
      element.replaceChildren = (..._nodes) => {
        element.replaceChildren = originalReplaceChildren;
        document
          .querySelector<HTMLButtonElement>("#refresh")
          ?.dispatchEvent(new MouseEvent("click", { bubbles: true }));
        throw new Error("Injected obsolete distribution commit failure");
      };
    });

    await page.locator("#refresh").click();
    await expect.poll(() => refreshRequests).toBe(2);
    await expect(page.locator("#status-title")).toHaveText(
      "Refresh failed · showing the prior snapshot",
    );
    await expect(page.locator("#filter-summary")).toHaveText(
      retainedFilterSummary,
    );
    for (const selector of [
      "#fee-age-tab",
      "#classification-lens-select",
      "#minimum-fee-rate",
      "#maximum-age",
      "#minimum-vsize",
      "#reset-filters",
    ]) {
      await expect(page.locator(selector)).toBeEnabled();
    }
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

  test("retains a complete comparison and cancels its sibling load after a completion-stage failure", async ({
    page,
  }) => {
    await installWorkerRequestObserver(page);
    const pageErrors: Error[] = [];
    page.on("pageerror", (error) => pageErrors.push(error));
    await page.goto("/compare/?left=vps-core-01&right=vps-knots-01");
    const status = page.locator("#comparison-status");
    await expect(status).toHaveAttribute(
      "data-readiness",
      "complete-feature-ready",
    );
    const sourceIds = await page
      .locator(".source-card > code")
      .allTextContents();
    const unionCount = (await page.locator("#union-count").textContent()) ?? "";
    await page.evaluate(() => {
      (window as WorkerRequestWindow).__atlasWorkerRequests = [];
    });

    let markSiblingStarted = (): void => undefined;
    const siblingStarted = new Promise<void>((resolve) => {
      markSiblingStarted = resolve;
    });
    let releaseSibling = (): void => undefined;
    const siblingRelease = new Promise<void>((resolve) => {
      releaseSibling = resolve;
    });
    let siblingStageHeld = false;
    let injected = false;
    await page.route(
      /\/api\/v2\/sources\/vps-core-01\/mempool\/stages\/membership\//,
      async (route) => {
        siblingStageHeld = true;
        markSiblingStarted();
        await siblingRelease;
        await route.continue().catch(() => undefined);
      },
    );
    await page.route(
      /\/api\/v2\/sources\/vps-knots-01\/mempool\/stages\/membership\//,
      async (route) => {
        await siblingStarted;
        if (injected) {
          await route.continue();
          return;
        }
        injected = true;
        await route.fulfill({
          status: 503,
          contentType: "application/json",
          body: '{"error":"injected completion-stage failure"}',
        });
      },
    );

    try {
      await page.locator("#comparison-refresh").click();
      await expect(status).toHaveAttribute("data-state", "error");
      await expect(page.locator("#comparison-status-title")).toHaveText(
        "Comparison unavailable",
      );
      await expect(page.locator("#comparison-status-detail")).toContainText(
        "Showing the last browser copy",
      );
      await expect
        .poll(() =>
          page.evaluate(() => {
            const requests =
              (window as WorkerRequestWindow).__atlasWorkerRequests ?? [];
            const siblingLoad = requests.find(
              (request) =>
                request.type === "load" && request.sourceId === "vps-core-01",
            );
            return (
              siblingLoad !== undefined &&
              requests.some(
                (request) =>
                  request.type === "cancel" &&
                  request.requestId === siblingLoad.requestId,
              )
            );
          }),
        )
        .toBe(true);

      expect(injected).toBe(true);
      expect(siblingStageHeld).toBe(true);
      await expect(status).toHaveAttribute("data-phase", "interactive");
      await expect(page.locator(".source-card > code")).toHaveText(sourceIds);
      await expect(page.locator("#union-count")).toHaveText(unionCount);
      await expect(page.locator("#comparison-stage")).toBeVisible();
      await expect(page.locator("#left-source")).toBeEnabled();
      await expect(page.locator("#right-source")).toBeEnabled();
      const commonRegion = page.locator(
        '#comparison-regions button[data-region="common"]',
      );
      await expect(commonRegion).toBeEnabled();
      await commonRegion.click();
      await expect(commonRegion).toHaveAttribute("aria-pressed", "true");
      await page.locator("#dist-scope-common").click();
      await expect(page.locator("#dist-scope-common")).toHaveAttribute(
        "aria-pressed",
        "true",
      );
      await expect(page.locator("#comparison-refresh")).toBeEnabled();
      expect(pageErrors).toEqual([]);
    } finally {
      releaseSibling();
    }
  });
});

test.describe("policy terrain raster", () => {
  test("keeps the active visual emphasis when a transaction is selected", async ({
    page,
  }) => {
    await page.goto(
      "/?source=vps-core-01&classifier=knots_bip110&rule=element_size",
    );
    await expect(page.locator("#page-status")).toHaveAttribute(
      "data-readiness",
      "complete-feature-ready",
    );
    await page.locator("#terrain-tab").click();
    const selectedRule = page.locator(
      '#rule-list button[data-rule="element_size"]',
    );
    await expect(selectedRule).toHaveAttribute("aria-pressed", "true");
    const canvas = page.locator("#terrain-canvas");
    const bounds = await canvas.boundingBox();
    expect(bounds).not.toBeNull();
    for (const yShare of [0.15, 0.35, 0.55, 0.75]) {
      for (const xShare of [0.15, 0.35, 0.55, 0.75]) {
        if (new URL(page.url()).searchParams.has("txid")) break;
        await canvas.click({
          position: {
            x: (bounds?.width ?? 2) * xShare,
            y: (bounds?.height ?? 2) * yShare,
          },
        });
      }
      if (new URL(page.url()).searchParams.has("txid")) break;
    }

    await expect
      .poll(() => new URL(page.url()).searchParams.get("txid"))
      .toMatch(/^[0-9a-f]{64}$/);
    await expect
      .poll(() => new URL(page.url()).searchParams.get("rule"))
      .toBe("element_size");
    await expect(selectedRule).toHaveAttribute("aria-pressed", "true");
    await expect(page.locator("#detail-status")).toHaveText(
      /Would violate policy|Compatible|Indeterminate/,
    );
    await expect(
      page.locator("#transaction-disclosure .detail-error"),
    ).toHaveCount(0);
    await expect(page.locator("#transaction-disclosure")).not.toContainText(
      '{"fixture":true}',
    );
    await waitForRendering(page);
    const marker = page.locator("#terrain-selection");
    await expect(marker).toBeVisible();
    await expect(marker).toHaveAttribute("style", /left: .*top:/);
    await expect(page.locator(".terrain-selection-marker")).toHaveCount(2);
    await expect(
      page.locator(".terrain-selection-marker:not([hidden])"),
    ).toHaveCount(1);

    await expect(selectedRule).toHaveAttribute("aria-pressed", "true");
  });

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
