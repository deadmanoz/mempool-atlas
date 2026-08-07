import { expect, test } from "@playwright/test";
import type { Browser, CDPSession, Page } from "@playwright/test";
import { mkdirSync, writeFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import type { AtlasCandidateReadyDetail } from "../src/candidate-ready";
import type { AtlasWorkerRequest } from "../src/atlas-worker-protocol";
import { RELEASE_GATES } from "./release-gates.mjs";

type Scenario = "node" | "comparison";
type StageKind = "population" | "membership" | "structure" | "classifier";

type MemoryBreakdown = {
  bytes: number;
  types: string[];
  attribution: Array<{ url?: string; scope?: string }>;
};

type MemoryResult = {
  supported: boolean;
  bytes: number | null;
  breakdown: MemoryBreakdown[];
  milestone_timestamp_ms: number;
  resolved_timestamp_ms: number;
  error: string | null;
};

type HeapUsage = {
  usedSize: number;
  totalSize: number;
  embedderHeapUsedSize: number;
  backingStorageSize: number;
};

type LcpRecord = {
  start_time_ms: number;
  render_time_ms: number;
  load_time_ms: number;
  size: number;
  selector: string | null;
  url: string;
};

type BrowserMetrics = {
  metadata_ready_ms: number | null;
  cls: number;
  layout_shifts: Array<{
    start_time_ms: number;
    value: number;
    sources: string[];
  }>;
  long_tasks: Array<{ start_time_ms: number; duration_ms: number }>;
  animation_frame_callbacks: Array<{
    start_time_ms: number;
    duration_ms: number;
    callback_name: string | null;
  }>;
  lcp: LcpRecord[];
};

type ResponsivenessInterval = {
  label: string;
  start_time_ms: number;
  end_time_ms: number;
};

type MeasuredInteraction = {
  label: string;
  handler_duration_ms: number;
  settle_duration_ms: number;
  responsiveness_interval: ResponsivenessInterval;
  outcome: Record<string, string | number | boolean>;
};

type ReplacementMeasurement = {
  memory: MemoryResult;
  responsivenessIntervals: readonly ResponsivenessInterval[];
};

type Bip110RuleNavigationMeasurement = {
  ruleIds: string[];
  selectedRule: string | null;
  handlerDurationsMs: number[];
  handlerDurationMs: number;
  responsivenessInterval: ResponsivenessInterval;
};

declare global {
  interface Window {
    __atlasPerfMetrics: BrowserMetrics;
    __atlasPerfReleaseCandidate?: () => void;
    __atlasCandidateReadyHook?: (
      detail: AtlasCandidateReadyDetail,
    ) => void | Promise<void>;
    __atlasWorkerRequests?: AtlasWorkerRequest[];
    __emptyWorkerReady?: boolean;
  }

  interface Performance {
    measureUserAgentSpecificMemory?: () => Promise<{
      bytes: number;
      breakdown: MemoryBreakdown[];
    }>;
  }
}

type SnapshotDescriptor = {
  source_id: string;
  transaction_count: number;
  total_vsize: number;
  differing_wtxid_count: number;
};

type BuildInfo = {
  web_build_id: string;
  fixture_manifest_version: number;
  fixture_generated_at_ms: number;
  snapshots: SnapshotDescriptor[];
};

type StageDescriptor = {
  kind: StageKind;
  classifier_id?: string;
  content_id: string;
  uncompressed_bytes: number;
  row_count: number;
  dependency_ids: string[];
};

type StagedManifest = {
  schema_version: number;
  source_id: string;
  publication_id: string;
  transaction_count: number;
  stages: StageDescriptor[];
};

type CdpRequestState = {
  request_id: string;
  url: string;
  method: string;
  started_at_seconds: number;
  response_at_seconds: number | null;
  finished_at_seconds: number | null;
  status: number | null;
  encoded_data_length: number | null;
  response_headers: Record<string, string | number>;
  loading_error: string | null;
};

type CdpRequestWillBeSent = {
  requestId: string;
  timestamp: number;
  request: { url: string; method: string };
};

type CdpResponseReceived = {
  requestId: string;
  timestamp: number;
  response: {
    status: number;
    headers: Record<string, string | number>;
  };
};

type CdpLoadingFinished = {
  requestId: string;
  timestamp: number;
  encodedDataLength: number;
};

type CdpLoadingFailed = {
  requestId: string;
  timestamp: number;
  errorText: string;
};

type CdpTargetAttached = {
  sessionId: string;
  targetInfo: { type: string; url: string };
};

type CdpTargetMessage = {
  sessionId: string;
  message: string;
};

type WorkerProtocolMessage = {
  method?: string;
  params?: unknown;
};

type V2RequestCapture = {
  requests: Map<string, CdpRequestState>;
  errors: string[];
};

type V2Route =
  | { resource_kind: "manifest"; source_id: string }
  | {
      resource_kind: "stage";
      source_id: string;
      stage_kind: StageKind;
      classifier_id: string | null;
      stage_id: string;
    };

type V2Transfer = V2Route & {
  request_id: string;
  url: string;
  method: string;
  status: number | null;
  content_encoding: string | null;
  content_id: string | null;
  encoded_body_bytes: number | null;
  decoded_body_bytes: number | null;
  cdp_encoded_data_length: number | null;
  ttfb_ms: number | null;
  transfer_excluding_ttfb_ms: number | null;
  server_processing_ms: number | null;
  started_at_seconds: number;
  response_at_seconds: number | null;
  finished_at_seconds: number | null;
  loading_error: string | null;
};

type QuorumTotals = {
  encoded_body_bytes: number;
  decoded_body_bytes: number;
  cdp_encoded_data_length: number;
  request_count: number;
  manifest_count: number;
  stage_count: number;
  stage_ids: string[];
  request_span_ms: number;
  transfer_window_ms: number;
  summed_ttfb_ms: number;
  summed_server_processing_ms: number;
};

type WorkerStageTiming = {
  kind: StageKind;
  classifierId: string | null;
  reused: boolean;
  fetchDigestParseMs: number;
  decodeValidatePackMs: number;
};

type WorkerQuorumTiming = {
  manifestFetchValidateMs: number;
  stages: WorkerStageTiming[];
  semanticValidationMs: number;
  quorumMs: number;
  supersessionRestarts: number;
};

type SourceWorkerQuorumTiming = WorkerQuorumTiming & {
  source_id: string;
};

type WorkerTimingTotals = {
  source_count: number;
  stage_count: number;
  reused_stage_count: number;
  fetched_stage_count: number;
  manifest_fetch_validate_ms: number;
  stage_fetch_digest_parse_ms: number;
  stage_decode_validate_pack_ms: number;
  semantic_validation_ms: number;
  summed_quorum_ms: number;
  maximum_source_quorum_ms: number;
  supersession_restarts: number;
};

type Throttle = {
  latency_ms: number;
  download_bytes_per_second: number;
  upload_bytes_per_second: number;
  cpu_slowdown: number;
};

const RESULT_DIRECTORY = fileURLToPath(
  new URL("../.perf-results/raw/", import.meta.url),
);

const MOBILE_THROTTLE: Readonly<Throttle> = Object.freeze({
  latency_ms: 150,
  download_bytes_per_second: 160_000,
  upload_bytes_per_second: 93_750,
  cpu_slowdown: 4,
});

const DESKTOP_THROTTLE: Readonly<Throttle> = Object.freeze({
  latency_ms: 0,
  download_bytes_per_second: -1,
  upload_bytes_per_second: -1,
  cpu_slowdown: 1,
});

const installObservers = async (page: Page): Promise<void> => {
  await page.addInitScript(() => {
    const metrics: BrowserMetrics = {
      metadata_ready_ms: null,
      cls: 0,
      layout_shifts: [],
      long_tasks: [],
      animation_frame_callbacks: [],
      lcp: [],
    };
    window.__atlasPerfMetrics = metrics;

    const nativeRequestAnimationFrame =
      window.requestAnimationFrame.bind(window);
    window.requestAnimationFrame = (callback: FrameRequestCallback): number =>
      nativeRequestAnimationFrame((timestamp) => {
        const started = performance.now();
        try {
          callback(timestamp);
        } finally {
          metrics.animation_frame_callbacks.push({
            start_time_ms: started,
            duration_ms: performance.now() - started,
            callback_name:
              ((
                callback as FrameRequestCallback & {
                  __atlasPerfLabel?: string;
                }
              ).__atlasPerfLabel ??
                callback.name) ||
              null,
          });
        }
      });

    const selectorFor = (element: Element | null): string | null => {
      if (element === null) return null;
      if (element.id !== "") return `#${element.id}`;
      const firstClass = element.classList.item(0);
      if (firstClass !== null) {
        return `${element.tagName.toLowerCase()}.${firstClass}`;
      }
      return element.tagName.toLowerCase();
    };

    try {
      new PerformanceObserver((list) => {
        for (const entry of list.getEntries()) {
          const candidate = entry as PerformanceEntry & {
            renderTime: number;
            loadTime: number;
            size: number;
            element: Element | null;
            url: string;
          };
          metrics.lcp.push({
            start_time_ms: candidate.startTime,
            render_time_ms: candidate.renderTime,
            load_time_ms: candidate.loadTime,
            size: candidate.size,
            selector: selectorFor(candidate.element),
            url: candidate.url,
          });
        }
      }).observe({ type: "largest-contentful-paint", buffered: true });
    } catch {
      // The result records an empty LCP set when the browser lacks the API.
    }
    try {
      new PerformanceObserver((list) => {
        for (const entry of list.getEntries()) {
          const shift = entry as PerformanceEntry & {
            value: number;
            hadRecentInput: boolean;
            sources?: Array<{ node?: Node }>;
          };
          if (!shift.hadRecentInput) {
            metrics.cls += shift.value;
            metrics.layout_shifts.push({
              start_time_ms: shift.startTime,
              value: shift.value,
              sources: (shift.sources ?? []).map(({ node }) =>
                node instanceof Element
                  ? (selectorFor(node) ?? node.tagName.toLowerCase())
                  : (node?.nodeName ?? "unknown"),
              ),
            });
          }
        }
      }).observe({ type: "layout-shift", buffered: true });
    } catch {
      // The result records zero when layout-shift entries are unavailable.
    }
    try {
      new PerformanceObserver((list) => {
        for (const entry of list.getEntries()) {
          metrics.long_tasks.push({
            start_time_ms: entry.startTime,
            duration_ms: entry.duration,
          });
        }
      }).observe({ type: "longtask", buffered: true });
    } catch {
      // The result records an empty set when long-task entries are unavailable.
    }

    const metadataIsReady = (): boolean => {
      const sourceSummary = document.querySelector("#source-summary");
      if (sourceSummary !== null) {
        return (
          sourceSummary.getAttribute("data-phase") !== "discovering-sources"
        );
      }
      const sourceCards = [...document.querySelectorAll(".source-card")];
      return (
        sourceCards.length === 2 &&
        sourceCards.every(
          (card) => card.getAttribute("data-phase") !== "discovering-sources",
        )
      );
    };
    const markMetadata = (): void => {
      if (metrics.metadata_ready_ms !== null || !metadataIsReady()) return;
      requestAnimationFrame(() => {
        metrics.metadata_ready_ms ??= performance.now();
      });
    };
    document.addEventListener("DOMContentLoaded", () => {
      const observer = new MutationObserver(markMetadata);
      observer.observe(document.body, {
        childList: true,
        subtree: true,
        characterData: true,
      });
      markMetadata();
    });
  });
};

const installWorkerRequestObserver = async (page: Page): Promise<void> => {
  await page.addInitScript(() => {
    window.__atlasWorkerRequests = [];
    const nativePostMessage = Worker.prototype.postMessage;
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
        window.__atlasWorkerRequests?.push(
          structuredClone(message) as AtlasWorkerRequest,
        );
      }
      Reflect.apply(
        nativePostMessage,
        this,
        transferOrOptions === undefined
          ? [message]
          : [message, transferOrOptions],
      );
    };
  });
};

