import { countFormat } from "./format";
import { yieldCooperatively } from "./cooperative-work";
import type {
  ComparisonSide,
  CurrentComparison,
  LoadedSourceSnapshot,
} from "./comparison-model";
import { packedSnapshotTransaction } from "./packed-store";

const FINGERPRINT_MAGIC = "ATLCFP01";
const OUTPOINT_MAGIC = "ATLCOP01";
const HEADER_BYTES = 24;
const FINGERPRINT_RECORD_BYTES = 12;
const OUTPOINT_BYTES = 36;
const MAX_FINGERPRINT_BYTES = 64 * 1024 * 1024;
const MAX_OUTPOINT_BYTES = 16 * 1024 * 1024;
const MAX_CANDIDATE_PAIRS = 512;
const MAX_VISIBLE_CONFLICTS = 24;
const EXACT_FETCH_CONCURRENCY = 6;
const FINGERPRINT_COOPERATIVE_BATCH = 16_384;

export interface ConflictFingerprintIndex {
  readonly totalRows: number;
  readonly coveredRows: number;
  readonly recordCount: number;
  readonly bytes: Uint8Array;
}

interface ExactOutpoints {
  readonly totalRows: number;
  readonly coveredRows: number;
  readonly sourceRow: number;
  readonly outpointCount: number;
  readonly bytes: Uint8Array;
}

interface ConflictCandidate {
  leftRow: number;
  rightRow: number;
  leftTxid: string;
  rightTxid: string;
}

export interface ConflictingSpendPair {
  leftTxid: string;
  rightTxid: string;
  sharedOutpointCount: number;
}

export interface ConflictingSpendAnalysis {
  conflicts: ConflictingSpendPair[];
  candidateCoverageTruncated: boolean;
  leftCoveredRows: number;
  leftTotalRows: number;
  rightCoveredRows: number;
  rightTotalRows: number;
}

type FetchBinary = (
  input: RequestInfo | URL,
  init?: RequestInit,
) => Promise<Response>;

const magicAt = (bytes: Uint8Array, expected: string): boolean => {
  if (bytes.byteLength < expected.length) return false;
  for (let index = 0; index < expected.length; index += 1) {
    if (bytes[index] !== expected.charCodeAt(index)) return false;
  }
  return true;
};

const checkedBodyLength = (
  count: number,
  recordBytes: number,
  label: string,
): number => {
  const length = HEADER_BYTES + count * recordBytes;
  if (!Number.isSafeInteger(length)) {
    throw new TypeError(`${label} record count exceeds browser limits`);
  }
  return length;
};

const fingerprintOffset = (record: number): number =>
  HEADER_BYTES + record * FINGERPRINT_RECORD_BYTES;

const fingerprintRow = (
  index: ConflictFingerprintIndex,
  record: number,
): number => {
  const offset = fingerprintOffset(record) + 8;
  return (
    ((index.bytes[offset] ?? 0) |
      ((index.bytes[offset + 1] ?? 0) << 8) |
      ((index.bytes[offset + 2] ?? 0) << 16) |
      ((index.bytes[offset + 3] ?? 0) << 24)) >>>
    0
  );
};

const compareFingerprints = (
  left: ConflictFingerprintIndex,
  leftRecord: number,
  right: ConflictFingerprintIndex,
  rightRecord: number,
): number => {
  const leftOffset = fingerprintOffset(leftRecord);
  const rightOffset = fingerprintOffset(rightRecord);
  for (let byte = 0; byte < 8; byte += 1) {
    const difference =
      (left.bytes[leftOffset + byte] ?? 0) -
      (right.bytes[rightOffset + byte] ?? 0);
    if (difference !== 0) return difference;
  }
  return 0;
};

