import { createHash } from "node:crypto";
import { readFileSync, readdirSync } from "node:fs";
import { createServer } from "node:http";
import { extname, join, relative, resolve, sep } from "node:path";
import { gzipSync } from "node:zlib";

const PORT = 4181;
const WEB_ROOT = resolve(import.meta.dirname, "..");
const DIST_ROOT = join(WEB_ROOT, "dist");
const FIXTURE_ROOT = join(WEB_ROOT, ".perf-fixtures", "performance");
const MANIFEST_PATH = join(FIXTURE_ROOT, "manifest.json");
const MANIFEST_VERSION = 2;
const PERFORMANCE_SOURCE_COUNT = 2;
const PERFORMANCE_TRANSACTION_COUNT = 70_000;
const GZIP_LEVEL = 6;

const MIME_TYPES = new Map([
  [".css", "text/css; charset=utf-8"],
  [".html", "text/html; charset=utf-8"],
  [".js", "text/javascript; charset=utf-8"],
  [".json", "application/json"],
  [".png", "image/png"],
  [".svg", "image/svg+xml"],
  [".webp", "image/webp"],
]);

const readManifest = () => {
  const manifest = JSON.parse(readFileSync(MANIFEST_PATH, "utf8"));
  if (
    manifest.manifest_version !== MANIFEST_VERSION ||
    manifest.profile !== "performance" ||
    typeof manifest.source_list !== "object" ||
    !Array.isArray(manifest.snapshots) ||
    manifest.snapshots.length !== PERFORMANCE_SOURCE_COUNT ||
    manifest.configuration?.requested_transaction_count_per_source !==
      PERFORMANCE_TRANSACTION_COUNT ||
    manifest.snapshots.some(
      (snapshot) =>
        snapshot.transaction_count !== PERFORMANCE_TRANSACTION_COUNT,
    )
  ) {
    throw new Error(
      "performance fixture export missing or incompatible; run just perf-fixtures",
    );
  }
  return manifest;
};

const manifest = readManifest();
const buildHash = createHash("sha256");

const filesBelow = (directory) => {
  const output = [];
  for (const entry of readdirSync(directory, { withFileTypes: true })) {
    const path = join(directory, entry.name);
    if (entry.isDirectory()) {
      output.push(...filesBelow(path));
    } else if (entry.isFile()) {
      output.push(path);
    }
  }
  return output.sort();
};

const encodedBody = (bytes, contentType, cacheControl, contentId = null) => {
  const etag = `W/"${contentId ?? createHash("sha256").update(bytes).digest("hex")}"`;
  return Object.freeze({
    identity: bytes,
    gzip: gzipSync(bytes, { level: GZIP_LEVEL }),
    contentType,
    cacheControl,
    contentId,
    etag,
  });
};

const routes = new Map();
for (const path of filesBelow(DIST_ROOT)) {
  const relativePath = relative(DIST_ROOT, path).split(sep).join("/");
  const bytes = readFileSync(path);
  buildHash.update(relativePath).update(bytes);
  const body = encodedBody(
    bytes,
    MIME_TYPES.get(extname(path)) ?? "application/octet-stream",
    relativePath.startsWith("assets/")
      ? "public, max-age=31536000, immutable"
      : "no-cache",
  );
  routes.set(`/${relativePath}`, body);
  if (relativePath === "index.html") routes.set("/", body);
  if (relativePath === "compare/index.html") routes.set("/compare/", body);
}

const loadFixtureBody = (descriptor, cacheControl) => {
  const bytes = readFileSync(join(FIXTURE_ROOT, descriptor.path));
  if (bytes.byteLength !== descriptor.uncompressed_bytes) {
    throw new Error(`fixture length mismatch for ${descriptor.path}`);
  }
  return encodedBody(
    bytes,
    descriptor.content_type,
    cacheControl,
    descriptor.content_id,
  );
};

