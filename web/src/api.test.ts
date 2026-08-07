import { afterEach, describe, expect, it, vi } from "vitest";

import {
  fetchSources,
  fetchTransactionDetail,
  parseSourceSummary,
  parseSourcesResponse,
  parseTransactionDetailResponse,
  transactionDetailMatchesSnapshot,
} from "./api";
import type {
  MempoolSnapshot,
  MempoolTransaction,
  TransactionDetailResponse,
} from "./types";

const TXID = "00".repeat(32);
const BLOCK_HASH = "01".repeat(32);

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

const readySource = () => ({
  ...waitingSource(),
  availability: "ready",
  last_poll_started_at_ms: 1_700_000_000_000,
  snapshot_observed_at_ms: 1_700_000_001_000,
  chain_tip: { height: 900_000, hash: BLOCK_HASH },
  transaction_count: 1,
  total_vsize: 141,
  classification: {
    state: "complete",
    revision: 3,
    classified_count: 1,
    unclassified_count: 0,
  },
});

const compactClassifications = () => [
  {
    classifier_id: "transaction_properties",
    state: "complete" as const,
    primary_label: null,
    labels: ["version_2", "p2wpkh"],
    missing_facts: [],
    evidence: null,
  },
  {
    classifier_id: "transaction_shape",
    state: "complete" as const,
    primary_label: "other_shape",
    labels: ["other_shape"],
    missing_facts: [],
    evidence: null,
  },
  {
    classifier_id: "data_protocols",
    state: "complete" as const,
    primary_label: "no_detected_protocol",
    labels: ["no_detected_protocol"],
    missing_facts: [],
    evidence: null,
  },
  {
    classifier_id: "knots_bip110",
    state: "complete" as const,
    primary_label: "violating",
    labels: ["violating"],
    missing_facts: [],
    evidence: null,
  },
];

const transactionDetail = (): TransactionDetailResponse => ({
  source_id: "core",
  snapshot_observed_at_ms: 1_700_000_001_000,
  classification_revision: 3,
  txid: TXID,
  wtxid: TXID,
  classifications: compactClassifications().map((result) => ({
    ...result,
    evidence: { fixture: true },
  })),
  assessment: {
    status: "violating",
    primary_rule: "element_size",
    violated_rules: ["element_size"],
    unknown_rules: [],
  },
  rules: [
    {
      rule: "output_size",
      number: 1,
      verdict: "pass",
      evidence_count: 0,
      evidence: [],
      missing_count: 0,
      missing: [],
    },
    {
      rule: "element_size",
      number: 2,
      verdict: "violate",
      evidence_count: 3,
      evidence: [{ location: "witness[0]" }],
      missing_count: 0,
      missing: [],
    },
    {
      rule: "undefined_version",
      number: 3,
      verdict: "pass",
      evidence_count: 0,
      evidence: [],
      missing_count: 0,
      missing: [],
    },
    {
      rule: "taproot_annex",
      number: 4,
      verdict: "pass",
      evidence_count: 0,
      evidence: [],
      missing_count: 0,
      missing: [],
    },
    {
      rule: "control_block_size",
      number: 5,
      verdict: "pass",
      evidence_count: 0,
      evidence: [],
      missing_count: 0,
      missing: [],
    },
    {
      rule: "op_success",
      number: 6,
      verdict: "pass",
      evidence_count: 0,
      evidence: [],
      missing_count: 0,
      missing: [],
    },
    {
      rule: "tapscript_op_if",
      number: 7,
      verdict: "pass",
      evidence_count: 0,
      evidence: [],
      missing_count: 0,
      missing: [],
    },
  ],
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
  vi.useRealTimers();
  vi.unstubAllGlobals();
});