const parseConflictFingerprintHeader = (
  body: ArrayBuffer,
  expectedRows?: number,
): ConflictFingerprintIndex => {
  const bytes = new Uint8Array(body);
  if (bytes.byteLength < HEADER_BYTES || !magicAt(bytes, FINGERPRINT_MAGIC)) {
    throw new TypeError("Invalid conflict fingerprint header");
  }
  const view = new DataView(body);
  const totalRows = view.getUint32(8, true);
  const coveredRows = view.getUint32(12, true);
  const recordCount = view.getUint32(16, true);
  const reserved = view.getUint32(20, true);
  if (reserved !== 0) {
    throw new TypeError("Unsupported conflict fingerprint encoding");
  }
  if (coveredRows > totalRows) {
    throw new TypeError("Conflict fingerprint coverage exceeds its population");
  }
  if (expectedRows !== undefined && totalRows !== expectedRows) {
    throw new TypeError(
      "Conflict fingerprint population does not match snapshot",
    );
  }
  if (
    bytes.byteLength !==
    checkedBodyLength(
      recordCount,
      FINGERPRINT_RECORD_BYTES,
      "Conflict fingerprint",
    )
  ) {
    throw new TypeError("Invalid conflict fingerprint body length");
  }
  const parsed: ConflictFingerprintIndex = {
    totalRows,
    coveredRows,
    recordCount,
    bytes,
  };
  return parsed;
};

const validateFingerprintRecord = (
  parsed: ConflictFingerprintIndex,
  record: number,
): void => {
  const row = fingerprintRow(parsed, record);
  if (row >= parsed.totalRows) {
    throw new TypeError("Conflict fingerprint row is outside its population");
  }
  if (record === 0) return;
  const order = compareFingerprints(parsed, record - 1, parsed, record);
  if (
    order > 0 ||
    (order === 0 &&
      fingerprintRow(parsed, record - 1) > fingerprintRow(parsed, record))
  ) {
    throw new TypeError("Conflict fingerprint records are not sorted");
  }
};

export const parseConflictFingerprintIndex = (
  body: ArrayBuffer,
  expectedRows?: number,
): ConflictFingerprintIndex => {
  const parsed = parseConflictFingerprintHeader(body, expectedRows);
  for (let record = 0; record < parsed.recordCount; record += 1) {
    validateFingerprintRecord(parsed, record);
  }
  return parsed;
};

const validateConflictFingerprintIndexCooperatively = async (
  parsed: ConflictFingerprintIndex,
  signal: AbortSignal,
): Promise<void> => {
  for (let record = 0; record < parsed.recordCount; record += 1) {
    validateFingerprintRecord(parsed, record);
    if (record > 0 && record % FINGERPRINT_COOPERATIVE_BATCH === 0) {
      await yieldCooperatively({ signal });
    }
  }
  signal.throwIfAborted();
};

const parseExactOutpoints = (
  body: ArrayBuffer,
  expected: {
    totalRows: number;
    coveredRows: number;
    sourceRow: number;
  },
): ExactOutpoints => {
  const bytes = new Uint8Array(body);
  if (bytes.byteLength < HEADER_BYTES || !magicAt(bytes, OUTPOINT_MAGIC)) {
    throw new TypeError("Invalid exact outpoint header");
  }
  const view = new DataView(body);
  const totalRows = view.getUint32(8, true);
  const coveredRows = view.getUint32(12, true);
  const sourceRow = view.getUint32(16, true);
  const outpointCount = view.getUint32(20, true);
  if (
    totalRows !== expected.totalRows ||
    coveredRows !== expected.coveredRows ||
    sourceRow !== expected.sourceRow
  ) {
    throw new TypeError(
      "Exact outpoints do not match the analyzed publication",
    );
  }
  if (
    bytes.byteLength !==
    checkedBodyLength(outpointCount, OUTPOINT_BYTES, "Exact outpoint")
  ) {
    throw new TypeError("Invalid exact outpoint body length");
  }
  return { totalRows, coveredRows, sourceRow, outpointCount, bytes };
};

const encodePath = (...parts: string[]): string =>
  parts.map((part) => encodeURIComponent(part)).join("/");

const fingerprintRoute = (source: LoadedSourceSnapshot): string => {
  if (source.structure_id === null) {
    throw new TypeError("Structure publication identity is unavailable");
  }
  return `/api/v2/sources/${encodePath(source.snapshot.source_id)}/mempool/conflict-fingerprints/${encodePath(source.population_id, source.structure_id)}`;
};

const outpointRoute = (source: LoadedSourceSnapshot, txid: string): string => {
  if (source.structure_id === null) {
    throw new TypeError("Structure publication identity is unavailable");
  }
  return `/api/v2/sources/${encodePath(source.snapshot.source_id)}/mempool/conflict-outpoints/${encodePath(source.population_id, source.structure_id, txid)}`;
};

