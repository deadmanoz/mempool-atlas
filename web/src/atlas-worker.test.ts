import { readFileSync } from "node:fs";

import { afterEach, describe, expect, it, vi } from "vitest";

import {
  classificationSetId,
  loadPackedPublication,
  parseManifest,
  publicationId,
  resultTuple,
  signedColumn,
  validateManifestRoots,
  validatePolicyResult,
} from "./atlas-worker";
import type { WorkerQuorumTiming } from "./atlas-worker-protocol";
import type { StageDescriptor, StagedSnapshotManifest } from "./types";

const encoder = new TextEncoder();

const sha = async (bytes: Uint8Array): Promise<string> =>
  Array.from(
    new Uint8Array(
      await crypto.subtle.digest("SHA-256", Uint8Array.from(bytes).buffer),
    ),
    (value) => value.toString(16).padStart(2, "0"),
  ).join("");

const json = (value: unknown): Uint8Array =>
  encoder.encode(JSON.stringify(value));

interface Fixture {
  manifest: StagedSnapshotManifest;
  manifestBytes: Uint8Array;
  stages: Map<string, Uint8Array>;
}

const descriptor = async (
  kind: StageDescriptor["kind"],
  body: Uint8Array,
  dependencies: string[],
  classifierId?: string,
): Promise<StageDescriptor> => ({
  kind,
  ...(classifierId === undefined ? {} : { classifier_id: classifierId }),
  content_id: await sha(body),
  uncompressed_bytes: body.byteLength,
  row_count: 0,
  dependency_ids: dependencies,
});

const fixture = async (
  mutation: "none" | "dependency" | "malformed" = "none",
  populationWidth = 1,
): Promise<Fixture> => {
  const populationBytes =
    mutation === "malformed"
      ? encoder.encode("{")
      : json({
          schema_version: 2,
          kind: "population",
          row_count: 0,
          dependency_ids: [],
          txids_base64: "",
          vsize: { width_bytes: populationWidth, values_base64: "" },
        });
  const population = await descriptor("population", populationBytes, []);
  const populationId = population.content_id;
  const classifierBytes = json({
    schema_version: 2,
    kind: "classifier",
    row_count: 0,
    dependency_ids: [populationId],
    population_id: populationId,
    classifier_id: "knots_bip110",
    result_dictionary: [],
    result_codes: { width_bytes: 1, values_base64: "" },
    bip110: {
      assessment_dictionary: [],
      assessment_codes: { width_bytes: 1, values_base64: "" },
    },
  });
  const classifier = await descriptor(
    "classifier",
    classifierBytes,
    [populationId],
    "knots_bip110",
  );
  const base = {
    schema_version: 2 as const,
    source: {
      source_id: "core",
      source_label: "Core",
      availability: "ready" as const,
      poll_interval_seconds: 300,
      last_poll_started_at_ms: 90,
      snapshot_observed_at_ms: 100,
      chain_tip: { height: 1, hash: "01".repeat(32) },
      transaction_count: 0,
      total_vsize: 0,
      classification: {
        state: "complete" as const,
        revision: 1,
        classified_count: 0,
        unclassified_count: 0,
      },
      last_error: null,
    },
    source_id: "core",
    source_label: "Core",
    collection_started_at_ms: 90,
    collection_completed_at_ms: 100,
    collection_duration_ms: 10,
    observed_at_ms: 100,
    classification_revision: 1,
    chain_tip: { height: 1, hash: "01".repeat(32) },
    transaction_count: 0,
    total_vsize: 0,
    classifier_catalog: [
      {
        id: "knots_bip110",
        version: "1",
        title: "Policy",
        methodology: "policy" as const,
        semantics: "rule_set" as const,
        required_facts: [],
        labels: [
          {
            key: "compatible",
            label: "Compatible",
            description: "Compatible",
          },
        ],
      },
    ],
    classification_summaries: [
      {
        classifier_id: "knots_bip110",
        complete_count: 0,
        partial_count: 0,
        unclassified_count: 0,
        label_counts: { compatible: 0 },
      },
    ],
    bip110_summary: {
      evaluator_id: "rdts-rules",
      evaluator_version: "1",
      scope: "knots_mempool_policy" as const,
      compatible_count: 0,
      violating_count: 0,
      indeterminate_count: 0,
      unclassified_count: 0,
    },
    row_count: 0,
    population_id: populationId,
    classification_set_id: "00".repeat(32),
    publication_id: "00".repeat(32),
    // The classification-set preimage reads only the catalog-aligned
    // descriptor at index three. Membership and structure are filled below.
    stages: [population, population, population, classifier],
  };
  base.classification_set_id = await classificationSetId(
    base as unknown as StagedSnapshotManifest,
  );
  const membershipBytes = json({
    schema_version: 2,
    kind: "membership",
    row_count: 0,
    dependency_ids: [populationId],
    population_id: populationId,
    differing_wtxid_bitset_base64: "",
    differing_wtxids_base64: "",
    weight: { width_bytes: 1, values_base64: "" },
    fee_sats: { width_bytes: 1, values_base64: "" },
    entered_at_ms: { width_bytes: 1, values_base64: "" },
    ancestor_count: { width_bytes: 1, values_base64: "" },
    ancestor_vsize: { width_bytes: 1, values_base64: "" },
    ancestor_fee_sats: { width_bytes: 1, values_base64: "" },
    descendant_count: { width_bytes: 1, values_base64: "" },
    descendant_vsize: { width_bytes: 1, values_base64: "" },
    replaceable_bitset_base64: "",
  });
  const membership = await descriptor("membership", membershipBytes, [
    populationId,
  ]);
  const structureBytes = json({
    schema_version: 2,
    kind: "structure",
    row_count: 0,
    dependency_ids:
      mutation === "dependency"
        ? [populationId, "ff".repeat(32)]
        : [populationId, base.classification_set_id],
    population_id: populationId,
    classification_set_id: base.classification_set_id,
    presence_bitset_base64: "",
    input_count: { width_bytes: 1, values_base64: "" },
    output_count: { width_bytes: 1, values_base64: "" },
    op_return_bytes: { width_bytes: 1, values_base64: "" },
    output_sats: { width_bytes: 1, values_base64: "" },
    witness_bytes: { width_bytes: 1, values_base64: "" },
  });
  const structure = await descriptor("structure", structureBytes, [
    populationId,
    base.classification_set_id,
  ]);
  const manifest = {
    ...base,
    stages: [population, membership, structure, classifier],
  } as unknown as StagedSnapshotManifest;
  manifest.publication_id = await publicationId(manifest);
  const manifestBytes = json(manifest);
  return {
    manifest,
    manifestBytes,
    stages: new Map([
      [population.content_id, populationBytes],
      [membership.content_id, membershipBytes],
      [structure.content_id, structureBytes],
      [classifier.content_id, classifierBytes],
    ]),
  };
};