const configureProfile = async (
  client: CDPSession,
  profile: string,
): Promise<Readonly<Throttle>> => {
  const throttle =
    profile === "mobile-slow-4g" ? MOBILE_THROTTLE : DESKTOP_THROTTLE;
  await client.send("Network.enable");
  await client.send("Network.setCacheDisabled", { cacheDisabled: true });
  await client.send("Network.emulateNetworkConditions", {
    offline: false,
    latency: throttle.latency_ms,
    downloadThroughput: throttle.download_bytes_per_second,
    uploadThroughput: throttle.upload_bytes_per_second,
    connectionType: profile === "mobile-slow-4g" ? "cellular4g" : "ethernet",
  });
  await client.send("Emulation.setCPUThrottlingRate", {
    rate: throttle.cpu_slowdown,
  });
  return throttle;
};

const v2Route = (url: string): V2Route | null => {
  const path = new URL(url).pathname;
  const manifest = path.match(/^\/api\/v2\/sources\/([^/]+)\/mempool$/);
  if (manifest !== null) {
    return {
      resource_kind: "manifest",
      source_id: decodeURIComponent(manifest[1] ?? ""),
    };
  }
  const classifier = path.match(
    /^\/api\/v2\/sources\/([^/]+)\/mempool\/stages\/classifier\/([^/]+)\/([0-9a-f]{64})$/,
  );
  if (classifier !== null) {
    return {
      resource_kind: "stage",
      source_id: decodeURIComponent(classifier[1] ?? ""),
      stage_kind: "classifier",
      classifier_id: decodeURIComponent(classifier[2] ?? ""),
      stage_id: classifier[3] ?? "",
    };
  }
  const stage = path.match(
    /^\/api\/v2\/sources\/([^/]+)\/mempool\/stages\/(population|membership|structure)\/([0-9a-f]{64})$/,
  );
  return stage === null
    ? null
    : {
        resource_kind: "stage",
        source_id: decodeURIComponent(stage[1] ?? ""),
        stage_kind: stage[2] as Exclude<StageKind, "classifier">,
        classifier_id: null,
        stage_id: stage[3] ?? "",
      };
};

const captureV2Requests = async (
  client: CDPSession,
  throttle: Readonly<Throttle>,
): Promise<V2RequestCapture> => {
  const requests = new Map<string, CdpRequestState>();
  const errors: string[] = [];
  let commandId = 0;
  const requestKey = (sessionId: string, requestId: string): string =>
    `${sessionId}:${requestId}`;
  const sendToWorker = async (
    sessionId: string,
    method: string,
    params: Record<string, unknown> = {},
  ): Promise<void> => {
    commandId += 1;
    await client.send("Target.sendMessageToTarget", {
      sessionId,
      message: JSON.stringify({ id: commandId, method, params }),
    });
  };
  const onRequest = (sessionId: string, event: CdpRequestWillBeSent): void => {
    if (v2Route(event.request.url) === null) return;
    const key = requestKey(sessionId, event.requestId);
    requests.set(key, {
      request_id: key,
      url: event.request.url,
      method: event.request.method,
      started_at_seconds: event.timestamp,
      response_at_seconds: null,
      finished_at_seconds: null,
      status: null,
      encoded_data_length: null,
      response_headers: {},
      loading_error: null,
    });
  };
  const onResponse = (sessionId: string, event: CdpResponseReceived): void => {
    const request = requests.get(requestKey(sessionId, event.requestId));
    if (request === undefined) return;
    request.status = event.response.status;
    request.response_at_seconds = event.timestamp;
    request.response_headers = event.response.headers;
  };
  const onFinished = (sessionId: string, event: CdpLoadingFinished): void => {
    const request = requests.get(requestKey(sessionId, event.requestId));
    if (request === undefined) return;
    request.finished_at_seconds = event.timestamp;
    request.encoded_data_length = event.encodedDataLength;
  };
  const onFailed = (sessionId: string, event: CdpLoadingFailed): void => {
    const request = requests.get(requestKey(sessionId, event.requestId));
    if (request === undefined) return;
    request.finished_at_seconds = event.timestamp;
    request.loading_error = event.errorText;
  };
  client.on("Target.receivedMessageFromTarget", (event: CdpTargetMessage) => {
    let message: WorkerProtocolMessage;
    try {
      message = JSON.parse(event.message) as WorkerProtocolMessage;
    } catch (error) {
      errors.push(error instanceof Error ? error.message : String(error));
      return;
    }
    switch (message.method) {
      case "Network.requestWillBeSent":
        onRequest(event.sessionId, message.params as CdpRequestWillBeSent);
        break;
      case "Network.responseReceived":
        onResponse(event.sessionId, message.params as CdpResponseReceived);
        break;
      case "Network.loadingFinished":
        onFinished(event.sessionId, message.params as CdpLoadingFinished);
        break;
      case "Network.loadingFailed":
        onFailed(event.sessionId, message.params as CdpLoadingFailed);
        break;
    }
  });
  client.on("Target.attachedToTarget", (event: CdpTargetAttached) => {
    const configureWorker = async (): Promise<void> => {
      if (event.targetInfo.type === "worker") {
        await sendToWorker(event.sessionId, "Network.enable");
        await sendToWorker(event.sessionId, "Network.setCacheDisabled", {
          cacheDisabled: true,
        });
        await sendToWorker(
          event.sessionId,
          "Network.emulateNetworkConditions",
          {
            offline: false,
            latency: throttle.latency_ms,
            downloadThroughput: throttle.download_bytes_per_second,
            uploadThroughput: throttle.upload_bytes_per_second,
            connectionType:
              throttle === MOBILE_THROTTLE ? "cellular4g" : "ethernet",
          },
        );
      }
      await sendToWorker(event.sessionId, "Runtime.runIfWaitingForDebugger");
    };
    void configureWorker().catch((error: unknown) => {
      errors.push(
        `${event.targetInfo.type} ${event.targetInfo.url}: ${
          error instanceof Error ? error.message : String(error)
        }`,
      );
    });
  });
  await client.send("Target.setAutoAttach", {
    autoAttach: true,
    waitForDebuggerOnStart: true,
    flatten: false,
  });
  return { requests, errors };
};

