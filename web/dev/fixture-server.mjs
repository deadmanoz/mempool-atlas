// Development-only Atlas v2 API server backed by canonical Rust-exported
// bytes. The timed request path selects buffers prepared at startup and never
// constructs or serializes a response.
import { readFileSync, writeSync } from "node:fs";
import { createServer } from "node:http";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { gzipSync } from "node:zlib";

const MANIFEST_VERSION = 2;
const FIXTURE_ERROR =
  "functional fixture export missing or incompatible; run just functional-fixtures";
const HERE = dirname(fileURLToPath(import.meta.url));
const FIXTURE_DIRECTORY = resolve(HERE, "../.perf-fixtures/functional");
const MANIFEST_PATH = join(FIXTURE_DIRECTORY, "manifest.json");
const CONTENT_ID = /^[0-9a-f]{64}$/;
const CLASSIFIER_ID = /^[a-z][a-z0-9_]*$/;
const SNAPSHOT_STAGE_KINDS = new Set(["population", "membership", "structure"]);
const NO_STORE = "no-store";
const MANIFEST_CACHE_CONTROL = "public, no-cache, must-revalidate";
const STAGE_CACHE_CONTROL =
  "public, max-age=31536000, immutable, must-revalidate";
const FIXTURE_PORT_TEXT = process.env.ATLAS_FIXTURE_PORT ?? "3101";
const EMULATE_CLOUDFLARE_HEADERS =
  process.env.ATLAS_FIXTURE_CLOUDFLARE_HEADERS === "1";
const FIXTURE_DETAIL_STATUS_TEXT =
  process.env.ATLAS_FIXTURE_DETAIL_STATUS ?? "200";

if (!/^(0|[1-9][0-9]{0,4})$/.test(FIXTURE_PORT_TEXT)) {
  writeSync(process.stderr.fd, "invalid ATLAS_FIXTURE_PORT\n");
  process.exit(2);
}
const FIXTURE_PORT = Number(FIXTURE_PORT_TEXT);
if (FIXTURE_PORT > 65535) {
  writeSync(process.stderr.fd, "invalid ATLAS_FIXTURE_PORT\n");
  process.exit(2);
}
if (!/^(200|404|503|503-unavailable)$/.test(FIXTURE_DETAIL_STATUS_TEXT)) {
  writeSync(process.stderr.fd, "invalid ATLAS_FIXTURE_DETAIL_STATUS\n");
  process.exit(2);
}
const FIXTURE_DETAIL_STATUS =
  FIXTURE_DETAIL_STATUS_TEXT === "503-unavailable"
    ? 503
    : Number(FIXTURE_DETAIL_STATUS_TEXT);