routes.set(
  manifest.source_list.request_path,
  loadFixtureBody(manifest.source_list.body, "no-store"),
);
for (const snapshot of manifest.snapshots) {
  routes.set(
    snapshot.manifest.request_path,
    loadFixtureBody(
      snapshot.manifest.body,
      "public, no-cache, must-revalidate",
    ),
  );
  for (const stage of snapshot.stages) {
    routes.set(
      stage.route.request_path,
      loadFixtureBody(stage.route.body, "public, no-cache, must-revalidate"),
    );
  }
}

const emptyWorkerScript = Buffer.from(
  'postMessage({ kind: "ready", at: performance.now() });',
);
const emptyWorkerHtml = Buffer.from(`<!doctype html>
<html lang="en"><meta charset="utf-8"><title>Empty worker baseline</title>
<script type="module">
  const worker = new Worker("/__perf/empty-worker.js", { type: "module" });
  worker.onmessage = () => { window.__emptyWorkerReady = true; };
</script></html>`);
routes.set(
  "/__perf/empty-worker.js",
  encodedBody(emptyWorkerScript, "text/javascript; charset=utf-8", "no-cache"),
);
routes.set(
  "/__perf/empty-worker.html",
  encodedBody(emptyWorkerHtml, "text/html; charset=utf-8", "no-cache"),
);

const webBuildId = buildHash.digest("hex");
routes.set(
  "/__perf/build.json",
  encodedBody(
    Buffer.from(
      JSON.stringify({
        web_build_id: webBuildId,
        fixture_manifest_version: manifest.manifest_version,
        fixture_generated_at_ms: manifest.generated_at_ms,
        snapshots: manifest.snapshots.map((snapshot) => ({
          source_id: snapshot.source_id,
          transaction_count: snapshot.transaction_count,
          total_vsize: snapshot.total_vsize,
          differing_wtxid_count: snapshot.differing_wtxid_count,
        })),
      }),
    ),
    "application/json",
    "no-store",
  ),
);

const notFound = encodedBody(
  Buffer.from('{"error":"not found"}'),
  "application/json",
  "no-store",
);

createServer((request, response) => {
  const startedAt = performance.now();
  if (request.method !== "GET" && request.method !== "HEAD") {
    const bytes = Buffer.from('{"error":"method not allowed"}');
    response.writeHead(405, {
      allow: "GET, HEAD",
      "cache-control": "no-store",
      "content-length": String(bytes.byteLength),
      "content-type": "application/json",
    });
    response.end(bytes);
    return;
  }
  const url = new URL(request.url ?? "/", "http://127.0.0.1");
  const body = routes.get(url.pathname) ?? notFound;
  const status = body === notFound ? 404 : 200;
  const commonHeaders = {
    "cache-control": body.cacheControl,
    "cross-origin-embedder-policy": "require-corp",
    "cross-origin-opener-policy": "same-origin",
    "cross-origin-resource-policy": "same-origin",
    etag: body.etag,
    "server-timing": `atlas;dur=${(performance.now() - startedAt).toFixed(3)}`,
    vary: "Accept-Encoding",
    "x-atlas-build-id": webBuildId,
    ...(body.contentId === null
      ? {}
      : { "x-atlas-content-id": body.contentId }),
    "x-atlas-uncompressed-length": String(body.identity.byteLength),
  };
  if (
    body.cacheControl !== "no-store" &&
    request.headers["if-none-match"] === body.etag
  ) {
    response.writeHead(304, commonHeaders);
    response.end();
    return;
  }
  const useGzip = request.headers["accept-encoding"]?.includes("gzip") ?? false;
  const bytes = useGzip ? body.gzip : body.identity;
  response.writeHead(status, {
    ...commonHeaders,
    ...(useGzip ? { "content-encoding": "gzip" } : {}),
    "content-length": String(bytes.byteLength),
    "content-type": body.contentType,
  });
  response.end(request.method === "HEAD" ? undefined : bytes);
}).listen(PORT, "127.0.0.1", () => {
  console.log(
    `performance server on 127.0.0.1:${PORT}, build ${webBuildId.slice(0, 12)}`,
  );
});
