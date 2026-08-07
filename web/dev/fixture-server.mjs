// Development-only Atlas v2 API server backed by canonical Rust-exported
// bytes. The timed request path selects buffers prepared at startup and never
// constructs or serializes a response.
import { readFileSync, writeSync } from "node:fs";
import { createServer } from "node:http";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const MANIFEST_VERSION = 2;
const FIXTURE_ERROR =
  "functional fixture export missing or incompatible; run just functional-fixtures";
const HERE = dirname(fileURLToPath(import.meta.url));
const FIXTURE_DIRECTORY = resolve(HERE, "../.perf-fixtures/functional");
const MANIFEST_PATH = join(FIXTURE_DIRECTORY, "manifest.json");
const CONTENT_ID = /^[0-9a-f]{64}$/;
const CLASSIFIER_ID = /^[a-z][a-z0-9_]*$/;
const SNAPSHOT_STAGE_KINDS = new Set(["population", "membership", "structure"]);

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

const loadBody = (descriptor, cacheable) => {
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
      cacheable,
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
  for (const stage of snapshot.stages) {
    addStageContract(snapshot, stage);
    addRoute(stage.route, true);
  }
}
for (const detail of manifest.transaction_details)
  addRoute(detail.route, false);

const errorBodies = Object.freeze({
  invalidStage: Buffer.from('{"error":"invalid v2 stage request"}'),
  notFound: Buffer.from('{"error":"not found"}'),
  queryNotSupported: Buffer.from(
    '{"error":"query-dependent v2 representations are not supported"}',
  ),
  superseded: Buffer.from('{"error":"stage is not current"}'),
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
  if (url.search.length > 0) {
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

  const bytes = body.bytes;
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
  if (body.cacheable && request.headers["if-none-match"] === etag) {
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