const fetchBinaryBody = async (
  route: string,
  signal: AbortSignal,
  maximumBytes: number,
  fetchBinary: FetchBinary,
): Promise<ArrayBuffer> => {
  const response = await fetchBinary(route, {
    method: "GET",
    headers: { Accept: "application/octet-stream" },
    signal,
  });
  if (!response.ok) {
    if (response.status === 409) {
      throw new Error(
        "The source publication changed before conflict analysis completed",
      );
    }
    if (response.status === 425) {
      throw new Error(
        "Source transaction facts are still loading; try the analysis again when classification completes",
      );
    }
    throw new Error(
      `Conflict analysis request failed (${response.status || "network error"})`,
    );
  }
  const contentLength = response.headers.get("content-length");
  if (
    contentLength !== null &&
    /^\d+$/.test(contentLength) &&
    Number(contentLength) > maximumBytes
  ) {
    throw new RangeError("Conflict analysis response exceeds browser limits");
  }
  const body = await response.arrayBuffer();
  if (body.byteLength > maximumBytes) {
    throw new RangeError("Conflict analysis response exceeds browser limits");
  }
  return body;
};

const comparisonCandidates = async (
  current: CurrentComparison,
  left: ConflictFingerprintIndex,
  right: ConflictFingerprintIndex,
  signal: AbortSignal,
): Promise<{
  candidates: ConflictCandidate[];
  candidateCoverageTruncated: boolean;
}> => {
  const candidates: ConflictCandidate[] = [];
  const seen = new Set<string>();
  let leftRecord = 0;
  let rightRecord = 0;
  let recordsVisited = 0;
  while (leftRecord < left.recordCount && rightRecord < right.recordCount) {
    recordsVisited += 1;
    if (recordsVisited % FINGERPRINT_COOPERATIVE_BATCH === 0) {
      await yieldCooperatively({ signal });
    }
    const order = compareFingerprints(left, leftRecord, right, rightRecord);
    if (order < 0) {
      leftRecord += 1;
      continue;
    }
    if (order > 0) {
      rightRecord += 1;
      continue;
    }
    let leftEnd = leftRecord + 1;
    while (
      leftEnd < left.recordCount &&
      compareFingerprints(left, leftRecord, left, leftEnd) === 0
    ) {
      leftEnd += 1;
    }
    let rightEnd = rightRecord + 1;
    while (
      rightEnd < right.recordCount &&
      compareFingerprints(right, rightRecord, right, rightEnd) === 0
    ) {
      rightEnd += 1;
    }
    for (let leftIndex = leftRecord; leftIndex < leftEnd; leftIndex += 1) {
      const leftRow = fingerprintRow(left, leftIndex);
      const leftTransaction = packedSnapshotTransaction(
        current.left.snapshot,
        leftRow,
      );
      if (leftTransaction === undefined) {
        throw new TypeError(
          "Conflict fingerprint references an unknown source row",
        );
      }
      for (
        let rightIndex = rightRecord;
        rightIndex < rightEnd;
        rightIndex += 1
      ) {
        const rightRow = fingerprintRow(right, rightIndex);
        const rightTransaction = packedSnapshotTransaction(
          current.right.snapshot,
          rightRow,
        );
        if (rightTransaction === undefined) {
          throw new TypeError(
            "Conflict fingerprint references an unknown source row",
          );
        }
        if (leftTransaction.txid === rightTransaction.txid) continue;
        const key = `${leftTransaction.txid}:${rightTransaction.txid}`;
        if (seen.has(key)) continue;
        if (candidates.length >= MAX_CANDIDATE_PAIRS) {
          return { candidates, candidateCoverageTruncated: true };
        }
        seen.add(key);
        candidates.push({
          leftRow,
          rightRow,
          leftTxid: leftTransaction.txid,
          rightTxid: rightTransaction.txid,
        });
      }
    }
    leftRecord = leftEnd;
    rightRecord = rightEnd;
  }
  signal.throwIfAborted();
  return { candidates, candidateCoverageTruncated: false };
};

