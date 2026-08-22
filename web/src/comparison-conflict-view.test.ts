// @vitest-environment happy-dom

import { beforeEach, describe, expect, it, vi } from "vitest";

import {
  analyzeConflictingSpends,
  conflictCoverageDescription,
  createComparisonConflictView,
  parseConflictFingerprintIndex,
} from "./comparison-conflict-view";
import {
  compareCurrentSnapshots,
  type CurrentComparison,
} from "./comparison-model";
import { loadedSource } from "./comparison-test-fixtures";
import { mempoolTransaction } from "./test-fixtures";

const ASCII = new TextEncoder();

const fingerprintBody = (
  totalRows: number,
  coveredRows: number,
  records: Array<{ fingerprint: number[]; row: number }>,
  reserved = 0,
): ArrayBuffer => {
  const body = new ArrayBuffer(24 + records.length * 12);
  const bytes = new Uint8Array(body);
  bytes.set(ASCII.encode("ATLCFP01"));
  const view = new DataView(body);
  view.setUint32(8, totalRows, true);
  view.setUint32(12, coveredRows, true);
  view.setUint32(16, records.length, true);
  view.setUint32(20, reserved, true);
  records.forEach(({ fingerprint, row }, index) => {
    const offset = 24 + index * 12;
    bytes.set(fingerprint, offset);
    view.setUint32(offset + 8, row, true);
  });
  return body;
};

const outpoint = (value: number): number[] => [
  ...Array.from({ length: 32 }, () => value),
  value,
  0,
  0,
  0,
];

const outpointBody = (
  totalRows: number,
  coveredRows: number,
  sourceRow: number,
  outpoints: number[][],
): ArrayBuffer => {
  const body = new ArrayBuffer(24 + outpoints.length * 36);
  const bytes = new Uint8Array(body);
  bytes.set(ASCII.encode("ATLCOP01"));
  const view = new DataView(body);
  view.setUint32(8, totalRows, true);
  view.setUint32(12, coveredRows, true);
  view.setUint32(16, sourceRow, true);
  view.setUint32(20, outpoints.length, true);
  outpoints.forEach((value, index) => bytes.set(value, 24 + index * 36));
  return body;
};

const binaryResponse = (body: ArrayBuffer, status = 200): Response =>
  new Response(body, {
    status,
    headers: { "content-type": "application/octet-stream" },
  });

const comparison = (
  leftValues: number[],
  rightValues: number[],
  differentTips = true,
): CurrentComparison => {
  const left = loadedSource(
    "core",
    leftValues.map((value) => mempoolTransaction(value)),
  );
  const right = loadedSource(
    "knots",
    rightValues.map((value) => mempoolTransaction(value)),
  );
  if (differentTips) {
    right.snapshot.chain_tip = {
      height: right.snapshot.chain_tip.height,
      hash: "11".repeat(32),
    };
  }
  return compareCurrentSnapshots(left, right);
};

const routeTxid = (route: string): string => route.split("/").at(-1) ?? "";

const waitForView = async (): Promise<void> => {
  await new Promise((resolve) => setTimeout(resolve, 0));
  await new Promise((resolve) => setTimeout(resolve, 0));
};

const viewElements = () => ({
  panel: document.querySelector<HTMLElement>("#conflict-analysis")!,
  action: document.querySelector<HTMLButtonElement>(
    "#conflict-analysis-action",
  )!,
  status: document.querySelector<HTMLElement>("#conflict-analysis-status")!,
  coverage: document.querySelector<HTMLElement>("#conflict-analysis-coverage")!,
  list: document.querySelector<HTMLElement>("#conflict-analysis-list")!,
});