describe("parseSourceSummary", () => {
  it("accepts coherent waiting, ready, stale, and error states", () => {
    const waiting = waitingSource();
    const ready = readySource();
    const stale = {
      ...readySource(),
      availability: "stale",
      last_error: "node unavailable",
    };
    const error = {
      ...waitingSource(),
      availability: "error",
      last_error: "initial poll failed",
    };

    expect(parseSourceSummary(waiting)).toBe(waiting);
    expect(parseSourceSummary(ready)).toBe(ready);
    expect(parseSourceSummary(stale)).toBe(stale);
    expect(parseSourceSummary(error)).toBe(error);
  });

  it("rejects partial snapshot metadata and classification count drift", () => {
    expect(() =>
      parseSourceSummary({ ...waitingSource(), transaction_count: 1 }),
    ).toThrow("partial snapshot");

    expect(() =>
      parseSourceSummary({
        ...readySource(),
        classification: {
          state: "complete",
          revision: 3,
          classified_count: 0,
          unclassified_count: 0,
        },
      }),
    ).toThrow("classification does not match its snapshot metadata");
  });

  it("rejects classification without a snapshot and availability mismatches", () => {
    expect(() =>
      parseSourceSummary({
        ...waitingSource(),
        classification: {
          state: "classifying",
          revision: 1,
          classified_count: 0,
          unclassified_count: 0,
        },
      }),
    ).toThrow("without a snapshot unexpectedly contains classification");

    expect(() =>
      parseSourceSummary({
        ...readySource(),
        availability: "waiting",
      }),
    ).toThrow("Unavailable source unexpectedly contains a snapshot");

    expect(() =>
      parseSourceSummary({
        ...waitingSource(),
        availability: "ready",
      }),
    ).toThrow("Available source is missing its snapshot");
  });

  it("requires errors only on failed source states", () => {
    expect(() =>
      parseSourceSummary({
        ...readySource(),
        last_error: "unexpected error",
      }),
    ).toThrow("Healthy source unexpectedly contains an error");

    expect(() =>
      parseSourceSummary({
        ...readySource(),
        availability: "stale",
      }),
    ).toThrow("Failed source is missing its error");
  });

  it("rejects URL dot segments, extra fields, and a zero poll interval", () => {
    for (const sourceId of [".", ".."]) {
      expect(() =>
        parseSourceSummary({ ...waitingSource(), source_id: sourceId }),
      ).toThrow("Invalid source summary");
    }
    expect(() =>
      parseSourceSummary({ ...waitingSource(), unexpected: true }),
    ).toThrow("Invalid source summary");
    expect(() =>
      parseSourceSummary({ ...waitingSource(), poll_interval_seconds: 0 }),
    ).toThrow("Invalid source summary");
  });
});