const settleFrames = async (page: Page): Promise<number> =>
  page.evaluate(
    () =>
      new Promise<number>((resolve) => {
        requestAnimationFrame(() => {
          requestAnimationFrame(() => resolve(performance.now()));
        });
      }),
  );

type ClickHandlerTiming = {
  intervalStartTimeMs: number;
  startTimeMs: number;
  endTimeMs: number;
  durationMs: number;
};

const measureClickHandler = async (
  page: Page,
  selector: string,
): Promise<ClickHandlerTiming> => {
  const intervalStartTimeMs = await page.evaluate(() => performance.now());
  const handler = await page.evaluate((targetSelector) => {
    const target = document.querySelector<HTMLElement>(targetSelector);
    if (target === null) {
      throw new Error(`interaction target ${targetSelector} is unavailable`);
    }
    const startTimeMs = performance.now();
    target.click();
    const endTimeMs = performance.now();
    return {
      startTimeMs,
      endTimeMs,
      durationMs: endTimeMs - startTimeMs,
    };
  }, selector);
  return { intervalStartTimeMs, ...handler };
};

const measuredInteraction = (
  label: string,
  handler: ClickHandlerTiming,
  settledAtMs: number,
  outcome: MeasuredInteraction["outcome"],
): MeasuredInteraction => ({
  label,
  handler_duration_ms: handler.durationMs,
  settle_duration_ms: settledAtMs - handler.intervalStartTimeMs,
  responsiveness_interval: {
    label,
    start_time_ms: handler.intervalStartTimeMs,
    end_time_ms: settledAtMs,
  },
  outcome,
});

const installPrimaryMemoryGate = async (
  page: Page,
  scenario: Scenario,
): Promise<{
  release: () => void;
  waitUntilEngaged: () => Promise<readonly string[]>;
}> => {
  let releaseGate = (): void => undefined;
  const held = new Promise<void>((resolve) => {
    releaseGate = resolve;
  });
  const expectedSourceCount = scenario === "node" ? 1 : 2;
  const heldSecondManifestSources = new Set<string>();
  let resolveEngaged = (_sourceIds: readonly string[]): void => undefined;
  const engaged = new Promise<readonly string[]>((resolve) => {
    resolveEngaged = resolve;
  });
  const manifestCounts = new Map<string, number>();
  const primaryClassifier =
    scenario === "node" ? "transaction_properties" : "knots_bip110";
  await page.route(
    /\/api\/v2\/sources\/[^/]+\/mempool(?:\/stages\/.*)?$/,
    async (route) => {
      const resource = v2Route(route.request().url());
      let requiredForPrimary = false;
      if (resource?.resource_kind === "manifest") {
        const count = (manifestCounts.get(resource.source_id) ?? 0) + 1;
        manifestCounts.set(resource.source_id, count);
        requiredForPrimary = count === 1;
        if (count === 2) {
          heldSecondManifestSources.add(resource.source_id);
          if (heldSecondManifestSources.size === expectedSourceCount) {
            resolveEngaged([...heldSecondManifestSources].sort());
          }
        }
      } else if (resource?.resource_kind === "stage") {
        requiredForPrimary =
          resource.stage_kind === "population" ||
          (resource.stage_kind === "classifier" &&
            resource.classifier_id === primaryClassifier);
      }
      if (!requiredForPrimary) await held;
      await route.continue();
    },
  );
  return {
    release: releaseGate,
    waitUntilEngaged: async () => {
      let timeout: ReturnType<typeof setTimeout> | undefined;
      try {
        return await Promise.race([
          engaged,
          new Promise<never>((_resolve, reject) => {
            timeout = setTimeout(
              () =>
                reject(
                  new Error(
                    `primary-memory completion gate did not hold ${expectedSourceCount} second manifest request(s)`,
                  ),
                ),
              30_000,
            );
          }),
        ]);
      } finally {
        if (timeout !== undefined) clearTimeout(timeout);
      }
    },
  };
};

const measurePrimaryMemorySample = async (
  browser: Browser,
  viewport: { width: number; height: number } | null,
  profile: string,
  scenario: Scenario,
  baseURL: string,
): Promise<{
  heap: HeapUsage;
  memory: MemoryResult;
  crossOriginIsolated: boolean;
  heldSecondManifestSourceIds: readonly string[];
}> => {
  const context = await browser.newContext({
    baseURL,
    viewport: viewport ?? { width: 1440, height: 900 },
  });
  const page = await context.newPage();
  const client = await context.newCDPSession(page);
  await client.send("HeapProfiler.enable");
  const throttle = await configureProfile(client, profile);
  const capture = await captureV2Requests(client, throttle);
  const gate = await installPrimaryMemoryGate(page, scenario);
  const path =
    scenario === "node"
      ? "/?source=perf-node-01"
      : "/compare/?left=perf-node-01&right=perf-node-02";
  try {
    await page.goto(path, {
      waitUntil: "domcontentloaded",
      timeout: 180_000,
    });
    await markTime(page, `atlas:${scenario}:primary-interactive`);
    const heldSecondManifestSourceIds = await gate.waitUntilEngaged();
    await settleFrames(page);
    await client.send("HeapProfiler.collectGarbage");
    const heap = await client.send("Runtime.getHeapUsage");
    const memory = await measureMemory(page);
    const crossOriginIsolated = await page.evaluate(
      () => window.crossOriginIsolated,
    );
    if (capture.errors.length > 0) {
      throw new Error(
        `primary-memory worker CDP capture failed: ${capture.errors.join("; ")}`,
      );
    }
    return {
      heap,
      memory,
      crossOriginIsolated,
      heldSecondManifestSourceIds,
    };
  } finally {
    gate.release();
    await context.close();
  }
};

const measureMemory = async (page: Page): Promise<MemoryResult> =>
  page.evaluate(async () => {
    const milestone = Date.now();
    const measure = performance.measureUserAgentSpecificMemory;
    if (measure === undefined) {
      return {
        supported: false,
        bytes: null,
        breakdown: [],
        milestone_timestamp_ms: milestone,
        resolved_timestamp_ms: Date.now(),
        error: "performance.measureUserAgentSpecificMemory is unavailable",
      };
    }
    try {
      const measurement = await Promise.race([
        measure.call(performance),
        new Promise<never>((_resolve, reject) => {
          setTimeout(
            () => reject(new Error("memory measurement timed out")),
            120_000,
          );
        }),
      ]);
      return {
        supported: true,
        bytes: measurement.bytes,
        breakdown: measurement.breakdown,
        milestone_timestamp_ms: milestone,
        resolved_timestamp_ms: Date.now(),
        error: null,
      };
    } catch (error) {
      return {
        supported: true,
        bytes: null,
        breakdown: [],
        milestone_timestamp_ms: milestone,
        resolved_timestamp_ms: Date.now(),
        error: error instanceof Error ? error.message : String(error),
      };
    }
  });

const header = (
  headers: Record<string, string | number>,
  name: string,
): string | null => {
  const entry = Object.entries(headers).find(
    ([key]) => key.toLowerCase() === name.toLowerCase(),
  );
  return entry === undefined ? null : String(entry[1]);
};

const numericHeader = (
  headers: Record<string, string | number>,
  name: string,
): number | null => {
  const raw = header(headers, name);
  if (raw === null) return null;
  const value = Number(raw);
  return Number.isFinite(value) ? value : null;
};

const serverTimingDuration = (
  headers: Record<string, string | number>,
): number | null => {
  const value = header(headers, "server-timing");
  const match = value?.match(/(?:^|,)\s*atlas;dur=([0-9.]+)/);
  if (match === undefined || match === null) return null;
  const duration = Number(match[1]);
  return Number.isFinite(duration) ? duration : null;
};

const requestsAsTransfers = (
  requests: Map<string, CdpRequestState>,
): V2Transfer[] =>
  [...requests.values()].map((request) => {
    const route = v2Route(request.url);
    if (route === null) throw new Error(`unrecognized v2 route ${request.url}`);
    const responseAt = request.response_at_seconds;
    const finishedAt = request.finished_at_seconds;
    return {
      ...route,
      request_id: request.request_id,
      url: request.url,
      method: request.method,
      status: request.status,
      content_encoding: header(request.response_headers, "content-encoding"),
      content_id: header(request.response_headers, "x-atlas-content-id"),
      encoded_body_bytes: numericHeader(
        request.response_headers,
        "content-length",
      ),
      decoded_body_bytes: numericHeader(
        request.response_headers,
        "x-atlas-uncompressed-length",
      ),
      cdp_encoded_data_length: request.encoded_data_length,
      ttfb_ms:
        responseAt === null
          ? null
          : (responseAt - request.started_at_seconds) * 1_000,
      // Concurrent worker events can expose a later response timestamp than
      // loadingFinished; preserve the only valid duration bound.
      transfer_excluding_ttfb_ms:
        responseAt === null || finishedAt === null
          ? null
          : Math.max(0, (finishedAt - responseAt) * 1_000),
      server_processing_ms: serverTimingDuration(request.response_headers),
      started_at_seconds: request.started_at_seconds,
      response_at_seconds: responseAt,
      finished_at_seconds: finishedAt,
      loading_error: request.loading_error,
    };
  });