describe("conflict fingerprint validation", () => {
  it("accepts the fixed binary layout and rejects reserved, truncated, and unsorted records", () => {
    const valid = fingerprintBody(3, 2, [
      { fingerprint: [0, 0, 0, 0, 0, 0, 0, 1], row: 0 },
      { fingerprint: [0, 0, 0, 0, 0, 0, 0, 2], row: 2 },
    ]);
    expect(parseConflictFingerprintIndex(valid, 3)).toMatchObject({
      totalRows: 3,
      coveredRows: 2,
      recordCount: 2,
    });
    expect(() =>
      parseConflictFingerprintIndex(
        fingerprintBody(3, 2, [{ fingerprint: Array(8).fill(0), row: 0 }], 1),
      ),
    ).toThrow("Unsupported conflict fingerprint encoding");
    expect(() =>
      parseConflictFingerprintIndex(valid.slice(0, valid.byteLength - 1)),
    ).toThrow("Invalid conflict fingerprint body length");
    expect(() =>
      parseConflictFingerprintIndex(
        fingerprintBody(3, 2, [
          { fingerprint: [0, 0, 0, 0, 0, 0, 0, 2], row: 0 },
          { fingerprint: [0, 0, 0, 0, 0, 0, 0, 1], row: 1 },
        ]),
      ),
    ).toThrow("Conflict fingerprint records are not sorted");
  });
});

describe("conflicting-spend analysis", () => {
  it("verifies fingerprint candidates against exact outpoints and rejects collisions", async () => {
    const current = comparison([1, 3], [2, 4]);
    const fingerprintOne = [0, 0, 0, 0, 0, 0, 0, 1];
    const fingerprintTwo = [0, 0, 0, 0, 0, 0, 0, 2];
    const fetchBinary = vi.fn(async (input: RequestInfo | URL) => {
      const route = String(input);
      if (route.includes("conflict-fingerprints")) {
        return binaryResponse(
          fingerprintBody(2, 2, [
            { fingerprint: fingerprintOne, row: 0 },
            { fingerprint: fingerprintTwo, row: 1 },
          ]),
        );
      }
      const txid = routeTxid(route);
      const isLeft = route.includes("/core/");
      const row = txid === mempoolTransaction(isLeft ? 1 : 2).txid ? 0 : 1;
      const values =
        row === 0 ? [outpoint(9)] : isLeft ? [outpoint(7)] : [outpoint(8)];
      return binaryResponse(outpointBody(2, 2, row, values));
    });

    const result = await analyzeConflictingSpends(
      current,
      new AbortController().signal,
      fetchBinary,
    );

    expect(result.conflicts).toEqual([
      {
        leftTxid: mempoolTransaction(1).txid,
        rightTxid: mempoolTransaction(2).txid,
        sharedOutpointCount: 1,
      },
    ]);
  });

  it("excludes a common txid before exact requests", async () => {
    const current = comparison([1], [1]);
    const fetchBinary = vi.fn(async (input: RequestInfo | URL) => {
      expect(String(input)).toContain("conflict-fingerprints");
      return binaryResponse(
        fingerprintBody(1, 1, [
          { fingerprint: [0, 0, 0, 0, 0, 0, 0, 1], row: 0 },
        ]),
      );
    });

    const result = await analyzeConflictingSpends(
      current,
      new AbortController().signal,
      fetchBinary,
    );

    expect(result.conflicts).toEqual([]);
    expect(fetchBinary).toHaveBeenCalledTimes(2);
  });

  it("deduplicates a candidate pair and counts all shared exact outpoints", async () => {
    const current = comparison([1], [2]);
    const fetchBinary = vi.fn(async (input: RequestInfo | URL) => {
      const route = String(input);
      if (route.includes("conflict-fingerprints")) {
        return binaryResponse(
          fingerprintBody(1, 1, [
            { fingerprint: [0, 0, 0, 0, 0, 0, 0, 1], row: 0 },
            { fingerprint: [0, 0, 0, 0, 0, 0, 0, 2], row: 0 },
          ]),
        );
      }
      return binaryResponse(outpointBody(1, 1, 0, [outpoint(3), outpoint(4)]));
    });

    const result = await analyzeConflictingSpends(
      current,
      new AbortController().signal,
      fetchBinary,
    );

    expect(result.conflicts[0]?.sharedOutpointCount).toBe(2);
    expect(fetchBinary).toHaveBeenCalledTimes(4);
  });

  it("surfaces publication supersession without replacing the comparison", async () => {
    const current = comparison([1], [2]);
    const fetchBinary = vi.fn(async () =>
      binaryResponse(new ArrayBuffer(0), 409),
    );

    await expect(
      analyzeConflictingSpends(
        current,
        new AbortController().signal,
        fetchBinary,
      ),
    ).rejects.toThrow("publication changed");
  });

  it("rejects malformed exact outpoint bodies after a fingerprint match", async () => {
    const current = comparison([1], [2]);
    const fetchBinary = vi.fn(async (input: RequestInfo | URL) => {
      const route = String(input);
      if (route.includes("conflict-fingerprints")) {
        return binaryResponse(
          fingerprintBody(1, 1, [
            { fingerprint: [0, 0, 0, 0, 0, 0, 0, 1], row: 0 },
          ]),
        );
      }
      return binaryResponse(new ArrayBuffer(24));
    });

    await expect(
      analyzeConflictingSpends(
        current,
        new AbortController().signal,
        fetchBinary,
      ),
    ).rejects.toThrow("Invalid exact outpoint header");
  });

  it("verifies a bounded prefix and marks additional candidates unverified", async () => {
    const rightValues = Array.from({ length: 513 }, (_, index) => index + 2);
    const current = comparison([1], rightValues);
    const rightRows = new Map(
      rightValues.map((value, row) => [mempoolTransaction(value).txid, row]),
    );
    const fingerprint = [0, 0, 0, 0, 0, 0, 0, 1];
    const fetchBinary = vi.fn(async (input: RequestInfo | URL) => {
      const route = String(input);
      if (route.includes("conflict-fingerprints")) {
        const isLeft = route.includes("/core/");
        return binaryResponse(
          fingerprintBody(
            isLeft ? 1 : rightValues.length,
            isLeft ? 1 : rightValues.length,
            isLeft
              ? [{ fingerprint, row: 0 }]
              : rightValues.map((_, row) => ({ fingerprint, row })),
          ),
        );
      }
      const isLeft = route.includes("/core/");
      const row = isLeft ? 0 : rightRows.get(routeTxid(route));
      if (row === undefined) throw new Error("missing fixture row");
      return binaryResponse(
        outpointBody(
          isLeft ? 1 : rightValues.length,
          isLeft ? 1 : rightValues.length,
          row,
          [outpoint(5)],
        ),
      );
    });

    const result = await analyzeConflictingSpends(
      current,
      new AbortController().signal,
      fetchBinary,
    );

    expect(result.candidateCoverageTruncated).toBe(true);
    expect(result.conflicts).toHaveLength(512);
    expect(fetchBinary).toHaveBeenCalledTimes(515);
    expect(conflictCoverageDescription(result, "Core", "Knots")).toContain(
      "additional fingerprint candidates were not verified, so the displayed conflict count is not exhaustive",
    );
  });
});