describe("parseTransactionDetailResponse", () => {
  it("accepts the canonical seven-rule detail", () => {
    const value = transactionDetail();

    expect(parseTransactionDetailResponse(value)).toBe(value);
  });

  it("rejects rule order and internally inconsistent verdicts", () => {
    const wrongOrder = transactionDetail();
    wrongOrder.rules[0] = {
      ...wrongOrder.rules[0]!,
      rule: "element_size",
    };
    expect(() => parseTransactionDetailResponse(wrongOrder)).toThrow(
      "Invalid rule assessment at index 0",
    );

    const wrongVerdict = transactionDetail();
    wrongVerdict.rules[1] = {
      ...wrongVerdict.rules[1]!,
      verdict: "pass",
    };
    expect(() => parseTransactionDetailResponse(wrongVerdict)).toThrow(
      "Inconsistent rule verdict at index 1",
    );
  });

  it("accepts a rule that is both proven and unresolved", () => {
    const value = transactionDetail();
    if (value.assessment === null) {
      throw new Error("Fixture unexpectedly lacks an assessment");
    }
    value.assessment.primary_rule = null;
    value.assessment.unknown_rules = ["element_size"];
    const policy = value.classifications.find(
      ({ classifier_id }) => classifier_id === "knots_bip110",
    )!;
    policy.state = "partial";
    policy.missing_facts = ["policy_facts"];
    value.rules[1]!.missing_count = 2;
    value.rules[1]!.missing = [{ location: "witness[1]" }];

    expect(parseTransactionDetailResponse(value)).toBe(value);
  });

  it("accepts a detailed partial classifier result without labels", () => {
    const value = transactionDetail();
    const shape = value.classifications.find(
      ({ classifier_id }) => classifier_id === "transaction_shape",
    )!;
    shape.state = "partial";
    shape.primary_label = null;
    shape.labels = [];
    shape.missing_facts = ["input_script_pubkeys"];

    expect(parseTransactionDetailResponse(value)).toBe(value);
  });

  it("requires exact counts with at most one bounded exemplar", () => {
    const tooMany = transactionDetail();
    tooMany.rules[1]!.evidence = [
      { location: "witness[0]" },
      { location: "witness[1]" },
    ];
    expect(() => parseTransactionDetailResponse(tooMany)).toThrow(
      "Invalid rule assessment at index 1",
    );

    const missingExemplar = transactionDetail();
    missingExemplar.rules[1]!.evidence = [];
    expect(() => parseTransactionDetailResponse(missingExemplar)).toThrow(
      "Invalid rule assessment at index 1",
    );
  });

  it("requires the canonical rule verdicts to match the assessment", () => {
    const value = transactionDetail();
    value.rules[1] = {
      ...value.rules[1]!,
      verdict: "pass",
      evidence_count: 0,
      evidence: [],
    };

    expect(() => parseTransactionDetailResponse(value)).toThrow(
      "Transaction detail rules do not match assessment",
    );
  });

  it("requires an assessment and matching policy classification", () => {
    const unclassified = transactionDetail() as unknown as Record<
      string,
      unknown
    >;
    unclassified.assessment = null;
    expect(() => parseTransactionDetailResponse(unclassified)).toThrow(
      "Classified transaction detail is missing its assessment",
    );

    const mismatchedPolicy = transactionDetail();
    const policy = mismatchedPolicy.classifications.find(
      ({ classifier_id }) => classifier_id === "knots_bip110",
    )!;
    policy.primary_label = "compatible";
    policy.labels = ["compatible"];
    expect(() => parseTransactionDetailResponse(mismatchedPolicy)).toThrow(
      "classifiers do not match policy assessment",
    );
  });

  it("rejects duplicate classifiers and missing identity fields", () => {
    const duplicateClassifier = transactionDetail();
    duplicateClassifier.classifications = [
      ...duplicateClassifier.classifications,
      duplicateClassifier.classifications[0]!,
    ];
    expect(() => parseTransactionDetailResponse(duplicateClassifier)).toThrow(
      "Invalid detailed classification at index 4",
    );

    const missingRevision = transactionDetail() as unknown as Record<
      string,
      unknown
    >;
    delete missingRevision.classification_revision;
    expect(() => parseTransactionDetailResponse(missingRevision)).toThrow(
      "Invalid transaction detail response",
    );
  });
});

