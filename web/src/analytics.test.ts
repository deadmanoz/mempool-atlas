// @vitest-environment happy-dom

import { afterAll, afterEach, beforeAll, describe, expect, it } from "vitest";
import type { Window as HappyDomWindow } from "happy-dom";

import { analyticsConfig, installAnalytics } from "./analytics";

const validEnvironment = {
  VITE_UMAMI_SCRIPT_URL: "https://analytics.example.com/script.js",
  VITE_UMAMI_WEBSITE_ID: "01234567-89ab-4cde-8fab-0123456789ab",
  VITE_UMAMI_DOMAINS: "atlas.example.com",
};

const happyDomWindow = window as unknown as HappyDomWindow;
const originalDisabledLoadingBehavior =
  happyDomWindow.happyDOM.settings.handleDisabledFileLoadingAsSuccess;

beforeAll(() => {
  happyDomWindow.happyDOM.settings.handleDisabledFileLoadingAsSuccess = true;
});

afterAll(() => {
  happyDomWindow.happyDOM.settings.handleDisabledFileLoadingAsSuccess =
    originalDisabledLoadingBehavior;
});

afterEach(() => {
  document.head
    .querySelectorAll("script[data-atlas-analytics]")
    .forEach((script) => script.remove());
});

describe("analyticsConfig", () => {
  it("is disabled when the deployment does not configure analytics", () => {
    expect(analyticsConfig({})).toBeNull();
    expect(installAnalytics({}, document)).toBe("disabled");
  });

  it("rejects incomplete, insecure, or malformed configuration", () => {
    expect(
      installAnalytics(
        { VITE_UMAMI_SCRIPT_URL: validEnvironment.VITE_UMAMI_SCRIPT_URL },
        document,
      ),
    ).toBe("invalid");
    expect(
      analyticsConfig({
        ...validEnvironment,
        VITE_UMAMI_SCRIPT_URL: "http://analytics.example.com/script.js",
      }),
    ).toBeNull();
    expect(
      analyticsConfig({
        ...validEnvironment,
        VITE_UMAMI_WEBSITE_ID: "not-a-website-id",
      }),
    ).toBeNull();
    expect(
      analyticsConfig({
        ...validEnvironment,
        VITE_UMAMI_DOMAINS: "atlas.example.com/path",
      }),
    ).toBeNull();
  });

  it("normalizes and deduplicates the configured hostnames", () => {
    expect(
      analyticsConfig({
        ...validEnvironment,
        VITE_UMAMI_DOMAINS:
          " ATLAS.EXAMPLE.COM,compare.example.com,atlas.example.com ",
      }),
    ).toEqual({
      scriptUrl: "https://analytics.example.com/script.js",
      websiteId: validEnvironment.VITE_UMAMI_WEBSITE_ID,
      domains: "atlas.example.com,compare.example.com",
    });
  });
});

describe("installAnalytics", () => {
  it("installs one deferred Umami script with the deployment attributes", () => {
    expect(installAnalytics(validEnvironment, document)).toBe("installed");

    const script = document.head.querySelector<HTMLScriptElement>(
      "script[data-atlas-analytics]",
    );
    expect(script?.src).toBe(validEnvironment.VITE_UMAMI_SCRIPT_URL);
    expect(script?.defer).toBe(true);
    expect(script?.dataset.websiteId).toBe(
      validEnvironment.VITE_UMAMI_WEBSITE_ID,
    );
    expect(script?.dataset.domains).toBe("atlas.example.com");
    expect(installAnalytics(validEnvironment, document)).toBe(
      "already-installed",
    );
    expect(
      document.head.querySelectorAll("script[data-atlas-analytics]"),
    ).toHaveLength(1);
  });
});
