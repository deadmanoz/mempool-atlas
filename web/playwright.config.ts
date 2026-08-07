import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";

import { defineConfig, devices } from "@playwright/test";

// Browser-level coverage runs against the deterministic fixture Atlas API in
// `dev/fixture-server.mjs`, never a live Bitcoin node. Playwright starts both
// the fixture API and Vite itself.
//
// The preview deliberately avoids Vite's default 5173 so an e2e run never
// attaches to, or fights with, a `just web-dev` server already in use. The
// fixture API port is fixed by `vite.config.ts`'s `/api` proxy. Every run owns
// a fresh fixture process so it must load the manifest generated immediately
// before Playwright starts. The explicit port preflights provide actionable
// errors while `reuseExistingServer: false` prevents attachment to a stale
// fixture or Vite process.
const FIXTURE_API_PORT = 3101;
const PREVIEW_PORT = 5174;
const PORT_PREFLIGHT = fileURLToPath(
  new URL("../scripts/check-web-e2e-ports.mjs", import.meta.url),
);

if (process.env.ATLAS_E2E_PORTS_PREFLIGHTED !== "1") {
  const preflight = spawnSync(
    process.execPath,
    [
      PORT_PREFLIGHT,
      "--fixture-port",
      String(FIXTURE_API_PORT),
      "--preview-port",
      String(PREVIEW_PORT),
    ],
    { encoding: "utf8" },
  );
  if (preflight.error !== undefined) throw preflight.error;
  if (preflight.status !== 0) {
    throw new Error(preflight.stderr.trim() || "web E2E port preflight failed");
  }
  process.env.ATLAS_E2E_PORTS_PREFLIGHTED = "1";
}

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
      command: `node ../scripts/check-web-e2e-ports.mjs --fixture-port ${FIXTURE_API_PORT} && node dev/fixture-server.mjs`,
      url: `http://127.0.0.1:${FIXTURE_API_PORT}/api/v2/sources`,
      reuseExistingServer: false,
      stdout: "ignore",
      stderr: "pipe",
    },
    {
      command: `node ../scripts/check-web-e2e-ports.mjs --preview-port ${PREVIEW_PORT} && npm run dev -- --port ${PREVIEW_PORT} --strictPort --host 127.0.0.1`,
      url: `http://127.0.0.1:${PREVIEW_PORT}/`,
      reuseExistingServer: false,
      stdout: "ignore",
      stderr: "pipe",
    },
  ],
});