describe("parseSourcesResponse", () => {
  it("accepts the strict v2 source-discovery body", () => {
    const value = { atlas_version: "1.0.0", sources: [waitingSource()] };
    expect(parseSourcesResponse(value)).toBe(value);
  });

  it("accepts a SemVer prerelease with build metadata", () => {
    const value = {
      atlas_version: "2.0.0-rc.1+perf.7",
      sources: [readySource()],
    };

    expect(parseSourcesResponse(value)).toBe(value);
  });

  it("rejects invalid versions, repeated IDs, and invalid source summaries", () => {
    expect(() =>
      parseSourcesResponse({
        atlas_version: "latest",
        sources: [waitingSource()],
      }),
    ).toThrow("Invalid sources response");
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
    expect(() =>
      parseSourcesResponse({
        atlas_version: "1.0.0",
        sources: [{ ...waitingSource(), source_id: ".." }],
      }),
    ).toThrow("Invalid source summary");
  });

  it("rejects unknown top-level fields", () => {
    expect(() =>
      parseSourcesResponse({
        atlas_version: "1.0.0",
        sources: [waitingSource()],
        legacy: true,
      }),
    ).toThrow("Invalid sources response");
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
      json: async () => ({
        type: "v2_unavailable",
        title: "Current v2 publication unavailable",
        status: 503,
        detail:
          "Atlas has not published a current complete snapshot for this source.",
      }),
    });
    vi.stubGlobal("fetch", fetchMock);

    const rejected = fetchTransactionDetail("core", TXID).catch(
      (error: unknown) => error,
    );
    await expect(rejected).resolves.toMatchObject({
      message: "Atlas request failed (503): Current v2 publication unavailable",
      name: "AtlasRequestError",
      problemType: "v2_unavailable",
      status: 503,
    });
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
  it("cancels a publication that exceeds the bounded load deadline", async () => {
    vi.useFakeTimers();
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
    let releasePrimary = (): void => undefined;
    const primaryPaint = new Promise<void>((resolve) => {
      releasePrimary = resolve;
    });
    const request = fetchSourcePublication(
      "core",
      undefined,
      "transaction_properties",
      () => primaryPaint,
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
    await Promise.resolve();
    const rejection = expect(request).rejects.toMatchObject({
      message: "Atlas v2 publication timed out",
      name: "TimeoutError",
    });

    await vi.advanceTimersByTimeAsync(120_000);

    expect(FakeWorker.instance.postMessage).toHaveBeenLastCalledWith({
      type: "cancel",
      requestId: 1,
    });
    await rejection;
    releasePrimary();
  });

  it("bounds a terminal worker error behind an unsettled primary callback", async () => {
    vi.useFakeTimers();
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
    let releasePrimary = (): void => undefined;
    const primaryPaint = new Promise<void>((resolve) => {
      releasePrimary = resolve;
    });
    const request = fetchSourcePublication(
      "core",
      undefined,
      "transaction_properties",
      () => primaryPaint,
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
    await Promise.resolve();
    FakeWorker.instance.emit({
      type: "error",
      requestId: 1,
      status: 503,
      problem: null,
      message: "Current v2 publication unavailable",
      retryable: false,
    });
    let settled = false;
    const rejected = request.catch((error: unknown) => {
      settled = true;
      return error;
    });

    await vi.advanceTimersByTimeAsync(119_999);
    expect(settled).toBe(false);
    await vi.advanceTimersByTimeAsync(1);

    await expect(rejected).resolves.toMatchObject({
      message: "Atlas request failed (503): Current v2 publication unavailable",
      status: 503,
    });
    expect(FakeWorker.instance.postMessage).toHaveBeenLastCalledWith({
      type: "cancel",
      requestId: 1,
    });
    releasePrimary();
  });

  it("cleans pending state when worker construction fails", async () => {
    vi.useFakeTimers();
    class FakeWorker {
      constructor() {
        throw new Error("worker construction failed");
      }
    }
    vi.stubGlobal("Worker", FakeWorker);
    vi.resetModules();
    const { fetchSourcePublication } = await import("./api");

    await expect(fetchSourcePublication("core")).rejects.toThrow(
      "worker construction failed",
    );
    expect(vi.getTimerCount()).toBe(0);
  });

  it("preserves a worker problem without duplicating the HTTP prefix", async () => {
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
    const request = fetchSourcePublication("core");

    FakeWorker.instance.emit({
      type: "error",
      requestId: 1,
      status: 503,
      problem: {
        type: "v2_unavailable",
        title: "Current v2 publication unavailable",
        status: 503,
        detail:
          "Atlas has not published a current complete snapshot for this source.",
      },
      message: "Current v2 publication unavailable",
      retryable: false,
    });

    const rejected = request.catch((error: unknown) => error);
    await expect(rejected).resolves.toMatchObject({
      message: "Atlas request failed (503): Current v2 publication unavailable",
      problemType: "v2_unavailable",
      status: 503,
    });
  });

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
