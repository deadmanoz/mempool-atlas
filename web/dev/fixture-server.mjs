// Development-only Atlas v2 API server backed by canonical Rust-exported
// bytes. The timed request path selects buffers prepared at startup and never
// constructs or serializes a response.
import { readFileSync } from "node:fs";
import { createServer } from "node:http";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const MANIFEST_VERSION = 2;
const FIXTURE_ERROR =
  "functional fixture export missing or incompatible; run just functional-fixtures";
const HERE = dirname(fileURLToPath(import.meta.url));
const FIXTURE_DIRECTORY = resolve(HERE, "../.perf-fixtures/functional");
const MANIFEST_PATH = join(FIXTURE_DIRECTORY, "manifest.json");

const failFixtureLoad = () => {
  process.stderr.write(`${FIXTURE_ERROR}\n`);
  process.exit(1);
};

const loadManifest = () => {
  try {
    const manifest = JSON.parse(readFileSync(MANIFEST_PATH, "utf8"));
    if (
      manifest === null ||
      typeof manifest !== "object" ||
      manifest.manifest_version !== MANIFEST_VERSION ||
      manifest.profile !== "functional" ||
      typeof manifest.source_list !== "object" ||
      !Array.isArray(manifest.snapshots) ||
      !Array.isArray(manifest.transaction_details)
    ) {
      failFixtureLoad();
    }
    return manifest;
  } catch {
    failFixtureLoad();
  }
};

const manifest = loadManifest();

const loadBody = (descriptor, cacheable) => {
  try {
    if (
      descriptor === null ||
      typeof descriptor !== "object" ||
      typeof descriptor.path !== "string" ||
      typeof descriptor.content_type !== "string" ||
      typeof descriptor.content_id !== "string" ||
      !/^[0-9a-f]{64}$/.test(descriptor.content_id) ||
      !Number.isSafeInteger(descriptor.uncompressed_bytes) ||
      descriptor.uncompressed_bytes < 0
    ) {
      failFixtureLoad();
    }
    const bytes = readFileSync(join(FIXTURE_DIRECTORY, descriptor.path));
    if (bytes.byteLength !== descriptor.uncompressed_bytes) {
      failFixtureLoad();
    }
    const parsed = JSON.parse(bytes.toString("utf8"));
    const dependencyMismatch = Buffer.from(
      JSON.stringify({
        ...parsed,
        dependency_ids:
          Array.isArray(parsed.dependency_ids) &&
          parsed.dependency_ids.length > 0
            ? ["00".repeat(32), ...parsed.dependency_ids.slice(1)]
            : ["00".repeat(32)],
      }),
    );
    return Object.freeze({
      bytes,
      cacheable,
      contentId: descriptor.content_id,
      contentType: descriptor.content_type,
      mutations: Object.freeze({
        "dependency-mismatch": dependencyMismatch,
        "digest-mismatch": Buffer.concat([bytes, Buffer.from("\n")]),
        "malformed-json": Buffer.from('{"schema_version":2'),
      }),
    });
  } catch {
    failFixtureLoad();
  }
};

const routes = new Map();
const stagePrefixes = new Set();

const addRoute = (route, cacheable) => {
  if (
    route === null ||
    typeof route !== "object" ||
    typeof route.request_path !== "string" ||
    !route.request_path.startsWith("/api/v2/")
  ) {
    failFixtureLoad();
  }
  routes.set(route.request_path, loadBody(route.body, cacheable));
};

addRoute(manifest.source_list, false);
for (const snapshot of manifest.snapshots) {
  if (
    snapshot === null ||
    typeof snapshot !== "object" ||
    typeof snapshot.source_id !== "string" ||
    !Array.isArray(snapshot.stages)
  ) {
    failFixtureLoad();
  }
  addRoute(snapshot.manifest, true);
  stagePrefixes.add(
    `/api/v2/sources/${encodeURIComponent(snapshot.source_id)}/mempool/stages/`,
  );
  for (const stage of snapshot.stages) addRoute(stage.route, true);
}
for (const detail of manifest.transaction_details)
  addRoute(detail.route, false);

const errorBodies = Object.freeze({
  notFound: Buffer.from('{"error":"not found"}'),
  superseded: Buffer.from('{"error":"stage is not current"}'),
  unavailable: Buffer.from(
    JSON.stringify({
      type: "v2_unavailable",
      title: "Current v2 publication is unavailable",
      status: 503,
    }),
  ),
});

const send = (request, response, status, bytes, headers = {}) => {
  response.writeHead(status, {
    "cache-control": "no-store",
    "content-length": String(bytes.byteLength),
    "content-type": "application/json",
    ...headers,
  });
  response.end(request.method === "HEAD" ? undefined : bytes);
};

const server = createServer((request, response) => {
  if (request.method !== "GET" && request.method !== "HEAD") {
    send(
      request,
      response,
      405,
      Buffer.from('{"error":"method not allowed"}'),
      {
        allow: "GET, HEAD",
      },
    );
    return;
  }
  const url = new URL(request.url ?? "/", "http://127.0.0.1");
  const body = routes.get(url.pathname);
  if (body === undefined) {
    if ([...stagePrefixes].some((prefix) => url.pathname.startsWith(prefix))) {
      send(request, response, 409, errorBodies.superseded);
      return;
    }
    send(request, response, 404, errorBodies.notFound);
    return;
  }

  const fixtureMutation = url.searchParams.get("fixture");
  if (fixtureMutation === "v2_unavailable" && /\/mempool$/.test(url.pathname)) {
    send(request, response, 503, errorBodies.unavailable, {
      "content-type": "application/problem+json",
    });
    return;
  }
  if (fixtureMutation === "superseded") {
    send(request, response, 409, errorBodies.superseded);
    return;
  }

  const bytes = body.mutations[fixtureMutation] ?? body.bytes;
  const etag = `W/"${body.contentId}"`;
  const headers = {
    "cache-control": body.cacheable
      ? "public, no-cache, must-revalidate"
      : "no-store",
    "content-type": body.contentType,
    "x-atlas-content-id": body.contentId,
    "x-atlas-uncompressed-length": String(bytes.byteLength),
    ...(body.cacheable ? { etag } : {}),
  };
  if (
    body.cacheable &&
    fixtureMutation === null &&
    request.headers["if-none-match"] === etag
  ) {
    response.writeHead(304, headers);
    response.end();
    return;
  }
  send(request, response, 200, bytes, headers);
});

server.listen(3101, "127.0.0.1", () => {
  console.log(
    `fixture atlas v2 api on 127.0.0.1:3101 (${manifest.generated_at_ms})`,
  );
  for (const snapshot of manifest.snapshots) {
    console.log(
      `  ${snapshot.source_id}: ${snapshot.transaction_count} tx, ${snapshot.total_vsize} vB, ${snapshot.stages.length} stages`,
    );
  }
});