const resultPath = (profile: string, scenario: string): string =>
  `${RESULT_DIRECTORY}${profile}-${scenario}.json`;

const writeResult = (
  profile: string,
  scenario: string,
  result: unknown,
): void => {
  mkdirSync(RESULT_DIRECTORY, { recursive: true });
  writeFileSync(
    resultPath(profile, scenario),
    `${JSON.stringify(result, null, 2)}\n`,
  );
};

const readBuildInfo = async (page: Page): Promise<BuildInfo> => {
  const response = await page.request.get(
    new URL("/__perf/build.json", page.url()).href,
  );
  if (!response.ok()) throw new Error("performance build metadata unavailable");
  return (await response.json()) as BuildInfo;
};

const readManifests = async (
  page: Page,
  sourceIds: readonly string[],
): Promise<StagedManifest[]> =>
  Promise.all(
    sourceIds.map(async (sourceId) => {
      const response = await page.request.get(
        new URL(
          `/api/v2/sources/${encodeURIComponent(sourceId)}/mempool`,
          page.url(),
        ).href,
      );
      if (!response.ok()) {
        throw new Error(`performance manifest unavailable for ${sourceId}`);
      }
      const manifest = (await response.json()) as StagedManifest;
      if (
        manifest.schema_version !== 2 ||
        manifest.source_id !== sourceId ||
        !Array.isArray(manifest.stages) ||
        manifest.stages.length === 0
      ) {
        throw new Error(`invalid performance manifest for ${sourceId}`);
      }
      return manifest;
    }),
  );

const descriptorKey = (sourceId: string, descriptor: StageDescriptor): string =>
  `${sourceId}:${descriptor.kind}:${descriptor.classifier_id ?? ""}:${descriptor.content_id}`;

const transferKey = (transfer: V2Transfer): string | null =>
  transfer.resource_kind === "manifest"
    ? null
    : `${transfer.source_id}:${transfer.stage_kind}:${transfer.classifier_id ?? ""}:${transfer.stage_id}`;

const successfulBodies = (transfers: readonly V2Transfer[]): V2Transfer[] =>
  transfers.filter(
    (transfer) =>
      transfer.method === "GET" &&
      transfer.status === 200 &&
      transfer.loading_error === null,
  );

const firstSuccessful = (
  transfers: readonly V2Transfer[],
  predicate: (transfer: V2Transfer) => boolean,
): V2Transfer => {
  const transfer = successfulBodies(transfers).find(predicate);
  if (transfer === undefined)
    throw new Error("required v2 body was not loaded");
  return transfer;
};

const finiteTotal = (
  transfers: readonly V2Transfer[],
  field:
    "encoded_body_bytes" | "decoded_body_bytes" | "cdp_encoded_data_length",
): number =>
  transfers.reduce((total, transfer) => {
    const value = transfer[field];
    if (value === null || !Number.isFinite(value) || value <= 0) {
      throw new Error(`${transfer.url} has no finite ${field}`);
    }
    return total + value;
  }, 0);

const quorumTotals = (transfers: readonly V2Transfer[]): QuorumTotals => {
  if (transfers.length === 0) throw new Error("v2 quorum has no requests");
  const starts = transfers.map((transfer) => transfer.started_at_seconds);
  const responses = transfers.map((transfer) => transfer.response_at_seconds);
  const finishes = transfers.map((transfer) => transfer.finished_at_seconds);
  if (
    responses.some((value) => value === null || !Number.isFinite(value)) ||
    finishes.some((value) => value === null || !Number.isFinite(value))
  ) {
    throw new Error("v2 quorum has unfinished requests");
  }
  const responseValues = responses as number[];
  const finishValues = finishes as number[];
  return {
    encoded_body_bytes: finiteTotal(transfers, "encoded_body_bytes"),
    decoded_body_bytes: finiteTotal(transfers, "decoded_body_bytes"),
    cdp_encoded_data_length: finiteTotal(transfers, "cdp_encoded_data_length"),
    request_count: transfers.length,
    manifest_count: transfers.filter(
      (transfer) => transfer.resource_kind === "manifest",
    ).length,
    stage_count: transfers.filter(
      (transfer) => transfer.resource_kind === "stage",
    ).length,
    stage_ids: transfers.flatMap((transfer) =>
      transfer.resource_kind === "stage" ? [transfer.stage_id] : [],
    ),
    request_span_ms: (Math.max(...finishValues) - Math.min(...starts)) * 1_000,
    transfer_window_ms:
      (Math.max(...finishValues) - Math.min(...responseValues)) * 1_000,
    summed_ttfb_ms: transfers.reduce(
      (total, transfer) => total + (transfer.ttfb_ms ?? 0),
      0,
    ),
    summed_server_processing_ms: transfers.reduce(
      (total, transfer) => total + (transfer.server_processing_ms ?? 0),
      0,
    ),
  };
};

const markTime = async (page: Page, name: string): Promise<number> => {
  await page.waitForFunction(
    (markName) => performance.getEntriesByName(markName, "mark").length > 0,
    name,
    { timeout: 180_000 },
  );
  return page.evaluate((markName) => {
    const mark = performance.getEntriesByName(markName, "mark")[0];
    if (mark === undefined)
      throw new Error(`missing readiness mark ${markName}`);
    return mark.startTime;
  }, name);
};

const markNow = (page: Page, name: string): Promise<number> =>
  page.evaluate((markName) => {
    performance.clearMarks(markName);
    performance.mark(markName);
    return performance.getEntriesByName(markName, "mark")[0]!.startTime;
  }, name);

const finiteNonNegative = (value: unknown, label: string): number => {
  if (typeof value !== "number" || !Number.isFinite(value) || value < 0) {
    throw new Error(`${label} is not a finite non-negative number`);
  }
  return value;
};

const workerTimingMark = async (
  page: Page,
  sourceId: string,
  quorum: "primary" | "complete",
): Promise<SourceWorkerQuorumTiming> => {
  const name = `atlas:${sourceId}:${quorum}-worker-timing`;
  await page.waitForFunction(
    (markName) => performance.getEntriesByName(markName, "mark").length > 0,
    name,
    { timeout: 180_000 },
  );
  const detail = await page.evaluate((markName) => {
    const mark = performance.getEntriesByName(markName, "mark")[0] as
      PerformanceMark | undefined;
    if (mark === undefined) throw new Error(`missing worker mark ${markName}`);
    return mark.detail as WorkerQuorumTiming;
  }, name);
  if (!Array.isArray(detail.stages) || detail.stages.length === 0) {
    throw new Error(`${name} has no stage timings`);
  }
  finiteNonNegative(
    detail.manifestFetchValidateMs,
    `${name}.manifestFetchValidateMs`,
  );
  finiteNonNegative(
    detail.semanticValidationMs,
    `${name}.semanticValidationMs`,
  );
  finiteNonNegative(detail.quorumMs, `${name}.quorumMs`);
  finiteNonNegative(
    detail.supersessionRestarts,
    `${name}.supersessionRestarts`,
  );
  if (!Number.isInteger(detail.supersessionRestarts)) {
    throw new Error(`${name}.supersessionRestarts is not an integer`);
  }
  detail.stages.forEach((stage, index) => {
    if (
      !["population", "membership", "structure", "classifier"].includes(
        stage.kind,
      )
    ) {
      throw new Error(`${name}.stages[${index}].kind is invalid`);
    }
    if (typeof stage.reused !== "boolean") {
      throw new Error(`${name}.stages[${index}].reused is invalid`);
    }
    finiteNonNegative(
      stage.fetchDigestParseMs,
      `${name}.stages[${index}].fetchDigestParseMs`,
    );
    finiteNonNegative(
      stage.decodeValidatePackMs,
      `${name}.stages[${index}].decodeValidatePackMs`,
    );
  });
  return { source_id: sourceId, ...detail };
};

