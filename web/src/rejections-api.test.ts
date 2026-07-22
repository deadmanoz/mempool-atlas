import { describe, expect, it } from "vitest";
import { parseSourceRejections } from "./rejections-api";

const validRejections = (): Record<string, unknown> => ({
  source_id: "source-a",
  as_of_ms: 1_752_710_400_000,
  availability: { status: "available" },
  window: {
    count: 2,
    oldest_at_ms: 1_752_707_280_000,
    newest_at_ms: 1_752_707_400_000,
  },
  by_reason: [
    { reason: "insufficient fee", count: 1, is_rollup: false },
    { reason: "min relay fee not met", count: 1, is_rollup: false },
  ],
  attribution: {
    classified_count: 1,
    unclassified_count: 1,
    taxonomies: [
      {
        key: "behavior",
        label: "Behavior",
        verdicts: [
          { verdict: "payment", count: 0 },
          { verdict: "data", count: 1 },
          { verdict: "unknown", count: 0 },
        ],
      },
    ],
  },
  recent: [
    {
      txid: "0".repeat(64),
      reason: "insufficient fee",
      observed_at_ms: 1_752_707_400_000,
      evidence_event_id: "source-a/session-a/16",
      verdicts: [],
    },
    {
      txid: "e".repeat(64),
      reason: "min relay fee not met",
      observed_at_ms: 1_752_707_280_000,
      evidence_event_id: "source-a/session-a/15",
      verdicts: [
        ["behavior", "data"],
        ["bip110", "conforming"],
      ],
    },
  ],
  next_cursor: "cursor-token",
});

