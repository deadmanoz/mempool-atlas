import { afterEach, describe, expect, it, vi } from "vitest";

import {
  AtlasRequestError,
  fetchSources,
  fetchTransactionDetail,
  parseSourcesResponse,
  transactionDetailMatchesSnapshot,
} from "./api";
import type {
  MempoolSnapshot,
  MempoolTransaction,
  TransactionDetailResponse,
} from "./types";

const TXID = "00".repeat(32);

const waitingSource = () => ({
  source_id: "core",
  source_label: "Bitcoin Core",
  availability: "waiting",
  poll_interval_seconds: 300,
  last_poll_started_at_ms: null,
  snapshot_observed_at_ms: null,
  chain_tip: null,
  transaction_count: null,
  total_vsize: null,
  classification: null,
  last_error: null,
});

const emptyUnsigned = () => ({ width: 1, values: new ArrayBuffer(0) });

const publicationManifest = {
  source: {
    ...waitingSource(),
    availability: "ready",
    last_poll_started_at_ms: 90,
    snapshot_observed_at_ms: 100,
    chain_tip: { height: 1, hash: "01".repeat(32) },
    transaction_count: 0,
    total_vsize: 0,
    classification: {
      state: "complete",
      revision: 1,
      classified_count: 0,
      unclassified_count: 0,
    },
  },
  source_id: "core",
  source_label: "Bitcoin Core",
  collection_started_at_ms: 90,
  collection_completed_at_ms: 100,
  collection_duration_ms: 10,
  observed_at_ms: 100,
  classification_revision: 1,
  chain_tip: { height: 1, hash: "01".repeat(32) },
  transaction_count: 0,
  total_vsize: 0,
  classifier_catalog: [],
  classification_summaries: [],
  bip110_summary: {
    evaluator_id: "rdts-rules",
    evaluator_version: "1",
    scope: "knots_mempool_policy",
    compatible_count: 0,
    violating_count: 0,
    indeterminate_count: 0,
    unclassified_count: 0,
  },
  row_count: 0,
  population_id: "00".repeat(32),
  classification_set_id: "11".repeat(32),
  publication_id: "22".repeat(32),
  stages: [],
};

const populationTransfer = {
  contentId: "00".repeat(32),
  txids: new ArrayBuffer(0),
  vsize: emptyUnsigned(),
};

const workerTiming = {
  manifestFetchValidateMs: 0,
  stages: [],
  semanticValidationMs: 0,
  quorumMs: 0,
  supersessionRestarts: 0,
  populationRebaseMs: null,
};

const completeTransfer = () => ({
  manifest: publicationManifest,
  population: populationTransfer,
  membership: {
    contentId: "33".repeat(32),
    differingWtxidBits: new ArrayBuffer(0),
    differingWtxidRanks: new ArrayBuffer(0),
    differingWtxids: new ArrayBuffer(0),
    weight: emptyUnsigned(),
    feeSats: emptyUnsigned(),
    enteredAtMs: emptyUnsigned(),
    ancestorCount: emptyUnsigned(),
    ancestorVsize: emptyUnsigned(),
    ancestorFeeSats: emptyUnsigned(),
    descendantCount: emptyUnsigned(),
    descendantVsize: emptyUnsigned(),
    replaceableBits: new ArrayBuffer(0),
  },
  structure: {
    contentId: "44".repeat(32),
    presenceBits: new ArrayBuffer(0),
    presenceRanks: new ArrayBuffer(0),
    inputCount: emptyUnsigned(),
    outputCount: emptyUnsigned(),
    opReturnBytes: emptyUnsigned(),
    outputSats: emptyUnsigned(),
    witnessBytes: emptyUnsigned(),
  },
  classifiers: [],
});

afterEach(() => {
  vi.unstubAllGlobals();
});

describe("parseSourcesResponse", () => {
  it("accepts the strict v2 source-discovery body", () => {
    const value = { atlas_version: "1.0.0", sources: [waitingSource()] };
    expect(parseSourcesResponse(value)).toBe(value);
  });

  it("rejects repeated source IDs and partial snapshot metadata", () => {
    expect(() =>
      parseSourcesResponse({
        atlas_version: "1.0.0",
        sources: [waitingSource(), waitingSource()],
      }),
    ).toThrow("repeats a source ID");
    expect(() =>
      parseSourcesResponse({
        atlas_version: "1.0.0",
        sources: [{ ...waitingSource(), transaction_count: 1 }],
      }),
    ).toThrow("partial snapshot");
  });
});