const workerTimingTotals = (
  timings: readonly SourceWorkerQuorumTiming[],
): WorkerTimingTotals => {
  if (timings.length === 0) throw new Error("worker timing quorum is empty");
  return {
    source_count: timings.length,
    stage_count: timings.reduce(
      (total, timing) => total + timing.stages.length,
      0,
    ),
    reused_stage_count: timings.reduce(
      (total, timing) =>
        total + timing.stages.filter((stage) => stage.reused).length,
      0,
    ),
    fetched_stage_count: timings.reduce(
      (total, timing) =>
        total + timing.stages.filter((stage) => !stage.reused).length,
      0,
    ),
    manifest_fetch_validate_ms: timings.reduce(
      (total, timing) => total + timing.manifestFetchValidateMs,
      0,
    ),
    stage_fetch_digest_parse_ms: timings.reduce(
      (total, timing) =>
        total +
        timing.stages.reduce(
          (stageTotal, stage) => stageTotal + stage.fetchDigestParseMs,
          0,
        ),
      0,
    ),
    stage_decode_validate_pack_ms: timings.reduce(
      (total, timing) =>
        total +
        timing.stages.reduce(
          (stageTotal, stage) => stageTotal + stage.decodeValidatePackMs,
          0,
        ),
      0,
    ),
    semantic_validation_ms: timings.reduce(
      (total, timing) => total + timing.semanticValidationMs,
      0,
    ),
    summed_quorum_ms: timings.reduce(
      (total, timing) => total + timing.quorumMs,
      0,
    ),
    maximum_source_quorum_ms: Math.max(
      ...timings.map((timing) => timing.quorumMs),
    ),
    supersession_restarts: timings.reduce(
      (total, timing) => total + timing.supersessionRestarts,
      0,
    ),
  };
};

const measureReplacementRetained = async (
  page: Page,
  scenario: Scenario,
): Promise<ReplacementMeasurement> => {
  await page.evaluate(() => {
    let held = true;
    window.__atlasCandidateReadyHook = async () => {
      if (!held) return;
      await new Promise<void>((resolve) => {
        window.__atlasPerfReleaseCandidate = () => {
          held = false;
          resolve();
        };
      });
    };
  });
  const button = page.locator(
    scenario === "node" ? "#refresh" : "#comparison-refresh",
  );
  const replacementStart = await markNow(
    page,
    `atlas:${scenario}:replacement-responsiveness-start`,
  );
  await button.click();
  const candidateMark = `atlas:${scenario}:replacement-candidate-ready`;
  const candidateReady = await markTime(page, candidateMark);
  let memory: MemoryResult;
  let commitStart = candidateReady;
  let replacementEnd = candidateReady;
  try {
    memory = await measureMemory(page);
  } finally {
    commitStart = await markNow(
      page,
      `atlas:${scenario}:replacement-commit-responsiveness-start`,
    );
    await page.evaluate(() => window.__atlasPerfReleaseCandidate?.());
    await markTime(page, `atlas:${scenario}:replacement-committed`);
    await expect(button).toBeEnabled({ timeout: 180_000 });
    await settleFrames(page);
    replacementEnd = await markNow(
      page,
      `atlas:${scenario}:replacement-responsiveness-end`,
    );
    await page.evaluate(() => {
      delete window.__atlasCandidateReadyHook;
      delete window.__atlasPerfReleaseCandidate;
    });
  }
  return {
    memory,
    responsivenessIntervals: [
      {
        label: "replacement-prepare",
        start_time_ms: replacementStart,
        end_time_ms: candidateReady,
      },
      {
        label: "replacement-commit",
        start_time_ms: commitStart,
        end_time_ms: replacementEnd,
      },
    ],
  };
};

const filterSummaryCounts = (
  summary: string,
): { filtered: number; total: number } => {
  const match = summary.match(/^Showing ([\d,]+) of ([\d,]+) transactions\.$/);
  if (match === null) {
    throw new Error(`node filter summary is invalid: ${summary}`);
  }
  return {
    filtered: Number((match[1] ?? "").replaceAll(",", "")),
    total: Number((match[2] ?? "").replaceAll(",", "")),
  };
};

const measureNodePackedStoreInteractions = async (
  page: Page,
): Promise<MeasuredInteraction[]> => {
  const feeAgeHandler = await measureClickHandler(page, "#fee-age-tab");
  await expect(page.locator("#fee-age-tab")).toHaveAttribute(
    "aria-selected",
    "true",
  );
  await expect(page.locator("#mempool-canvas")).toHaveAttribute(
    "aria-label",
    "Fee rate by age view containing 70,000 filtered transactions.",
  );
  const feeAgeSettledAtMs = await settleFrames(page);
  const feeAgeMeasurement = measuredInteraction(
    "node-fee-rate-by-age-activation",
    feeAgeHandler,
    feeAgeSettledAtMs,
    {
      selected_lens: "fee-rate-by-age",
      rendered_transaction_count: 70_000,
    },
  );

  await page.locator("#minimum-fee-rate").fill("2");
  const filterHandler = await measureClickHandler(
    page,
    '#filters button[type="submit"]',
  );
  await expect(page.locator("#filter-summary")).toHaveText(
    /^Showing [\d,]+ of 70,000 transactions\.$/,
  );
  const summary = (await page.locator("#filter-summary").textContent()) ?? "";
  const counts = filterSummaryCounts(summary);
  await expect(page.locator("#mempool-canvas")).toHaveAttribute(
    "aria-label",
    new RegExp(
      `^Fee rate by age view containing ${counts.filtered.toLocaleString("en-US")} filtered transactions\\.$`,
    ),
  );
  const filterSettledAtMs = await settleFrames(page);
  const filterMeasurement = measuredInteraction(
    "node-filter-submit",
    filterHandler,
    filterSettledAtMs,
    {
      minimum_fee_rate: 2,
      filtered_transaction_count: counts.filtered,
      source_transaction_count: counts.total,
      rendered_transaction_count: counts.filtered,
    },
  );

  if (
    counts.filtered <= 0 ||
    counts.filtered >= counts.total ||
    counts.total !== 70_000
  ) {
    throw new Error("node interaction did not exercise the production fixture");
  }
  return [feeAgeMeasurement, filterMeasurement];
};

const measureComparisonPackedStoreInteraction = async (
  page: Page,
): Promise<MeasuredInteraction[]> => {
  const root = page.locator("#comparison-distributions");
  await expect(root).toBeVisible();
  await expect(root).toHaveAttribute("aria-busy", "false");
  const handler = await measureClickHandler(page, "#dist-scope-common");
  await expect(page.locator("#dist-scope-common")).toHaveAttribute(
    "aria-pressed",
    "true",
  );
  await expect(root).toHaveAttribute("aria-busy", "false", {
    timeout: 180_000,
  });
  await expect(page.locator("#dist-comp-left-title")).toContainText(
    "present in both",
  );
  const settledAtMs = await settleFrames(page);
  return [
    measuredInteraction(
      "comparison-distribution-scope-switch",
      handler,
      settledAtMs,
      {
        selected_scope: "common",
        left_model_committed: true,
        right_model_committed: true,
      },
    ),
  ];
};

const measureBip110RuleNavigation = async (
  page: Page,
): Promise<Bip110RuleNavigationMeasurement> => {
  await page.locator("#terrain-tab").click();
  await settleFrames(page);
  await page.evaluate(() => {
    const classifier = document.querySelector<HTMLSelectElement>(
      "#classification-lens-select",
    );
    if (classifier === null) {
      throw new Error("classifier selector is unavailable");
    }
    classifier.value = "knots_bip110";
    classifier.dispatchEvent(new Event("change", { bubbles: true }));
  });
  await settleFrames(page);
  const start = await markNow(page, "atlas:node:bip110-rule-navigation-start");
  const ruleIds = await page.evaluate(() => {
    const classifier = document.querySelector<HTMLSelectElement>(
      "#classification-lens-select",
    );
    if (classifier?.value !== "knots_bip110") {
      throw new Error("BIP-110 classifier did not become active");
    }
    return [
      ...document.querySelectorAll<HTMLButtonElement>(
        "#rule-list button[data-rule]",
      ),
    ].flatMap(({ dataset }) =>
      dataset.rule === undefined ? [] : [dataset.rule],
    );
  });
  const handlerDurationsMs: number[] = [];
  for (const ruleId of ruleIds) {
    handlerDurationsMs.push(
      await page.evaluate((selectedRuleId) => {
        const button = [
          ...document.querySelectorAll<HTMLButtonElement>(
            "#rule-list button[data-rule]",
          ),
        ].find(({ dataset }) => dataset.rule === selectedRuleId);
        if (button === undefined) {
          throw new Error(`BIP-110 rule ${selectedRuleId} is unavailable`);
        }
        const handlerStart = performance.now();
        button.click();
        return performance.now() - handlerStart;
      }, ruleId),
    );
    await settleFrames(page);
  }
  const selectedRule = await page.evaluate(
    () =>
      document.querySelector<HTMLButtonElement>(
        '#rule-list button[data-rule][aria-pressed="true"]',
      )?.dataset.rule ?? null,
  );
  const end = await markNow(page, "atlas:node:bip110-rule-navigation-end");
  return {
    ruleIds,
    selectedRule,
    handlerDurationsMs,
    handlerDurationMs: Math.max(0, ...handlerDurationsMs),
    responsivenessInterval: {
      label: "bip110-rule-navigation",
      start_time_ms: start,
      end_time_ms: end,
    },
  };
};