const publicationDigestGoldenManifest = (): StagedSnapshotManifest =>
  parseManifest(
    JSON.parse(
      readFileSync(
        new URL(
          "../../tests/fixtures/publication-digest-v2.json",
          import.meta.url,
        ),
        "utf8",
      ),
    ),
  );

const response = (bytes: Uint8Array, status = 200): Response =>
  new Response(Uint8Array.from(bytes).buffer, {
    status,
    headers: { "Content-Type": "application/json" },
  });

afterEach(() => {
  vi.useRealTimers();
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

describe("v2 manifest validation", () => {
  it("rejects base64 with non-canonical padding bits", () => {
    expect(() =>
      signedColumn(
        { width_bytes: 1, values_base64: "AR==" },
        1,
        "ancestor fees",
      ),
    ).toThrow("Non-canonical ancestor fees values");
  });

  it("accepts the Rust seven-byte encoding of the minimum safe signed value", () => {
    const parsed = signedColumn(
      { width_bytes: 7, values_base64: "AQAAAAAA4A==" },
      1,
      "ancestor fees",
    );
    expect(parsed.width).toBe(7);
    expect(new Uint8Array(parsed.values)).toEqual(
      Uint8Array.from([1, 0, 0, 0, 0, 0, 0xe0]),
    );
  });

  it("matches the Rust publication digest fixture", async () => {
    const manifest = publicationDigestGoldenManifest();

    await expect(classificationSetId(manifest)).resolves.toBe(
      manifest.classification_set_id,
    );
    await expect(publicationId(manifest)).resolves.toBe(
      manifest.publication_id,
    );
  });

  it("recomputes both digest roots", async () => {
    const current = await fixture();
    const parsed = parseManifest(
      JSON.parse(new TextDecoder().decode(current.manifestBytes)),
    );
    await expect(validateManifestRoots(parsed)).resolves.toBeUndefined();
    await expect(
      validateManifestRoots({ ...parsed, publication_id: "ff".repeat(32) }),
    ).rejects.toThrow("Publication digest mismatch");
  });

  it("rejects a descriptor dependency mismatch", async () => {
    const current = await fixture();
    const value = JSON.parse(new TextDecoder().decode(current.manifestBytes));
    value.stages[1].dependency_ids = ["ff".repeat(32)];
    expect(() => parseManifest(value)).toThrow("invalid dependency graph");
  });

  it("rejects source identity and protocol limits that differ from Rust", async () => {
    const current = await fixture();
    const chainMismatch = JSON.parse(
      new TextDecoder().decode(current.manifestBytes),
    );
    chainMismatch.source.chain_tip.hash = "ff".repeat(32);
    expect(() => parseManifest(chainMismatch)).toThrow(
      "classification metadata is inconsistent",
    );

    const oversizedRows = JSON.parse(
      new TextDecoder().decode(current.manifestBytes),
    );
    oversizedRows.row_count = 200_001;
    oversizedRows.transaction_count = 200_001;
    expect(() => parseManifest(oversizedRows)).toThrow(
      "row count exceeds the supported maximum",
    );

    const oversizedStage = JSON.parse(
      new TextDecoder().decode(current.manifestBytes),
    );
    oversizedStage.stages[0].uncompressed_bytes = 64 * 1024 * 1024 + 1;
    expect(() => parseManifest(oversizedStage)).toThrow(
      "Invalid stage descriptor",
    );
  });

  it("requires internally coherent collection timing", async () => {
    const current = await fixture();
    const manifest = (): Record<string, unknown> =>
      JSON.parse(new TextDecoder().decode(current.manifestBytes)) as Record<
        string,
        unknown
      >;

    const completionBeforeStart = manifest();
    completionBeforeStart.collection_completed_at_ms = 89;
    expect(() => parseManifest(completionBeforeStart)).toThrow(
      "Manifest collection timing is inconsistent",
    );

    const wrongDuration = manifest();
    wrongDuration.collection_duration_ms = 9;
    expect(() => parseManifest(wrongDuration)).toThrow(
      "Manifest collection timing is inconsistent",
    );

    const wrongObservation = manifest();
    wrongObservation.observed_at_ms = 99;
    (
      wrongObservation.source as Record<string, unknown>
    ).snapshot_observed_at_ms = 99;
    expect(() => parseManifest(wrongObservation)).toThrow(
      "Manifest collection timing is inconsistent",
    );
  });

  it("rejects duplicate result labels and missing complete labels", () => {
    expect(() =>
      resultTuple({
        state: "complete",
        primary_label: "compatible",
        labels: ["compatible", "compatible"],
        missing_facts: [],
      }),
    ).toThrow("Inconsistent classifier result dictionary entry");
    expect(() =>
      resultTuple({
        state: "complete",
        primary_label: null,
        labels: [],
        missing_facts: [],
      }),
    ).toThrow("Inconsistent classifier result dictionary entry");
  });

  it("rejects a policy result that contradicts its assessment", () => {
    expect(() =>
      validatePolicyResult(
        {
          state: "complete",
          primary_label: "violating",
          labels: ["violating"],
          missing_facts: [],
        },
        {
          status: "compatible",
          primary_rule: null,
          violated_rules: [],
          unknown_rules: [],
        },
      ),
    ).toThrow("result does not match its assessment");
  });
});

describe("v2 coherent publication loading", () => {
  it("preserves a structured manifest problem without an HTTP message prefix", async () => {
    const problem = {
      type: "v2_unavailable",
      title: "Current v2 publication unavailable",
      status: 503,
      detail:
        "Atlas has not published a current complete snapshot for this source.",
    };
    vi.stubGlobal(
      "fetch",
      vi.fn(async () =>
        response(new TextEncoder().encode(JSON.stringify(problem)), 503),
      ),
    );

    await expect(
      loadPackedPublication(
        "core",
        "knots_bip110",
        new AbortController().signal,
        vi.fn(),
        vi.fn(),
      ),
    ).rejects.toMatchObject({
      message: problem.title,
      problem,
      status: 503,
    });
  });

  it("reports worker decode, validation, and packing timing for both quorums", async () => {
    const current = await fixture();
    const requests: string[] = [];
    vi.stubGlobal(
      "fetch",
      vi.fn(async (input: string | URL | Request) => {
        const path = String(input);
        requests.push(path);
        if (path.endsWith("/mempool")) return response(current.manifestBytes);
        const bytes = current.stages.get(path.slice(path.lastIndexOf("/") + 1));
        return response(
          bytes ?? json({ error: "missing" }),
          bytes === undefined ? 404 : 200,
        );
      }),
    );
    const primary = vi.fn();
    const complete = vi.fn();

    await loadPackedPublication(
      "core",
      "knots_bip110",
      new AbortController().signal,
      primary,
      complete,
    );

    expect(primary).toHaveBeenCalledOnce();
    expect(primary.mock.calls[0]?.[1]).toMatchObject({
      stages: [
        { kind: "population", classifierId: null, reused: false },
        {
          kind: "classifier",
          classifierId: "knots_bip110",
          reused: false,
        },
      ],
      supersessionRestarts: 0,
      populationRebaseMs: null,
    });
    expect(complete).toHaveBeenCalledOnce();
    const completeTiming = complete.mock.calls[0]?.[0] as WorkerQuorumTiming;
    expect(completeTiming.stages.map(({ kind }) => kind)).toEqual([
      "population",
      "membership",
      "structure",
      "classifier",
    ]);
    expect(completeTiming.stages.map(({ reused }) => reused)).toEqual([
      true,
      false,
      false,
      true,
    ]);
    expect(requests.filter((path) => path.endsWith("/mempool"))).toHaveLength(
      2,
    );
    for (const contentId of current.stages.keys()) {
      expect(
        requests.filter((path) => path.endsWith(`/${contentId}`)),
      ).toHaveLength(1);
    }
  });

  it("falls back from an unavailable requested classifier to the first published lane", async () => {
    const current = await fixture();
    const requests: string[] = [];
    vi.stubGlobal(
      "fetch",
      vi.fn(async (input: string | URL | Request) => {
        const path = String(input);
        requests.push(path);
        if (path.endsWith("/mempool")) return response(current.manifestBytes);
        const bytes = current.stages.get(path.slice(path.lastIndexOf("/") + 1));
        return response(
          bytes ?? json({ error: "missing" }),
          bytes === undefined ? 404 : 200,
        );
      }),
    );
    const primary = vi.fn();

    await loadPackedPublication(
      "core",
      "unavailable_classifier",
      new AbortController().signal,
      primary,
    );

    expect(primary).toHaveBeenCalledOnce();
    expect(primary.mock.calls[0]?.[0].classifiers[0]?.classifierId).toBe(
      "knots_bip110",
    );
    expect(
      requests.some((path) => path.includes("unavailable_classifier")),
    ).toBe(false);
  });

  it("restarts three delayed superseded candidates and reuses surviving sibling stages", async () => {
    const current = await fixture();
    let conflicts = 0;
    let elapsed = 0;
    const requests: string[] = [];
    vi.spyOn(performance, "now").mockImplementation(() => elapsed);
    vi.stubGlobal(
      "fetch",
      vi.fn(async (input: string | URL | Request, init?: RequestInit) => {
        const path = String(input);
        requests.push(path);
        if (path.endsWith("/mempool")) return response(current.manifestBytes);
        if (path.includes("/stages/membership/") && conflicts < 3) {
          conflicts += 1;
          elapsed += 11_000;
          return response(json({ error: "superseded" }), 409);
        }
        if (path.includes("/stages/structure/")) {
          await new Promise((resolve) => setTimeout(resolve, 0));
          if (init?.signal?.aborted) {
            throw new DOMException("Aborted", "AbortError");
          }
        }
        const id = path.slice(path.lastIndexOf("/") + 1);
        const bytes = current.stages.get(id);
        return bytes === undefined
          ? response(json({ error: "missing" }), 404)
          : response(bytes);
      }),
    );
    const primary = vi.fn();
    const complete = vi.fn();

    const loaded = await loadPackedPublication(
      "core",
      "knots_bip110",
      new AbortController().signal,
      primary,
      complete,
    );

    expect(loaded.manifest.publication_id).toBe(
      current.manifest.publication_id,
    );
    expect(primary).toHaveBeenCalledOnce();
    expect(complete).toHaveBeenCalledOnce();
    const completeTiming = complete.mock.calls[0]?.[0] as WorkerQuorumTiming;
    expect(completeTiming.supersessionRestarts).toBe(3);
    expect(completeTiming.populationRebaseMs).toBeNull();
    expect(completeTiming.stages.map(({ reused }) => reused)).toEqual([
      true,
      false,
      true,
      true,
    ]);
    expect(requests.every((path) => path.startsWith("/api/v2/"))).toBe(true);
    expect(requests.filter((path) => path.endsWith("/mempool"))).toHaveLength(
      5,
    );
    expect(
      requests.filter((path) => path.includes("/stages/membership/")),
    ).toHaveLength(4);
    expect(
      requests.filter((path) => path.includes("/stages/structure/")),
    ).toHaveLength(1);
  });

  it("terminates with retryable 409 after a fourth supersession", async () => {
    const current = await fixture();
    let conflicts = 0;
    const primary = vi.fn();
    vi.stubGlobal(
      "fetch",
      vi.fn(async (input: string | URL | Request) => {
        const path = String(input);
        if (path.endsWith("/mempool")) return response(current.manifestBytes);
        if (path.includes("/stages/membership/")) {
          conflicts += 1;
          return response(json({ error: "superseded" }), 409);
        }
        const bytes = current.stages.get(path.slice(path.lastIndexOf("/") + 1));
        return response(
          bytes ?? json({ error: "missing" }),
          bytes === undefined ? 404 : 200,
        );
      }),
    );

    await expect(
      loadPackedPublication(
        "core",
        "knots_bip110",
        new AbortController().signal,
        primary,
      ),
    ).rejects.toMatchObject({
      status: 409,
      message: "Source changed while loading; retry when it settles",
    });
    expect(primary).toHaveBeenCalledOnce();
    expect(conflicts).toBe(4);
  });

  it.each([400, 404] as const)(
    "treats terminal stage status %i as non-retryable",
    async (status) => {
      const current = await fixture();
      let manifests = 0;
      let stages = 0;
      vi.stubGlobal(
        "fetch",
        vi.fn(async (input: string | URL | Request) => {
          const path = String(input);
          if (path.endsWith("/mempool")) {
            manifests += 1;
            return response(current.manifestBytes);
          }
          stages += 1;
          return response(json({ error: "terminal stage failure" }), status);
        }),
      );

      await expect(
        loadPackedPublication(
          "core",
          "knots_bip110",
          new AbortController().signal,
        ),
      ).rejects.toMatchObject({ status });
      expect(manifests).toBe(1);
      expect(stages).toBeGreaterThan(0);
    },
  );

  it("rebases one complete candidate without withdrawing primary readiness", async () => {
    const initial = await fixture("none", 1);
    const rebased = await fixture("none", 2);
    const stages = new Map([...initial.stages, ...rebased.stages]);
    let manifests = 0;
    const primary = vi.fn();
    const complete = vi.fn();
    vi.stubGlobal(
      "fetch",
      vi.fn(async (input: string | URL | Request) => {
        const path = String(input);
        if (path.endsWith("/mempool")) {
          manifests += 1;
          return response(
            manifests === 1 ? initial.manifestBytes : rebased.manifestBytes,
          );
        }
        const bytes = stages.get(path.slice(path.lastIndexOf("/") + 1));
        return response(
          bytes ?? json({ error: "missing" }),
          bytes === undefined ? 404 : 200,
        );
      }),
    );

    const loaded = await loadPackedPublication(
      "core",
      "knots_bip110",
      new AbortController().signal,
      primary,
      complete,
    );

    expect(primary).toHaveBeenCalledOnce();
    expect(primary.mock.calls[0]?.[0].manifest.population_id).toBe(
      initial.manifest.population_id,
    );
    expect(loaded.manifest.population_id).toBe(rebased.manifest.population_id);
    expect(complete).toHaveBeenCalledOnce();
    expect(complete.mock.calls[0]?.[0]).toMatchObject({
      supersessionRestarts: 0,
      populationRebaseMs: expect.any(Number),
    });
  });

  it("terminates a second population rebase while retaining committed primary readiness", async () => {
    const initial = await fixture("none", 1);
    const firstRebase = await fixture("none", 2);
    const secondRebase = await fixture("none", 3);
    const stages = new Map([
      ...initial.stages,
      ...firstRebase.stages,
      ...secondRebase.stages,
    ]);
    const firstMembershipId = firstRebase.manifest.stages[1]?.content_id;
    const secondStageIds = new Set(
      secondRebase.manifest.stages.map(({ content_id }) => content_id),
    );
    const stageRequests: string[] = [];
    let manifests = 0;
    const primary = vi.fn();
    vi.stubGlobal(
      "fetch",
      vi.fn(async (input: string | URL | Request) => {
        const path = String(input);
        if (path.endsWith("/mempool")) {
          manifests += 1;
          return response(
            manifests === 1
              ? initial.manifestBytes
              : manifests === 2
                ? firstRebase.manifestBytes
                : secondRebase.manifestBytes,
          );
        }
        stageRequests.push(path);
        const id = path.slice(path.lastIndexOf("/") + 1);
        if (id === firstMembershipId) {
          return response(json({ error: "superseded" }), 409);
        }
        const bytes = stages.get(id);
        return response(
          bytes ?? json({ error: "missing" }),
          bytes === undefined ? 404 : 200,
        );
      }),
    );

    await expect(
      loadPackedPublication(
        "core",
        "knots_bip110",
        new AbortController().signal,
        primary,
      ),
    ).rejects.toMatchObject({
      status: 409,
      message: "Source changed repeatedly while loading; retry when it settles",
    });
    expect(primary).toHaveBeenCalledOnce();
    expect(primary.mock.calls[0]?.[0].manifest.population_id).toBe(
      initial.manifest.population_id,
    );
    expect(
      stageRequests.some((path) =>
        secondStageIds.has(path.slice(path.lastIndexOf("/") + 1)),
      ),
    ).toBe(false);
  });

  it.each([
    ["dependency", "dependencies"],
    ["malformed", "Malformed population stage JSON"],
  ] as const)(
    "rejects a %s stage without committing",
    async (kind, message) => {
      const current = await fixture(kind);
      vi.stubGlobal(
        "fetch",
        vi.fn(async (input: string | URL | Request) => {
          const path = String(input);
          if (path.endsWith("/mempool")) return response(current.manifestBytes);
          const bytes = current.stages.get(
            path.slice(path.lastIndexOf("/") + 1),
          );
          return response(
            bytes ?? json({ error: "missing" }),
            bytes === undefined ? 404 : 200,
          );
        }),
      );

      await expect(
        loadPackedPublication(
          "core",
          "knots_bip110",
          new AbortController().signal,
        ),
      ).rejects.toThrow(message);
    },
  );

  it("rejects a content digest mismatch before JSON decoding", async () => {
    const current = await fixture();
    vi.stubGlobal(
      "fetch",
      vi.fn(async (input: string | URL | Request) => {
        const path = String(input);
        if (path.endsWith("/mempool")) return response(current.manifestBytes);
        const bytes = current.stages.get(path.slice(path.lastIndexOf("/") + 1));
        if (bytes === undefined)
          return response(json({ error: "missing" }), 404);
        const corrupted = bytes.slice();
        corrupted[0] = (corrupted[0] ?? 0) ^ 1;
        return response(corrupted);
      }),
    );
    await expect(
      loadPackedPublication(
        "core",
        "knots_bip110",
        new AbortController().signal,
      ),
    ).rejects.toThrow("content digest mismatch");
  });

  it("does not publish a primary stage whose rows contradict the manifest", async () => {
    const current = await fixture();
    current.manifest.total_vsize = 1;
    current.manifest.source.total_vsize = 1;
    current.manifest.publication_id = await publicationId(current.manifest);
    current.manifestBytes = json(current.manifest);
    vi.stubGlobal(
      "fetch",
      vi.fn(async (input: string | URL | Request) => {
        const path = String(input);
        if (path.endsWith("/mempool")) return response(current.manifestBytes);
        const bytes = current.stages.get(path.slice(path.lastIndexOf("/") + 1));
        return response(
          bytes ?? json({ error: "missing" }),
          bytes === undefined ? 404 : 200,
        );
      }),
    );
    const onPrimary = vi.fn();

    await expect(
      loadPackedPublication(
        "core",
        "knots_bip110",
        new AbortController().signal,
        onPrimary,
      ),
    ).rejects.toThrow("total vsize");
    expect(onPrimary).not.toHaveBeenCalled();
  });

  it("honors Retry-After and bounds rate-limit retries", async () => {
    vi.useFakeTimers();
    const fetchMock = vi.fn().mockResolvedValue(
      new Response(Uint8Array.from(json({ error: "slow down" })).buffer, {
        status: 429,
        headers: { "Retry-After": "1" },
      }),
    );
    vi.stubGlobal("fetch", fetchMock);

    const loading = loadPackedPublication(
      "core",
      "knots_bip110",
      new AbortController().signal,
    );
    const rejected = expect(loading).rejects.toThrow(
      "rate limit retries exhausted after 4 attempts",
    );
    await vi.runAllTimersAsync();
    await rejected;
    expect(fetchMock).toHaveBeenCalledTimes(4);
  });
});
