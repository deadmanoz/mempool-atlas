import { defineConfig } from "@playwright/test";

const PERF_PORT = 4181;

export default defineConfig({
  testDir: ".",
  testMatch: "performance.spec.ts",
  fullyParallel: false,
  workers: 1,
  forbidOnly: true,
  retries: 0,
  reporter: "list",
  timeout: 240_000,
  expect: { timeout: 180_000 },
  outputDir: "../.perf-results/playwright",
  use: {
    headless: false,
    baseURL: `http://127.0.0.1:${PERF_PORT}`,
    trace: "off",
    screenshot: "off",
    video: "off",
    launchOptions: {
      args: ["--enable-blink-features=ForceEagerMeasureMemory"],
    },
  },
  projects: [
    {
      name: "desktop",
      use: {
        viewport: { width: 1440, height: 900 },
      },
    },
    {
      name: "mobile-slow-4g",
      use: {
        viewport: { width: 390, height: 844 },
      },
    },
  ],
  webServer: {
    command: "node server.mjs",
    url: `http://127.0.0.1:${PERF_PORT}/__perf/build.json`,
    reuseExistingServer: false,
    timeout: 120_000,
    stdout: "pipe",
    stderr: "pipe",
  },
});
