import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { createServer } from "node:net";
import { dirname, join } from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

const HERE = dirname(fileURLToPath(import.meta.url));
const PREFLIGHT = join(HERE, "check-web-e2e-ports.mjs");

test("an occupied fixture port explains how to stop web-fixtures", async () => {
  const server = createServer();
  await new Promise((resolve, reject) => {
    server.once("error", reject);
    server.listen({ host: "127.0.0.1", port: 0 }, resolve);
  });
  try {
    const address = server.address();
    assert.notEqual(address, null);
    assert.equal(typeof address, "object");
    const result = spawnSync(
      process.execPath,
      [PREFLIGHT, "--fixture-port", String(address.port)],
      { encoding: "utf8" },
    );
    assert.equal(result.status, 1);
    assert.match(result.stderr, /fixture API port .* is already in use/);
    assert.match(result.stderr, /Stop `just web-fixtures`/);
    assert.match(result.stderr, /will not reuse an existing fixture server/);
  } finally {
    await new Promise((resolve, reject) => {
      server.close((error) =>
        error === undefined ? resolve() : reject(error),
      );
    });
  }
});