const outpointHex = (outpoints: ExactOutpoints, index: number): string => {
  const offset = HEADER_BYTES + index * OUTPOINT_BYTES;
  let value = "";
  for (let byte = 0; byte < OUTPOINT_BYTES; byte += 1) {
    value += (outpoints.bytes[offset + byte] ?? 0)
      .toString(16)
      .padStart(2, "0");
  }
  return value;
};

const sharedExactOutpointCount = async (
  left: ExactOutpoints,
  right: ExactOutpoints,
  signal: AbortSignal,
): Promise<number> => {
  const [smaller, larger] =
    left.outpointCount <= right.outpointCount ? [left, right] : [right, left];
  const values = new Set<string>();
  for (let index = 0; index < smaller.outpointCount; index += 1) {
    values.add(outpointHex(smaller, index));
    if (index > 0 && index % 1_024 === 0) {
      await yieldCooperatively({ signal });
    }
  }
  let matches = 0;
  for (let index = 0; index < larger.outpointCount; index += 1) {
    if (values.has(outpointHex(larger, index))) matches += 1;
    if (index > 0 && index % 1_024 === 0) {
      await yieldCooperatively({ signal });
    }
  }
  signal.throwIfAborted();
  return matches;
};

const mapConcurrent = async <T>(
  length: number,
  concurrency: number,
  work: (index: number) => Promise<T>,
): Promise<T[]> => {
  const values = new Array<T>(length);
  let next = 0;
  await Promise.all(
    Array.from({ length: Math.min(length, concurrency) }, async () => {
      while (next < length) {
        const index = next;
        next += 1;
        values[index] = await work(index);
      }
    }),
  );
  return values;
};

export const analyzeConflictingSpends = async (
  current: CurrentComparison,
  signal: AbortSignal,
  fetchBinary: FetchBinary = fetch,
): Promise<ConflictingSpendAnalysis> => {
  if (
    current.left.snapshot.chain_tip.hash ===
    current.right.snapshot.chain_tip.hash
  ) {
    throw new TypeError(
      "Conflicting-spend analysis is available only for different chain tips",
    );
  }
  if (
    current.left.structure_id === null ||
    current.right.structure_id === null
  ) {
    throw new TypeError("Structure publication identity is unavailable");
  }
  const [leftBody, rightBody] = await Promise.all([
    fetchBinaryBody(
      fingerprintRoute(current.left),
      signal,
      MAX_FINGERPRINT_BYTES,
      fetchBinary,
    ),
    fetchBinaryBody(
      fingerprintRoute(current.right),
      signal,
      MAX_FINGERPRINT_BYTES,
      fetchBinary,
    ),
  ]);
  const left = parseConflictFingerprintHeader(
    leftBody,
    current.left.snapshot.transaction_count,
  );
  const right = parseConflictFingerprintHeader(
    rightBody,
    current.right.snapshot.transaction_count,
  );
  await Promise.all([
    validateConflictFingerprintIndexCooperatively(left, signal),
    validateConflictFingerprintIndexCooperatively(right, signal),
  ]);
  const { candidates, candidateCoverageTruncated } = await comparisonCandidates(
    current,
    left,
    right,
    signal,
  );
  const leftExact = new Map<string, Promise<ExactOutpoints>>();
  const rightExact = new Map<string, Promise<ExactOutpoints>>();
  const exactFor = (
    side: ComparisonSide,
    txid: string,
    row: number,
  ): Promise<ExactOutpoints> => {
    const source = current[side];
    const index = side === "left" ? left : right;
    const cache = side === "left" ? leftExact : rightExact;
    let pending = cache.get(txid);
    if (pending === undefined) {
      pending = fetchBinaryBody(
        outpointRoute(source, txid),
        signal,
        MAX_OUTPOINT_BYTES,
        fetchBinary,
      ).then((body) =>
        parseExactOutpoints(body, {
          totalRows: index.totalRows,
          coveredRows: index.coveredRows,
          sourceRow: row,
        }),
      );
      cache.set(txid, pending);
    }
    return pending;
  };
  const verified = await mapConcurrent(
    candidates.length,
    EXACT_FETCH_CONCURRENCY,
    async (index) => {
      const candidate = candidates[index];
      if (candidate === undefined) return null;
      const [leftOutpoints, rightOutpoints] = await Promise.all([
        exactFor("left", candidate.leftTxid, candidate.leftRow),
        exactFor("right", candidate.rightTxid, candidate.rightRow),
      ]);
      const sharedOutpointCount = await sharedExactOutpointCount(
        leftOutpoints,
        rightOutpoints,
        signal,
      );
      return sharedOutpointCount > 0
        ? {
            leftTxid: candidate.leftTxid,
            rightTxid: candidate.rightTxid,
            sharedOutpointCount,
          }
        : null;
    },
  );
  return {
    conflicts: verified.filter(
      (pair): pair is ConflictingSpendPair => pair !== null,
    ),
    candidateCoverageTruncated,
    leftCoveredRows: left.coveredRows,
    leftTotalRows: left.totalRows,
    rightCoveredRows: right.coveredRows,
    rightTotalRows: right.totalRows,
  };
};