describe("request paths", () => {
  it("uses the source discovery endpoint", async () => {
    const fetchMock = vi.fn().mockResolvedValue({
      ok: true,
      json: async () => ({
        atlas_version: "1.0.0",
        sources: [waitingSource()],
      }),
    });
    vi.stubGlobal("fetch", fetchMock);

    await fetchSources();

    expect(fetchMock).toHaveBeenCalledWith("/api/v2/sources", {
      headers: { Accept: "application/json" },
    });
  });

  it("uses the transaction-detail endpoint", async () => {
    const fetchMock = vi.fn().mockResolvedValue({
      ok: false,
      status: 503,
      json: async () => ({ title: "Unavailable" }),
    });
    vi.stubGlobal("fetch", fetchMock);

    await expect(fetchTransactionDetail("core", TXID)).rejects.toBeInstanceOf(
      AtlasRequestError,
    );
    expect(fetchMock).toHaveBeenCalledWith(
      `/api/v2/sources/core/transactions/${TXID}`,
      { headers: { Accept: "application/json" } },
    );
  });
});

describe("transactionDetailMatchesSnapshot", () => {
  it("requires the exact source, observation, witness, and revision", () => {
    const transaction = {
      txid: TXID,
      wtxid: TXID,
      classifications: [],
      bip110: null,
    } as unknown as MempoolTransaction;
    const snapshot = {
      source_id: "core",
      observed_at_ms: 100,
      classification_revision: 4,
    } as unknown as MempoolSnapshot;
    const detail = {
      source_id: "core",
      snapshot_observed_at_ms: 100,
      classification_revision: 4,
      txid: TXID,
      wtxid: TXID,
      classifications: [],
      assessment: null,
    } as unknown as TransactionDetailResponse;

    expect(
      transactionDetailMatchesSnapshot(snapshot, transaction, detail),
    ).toBe(true);
    expect(
      transactionDetailMatchesSnapshot(snapshot, transaction, {
        ...detail,
        wtxid: "11".repeat(32),
      }),
    ).toBe(false);
  });
});

