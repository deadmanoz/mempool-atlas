import type { MempoolTransaction } from "./types";
import {
  forEachCooperatively,
  yieldCooperatively,
  type CooperativeWorkOptions,
} from "./cooperative-work";
import { comparePackedTransactionRowsByVsize } from "./packed-store";

interface TransactionViewMetadata {
  source: readonly MempoolTransaction[];
  rows: Uint32Array;
}

const metadataByView = new WeakMap<
  readonly MempoolTransaction[],
  TransactionViewMetadata
>();

const rowForProperty = (property: PropertyKey): number | null => {
  if (typeof property !== "string" || !/^(0|[1-9][0-9]*)$/.test(property)) {
    return null;
  }
  const index = Number(property);
  return Number.isSafeInteger(index) ? index : null;
};

const createView = (
  source: readonly MempoolTransaction[],
  rows: Uint32Array,
): MempoolTransaction[] => {
  const target: MempoolTransaction[] = [];
  const view = new Proxy(target, {
    get(array, property, receiver) {
      if (property === "length") return rows.length;
      if (property === Symbol.iterator) {
        return function* (): IterableIterator<MempoolTransaction> {
          for (const row of rows) {
            const transaction = source[row];
            if (transaction !== undefined) yield transaction;
          }
        };
      }
      const index = rowForProperty(property);
      return index === null
        ? Reflect.get(array, property, receiver)
        : rows[index] === undefined
          ? undefined
          : source[rows[index]];
    },
    has(array, property) {
      const index = rowForProperty(property);
      return index === null
        ? Reflect.has(array, property)
        : index < rows.length;
    },
    ownKeys() {
      return [
        ...Array.from({ length: rows.length }, (_, index) => String(index)),
        "length",
      ];
    },
    getOwnPropertyDescriptor(array, property) {
      const index = rowForProperty(property);
      if (index === null) {
        return Reflect.getOwnPropertyDescriptor(array, property);
      }
      return index < rows.length
        ? { configurable: true, enumerable: true, writable: false }
        : undefined;
    },
  });
  metadataByView.set(view, { source, rows });
  return view;
};

const compactRows = (rows: readonly number[] | Uint32Array): Uint32Array => {
  const compact = new Uint32Array(rows.length);
  for (let index = 0; index < rows.length; index += 1) {
    const row = rows[index];
    if (
      row === undefined ||
      !Number.isSafeInteger(row) ||
      row < 0 ||
      row > 0xffff_ffff
    ) {
      throw new RangeError(`Invalid transaction row ${String(row)}`);
    }
    compact[index] = row;
  }
  return compact;
};

const metadataFor = (
  transactions: readonly MempoolTransaction[],
): TransactionViewMetadata | null => metadataByView.get(transactions) ?? null;

export const transactionIndexView = (
  transactions: readonly MempoolTransaction[],
  rows: readonly number[] | Uint32Array,
): MempoolTransaction[] => {
  const selectedRows = compactRows(rows);
  const parent = metadataFor(transactions);
  if (parent === null) {
    for (const row of selectedRows) {
      if (row >= transactions.length) {
        throw new RangeError(`Transaction row ${row} is out of range`);
      }
    }
    return createView(transactions, selectedRows);
  }

  const flattenedRows = new Uint32Array(selectedRows.length);
  for (let index = 0; index < selectedRows.length; index += 1) {
    const parentRow = parent.rows[selectedRows[index] ?? 0];
    if (parentRow === undefined) {
      throw new RangeError(
        `Transaction row ${String(selectedRows[index])} is out of range`,
      );
    }
    flattenedRows[index] = parentRow;
  }
  return createView(parent.source, flattenedRows);
};

/**
 * Create a packed view over row storage already owned by another current-state
 * model. The caller must retain the rows without mutating them for the view's
 * lifetime. This avoids copying comparison membership indexes while preserving
 * the packed source metadata used by sort and lookup hot paths.
 */
export const transactionIndexViewFromRetainedRows = (
  transactions: readonly MempoolTransaction[],
  rows: Uint32Array,
): MempoolTransaction[] => {
  if (metadataFor(transactions) !== null) {
    throw new TypeError("Retained row views require a root transaction source");
  }
  for (const row of rows) {
    if (row >= transactions.length) {
      throw new RangeError(`Transaction row ${row} is out of range`);
    }
  }
  return createView(transactions, rows);
};

export const filterTransactionView = (
  transactions: readonly MempoolTransaction[],
  predicate: (transaction: MempoolTransaction, index: number) => boolean,
): MempoolTransaction[] => {
  const parent = metadataFor(transactions);
  const source = parent?.source ?? transactions;
  const retainedRows = new Uint32Array(transactions.length);
  let retainedCount = 0;
  for (let index = 0; index < transactions.length; index += 1) {
    const sourceRow = parent?.rows[index] ?? index;
    const transaction = source[sourceRow];
    if (transaction !== undefined && predicate(transaction, index)) {
      retainedRows[retainedCount] = sourceRow;
      retainedCount += 1;
    }
  }
  return createView(source, retainedRows.slice(0, retainedCount));
};

export const filterTransactionViewCooperatively = async (
  transactions: readonly MempoolTransaction[],
  predicate: (transaction: MempoolTransaction, index: number) => boolean,
  options: CooperativeWorkOptions = {},
): Promise<MempoolTransaction[]> => {
  const parent = metadataFor(transactions);
  const source = parent?.source ?? transactions;
  const retainedRows = new Uint32Array(transactions.length);
  let retainedCount = 0;
  await forEachCooperatively(
    transactions,
    (transaction, index) => {
      if (!predicate(transaction, index)) return;
      retainedRows[retainedCount] = parent?.rows[index] ?? index;
      retainedCount += 1;
    },
    options,
  );
  options.signal?.throwIfAborted();
  return createView(source, retainedRows.slice(0, retainedCount));
};

