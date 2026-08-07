import { defineConfig, devices } from "@playwright/test";

// Browser-level coverage runs against the deterministic fixture Atlas API in
// `dev/fixture-server.mjs`, never a live Bitcoin node. Playwright starts both
// the fixture API and Vite itself.
//
// The preview deliberately avoids Vite's default 5173 so an e2e run never
// attaches to, or fights with, a `just web-dev` server already in use. The
// fixture API port is fixed by `vite.config.ts`'s `/api` proxy. Every run owns
// a fresh fixture process so it must load the manifest generated immediately
// before Playwright starts. `reuseExistingServer: false` also means an occupied
// port fails the run instead of attaching to a stale fixture or Vite process;
// stop any process on 3101 or 5174 before rerunning the suite.
const FIXTURE_API_PORT = 3101;
const PREVIEW_PORT = 5174;

export default defineConfig({
  testDir: "./e2e",
  fullyParallel: true,
  forbidOnly: Boolean(process.env.CI),
  retries: process.env.CI ? 1 : 0,
  reporter: process.env.CI ? "github" : "list",
  use: {
    baseURL: `http://127.0.0.1:${PREVIEW_PORT}`,
    trace: "on-first-retry",
  },
  projects: [
    {
      name: "mobile",
      use: {
        ...devices["Desktop Chrome"],
        viewport: { width: 390, height: 844 },
      },
    },
    {
      name: "tablet",
      use: {
        ...devices["Desktop Chrome"],
        viewport: { width: 768, height: 1024 },
      },
    },
    {
      name: "desktop",
      use: {
        ...devices["Desktop Chrome"],
        viewport: { width: 1440, height: 900 },
      },
    },
  ],
  webServer: [
    {
      command: "node dev/fixture-server.mjs",
      url: `http://127.0.0.1:${FIXTURE_API_PORT}/api/v2/sources`,
      reuseExistingServer: false,
      stdout: "ignore",
      stderr: "pipe",
    },
    {
      command: `npm run dev -- --port ${PREVIEW_PORT} --strictPort --host 127.0.0.1`,
      url: `http://127.0.0.1:${PREVIEW_PORT}/`,
      reuseExistingServer: false,
      stdout: "ignore",
      stderr: "pipe",
    },
  ],
});