const runScenario = async (
  page: Page,
  profile: string,
  scenario: Scenario,
  baseURL: string,
): Promise<void> => {
  const browser = page.context().browser();
  if (browser === null) throw new Error("performance browser is unavailable");
  const primarySample = await measurePrimaryMemorySample(
    browser,
    page.viewportSize(),
    profile,
    scenario,
    baseURL,
  );
  const client = await page.context().newCDPSession(page);
  await client.send("HeapProfiler.enable");
  const throttle = await configureProfile(client, profile);
  const cdpCapture = await captureV2Requests(client, throttle);
  await installObservers(page);
  const path =
    scenario === "node"
      ? "/?source=perf-node-01"
      : "/compare/?left=perf-node-01&right=perf-node-02";
  const statusSelector =
    scenario === "node" ? "#page-status" : "#comparison-status";
  const primaryMark = `atlas:${scenario}:primary-interactive`;
  const completeMark = `atlas:${scenario}:complete-feature-ready`;

  await page.goto(path, {
    waitUntil: "domcontentloaded",
    timeout: 180_000,
  });
  const primaryInteractionMs = await markTime(page, primaryMark);

  const completeFeatureReadyMs = await markTime(page, completeMark);
  await expect(page.locator(statusSelector)).toHaveAttribute(
    "data-readiness",
    "complete-feature-ready",
  );
  await expect(page.locator(statusSelector)).toHaveAttribute(
    "data-state",
    /ready|stale/,
  );
  await settleFrames(page);

  const readinessMetrics = await page.evaluate(() => window.__atlasPerfMetrics);
  const buildInfo = await readBuildInfo(page);
  const selectedSnapshots = buildInfo.snapshots.filter((snapshot) =>
    scenario === "node"
      ? snapshot.source_id === "perf-node-01"
      : ["perf-node-01", "perf-node-02"].includes(snapshot.source_id),
  );
  const sourceIds = selectedSnapshots.map((snapshot) => snapshot.source_id);
  const manifests = await readManifests(page, sourceIds);
  const [primaryWorkerTimings, completeWorkerTimings] = await Promise.all([
    Promise.all(
      sourceIds.map((sourceId) => workerTimingMark(page, sourceId, "primary")),
    ),
    Promise.all(
      sourceIds.map((sourceId) => workerTimingMark(page, sourceId, "complete")),
    ),
  ]);
  if (cdpCapture.errors.length > 0) {
    throw new Error(
      `worker CDP capture failed: ${cdpCapture.errors.join("; ")}`,
    );
  }
  const transfers = requestsAsTransfers(cdpCapture.requests);
  const successful = successfulBodies(transfers);

  const expectedDescriptors = new Map<string, StageDescriptor>();
  manifests.forEach((manifest) => {
    manifest.stages.forEach((descriptor) => {
      expectedDescriptors.set(
        descriptorKey(manifest.source_id, descriptor),
        descriptor,
      );
    });
  });
  const primaryClassifier =
    scenario === "node" ? "transaction_properties" : "knots_bip110";
  const primaryTransfers: V2Transfer[] = [];
  manifests.forEach((manifest) => {
    primaryTransfers.push(
      firstSuccessful(
        transfers,
        (transfer) =>
          transfer.resource_kind === "manifest" &&
          transfer.source_id === manifest.source_id,
      ),
    );
    for (const descriptor of manifest.stages.filter(
      (entry) =>
        entry.kind === "population" ||
        (entry.kind === "classifier" &&
          entry.classifier_id === primaryClassifier),
    )) {
      const key = descriptorKey(manifest.source_id, descriptor);
      primaryTransfers.push(
        firstSuccessful(transfers, (transfer) => transferKey(transfer) === key),
      );
    }
  });
  const completeTransfers = successful;
  const primaryQuorum = quorumTotals(primaryTransfers);
  const completeQuorum = quorumTotals(completeTransfers);

  const statusCounts = transfers.reduce<Record<string, number>>(
    (counts, transfer) => {
      const status =
        transfer.status === null ? "missing" : String(transfer.status);
      counts[status] = (counts[status] ?? 0) + 1;
      return counts;
    },
    {},
  );
  const stageTransfers = successful.flatMap((transfer) => {
    if (transfer.resource_kind !== "stage") return [];
    const key = transferKey(transfer);
    const descriptor = key === null ? undefined : expectedDescriptors.get(key);
    return [
      {
        source_id: transfer.source_id,
        kind: transfer.stage_kind,
        classifier_id: transfer.classifier_id,
        stage_id: transfer.stage_id,
        descriptor_uncompressed_bytes: descriptor?.uncompressed_bytes ?? null,
        encoded_body_bytes: transfer.encoded_body_bytes,
        decoded_body_bytes: transfer.decoded_body_bytes,
        cdp_encoded_data_length: transfer.cdp_encoded_data_length,
        content_encoding: transfer.content_encoding,
        ttfb_ms: transfer.ttfb_ms,
        transfer_excluding_ttfb_ms: transfer.transfer_excluding_ttfb_ms,
        server_processing_ms: transfer.server_processing_ms,
      },
    ];
  });

  const preInstrumentationEnd = await markNow(
    page,
    `atlas:${scenario}:responsiveness-pre-instrumentation-end`,
  );
  const replacement = await measureReplacementRetained(page, scenario);
  await settleFrames(page);
  await client.send("HeapProfiler.collectGarbage");
  const completeHeap = await client.send("Runtime.getHeapUsage");
  const completeMemory = await measureMemory(page);
  const measuredInteractions =
    scenario === "node"
      ? await measureNodePackedStoreInteractions(page)
      : await measureComparisonPackedStoreInteraction(page);
  const bip110RuleNavigation =
    scenario === "node" ? await measureBip110RuleNavigation(page) : null;
  await settleFrames(page);
  const bip110MemorySample =
    bip110RuleNavigation === null
      ? null
      : await (async () => {
          await client.send("HeapProfiler.collectGarbage");
          const pageHeap = await client.send("Runtime.getHeapUsage");
          return {
            page_heap: {
              used_size_bytes: pageHeap.usedSize,
              total_size_bytes: pageHeap.totalSize,
              forced_collection: true,
            },
            worker_inclusive_memory: await measureMemory(page),
          };
        })();
  const browserMetrics = await page.evaluate(() => window.__atlasPerfMetrics);
  const domCount = await page.locator("*").count();
  const metadataReady = readinessMetrics.metadata_ready_ms;
  if (metadataReady === null) {
    throw new Error("metadata readiness was not recorded");
  }
  const responsivenessIntervals: readonly ResponsivenessInterval[] = [
    {
      label: "post-metadata-pre-instrumentation",
      start_time_ms: metadataReady,
      end_time_ms: preInstrumentationEnd,
    },
    ...replacement.responsivenessIntervals,
    ...measuredInteractions.map(
      ({ responsiveness_interval: interval }) => interval,
    ),
    ...(bip110RuleNavigation === null
      ? []
      : [bip110RuleNavigation.responsivenessInterval]),
  ];
  const insideResponsivenessInterval = (startTimeMs: number): boolean =>
    responsivenessIntervals.some(
      ({ start_time_ms: start, end_time_ms: end }) =>
        startTimeMs >= start && startTimeMs <= end,
    );
  const responsivenessLongTasks = browserMetrics.long_tasks.filter((task) =>
    insideResponsivenessInterval(task.start_time_ms),
  );
  const responsivenessFrames = browserMetrics.animation_frame_callbacks.filter(
    (callback) => insideResponsivenessInterval(callback.start_time_ms),
  );
  const maximumResponsivenessLongTaskMs = Math.max(
    0,
    ...responsivenessLongTasks.map((task) => task.duration_ms),
  );
  const maximumFrameCallbackMs = Math.max(
    0,
    ...responsivenessFrames.map((callback) => callback.duration_ms),
  );
  const maximumInteractionHandlerMs = Math.max(
    ...measuredInteractions.map(
      ({ handler_duration_ms }) => handler_duration_ms,
    ),
  );
  const maximumInteractionSettleMs = Math.max(
    ...measuredInteractions.map(({ settle_duration_ms }) => settle_duration_ms),
  );
  const gate = RELEASE_GATES[scenario];
  const result = {
    schema_version: 2,
    result_kind: "stable-success",
    captured_at: new Date().toISOString(),
    profile,
    scenario,
    path,
    browser_version: page.context().browser()?.version() ?? "unknown",
    viewport: page.viewportSize(),
    throttle,
    web_build_id: buildInfo.web_build_id,
    fixture_manifest_version: buildInfo.fixture_manifest_version,
    fixture_generated_at_ms: buildInfo.fixture_generated_at_ms,
    snapshot_transaction_count: selectedSnapshots[0]?.transaction_count ?? 0,
    differing_wtxid_count: selectedSnapshots[0]?.differing_wtxid_count ?? 0,
    metadata_usable_ms: metadataReady,
    primary_interaction_ms: primaryInteractionMs,
    complete_feature_ready_ms: completeFeatureReadyMs,
    publication_descriptors: manifests.map((manifest) => ({
      source_id: manifest.source_id,
      publication_id: manifest.publication_id,
      transaction_count: manifest.transaction_count,
      stages: manifest.stages,
    })),
    v2_requests: transfers,
    request_outcomes: {
      status_counts: statusCounts,
      successful_manifest_bodies: successful.filter(
        (transfer) => transfer.resource_kind === "manifest",
      ).length,
      successful_stage_bodies: successful.filter(
        (transfer) => transfer.resource_kind === "stage",
      ).length,
      recovery_faults_supported: true,
    },
    stage_transfers: stageTransfers,
    worker_timings: {
      primary: primaryWorkerTimings,
      complete: completeWorkerTimings,
    },
    quorum_totals: {
      primary: {
        ...primaryQuorum,
        worker: workerTimingTotals(primaryWorkerTimings),
      },
      complete: {
        ...completeQuorum,
        worker: workerTimingTotals(completeWorkerTimings),
      },
    },
    cls: browserMetrics.cls,
    layout_shifts: browserMetrics.layout_shifts,
    lcp_entries: browserMetrics.lcp,
    long_tasks: browserMetrics.long_tasks,
    responsiveness_intervals: responsivenessIntervals,
    responsiveness_long_tasks: responsivenessLongTasks,
    maximum_responsiveness_long_task_ms: maximumResponsivenessLongTaskMs,
    animation_frame_callbacks: browserMetrics.animation_frame_callbacks,
    responsiveness_animation_frame_callbacks: responsivenessFrames,
    maximum_animation_frame_callback_ms: maximumFrameCallbackMs,
    measured_interactions: measuredInteractions,
    maximum_interaction_handler_ms: maximumInteractionHandlerMs,
    maximum_interaction_settle_ms: maximumInteractionSettleMs,
    readiness_contract: {
      complete_models_committed: true,
      deferred_density_raster_excluded: true,
    },
    bip110_rule_navigation: bip110RuleNavigation,
    bip110_memory_sample: bip110MemorySample,
    dom_count: domCount,
    primary_memory_sample: {
      isolated_context: true,
      cross_origin_isolated: primarySample.crossOriginIsolated,
      completion_gate: {
        held_second_manifest_source_ids:
          primarySample.heldSecondManifestSourceIds,
      },
      page_heap: {
        used_size_bytes: primarySample.heap.usedSize,
        total_size_bytes: primarySample.heap.totalSize,
        forced_collection: true,
      },
      worker_inclusive_memory: primarySample.memory,
    },
    page_heap: {
      complete: {
        used_size_bytes: completeHeap.usedSize,
        total_size_bytes: completeHeap.totalSize,
        forced_collection: true,
      },
    },
    worker_inclusive_memory: {
      complete: completeMemory,
      replacement_retained: replacement.memory,
      replacement_retained_gate_bytes: gate.replacement_retained_bytes,
    },
    cross_origin_isolated: await page.evaluate(() => crossOriginIsolated),
  };
  writeResult(profile, scenario, result);

  expect(result.cross_origin_isolated).toBe(true);
  expect(
    result.metadata_usable_ms,
    "metadata milestone was not recorded",
  ).not.toBeNull();
  expect(
    result.metadata_usable_ms ?? Number.POSITIVE_INFINITY,
  ).toBeLessThanOrEqual(RELEASE_GATES.metadata_usable_ms);
  expect(result.primary_interaction_ms).toBeLessThanOrEqual(
    gate.primary_interaction_ms,
  );
  expect(result.complete_feature_ready_ms).toBeLessThanOrEqual(
    gate.complete_feature_ready_ms,
  );
  expect(primaryQuorum.encoded_body_bytes).toBeLessThanOrEqual(
    gate.primary_encoded_body_bytes,
  );
  expect(completeQuorum.encoded_body_bytes).toBeLessThanOrEqual(
    gate.complete_encoded_body_bytes,
  );
  expect(statusCounts["304"] ?? 0).toBe(0);
  expect(statusCounts["409"] ?? 0).toBe(0);
  expect(statusCounts["429"] ?? 0).toBe(0);
  expect(statusCounts.missing ?? 0).toBe(0);
  expect(
    Object.keys(statusCounts).filter((status) => status !== "200"),
  ).toHaveLength(0);
  for (const manifest of manifests) {
    const primaryTiming = primaryWorkerTimings.find(
      (timing) => timing.source_id === manifest.source_id,
    );
    const completeTiming = completeWorkerTimings.find(
      (timing) => timing.source_id === manifest.source_id,
    );
    expect(primaryTiming?.stages).toHaveLength(2);
    expect(primaryTiming?.stages.every((stage) => !stage.reused)).toBe(true);
    expect(completeTiming?.stages).toHaveLength(manifest.stages.length);
    for (const stage of completeTiming?.stages ?? []) {
      const reused =
        stage.kind === "population" ||
        (stage.kind === "classifier" &&
          stage.classifierId === primaryClassifier);
      expect(stage.reused).toBe(reused);
      if (reused) {
        expect(stage.fetchDigestParseMs).toBe(0);
        expect(stage.decodeValidatePackMs).toBe(0);
      }
    }
  }
  for (const transfer of successful) {
    const key = transferKey(transfer);
    if (key !== null) expect(expectedDescriptors.has(key)).toBe(true);
  }
  for (const manifest of manifests) {
    const manifestBodies = successful.filter(
      (transfer) =>
        transfer.resource_kind === "manifest" &&
        transfer.source_id === manifest.source_id,
    );
    expect(manifestBodies.length).toBeGreaterThanOrEqual(1);
    expect(manifestBodies.length).toBeLessThanOrEqual(2);
    expect(
      new Set(manifestBodies.map(({ content_id }) => content_id)).size,
    ).toBe(1);
    expect(manifestBodies[0]?.content_id).not.toBeNull();
    for (const descriptor of manifest.stages) {
      const key = descriptorKey(manifest.source_id, descriptor);
      const bodies = successful.filter(
        (transfer) => transferKey(transfer) === key,
      );
      expect(bodies).toHaveLength(1);
      expect(bodies[0]?.content_id).toBe(descriptor.content_id);
      expect(bodies[0]?.decoded_body_bytes).toBe(descriptor.uncompressed_bytes);
    }
  }
  expect(expectedDescriptors.size).toBe(
    manifests.reduce((total, manifest) => total + manifest.stages.length, 0),
  );
  expect(result.maximum_responsiveness_long_task_ms).toBeLessThanOrEqual(
    RELEASE_GATES.maximum_responsiveness_long_task_ms,
  );
  expect(result.maximum_interaction_handler_ms).toBeLessThanOrEqual(
    RELEASE_GATES.interaction_handler_ms,
  );
  expect(result.maximum_interaction_settle_ms).toBeLessThanOrEqual(
    RELEASE_GATES.interaction_settle_ms,
  );
  if (bip110RuleNavigation !== null) {
    expect(bip110RuleNavigation.ruleIds).toHaveLength(7);
    expect(new Set(bip110RuleNavigation.ruleIds).size).toBe(7);
    expect(bip110RuleNavigation.handlerDurationsMs).toHaveLength(7);
    expect(Math.max(...bip110RuleNavigation.handlerDurationsMs)).toBe(
      bip110RuleNavigation.handlerDurationMs,
    );
    expect(bip110RuleNavigation.selectedRule).toBe(
      bip110RuleNavigation.ruleIds.at(-1),
    );
    expect(bip110RuleNavigation.handlerDurationMs).toBeLessThanOrEqual(
      RELEASE_GATES.bip110_rule_navigation_handler_ms,
    );
    expect(bip110MemorySample).not.toBeNull();
    expect(gate.bip110_page_heap_bytes).not.toBeNull();
    expect(gate.bip110_cross_context_bytes).not.toBeNull();
    expect(
      bip110MemorySample?.page_heap.used_size_bytes ?? Number.POSITIVE_INFINITY,
    ).toBeLessThanOrEqual(
      gate.bip110_page_heap_bytes ?? Number.NEGATIVE_INFINITY,
    );
    expect(bip110MemorySample?.worker_inclusive_memory.supported).toBe(true);
    expect(bip110MemorySample?.worker_inclusive_memory.error).toBeNull();
    expect(
      bip110MemorySample?.worker_inclusive_memory.bytes ??
        Number.POSITIVE_INFINITY,
    ).toBeLessThanOrEqual(
      gate.bip110_cross_context_bytes ?? Number.NEGATIVE_INFINITY,
    );
  } else {
    expect(bip110MemorySample).toBeNull();
  }
  expect(result.maximum_animation_frame_callback_ms).toBeLessThanOrEqual(
    profile === "mobile-slow-4g"
      ? RELEASE_GATES.maximum_animation_frame_callback_ms["mobile-slow-4g"]
      : RELEASE_GATES.maximum_animation_frame_callback_ms.desktop,
  );
  expect(result.cls, JSON.stringify(result.layout_shifts)).toBeLessThanOrEqual(
    RELEASE_GATES.cls,
  );
  expect(result.primary_memory_sample.isolated_context).toBe(true);
  expect(result.primary_memory_sample.cross_origin_isolated).toBe(true);
  expect(
    result.primary_memory_sample.page_heap.used_size_bytes,
  ).toBeLessThanOrEqual(gate.page_heap_bytes);
  expect(result.page_heap.complete.used_size_bytes).toBeLessThanOrEqual(
    gate.page_heap_bytes,
  );
  expect(result.primary_memory_sample.worker_inclusive_memory.supported).toBe(
    true,
  );
  expect(result.primary_memory_sample.worker_inclusive_memory.error).toBeNull();
  expect(
    result.primary_memory_sample.worker_inclusive_memory.bytes,
  ).not.toBeNull();
  expect(
    result.primary_memory_sample.worker_inclusive_memory.bytes ??
      Number.POSITIVE_INFINITY,
  ).toBeLessThanOrEqual(gate.cross_context_bytes);
  expect(completeMemory.supported).toBe(true);
  expect(completeMemory.error).toBeNull();
  expect(completeMemory.bytes).not.toBeNull();
  expect(completeMemory.bytes ?? Number.POSITIVE_INFINITY).toBeLessThanOrEqual(
    gate.cross_context_bytes,
  );
  expect(replacement.memory.supported).toBe(true);
  expect(replacement.memory.error).toBeNull();
  expect(replacement.memory.bytes).not.toBeNull();
  expect(
    replacement.memory.bytes ?? Number.POSITIVE_INFINITY,
  ).toBeLessThanOrEqual(gate.replacement_retained_bytes);
  if (profile === "desktop") {
    const finalLcp = result.lcp_entries.at(-1);
    const expectedSelector =
      scenario === "node"
        ? "#source-classification-summary"
        : "p.source-card-classification";
    expect(
      finalLcp,
      `desktop ${scenario} recorded no LCP candidate`,
    ).toBeDefined();
    expect(finalLcp?.selector).toBe(expectedSelector);
    expect(
      finalLcp?.render_time_ms ?? Number.POSITIVE_INFINITY,
    ).toBeLessThanOrEqual(2_500);
  }
};