describe("parseSourceRejections", () => {
  it("accepts a full rejections payload", () => {
    const rejections = parseSourceRejections(validRejections());
    expect(rejections.source_id).toBe("source-a");
    expect(rejections.availability.status).toBe("available");
    expect(rejections.window.count).toBe(2);
    expect(rejections.window.oldest_at_ms).toBe(1_752_707_280_000);
    expect(rejections.window.newest_at_ms).toBe(1_752_707_400_000);
    expect(rejections.by_reason).toHaveLength(2);
    expect(rejections.by_reason[0]?.is_rollup).toBe(false);
    expect(rejections.attribution.classified_count).toBe(1);
    expect(rejections.attribution.unclassified_count).toBe(1);
    expect(rejections.attribution.taxonomies[0]?.verdicts[1]).toEqual({
      verdict: "data",
      count: 1,
    });
    expect(rejections.recent).toHaveLength(2);
    expect(rejections.recent[0]?.verdicts).toEqual([]);
    expect(rejections.recent[1]?.verdicts).toEqual([
      ["behavior", "data"],
      ["bip110", "conforming"],
    ]);
    expect(rejections.next_cursor).toBe("cursor-token");
  });

  it("accepts an empty window with no timestamps and no cursor", () => {
    const rejections = parseSourceRejections({
      source_id: "source-a",
      as_of_ms: 1_752_710_400_000,
      availability: { status: "available" },
      window: { count: 0 },
      by_reason: [],
      attribution: {
        classified_count: 0,
        unclassified_count: 0,
        taxonomies: [],
      },
      recent: [],
    });
    expect(rejections.window.count).toBe(0);
    expect(rejections.window.oldest_at_ms).toBeUndefined();
    expect(rejections.window.newest_at_ms).toBeUndefined();
    expect(rejections.next_cursor).toBeUndefined();
    expect(rejections.recent).toEqual([]);
  });

  it("accepts an honest not-collected envelope", () => {
    const rejections = parseSourceRejections({
      source_id: "source-a",
      as_of_ms: 1_752_710_400_000,
      availability: { status: "not_collected" },
      window: { count: 0 },
      by_reason: [],
      attribution: {
        classified_count: 0,
        unclassified_count: 0,
        taxonomies: [],
      },
      recent: [],
    });

    expect(rejections.availability.status).toBe("not_collected");
    expect(rejections.window.count).toBe(0);
  });

  it("rejects observations when evidence is marked not collected", () => {
    const payload = validRejections();
    payload.availability = { status: "not_collected" };
    expect(() => parseSourceRejections(payload)).toThrow(
      "Rejection evidence marked not_collected must not contain observations",
    );
  });

  it("distinguishes a literal other reason from the synthetic rollup", () => {
    const payload = validRejections();
    payload.by_reason = [
      { reason: "other", count: 1, is_rollup: false },
      { reason: "other", count: 1, is_rollup: true },
    ];

    const rejections = parseSourceRejections(payload);

    expect(rejections.by_reason).toEqual([
      { reason: "other", count: 1, is_rollup: false },
      { reason: "other", count: 1, is_rollup: true },
    ]);
  });

  it.each([
    [
      "a missing availability",
      (payload: Record<string, unknown>): void => {
        delete payload.availability;
      },
    ],
    [
      "an unknown availability status",
      (payload: Record<string, unknown>): void => {
        payload.availability = { status: "unknown" };
      },
    ],
    [
      "a negative window count",
      (payload: Record<string, unknown>): void => {
        (payload.window as Record<string, unknown>).count = -1;
      },
    ],
    [
      "a non-integer window timestamp",
      (payload: Record<string, unknown>): void => {
        (payload.window as Record<string, unknown>).oldest_at_ms = 1.5;
      },
    ],
    [
      "a reason count that is not a number",
      (payload: Record<string, unknown>): void => {
        (payload.by_reason as Record<string, unknown>[])[0]!.count = "many";
      },
    ],
    [
      "a reason entry without its rollup marker",
      (payload: Record<string, unknown>): void => {
        delete (payload.by_reason as Record<string, unknown>[])[0]!.is_rollup;
      },
    ],
    [
      "attribution missing its classified count",
      (payload: Record<string, unknown>): void => {
        delete (payload.attribution as Record<string, unknown>)
          .classified_count;
      },
    ],
    [
      "a taxonomy verdict count missing its verdict key",
      (payload: Record<string, unknown>): void => {
        const attribution = payload.attribution as Record<string, unknown>;
        const taxonomies = attribution.taxonomies as Record<string, unknown>[];
        const verdicts = taxonomies[0]!.verdicts as Record<string, unknown>[];
        delete verdicts[0]!.verdict;
      },
    ],
    [
      "a recent record missing its txid",
      (payload: Record<string, unknown>): void => {
        delete (payload.recent as Record<string, unknown>[])[0]!.txid;
      },
    ],
    [
      "a recent record with a malformed txid",
      (payload: Record<string, unknown>): void => {
        (payload.recent as Record<string, unknown>[])[0]!.txid = "not-a-txid";
      },
    ],
    [
      "a verdict pair that is not a two-tuple",
      (payload: Record<string, unknown>): void => {
        (payload.recent as Record<string, unknown>[])[1]!.verdicts = [
          ["behavior"],
        ];
      },
    ],
    [
      "an empty next cursor",
      (payload: Record<string, unknown>): void => {
        payload.next_cursor = "";
      },
    ],
    [
      "a non-empty window without both timestamps",
      (payload: Record<string, unknown>): void => {
        delete (payload.window as Record<string, unknown>).newest_at_ms;
      },
    ],
    [
      "window timestamps in reverse order",
      (payload: Record<string, unknown>): void => {
        (payload.window as Record<string, unknown>).oldest_at_ms =
          1_752_707_500_000;
      },
    ],
    [
      "reason counts that do not sum to the window",
      (payload: Record<string, unknown>): void => {
        (payload.by_reason as Record<string, unknown>[])[0]!.count = 2;
      },
    ],
    [
      "attribution counts that do not sum to the window",
      (payload: Record<string, unknown>): void => {
        (payload.attribution as Record<string, unknown>).unclassified_count = 0;
      },
    ],
    [
      "taxonomy verdict counts that do not sum to classified_count",
      (payload: Record<string, unknown>): void => {
        const attribution = payload.attribution as Record<string, unknown>;
        const taxonomies = attribution.taxonomies as Record<string, unknown>[];
        const verdicts = taxonomies[0]!.verdicts as Record<string, unknown>[];
        verdicts[1]!.count = 0;
      },
    ],
  ])("rejects %s", (_name, mutate) => {
    const payload = validRejections();
    mutate(payload);
    expect(() => parseSourceRejections(payload)).toThrow(TypeError);
  });
});