const failFixtureLoad = () => {
  writeSync(process.stderr.fd, `${FIXTURE_ERROR}\n`);
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

const loadBody = (descriptor, cacheControl) => {
  try {
    if (
      descriptor === null ||
      typeof descriptor !== "object" ||
      typeof descriptor.path !== "string" ||
      typeof descriptor.content_type !== "string" ||
      typeof descriptor.content_id !== "string" ||
      !CONTENT_ID.test(descriptor.content_id) ||
      !Number.isSafeInteger(descriptor.uncompressed_bytes) ||
      descriptor.uncompressed_bytes < 0
    ) {
      failFixtureLoad();
    }
    const bytes = readFileSync(join(FIXTURE_DIRECTORY, descriptor.path));
    if (bytes.byteLength !== descriptor.uncompressed_bytes) {
      failFixtureLoad();
    }
    JSON.parse(bytes.toString("utf8"));
    return Object.freeze({
      bytes,
      cacheable: cacheControl !== NO_STORE,
      cacheControl,
      gzipBytes: gzipSync(bytes),
      contentId: descriptor.content_id,
      contentType: descriptor.content_type,
    });
  } catch {
    failFixtureLoad();
  }
};

const routes = new Map();
const stageContracts = new Map();

const stageLaneKey = (kind, classifierId) => `${kind}\0${classifierId ?? ""}`;

const addStageContract = (snapshot, stage) => {
  if (stage === null || typeof stage !== "object") {
    failFixtureLoad();
  }
  const classifierId = stage.classifier_id ?? null;
  if (
    (stage.kind !== "classifier" && !SNAPSHOT_STAGE_KINDS.has(stage.kind)) ||
    (stage.kind === "classifier"
      ? typeof classifierId !== "string" || !CLASSIFIER_ID.test(classifierId)
      : classifierId !== null)
  ) {
    failFixtureLoad();
  }
  const contentId = stage.route?.body?.content_id;
  if (typeof contentId !== "string" || !CONTENT_ID.test(contentId)) {
    failFixtureLoad();
  }
  let contract = stageContracts.get(snapshot.source_id);
  if (contract === undefined) {
    contract = {
      contentIds: new Set(),
      lanes: new Map(),
    };
    stageContracts.set(snapshot.source_id, contract);
  }
  const lane = stageLaneKey(stage.kind, classifierId);
  if (contract.lanes.has(lane) || contract.contentIds.has(contentId)) {
    failFixtureLoad();
  }
  contract.lanes.set(lane, contentId);
  contract.contentIds.add(contentId);
};

const decodedSegment = (segment) => {
  try {
    return decodeURIComponent(segment);
  } catch {
    return null;
  }
};

// Mirror `src/api.rs`: malformed route parameters are 400, an unknown source,
// lane, or content ID used on the wrong current lane is 404, and a well-formed
// non-current ID for an existing lane is the retryable 409 supersession case.
const stageErrorStatus = (pathname) => {
  const segments = pathname.split("/");
  if (
    segments.length < 9 ||
    segments[0] !== "" ||
    segments[1] !== "api" ||
    segments[2] !== "v2" ||
    segments[3] !== "sources" ||
    segments[5] !== "mempool" ||
    segments[6] !== "stages"
  ) {
    return 404;
  }

  const sourceId = decodedSegment(segments[4]);
  const kind = decodedSegment(segments[7]);
  if (sourceId === null || kind === null) return 400;

  let classifierId = null;
  let contentId;
  if (segments.length === 9) {
    contentId = decodedSegment(segments[8]);
    if (!SNAPSHOT_STAGE_KINDS.has(kind)) return 400;
  } else if (segments.length === 10 && kind === "classifier") {
    classifierId = decodedSegment(segments[8]);
    contentId = decodedSegment(segments[9]);
    if (classifierId === null || !CLASSIFIER_ID.test(classifierId)) return 400;
  } else {
    return 404;
  }
  if (contentId === null || !CONTENT_ID.test(contentId)) return 400;

  const contract = stageContracts.get(sourceId);
  if (contract === undefined) return 404;
  const expected = contract.lanes.get(stageLaneKey(kind, classifierId));
  if (expected === undefined || contract.contentIds.has(contentId)) return 404;
  return 409;
};

const addRoute = (route, cacheControl) => {
  if (
    route === null ||
    typeof route !== "object" ||
    typeof route.request_path !== "string" ||
    !route.request_path.startsWith("/api/v2/")
  ) {
    failFixtureLoad();
  }
  routes.set(route.request_path, loadBody(route.body, cacheControl));
};

addRoute(manifest.source_list, NO_STORE);
for (const snapshot of manifest.snapshots) {
  if (
    snapshot === null ||
    typeof snapshot !== "object" ||
    typeof snapshot.source_id !== "string" ||
    !Array.isArray(snapshot.stages)
  ) {
    failFixtureLoad();
  }
  addRoute(snapshot.manifest, MANIFEST_CACHE_CONTROL);
  for (const stage of snapshot.stages) {
    addStageContract(snapshot, stage);
    addRoute(stage.route, STAGE_CACHE_CONTROL);
  }
}
for (const detail of manifest.transaction_details)
  addRoute(detail.route, NO_STORE);

const errorBodies = Object.freeze({
  invalidStage: Buffer.from('{"error":"invalid v2 stage request"}'),
  notFound: Buffer.from('{"error":"not found"}'),
  queryNotSupported: Buffer.from(
    '{"error":"query-dependent v2 representations are not supported"}',
  ),
  superseded: Buffer.from('{"error":"stage is not current"}'),
});
const transactionDetailPath =
  /^\/api\/v2\/sources\/[^/]+\/transactions\/([0-9a-f]{64})$/;

const cloudflareHeaders = (cacheStatus) =>
  EMULATE_CLOUDFLARE_HEADERS
    ? {
        "cf-cache-status": cacheStatus,
        "cf-ray": "fixture-ray",
      }
    : {};

const send = (
  request,
  response,
  status,
  bytes,
  headers = {},
  cacheStatus = "BYPASS",
) => {
  response.writeHead(status, {
    "cache-control": "no-store",
    "content-length": String(bytes.byteLength),
    "content-type": "application/json",
    ...cloudflareHeaders(cacheStatus),
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
  const requestTarget = request.url ?? "/";
  const url = new URL(requestTarget, "http://127.0.0.1");
  if (requestTarget.includes("?")) {
    send(request, response, 400, errorBodies.queryNotSupported);
    return;
  }
  const body = routes.get(url.pathname);
  if (body === undefined) {
    const status = stageErrorStatus(url.pathname);
    if (status === 409) {
      send(request, response, 409, errorBodies.superseded);
      return;
    }
    send(
      request,
      response,
      status,
      status === 400 ? errorBodies.invalidStage : errorBodies.notFound,
    );
    return;
  }
  const detailMatch = url.pathname.match(transactionDetailPath);
  if (detailMatch !== null && FIXTURE_DETAIL_STATUS !== 200) {
    const txid = detailMatch[1];
    if (FIXTURE_DETAIL_STATUS_TEXT === "503-unavailable") {
      send(
        request,
        response,
        503,
        Buffer.from(
          JSON.stringify({
            type: "v2_unavailable",
            title: "Current v2 publication unavailable",
            status: 503,
            detail: "The source has not published a complete v2 snapshot yet.",
          }),
        ),
        { "content-type": "application/problem+json" },
      );
      return;
    }
    const detailError =
      FIXTURE_DETAIL_STATUS === 404
        ? `transaction "${txid}" is not in the current snapshot`
        : `transaction "${txid}" is present but has no policy assessment in the current snapshot`;
    send(
      request,
      response,
      FIXTURE_DETAIL_STATUS,
      Buffer.from(JSON.stringify({ error: detailError })),
    );
    return;
  }

  const bytes = body.bytes;
  const etag = `W/"${body.contentId}"`;
  const headers = {
    "cache-control": body.cacheControl,
    "content-type": body.contentType,
    "x-atlas-content-id": body.contentId,
    "x-atlas-uncompressed-length": String(bytes.byteLength),
    ...(body.cacheable ? { etag } : {}),
  };
  if (body.cacheable && request.headers["if-none-match"] === etag) {
    response.writeHead(304, {
      ...cloudflareHeaders("REVALIDATED"),
      ...headers,
    });
    response.end();
    return;
  }
  const acceptsGzip = /(?:^|,)\s*gzip(?:\s*;|\s*,|\s*$)/i.test(
    request.headers["accept-encoding"] ?? "",
  );
  send(
    request,
    response,
    200,
    acceptsGzip ? body.gzipBytes : bytes,
    {
      ...headers,
      ...(acceptsGzip ? { "content-encoding": "gzip" } : {}),
    },
    body.cacheable ? "MISS" : "BYPASS",
  );
});

server.listen(FIXTURE_PORT, "127.0.0.1", () => {
  const address = server.address();
  if (address === null || typeof address === "string") {
    throw new Error("fixture server did not bind a TCP address");
  }
  console.log(
    `fixture atlas v2 api on 127.0.0.1:${address.port} (${manifest.generated_at_ms})`,
  );
  for (const snapshot of manifest.snapshots) {
    console.log(
      `  ${snapshot.source_id}: ${snapshot.transaction_count} tx, ${snapshot.total_vsize} vB, ${snapshot.stages.length} stages`,
    );
  }
});