for (const scenario of ["node", "comparison"] as const) {
  test(`${scenario} staged production build`, async ({ page }, testInfo) => {
    const baseURL = testInfo.project.use.baseURL;
    if (typeof baseURL !== "string") {
      throw new Error("performance project must configure a baseURL");
    }
    await runScenario(page, testInfo.project.name, scenario, baseURL);
  });
}

test("node recovers after one manifest failure", async ({ page }, testInfo) => {
  test.skip(testInfo.project.name !== "desktop", "fault cases run once");
  await page.goto("/?source=perf-node-01", { waitUntil: "domcontentloaded" });
  await expect(page.locator("#page-status")).toHaveAttribute(
    "data-readiness",
    "complete-feature-ready",
    { timeout: 180_000 },
  );
  const transactionCount = await page
    .locator("#transaction-count")
    .textContent();
  let injected = false;
  await page.route(
    /\/api\/v2\/sources\/perf-node-01\/mempool(?:\?.*)?$/,
    async (route) => {
      if (!injected) {
        injected = true;
        await route.fulfill({
          status: 503,
          contentType: "application/json",
          body: '{"error":"injected manifest failure"}',
        });
      } else await route.continue();
    },
  );
  await page.locator("#refresh").click();
  await expect(page.locator("#page-status")).toHaveAttribute(
    "data-state",
    "error",
    { timeout: 180_000 },
  );
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
  await page.evaluate(() =>
    performance.clearMarks("atlas:node:complete-feature-ready"),
  );
  await page.locator("#refresh").click();
  const recoveredAtMs = await markTime(
    page,
    "atlas:node:complete-feature-ready",
  );
  await expect(page.locator("#page-status")).toHaveAttribute(
    "data-state",
    /ready|stale/,
  );
  const buildInfo = await readBuildInfo(page);
  writeResult(testInfo.project.name, "node-manifest-retry", {
    schema_version: 2,
    result_kind: "recovery-fault",
    captured_at: new Date().toISOString(),
    profile: testInfo.project.name,
    scenario: "node",
    fault: "refresh-manifest-503-once",
    injected,
    retained_active: true,
    recovered: true,
    recovered_at_ms: recoveredAtMs,
    web_build_id: buildInfo.web_build_id,
  });
});