describe("publication worker lifecycle", () => {
  it("does not deliver the complete publication before primary paint finishes", async () => {
    class FakeWorker {
      static instance: FakeWorker;
      readonly listeners = new Map<string, Array<(event: any) => void>>();
      readonly postMessage = vi.fn();

      constructor() {
        FakeWorker.instance = this;
      }

      addEventListener(type: string, listener: (event: any) => void): void {
        const listeners = this.listeners.get(type) ?? [];
        listeners.push(listener);
        this.listeners.set(type, listeners);
      }

      emit(data: unknown): void {
        this.listeners
          .get("message")
          ?.forEach((listener) => listener({ data }));
      }
    }
    vi.stubGlobal("Worker", FakeWorker);
    vi.resetModules();
    const { fetchSourcePublication } = await import("./api");
    let finishPrimaryPaint = (): void => undefined;
    const primaryPaint = new Promise<void>((resolve) => {
      finishPrimaryPaint = resolve;
    });
    const onPrimary = vi.fn(() => primaryPaint);
    let completeDelivered = false;
    const request = fetchSourcePublication(
      "core",
      undefined,
      "transaction_properties",
      onPrimary,
    ).then((publication) => {
      completeDelivered = true;
      return publication;
    });

    FakeWorker.instance.emit({
      type: "primary",
      requestId: 1,
      publication: {
        manifest: publicationManifest,
        population: populationTransfer,
        classifiers: [],
      },
      timing: workerTiming,
    });
    await Promise.resolve();
    expect(onPrimary).toHaveBeenCalledOnce();
    FakeWorker.instance.emit({
      type: "complete",
      requestId: 1,
      publication: completeTransfer(),
      timing: workerTiming,
    });
    await Promise.resolve();
    expect(completeDelivered).toBe(false);

    finishPrimaryPaint();
    await expect(request).resolves.toMatchObject({
      publication: { transaction_count: 0 },
    });
  });

  it("delivers a terminal worker failure after in-flight primary paint", async () => {
    class FakeWorker {
      static instance: FakeWorker;
      readonly listeners = new Map<string, Array<(event: any) => void>>();
      readonly postMessage = vi.fn();
      readonly terminate = vi.fn();

      constructor() {
        FakeWorker.instance = this;
      }

      addEventListener(type: string, listener: (event: any) => void): void {
        const listeners = this.listeners.get(type) ?? [];
        listeners.push(listener);
        this.listeners.set(type, listeners);
      }

      emit(type: string, event: any): void {
        this.listeners.get(type)?.forEach((listener) => listener(event));
      }
    }
    vi.stubGlobal("Worker", FakeWorker);
    vi.resetModules();
    const { fetchSourcePublication } = await import("./api");
    let releasePrimary = (): void => undefined;
    const primaryPaint = new Promise<void>((resolve) => {
      releasePrimary = resolve;
    });
    let rejected: Error | undefined;
    const request = fetchSourcePublication(
      "core",
      undefined,
      "transaction_properties",
      () => primaryPaint,
    );
    void request.catch((error: Error) => {
      rejected = error;
    });

    FakeWorker.instance.emit("message", {
      data: {
        type: "primary",
        requestId: 1,
        publication: {
          manifest: publicationManifest,
          population: populationTransfer,
          classifiers: [],
        },
        timing: workerTiming,
      },
    });
    await Promise.resolve();
    FakeWorker.instance.emit("error", {
      message: "worker crashed",
      preventDefault: vi.fn(),
    });
    await Promise.resolve();
    expect(rejected).toBeUndefined();

    releasePrimary();
    await expect(request).rejects.toThrow("worker crashed");
    expect(FakeWorker.instance.terminate).toHaveBeenCalledOnce();
  });

  it("cancels the worker request when primary delivery rejects", async () => {
    class FakeWorker {
      static instance: FakeWorker;
      readonly listeners = new Map<string, Array<(event: any) => void>>();
      readonly postMessage = vi.fn();

      constructor() {
        FakeWorker.instance = this;
      }

      addEventListener(type: string, listener: (event: any) => void): void {
        const listeners = this.listeners.get(type) ?? [];
        listeners.push(listener);
        this.listeners.set(type, listeners);
      }

      emit(data: unknown): void {
        this.listeners
          .get("message")
          ?.forEach((listener) => listener({ data }));
      }
    }
    vi.stubGlobal("Worker", FakeWorker);
    vi.resetModules();
    const { fetchSourcePublication } = await import("./api");
    const request = fetchSourcePublication(
      "core",
      undefined,
      "transaction_properties",
      () => Promise.reject(new Error("primary paint failed")),
    );

    FakeWorker.instance.emit({
      type: "primary",
      requestId: 1,
      publication: {
        manifest: publicationManifest,
        population: populationTransfer,
        classifiers: [],
      },
      timing: workerTiming,
    });

    await expect(request).rejects.toThrow("primary paint failed");
    expect(FakeWorker.instance.postMessage).toHaveBeenLastCalledWith({
      type: "cancel",
      requestId: 1,
    });
  });

  it("rejects and recreates the worker after malformed complete materialization", async () => {
    class FakeWorker {
      static readonly instances: FakeWorker[] = [];
      readonly listeners = new Map<string, Array<(event: any) => void>>();
      readonly postMessage = vi.fn();
      readonly terminate = vi.fn();

      constructor() {
        FakeWorker.instances.push(this);
      }

      addEventListener(type: string, listener: (event: any) => void): void {
        const listeners = this.listeners.get(type) ?? [];
        listeners.push(listener);
        this.listeners.set(type, listeners);
      }

      emit(data: unknown): void {
        this.listeners
          .get("message")
          ?.forEach((listener) => listener({ data }));
      }
    }
    vi.stubGlobal("Worker", FakeWorker);
    vi.resetModules();
    const { fetchSourcePublication } = await import("./api");
    const request = fetchSourcePublication("core");
    const first = FakeWorker.instances[0]!;
    first.emit({
      type: "primary",
      requestId: 1,
      publication: {
        manifest: publicationManifest,
        population: populationTransfer,
        classifiers: [],
      },
      timing: workerTiming,
    });
    await Promise.resolve();
    const malformed = completeTransfer();
    malformed.membership.differingWtxidRanks = new ArrayBuffer(1);
    first.emit({
      type: "complete",
      requestId: 1,
      publication: malformed,
      timing: workerTiming,
    });

    await expect(request).rejects.toThrow();
    expect(first.terminate).toHaveBeenCalledOnce();
    const controller = new AbortController();
    const retry = fetchSourcePublication("core", controller.signal);
    expect(FakeWorker.instances).toHaveLength(2);
    controller.abort();
    await expect(retry).rejects.toMatchObject({ name: "AbortError" });
  });

  it.each(["error", "messageerror"] as const)(
    "recreates the worker after a terminal %s event",
    async (eventType) => {
      class FakeWorker {
        static readonly instances: FakeWorker[] = [];
        readonly listeners = new Map<string, Array<(event: any) => void>>();
        readonly postMessage = vi.fn();
        readonly terminate = vi.fn();

        constructor() {
          FakeWorker.instances.push(this);
        }

        addEventListener(type: string, listener: (event: any) => void): void {
          const listeners = this.listeners.get(type) ?? [];
          listeners.push(listener);
          this.listeners.set(type, listeners);
        }

        emit(type: string, event: any): void {
          this.listeners.get(type)?.forEach((listener) => listener(event));
        }
      }
      vi.stubGlobal("Worker", FakeWorker);
      vi.resetModules();
      const { fetchSourcePublication } = await import("./api");

      const first = fetchSourcePublication("core");
      const firstRejection = expect(first).rejects.toThrow(
        eventType === "error" ? "worker crashed" : "unreadable message",
      );
      const failed = FakeWorker.instances[0];
      expect(failed).toBeDefined();
      failed?.emit(
        eventType,
        eventType === "error"
          ? { message: "worker crashed", preventDefault: vi.fn() }
          : {},
      );
      await firstRejection;
      expect(failed?.terminate).toHaveBeenCalledOnce();

      const controller = new AbortController();
      const second = fetchSourcePublication("core", controller.signal);
      expect(FakeWorker.instances).toHaveLength(2);
      controller.abort();
      await expect(second).rejects.toMatchObject({ name: "AbortError" });
    },
  );
});