interface ConflictViewElements {
  panel: HTMLElement;
  action: HTMLButtonElement;
  status: HTMLElement;
  coverage: HTMLElement;
  list: HTMLElement;
}

export interface ComparisonConflictView {
  render(current: CurrentComparison, complete: boolean): void;
  reset(): void;
}

const comparisonIdentity = (current: CurrentComparison): string =>
  [
    current.left.snapshot.source_id,
    current.left.population_id,
    current.left.structure_id,
    current.right.snapshot.source_id,
    current.right.population_id,
    current.right.structure_id,
  ].join(":");

const compactTxid = (txid: string): string =>
  `${txid.slice(0, 10)}…${txid.slice(-8)}`;

export const conflictCoverageDescription = (
  result: ConflictingSpendAnalysis,
  leftLabel: string,
  rightLabel: string,
): string => {
  const partial =
    result.leftCoveredRows < result.leftTotalRows ||
    result.rightCoveredRows < result.rightTotalRows;
  let description = partial
    ? `Partial coverage: ${leftLabel} ${countFormat.format(result.leftCoveredRows)} of ${countFormat.format(result.leftTotalRows)}; ${rightLabel} ${countFormat.format(result.rightCoveredRows)} of ${countFormat.format(result.rightTotalRows)}. Uncovered transactions were not analyzed. Each listed pair spends at least one identical outpoint; Atlas does not infer intent, relay cause, rejection, or safety.`
    : "All transactions in both snapshots were analyzed. Each listed pair spends at least one identical outpoint; Atlas does not infer intent, relay cause, rejection, or safety.";
  if (result.candidateCoverageTruncated) {
    description = `${description} Analysis limit reached: additional fingerprint candidates were not verified, so the displayed conflict count is not exhaustive.`;
  }
  return description;
};