test("comparison recovers after one completion-stage failure", async ({
  page,
}, testInfo) => {
  test.skip(testInfo.project.name !== "desktop", "fault cases run once");
  await installWorkerRequestObserver(page);
  await page.goto("/compare/?left=perf-node-01&right=perf-node-02", {
    waitUntil: "domcontentloaded",
  });
  await expect(page.locator("#comparison-status")).toHaveAttribute(
    "data-readiness",
    "complete-feature-ready",
    { timeout: 180_000 },
  );
  const sourceIds = await page.locator(".source-card > code").allTextContents();
  const unionCount = await page.locator("#union-count").textContent();
  await page.evaluate(() => {
    window.__atlasWorkerRequests = [];
  });
  let siblingStageHeld = false;
  await page.route(
    /\/api\/v2\/sources\/perf-node-01\/mempool\/stages\/membership\//,
    async (route) => {
      siblingStageHeld = true;
      await new Promise((resolve) => setTimeout(resolve, 1_000));
      await route.continue().catch(() => {});
    },
  );
  let injected = false;
  await page.route(
    /\/api\/v2\/sources\/perf-node-02\/mempool\/stages\/membership\//,
    async (route) => {
      if (!injected) {
        injected = true;
        await route.fulfill({
          status: 503,
          contentType: "application/json",
          body: '{"error":"injected stage failure"}',
        });
      } else await route.continue();
    },
  );
  await page.locator("#comparison-refresh").click();
  await expect(page.locator("#comparison-status")).toHaveAttribute(
    "data-state",
    "error",
    { timeout: 180_000 },
  );
  const siblingCancelled = await page.evaluate(() => {
    const requests = window.__atlasWorkerRequests ?? [];
    const siblingLoad = requests.find(
      (request) =>
        request.type === "load" && request.sourceId === "perf-node-01",
    );
    return (
      siblingLoad !== undefined &&
      requests.some(
        (request) =>
          request.type === "cancel" &&
          request.requestId === siblingLoad.requestId,
      )
    );
  });
  expect(siblingStageHeld).toBe(true);
  expect(siblingCancelled).toBe(true);
  await expect(page.locator(".source-card > code")).toHaveText(sourceIds);
  await expect(page.locator("#union-count")).toHaveText(unionCount ?? "");
  await page.locator("#dist-scope-common").click();
  await expect(page.locator("#dist-scope-common")).toHaveAttribute(
    "aria-pressed",
    "true",
  );
  await page.evaluate(() =>
    performance.clearMarks("atlas:comparison:complete-feature-ready"),
  );
  await page.locator("#comparison-refresh").click();
  const recoveredAtMs = await markTime(
    page,
    "atlas:comparison:complete-feature-ready",
  );
  await expect(page.locator("#comparison-status")).toHaveAttribute(
    "data-state",
    /ready|stale/,
  );
  const buildInfo = await readBuildInfo(page);
  writeResult(testInfo.project.name, "comparison-stage-retry", {
    schema_version: 2,
    result_kind: "recovery-fault",
    captured_at: new Date().toISOString(),
    profile: testInfo.project.name,
    scenario: "comparison",
    fault: "refresh-membership-503-once",
    injected,
    sibling_stage_held: siblingStageHeld,
    retained_active: true,
    sibling_cancelled: siblingCancelled,
    recovered: true,
    recovered_at_ms: recoveredAtMs,
    web_build_id: buildInfo.web_build_id,
  });
});

test("empty worker memory baseline", async ({ page }, testInfo) => {
  const client = await page.context().newCDPSession(page);
  const throttle = await configureProfile(client, testInfo.project.name);
  await page.goto("/__perf/empty-worker.html", { waitUntil: "load" });
  await page.waitForFunction(() => window.__emptyWorkerReady === true);
  await settleFrames(page);
  await client.send("HeapProfiler.enable");
  await client.send("HeapProfiler.collectGarbage");
  const heap = await client.send("Runtime.getHeapUsage");
  const memory = await measureMemory(page);
  const buildInfo = await readBuildInfo(page);
  writeResult(testInfo.project.name, "empty-worker", {
    schema_version: 2,
    result_kind: "memory-baseline",
    captured_at: new Date().toISOString(),
    profile: testInfo.project.name,
    scenario: "empty-worker",
    browser_version: page.context().browser()?.version() ?? "unknown",
    web_build_id: buildInfo.web_build_id,
    viewport: page.viewportSize(),
    throttle,
    cross_origin_isolated: await page.evaluate(() => crossOriginIsolated),
    page_heap: {
      used_size_bytes: heap.usedSize,
      total_size_bytes: heap.totalSize,
    },
    worker_inclusive_memory: memory,
  });
});
