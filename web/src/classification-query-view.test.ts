// @vitest-environment happy-dom

import { afterEach, describe, expect, it, vi } from "vitest";

const terrain = vi.hoisted(() => {
  const transactionId = "01".repeat(32);
  const glyph = {
    txid: transactionId,
    sectionKey: "complete",
    regionKey: "complete:matching",
    rect: { x: 10, y: 12, width: 8, height: 6 },
    vsize: 200,
  };
  return {
    transactionId,
    glyph,
    layout: {
      width: 100,
      height: 50,
      mode: "count",
      sections: [
        {
          key: "complete",
          rect: { x: 0, y: 0, width: 100, height: 50 },
          contentRect: { x: 0, y: 10, width: 100, height: 40 },
          transactionCount: 1,
          totalVsize: 200,
          weight: 1,
          labelHeight: 10,
        },
      ],
      regions: [],
      glyphs: [glyph],
    },
  };
});

vi.mock("./bucket-terrain", async (importOriginal) => {
  const actual = await importOriginal<typeof import("./bucket-terrain")>();
  return {
    ...actual,
    renderBucketTerrain: vi.fn(() => terrain.layout),
    hitTestBucketTerrain: vi.fn(() => ({
      kind: "transaction",
      glyph: terrain.glyph,
    })),
  };
});

import { createClassificationQueryView } from "./classification-query-view";
import { mempoolTransaction } from "./test-fixtures";
import type { ClassifierDescriptor, MempoolSnapshot } from "./types";

const descriptor: ClassifierDescriptor = {
  id: "transaction_properties",
  version: "1",
  title: "Transaction properties",
  methodology: "exact",
  semantics: "multi_label",
  required_facts: ["raw_transaction"],
  labels: [{ key: "version_2", label: "Version 2", description: "Version." }],
};

const snapshot = (): MempoolSnapshot => {
  const transaction = mempoolTransaction(1, {
    txid: terrain.transactionId,
    wtxid: terrain.transactionId,
    classifications: [
      {
        classifier_id: descriptor.id,
        state: "complete",
        primary_label: "version_2",
        labels: ["version_2"],
        missing_facts: [],
        evidence: null,
      },
    ],
  });
  return {
    source_id: "core",
    source_label: "Bitcoin Core",
    collection_started_at_ms: 1,
    collection_completed_at_ms: 2,
    collection_duration_ms: 1,
    observed_at_ms: 2,
    classification_revision: 1,
    chain_tip: { height: 1, hash: "ab".repeat(32) },
    transaction_count: 1,
    total_vsize: transaction.vsize,
    classifier_catalog: [descriptor],
    classification_summaries: [
      {
        classifier_id: descriptor.id,
        complete_count: 1,
        partial_count: 0,
        unclassified_count: 0,
        label_counts: { version_2: 1 },
      },
    ],
    bip110_summary: {
      evaluator_id: "rdts-rules",
      evaluator_version: "1",
      scope: "knots_mempool_policy",
      compatible_count: 0,
      violating_count: 0,
      indeterminate_count: 0,
      unclassified_count: 1,
    },
    transactions: [transaction],
  };
};

afterEach(() => {
  vi.restoreAllMocks();
  document.body.replaceChildren();
});

describe("classification query selection", () => {
  it("replays a click made before the first layout frame commits", () => {
    const frames: FrameRequestCallback[] = [];
    vi.spyOn(window, "requestAnimationFrame").mockImplementation((callback) => {
      frames.push(callback);
      return frames.length;
    });
    vi.spyOn(window, "cancelAnimationFrame").mockImplementation(() => {});

    const root = document.createElement("section");
    const stage = document.createElement("div");
    const canvas = document.createElement("canvas");
    canvas.getBoundingClientRect = () =>
      ({ left: 0, top: 0, width: 100, height: 50 }) as DOMRect;
    const selectionMarker = document.createElement("div");
    const regions = document.createElement("div");
    const summary = document.createElement("p");
    const empty = document.createElement("p");
    const hint = document.createElement("p");
    const activeOption = document.createElement("div");
    const navigationStatus = document.createElement("p");
    stage.append(canvas, selectionMarker, regions, activeOption);
    root.append(stage, summary, empty, hint, navigationStatus);
    document.body.append(root);
    const onSelectTransaction = vi.fn();
    const view = createClassificationQueryView(
      {
        root,
        stage,
        canvas,
        selectionMarker,
        regions,
        summary,
        empty,
        hint,
        activeOption,
        navigationStatus,
      },
      onSelectTransaction,
    );

    view.render({
      snapshot: snapshot(),
      descriptor,
      labelKeys: ["version_2"],
      matchMode: "any",
      metric: "count",
      selectedTxid: null,
    });
    stage.dispatchEvent(
      new MouseEvent("click", { clientX: 14, clientY: 15, bubbles: true }),
    );
    expect(onSelectTransaction).not.toHaveBeenCalled();

    frames.shift()?.(performance.now());

    expect(onSelectTransaction).toHaveBeenCalledTimes(1);
    expect(onSelectTransaction).toHaveBeenCalledWith(
      expect.objectContaining({ txid: terrain.transactionId }),
    );
  });
});