export const sortTransactionView = (
  transactions: readonly MempoolTransaction[],
  compare: (left: MempoolTransaction, right: MempoolTransaction) => number,
): MempoolTransaction[] => {
  const parent = metadataFor(transactions);
  const source = parent?.source ?? transactions;
  const rows =
    parent?.rows.slice() ??
    Uint32Array.from({ length: transactions.length }, (_, index) => index);
  rows.sort((leftRow, rightRow) => {
    const left = source[leftRow];
    const right = source[rightRow];
    if (left === undefined || right === undefined) {
      return left === undefined ? (right === undefined ? 0 : 1) : -1;
    }
    return compare(left, right);
  });
  return createView(source, rows);
};

export const sortTransactionViewByVsize = (
  transactions: readonly MempoolTransaction[],
): MempoolTransaction[] => {
  const parent = metadataFor(transactions);
  const source = parent?.source ?? transactions;
  const rows =
    parent?.rows.slice() ??
    Uint32Array.from({ length: transactions.length }, (_, index) => index);
  rows.sort((leftRow, rightRow) => {
    const packedOrder = comparePackedTransactionRowsByVsize(
      source,
      leftRow,
      rightRow,
    );
    if (packedOrder !== null) return packedOrder;
    const left = source[leftRow];
    const right = source[rightRow];
    if (left === undefined || right === undefined) {
      return left === undefined ? (right === undefined ? 0 : 1) : -1;
    }
    return right.vsize - left.vsize || left.txid.localeCompare(right.txid);
  });
  return createView(source, rows);
};

export interface CooperativeTransactionSortOptions extends CooperativeWorkOptions {
  timeBudgetMs?: number;
}

const DEFAULT_SORT_TIME_BUDGET_MS = 4;
const SORT_TIME_CHECK_INTERVAL = 256;

const compareTransactionRowsByVsize = (
  source: readonly MempoolTransaction[],
  leftRow: number,
  rightRow: number,
): number => {
  const packedOrder = comparePackedTransactionRowsByVsize(
    source,
    leftRow,
    rightRow,
  );
  if (packedOrder !== null) return packedOrder;
  const left = source[leftRow];
  const right = source[rightRow];
  if (left === undefined || right === undefined) {
    return left === undefined ? (right === undefined ? 0 : 1) : -1;
  }
  return right.vsize - left.vsize || left.txid.localeCompare(right.txid);
};

/**
 * Stable bottom-up merge sort for large packed transaction views. The sort
 * remains private until complete and yields on a wall-clock budget, so one
 * large classifier bucket cannot monopolise the browser main thread.
 */
export const sortTransactionViewByVsizeCooperatively = async (
  transactions: readonly MempoolTransaction[],
  options: CooperativeTransactionSortOptions = {},
): Promise<MempoolTransaction[]> => {
  const timeBudgetMs = options.timeBudgetMs ?? DEFAULT_SORT_TIME_BUDGET_MS;
  if (!Number.isFinite(timeBudgetMs) || timeBudgetMs <= 0) {
    throw new RangeError("Cooperative sort time budget must be positive");
  }
  options.signal?.throwIfAborted();
  const parent = metadataFor(transactions);
  const source = parent?.source ?? transactions;
  let activeRows =
    parent?.rows.slice() ??
    Uint32Array.from({ length: transactions.length }, (_, index) => index);
  if (activeRows.length < 2) return createView(source, activeRows);
  let scratchRows = new Uint32Array(activeRows.length);
  let sliceStartedAt = performance.now();
  let operationsSinceCheck = 0;

  for (let width = 1; width < activeRows.length; width *= 2) {
    for (let start = 0; start < activeRows.length; start += width * 2) {
      const middle = Math.min(start + width, activeRows.length);
      const end = Math.min(start + width * 2, activeRows.length);
      let left = start;
      let right = middle;
      let output = start;
      while (left < middle || right < end) {
        if (
          right >= end ||
          (left < middle &&
            compareTransactionRowsByVsize(
              source,
              activeRows[left] ?? 0,
              activeRows[right] ?? 0,
            ) <= 0)
        ) {
          scratchRows[output] = activeRows[left] ?? 0;
          left += 1;
        } else {
          scratchRows[output] = activeRows[right] ?? 0;
          right += 1;
        }
        output += 1;
        operationsSinceCheck += 1;
        if (operationsSinceCheck >= SORT_TIME_CHECK_INTERVAL) {
          operationsSinceCheck = 0;
          if (performance.now() - sliceStartedAt >= timeBudgetMs) {
            await yieldCooperatively(options);
            sliceStartedAt = performance.now();
          }
        }
      }
    }
    [activeRows, scratchRows] = [scratchRows, activeRows];
  }
  options.signal?.throwIfAborted();
  return createView(source, activeRows);
};

export const concatenateTransactionViews = (
  transactions: readonly MempoolTransaction[],
  populations: readonly (readonly MempoolTransaction[])[],
): MempoolTransaction[] => {
  const parent = metadataFor(transactions);
  const source = parent?.source ?? transactions;
  const metadata = populations.map((population) => metadataFor(population));
  if (metadata.some((entry) => entry === null || entry.source !== source)) {
    throw new TypeError("Transaction views do not share one source");
  }
  const total = metadata.reduce(
    (count, entry) => count + (entry?.rows.length ?? 0),
    0,
  );
  const rows = new Uint32Array(total);
  let offset = 0;
  for (const entry of metadata) {
    if (entry === null) continue;
    rows.set(entry.rows, offset);
    offset += entry.rows.length;
  }
  return createView(source, rows);
};