export const createComparisonConflictView = (
  { panel, action, status, coverage, list }: ConflictViewElements,
  selectTransaction: (side: ComparisonSide, txid: string) => void,
  fetchBinary: FetchBinary = fetch,
): ComparisonConflictView => {
  let current: CurrentComparison | null = null;
  let identity: string | null = null;
  let controller: AbortController | null = null;
  let generation = 0;

  const clearResult = (): void => {
    controller?.abort();
    controller = null;
    generation += 1;
    status.textContent =
      "Compare exact spent outpoints without changing the live comparison.";
    coverage.textContent = "";
    list.replaceChildren();
    action.disabled = false;
    action.textContent = "Analyze conflicting spends";
  };

  const reset = (): void => {
    current = null;
    identity = null;
    clearResult();
    panel.hidden = true;
  };

  action.addEventListener("click", () => {
    const analyzing = current;
    const analyzingIdentity = identity;
    if (analyzing === null || analyzingIdentity === null) return;
    controller?.abort();
    const activeController = new AbortController();
    controller = activeController;
    const activeGeneration = ++generation;
    action.disabled = true;
    action.textContent = "Analyzing…";
    status.textContent =
      "Loading compact source-local fingerprints, then verifying candidates with exact outpoints…";
    coverage.textContent = "";
    list.replaceChildren();
    void analyzeConflictingSpends(
      analyzing,
      activeController.signal,
      fetchBinary,
    )
      .then((result) => {
        if (
          activeController.signal.aborted ||
          identity !== analyzingIdentity ||
          generation !== activeGeneration
        ) {
          return;
        }
        const displayed = current ?? analyzing;
        status.textContent = `${countFormat.format(result.conflicts.length)} conflicting transaction ${result.conflicts.length === 1 ? "pair" : "pairs"} among ${countFormat.format(result.leftCoveredRows)}/${countFormat.format(result.rightCoveredRows)} analyzed transactions.`;
        coverage.textContent = conflictCoverageDescription(
          result,
          displayed.left.snapshot.source_label,
          displayed.right.snapshot.source_label,
        );
        const items = result.conflicts
          .slice(0, MAX_VISIBLE_CONFLICTS)
          .map((conflict, index) => {
            const item = document.createElement("li");
            const title = document.createElement("span");
            title.textContent = `Pair ${countFormat.format(index + 1)} · ${countFormat.format(conflict.sharedOutpointCount)} shared ${conflict.sharedOutpointCount === 1 ? "outpoint" : "outpoints"}`;
            const left = document.createElement("button");
            left.type = "button";
            left.textContent = `A ${compactTxid(conflict.leftTxid)}`;
            left.title = conflict.leftTxid;
            left.setAttribute(
              "aria-label",
              `Inspect ${conflict.leftTxid} from ${displayed.left.snapshot.source_label}`,
            );
            left.addEventListener("click", () =>
              selectTransaction("left", conflict.leftTxid),
            );
            const right = document.createElement("button");
            right.type = "button";
            right.textContent = `B ${compactTxid(conflict.rightTxid)}`;
            right.title = conflict.rightTxid;
            right.setAttribute(
              "aria-label",
              `Inspect ${conflict.rightTxid} from ${displayed.right.snapshot.source_label}`,
            );
            right.addEventListener("click", () =>
              selectTransaction("right", conflict.rightTxid),
            );
            item.append(title, left, right);
            return item;
          });
        if (result.conflicts.length > MAX_VISIBLE_CONFLICTS) {
          const remainder = document.createElement("li");
          remainder.textContent = `${countFormat.format(result.conflicts.length - MAX_VISIBLE_CONFLICTS)} additional conflicting transaction pairs are not listed.`;
          items.push(remainder);
        }
        list.replaceChildren(...items);
      })
      .catch((error: unknown) => {
        if (
          activeController.signal.aborted ||
          identity !== analyzingIdentity ||
          generation !== activeGeneration
        ) {
          return;
        }
        status.textContent =
          error instanceof Error
            ? error.message
            : "Conflicting-spend analysis failed";
        coverage.textContent =
          "The current membership comparison remains available.";
      })
      .finally(() => {
        if (
          current !== null &&
          identity === analyzingIdentity &&
          generation === activeGeneration
        ) {
          controller = null;
          action.disabled = false;
          action.textContent = "Analyze again";
        }
      });
  });

  return {
    render(next, complete): void {
      const nextIdentity = comparisonIdentity(next);
      if (identity !== nextIdentity) {
        identity = nextIdentity;
        clearResult();
      }
      const visible =
        complete &&
        next.left.snapshot.chain_tip.hash !==
          next.right.snapshot.chain_tip.hash &&
        next.left.structure_id !== null &&
        next.right.structure_id !== null;
      const lifecycleTerminal = (source: LoadedSourceSnapshot): boolean => {
        const state = source.source.classification?.state;
        return state === "complete" || state === "paused";
      };
      const available =
        visible &&
        lifecycleTerminal(next.left) &&
        lifecycleTerminal(next.right);
      current = available ? next : null;
      panel.hidden = !visible;
      if (!visible) {
        controller?.abort();
      } else if (!available) {
        controller?.abort();
        controller = null;
        generation += 1;
        action.disabled = true;
        action.textContent = "Waiting for transaction facts";
        status.textContent =
          "Conflicting-spend analysis will be available when both sources finish or pause transaction classification.";
        coverage.textContent = "";
        list.replaceChildren();
      } else if (controller === null) {
        action.disabled = false;
        if (action.textContent === "Waiting for transaction facts") {
          action.textContent = "Analyze conflicting spends";
          status.textContent =
            "Compare exact spent outpoints without changing the live comparison.";
        }
      }
    },
    reset,
  };
};