describe("comparison conflict view", () => {
  beforeEach(() => {
    document.body.innerHTML = `
      <section id="conflict-analysis" hidden>
        <button id="conflict-analysis-action"></button>
        <p id="conflict-analysis-status"></p>
        <p id="conflict-analysis-coverage"></p>
        <ol id="conflict-analysis-list"></ol>
      </section>`;
  });

  it("makes zero requests for snapshots on the same chain tip", async () => {
    const fetchBinary = vi.fn();
    const conflictView = createComparisonConflictView(
      viewElements(),
      vi.fn(),
      fetchBinary,
    );
    conflictView.render(comparison([1], [2], false), true);

    viewElements().action.click();
    await waitForView();

    expect(viewElements().panel.hidden).toBe(true);
    expect(fetchBinary).not.toHaveBeenCalled();
  });

  it("waits for both source classification lifecycles to become terminal", async () => {
    const current = comparison([1], [2]);
    current.right.source.classification = {
      state: "classifying",
      revision: 1,
      classified_count: 0,
      unclassified_count: 1,
    };
    const fetchBinary = vi.fn();
    const conflictView = createComparisonConflictView(
      viewElements(),
      vi.fn(),
      fetchBinary,
    );
    conflictView.render(current, true);

    expect(viewElements().panel.hidden).toBe(false);
    expect(viewElements().action.disabled).toBe(true);
    expect(viewElements().status.textContent).toContain(
      "when both sources finish or pause",
    );
    viewElements().action.click();
    await waitForView();
    expect(fetchBinary).not.toHaveBeenCalled();
  });

  it("discloses partial coverage and selects either transaction through the existing callback", async () => {
    const current = comparison([1, 3], [2, 4]);
    const select = vi.fn();
    const fetchBinary = vi.fn(async (input: RequestInfo | URL) => {
      const route = String(input);
      if (route.includes("conflict-fingerprints")) {
        return binaryResponse(
          fingerprintBody(2, route.includes("/core/") ? 1 : 2, [
            { fingerprint: [0, 0, 0, 0, 0, 0, 0, 1], row: 0 },
          ]),
        );
      }
      return binaryResponse(
        outpointBody(2, route.includes("/core/") ? 1 : 2, 0, [outpoint(5)]),
      );
    });
    const conflictView = createComparisonConflictView(
      viewElements(),
      select,
      fetchBinary,
    );
    conflictView.render(current, true);
    viewElements().action.click();
    await waitForView();

    expect(viewElements().status.textContent).toBe(
      "1 conflicting transaction pair among 1/2 analyzed transactions.",
    );
    expect(viewElements().coverage.textContent).toContain("Partial coverage");
    const buttons = [...viewElements().list.querySelectorAll("button")];
    buttons[0]?.click();
    buttons[1]?.click();
    expect(select).toHaveBeenNthCalledWith(
      1,
      "left",
      mempoolTransaction(1).txid,
    );
    expect(select).toHaveBeenNthCalledWith(
      2,
      "right",
      mempoolTransaction(2).txid,
    );
  });

  it("aborts analysis when the compared population is replaced", async () => {
    let observedSignal: AbortSignal | null = null;
    const fetchBinary = vi.fn(
      (_input: RequestInfo | URL, init?: RequestInit) =>
        new Promise<Response>((_resolve, reject) => {
          observedSignal = init?.signal as AbortSignal;
          observedSignal.addEventListener(
            "abort",
            () => reject(new DOMException("Aborted", "AbortError")),
            { once: true },
          );
        }),
    );
    const conflictView = createComparisonConflictView(
      viewElements(),
      vi.fn(),
      fetchBinary,
    );
    const first = comparison([1], [2]);
    conflictView.render(first, true);
    viewElements().action.click();
    await Promise.resolve();
    const replacement = comparison([1], [2]);
    replacement.left.population_id = "ff".repeat(32);
    conflictView.render(replacement, true);

    expect(observedSignal).not.toBeNull();
    expect((observedSignal as AbortSignal | null)?.aborted).toBe(true);
    expect(viewElements().status.textContent).toContain(
      "Compare exact spent outpoints",
    );
  });

  it("retains results across metadata-only publication changes", async () => {
    const current = comparison([1], [2]);
    const fetchBinary = vi.fn(async (input: RequestInfo | URL) => {
      const route = String(input);
      if (route.includes("conflict-fingerprints")) {
        return binaryResponse(
          fingerprintBody(1, 1, [
            { fingerprint: [0, 0, 0, 0, 0, 0, 0, 1], row: 0 },
          ]),
        );
      }
      return binaryResponse(outpointBody(1, 1, 0, [outpoint(5)]));
    });
    const conflictView = createComparisonConflictView(
      viewElements(),
      vi.fn(),
      fetchBinary,
    );
    conflictView.render(current, true);
    viewElements().action.click();
    await waitForView();
    const resultCopy = viewElements().status.textContent;
    const metadataReplacement = comparison([1], [2]);
    metadataReplacement.left.source = {
      ...metadataReplacement.left.source,
      last_poll_started_at_ms: 1_700_000_123_000,
    };
    conflictView.render(metadataReplacement, true);

    expect(viewElements().status.textContent).toBe(resultCopy);
  });
});
