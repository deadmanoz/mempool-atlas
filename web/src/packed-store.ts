import type {
  PackedClassifierTransfer,
  PackedPrimaryPublicationTransfer,
  PackedPublicationTransfer,
  PackedSignedColumnTransfer,
  PackedUnsignedColumnTransfer,
} from "./atlas-worker-protocol";
import type {
  ClassificationResult,
  MempoolSnapshot,
  MempoolTransaction,
  LoadedSourcePublication,
  TransactionStructure,
} from "./types";

const HEX = "0123456789abcdef";
const TXID = /^[0-9a-f]{64}$/;
const ROW_CACHE_LIMIT = 256;

const isValidRow = (row: number, rowCount: number): boolean =>
  Number.isSafeInteger(row) && row >= 0 && row < rowCount;

const bit = (bytes: Uint8Array, row: number): boolean =>
  ((bytes[row >> 3] ?? 0) & (1 << (row & 7))) !== 0;

const hashAt = (bytes: Uint8Array, row: number): string => {
  const offset = row * 32;
  let result = "";
  for (let index = 0; index < 32; index += 1) {
    const value = bytes[offset + index] ?? 0;
    result += HEX.charAt(value >> 4) + HEX.charAt(value & 0x0f);
  }
  return result;
};

const parseHash = (value: string): Uint8Array | null => {
  if (!TXID.test(value)) return null;
  const bytes = new Uint8Array(32);
  for (let index = 0; index < 32; index += 1) {
    bytes[index] = Number.parseInt(value.slice(index * 2, index * 2 + 2), 16);
  }
  return bytes;
};

class UnsignedColumn {
  private readonly bytes: Uint8Array;

  constructor(private readonly transfer: PackedUnsignedColumnTransfer) {
    this.bytes = new Uint8Array(transfer.values);
  }

  at(row: number): number {
    let value = 0;
    const offset = row * this.transfer.width;
    for (let index = this.transfer.width - 1; index >= 0; index -= 1) {
      value = value * 256 + (this.bytes[offset + index] ?? 0);
      if (!Number.isSafeInteger(value)) {
        throw new TypeError("Packed unsigned integer is unsafe");
      }
    }
    return value;
  }
}

class SignedColumn {
  private readonly bytes: Uint8Array;

  constructor(private readonly transfer: PackedSignedColumnTransfer) {
    this.bytes = new Uint8Array(transfer.values);
  }

  at(row: number): number {
    let value = 0n;
    const offset = row * this.transfer.width;
    for (let index = this.transfer.width - 1; index >= 0; index -= 1) {
      value = (value << 8n) | BigInt(this.bytes[offset + index] ?? 0);
    }
    const bits = BigInt(this.transfer.width * 8);
    const signed = value & (1n << (bits - 1n)) ? value - (1n << bits) : value;
    const number = Number(signed);
    if (!Number.isSafeInteger(number)) {
      throw new TypeError("Packed signed integer is unsafe");
    }
    return number;
  }
}

class ClassifierColumn {
  readonly codes: UnsignedColumn;
  readonly assessmentCodes: UnsignedColumn | null;

  constructor(readonly transfer: PackedClassifierTransfer) {
    this.codes = new UnsignedColumn(transfer.resultCodes);
    this.assessmentCodes =
      transfer.assessmentCodes === null
        ? null
        : new UnsignedColumn(transfer.assessmentCodes);
  }

  result(row: number): ClassificationResult | null {
    const code = this.codes.at(row);
    if (code === 0) return null;
    const tuple = this.transfer.resultDictionary[code - 1];
    if (tuple === undefined)
      throw new TypeError("Missing classifier dictionary entry");
    return {
      classifier_id: this.transfer.classifierId,
      state: tuple.state,
      primary_label: tuple.primary_label,
      labels: tuple.labels,
      missing_facts: tuple.missing_facts,
      evidence: null,
    };
  }
}

const isArrayIndex = (property: PropertyKey): number | null => {
  if (typeof property !== "string" || !/^(0|[1-9][0-9]*)$/.test(property)) {
    return null;
  }
  const index = Number(property);
  return Number.isSafeInteger(index) ? index : null;
};

export class PackedPublicationStore {
  readonly snapshot: MempoolSnapshot;
  private readonly txids: Uint8Array;
  private readonly vsize: UnsignedColumn;
  private readonly differingWtxidBits: Uint8Array;
  private readonly differingWtxidRanks: Uint32Array;
  private readonly differingWtxids: Uint8Array;
  private readonly weight: UnsignedColumn;
  private readonly feeSats: UnsignedColumn;
  private readonly enteredAtMs: UnsignedColumn;
  private readonly ancestorCount: UnsignedColumn;
  private readonly ancestorVsize: UnsignedColumn;
  private readonly ancestorFeeSats: SignedColumn;
  private readonly descendantCount: UnsignedColumn;
  private readonly descendantVsize: UnsignedColumn;
  private readonly replaceableBits: Uint8Array;
  private readonly structureBits: Uint8Array;
  private readonly structureRanks: Uint32Array;
  private readonly inputCount: UnsignedColumn;
  private readonly outputCount: UnsignedColumn;
  private readonly opReturnBytes: UnsignedColumn;
  private readonly outputSats: UnsignedColumn;
  private readonly witnessBytes: UnsignedColumn;
  private readonly classifiers: ClassifierColumn[];
  private readonly rowCache = new Map<number, MempoolTransaction>();

  constructor(readonly publication: PackedPublicationTransfer) {
    const { manifest, population, membership, structure } = publication;
    this.txids = new Uint8Array(population.txids);
    this.vsize = new UnsignedColumn(population.vsize);
    this.differingWtxidBits = new Uint8Array(membership.differingWtxidBits);
    this.differingWtxidRanks = new Uint32Array(membership.differingWtxidRanks);
    this.differingWtxids = new Uint8Array(membership.differingWtxids);
    this.weight = new UnsignedColumn(membership.weight);
    this.feeSats = new UnsignedColumn(membership.feeSats);
    this.enteredAtMs = new UnsignedColumn(membership.enteredAtMs);
    this.ancestorCount = new UnsignedColumn(membership.ancestorCount);
    this.ancestorVsize = new UnsignedColumn(membership.ancestorVsize);
    this.ancestorFeeSats = new SignedColumn(membership.ancestorFeeSats);
    this.descendantCount = new UnsignedColumn(membership.descendantCount);
    this.descendantVsize = new UnsignedColumn(membership.descendantVsize);
    this.replaceableBits = new Uint8Array(membership.replaceableBits);
    this.structureBits = new Uint8Array(structure.presenceBits);
    this.structureRanks = new Uint32Array(structure.presenceRanks);
    this.inputCount = new UnsignedColumn(structure.inputCount);
    this.outputCount = new UnsignedColumn(structure.outputCount);
    this.opReturnBytes = new UnsignedColumn(structure.opReturnBytes);
    this.outputSats = new UnsignedColumn(structure.outputSats);
    this.witnessBytes = new UnsignedColumn(structure.witnessBytes);
    this.classifiers = publication.classifiers.map(
      (classifier) => new ClassifierColumn(classifier),
    );
    txidColumns.set(this, this.txids);

    const transactions = this.transactionView();
    transactionStores.set(transactions, this);
    this.snapshot = {
      source_id: manifest.source_id,
      source_label: manifest.source_label,
      collection_started_at_ms: manifest.collection_started_at_ms,
      collection_completed_at_ms: manifest.collection_completed_at_ms,
      collection_duration_ms: manifest.collection_duration_ms,
      observed_at_ms: manifest.observed_at_ms,
      classification_revision: manifest.classification_revision,
      chain_tip: manifest.chain_tip,
      transaction_count: manifest.transaction_count,
      total_vsize: manifest.total_vsize,
      classifier_catalog: manifest.classifier_catalog,
      classification_summaries: manifest.classification_summaries,
      bip110_summary: manifest.bip110_summary,
      transactions,
    };
    stores.set(this.snapshot, this);
    completeSnapshots.add(this.snapshot);
  }

  private structure(row: number): TransactionStructure | null {
    if (!bit(this.structureBits, row)) return null;
    const packedRow = this.structureRanks[row] ?? 0;
    return {
      input_count: this.inputCount.at(packedRow),
      output_count: this.outputCount.at(packedRow),
      op_return_bytes: this.opReturnBytes.at(packedRow),
      output_sats: this.outputSats.at(packedRow),
      witness_bytes: this.witnessBytes.at(packedRow),
    };
  }

  private wtxid(row: number, txid: string): string {
    if (!bit(this.differingWtxidBits, row)) return txid;
    return hashAt(this.differingWtxids, this.differingWtxidRanks[row] ?? 0);
  }

  get rowCount(): number {
    return this.publication.manifest.row_count;
  }

  rowVsize(row: number): number | undefined {
    return isValidRow(row, this.rowCount) ? this.vsize.at(row) : undefined;
  }

  rowWtxidByte(row: number, byte: number): number | undefined {
    if (!isValidRow(row, this.rowCount) || byte < 0 || byte >= 32) {
      return undefined;
    }
    return bit(this.differingWtxidBits, row)
      ? this.differingWtxids[(this.differingWtxidRanks[row] ?? 0) * 32 + byte]
      : this.txids[row * 32 + byte];
  }

  transaction(row: number): MempoolTransaction | undefined {
    if (!isValidRow(row, this.rowCount)) return undefined;
    const cached = this.rowCache.get(row);
    if (cached !== undefined) {
      this.rowCache.delete(row);
      this.rowCache.set(row, cached);
      return cached;
    }
    const txid = hashAt(this.txids, row);
    const classifications = this.classifiers
      .map((classifier) => classifier.result(row))
      .filter((result): result is ClassificationResult => result !== null);
    const policy = this.classifiers.find(
      (classifier) => classifier.transfer.classifierId === "knots_bip110",
    );
    const assessmentCode = policy?.assessmentCodes?.at(row) ?? 0;
    const transaction: MempoolTransaction = {
      txid,
      wtxid: this.wtxid(row, txid),
      vsize: this.vsize.at(row),
      weight: this.weight.at(row),
      fee_sats: this.feeSats.at(row),
      entered_at_ms: this.enteredAtMs.at(row),
      ancestor_count: this.ancestorCount.at(row),
      ancestor_vsize: this.ancestorVsize.at(row),
      ancestor_fee_sats: this.ancestorFeeSats.at(row),
      descendant_count: this.descendantCount.at(row),
      descendant_vsize: this.descendantVsize.at(row),
      replaceable: bit(this.replaceableBits, row),
      structure: this.structure(row),
      classifications,
      bip110:
        assessmentCode === 0
          ? null
          : (policy?.transfer.assessmentDictionary?.[assessmentCode - 1] ??
            null),
    };
    this.rowCache.set(row, transaction);
    if (this.rowCache.size > ROW_CACHE_LIMIT) {
      const oldest = this.rowCache.keys().next().value;
      if (oldest !== undefined) this.rowCache.delete(oldest);
    }
    return transaction;
  }

  find(txid: string): MempoolTransaction | undefined {
    const target = parseHash(txid);
    if (target === null) return undefined;
    let low = 0;
    let high = this.publication.manifest.row_count;
    while (low < high) {
      const middle = low + Math.floor((high - low) / 2);
      let order = 0;
      for (let index = 0; index < 32; index += 1) {
        const left = this.txids[middle * 32 + index] ?? 0;
        const right = target[index] ?? 0;
        if (left !== right) {
          order = left < right ? -1 : 1;
          break;
        }
      }
      if (order < 0) low = middle + 1;
      else high = middle;
    }
    const candidate = this.transaction(low);
    return candidate?.txid === txid ? candidate : undefined;
  }

  private transactionView(): MempoolTransaction[] {
    const store = this;
    const target: MempoolTransaction[] = [];
    return new Proxy(target, {
      get(array, property, receiver) {
        if (property === "length") return store.publication.manifest.row_count;
        if (property === Symbol.iterator) {
          return function* (): IterableIterator<MempoolTransaction> {
            for (
              let row = 0;
              row < store.publication.manifest.row_count;
              row += 1
            ) {
              const transaction = store.transaction(row);
              if (transaction !== undefined) yield transaction;
            }
          };
        }
        const row = isArrayIndex(property);
        return row === null
          ? Reflect.get(array, property, receiver)
          : store.transaction(row);
      },
      has(array, property) {
        const row = isArrayIndex(property);
        return row === null
          ? Reflect.has(array, property)
          : row < store.publication.manifest.row_count;
      },
      getOwnPropertyDescriptor(array, property) {
        const row = isArrayIndex(property);
        if (row !== null && row < store.publication.manifest.row_count) {
          return {
            configurable: true,
            enumerable: true,
            writable: false,
            value: store.transaction(row),
          };
        }
        return Reflect.getOwnPropertyDescriptor(array, property);
      },
    });
  }
}

export class PackedPrimaryPublicationStore {
  readonly snapshot: MempoolSnapshot;
  private readonly txids: Uint8Array;
  private readonly vsize: UnsignedColumn;
  private readonly classifiers: ClassifierColumn[];
  private readonly rowCache = new Map<number, MempoolTransaction>();

  constructor(readonly publication: PackedPrimaryPublicationTransfer) {
    this.txids = new Uint8Array(publication.population.txids);
    this.vsize = new UnsignedColumn(publication.population.vsize);
    this.classifiers = publication.classifiers.map(
      (classifier) => new ClassifierColumn(classifier),
    );
    txidColumns.set(this, this.txids);
    const manifest = publication.manifest;
    const transactions = this.transactionView();
    transactionStores.set(transactions, this);
    this.snapshot = {
      source_id: manifest.source_id,
      source_label: manifest.source_label,
      collection_started_at_ms: manifest.collection_started_at_ms,
      collection_completed_at_ms: manifest.collection_completed_at_ms,
      collection_duration_ms: manifest.collection_duration_ms,
      observed_at_ms: manifest.observed_at_ms,
      classification_revision: manifest.classification_revision,
      chain_tip: manifest.chain_tip,
      transaction_count: manifest.transaction_count,
      total_vsize: manifest.total_vsize,
      classifier_catalog: manifest.classifier_catalog,
      classification_summaries: manifest.classification_summaries,
      bip110_summary: manifest.bip110_summary,
      transactions,
    };
    stores.set(this.snapshot, this);
  }

  get rowCount(): number {
    return this.publication.manifest.row_count;
  }

  rowVsize(row: number): number | undefined {
    return isValidRow(row, this.rowCount) ? this.vsize.at(row) : undefined;
  }

  rowWtxidByte(_row: number, _byte: number): number | undefined {
    return undefined;
  }

  transaction(row: number): MempoolTransaction | undefined {
    if (!isValidRow(row, this.rowCount)) return undefined;
    const cached = this.rowCache.get(row);
    if (cached !== undefined) return cached;
    const policy = this.classifiers.find(
      ({ transfer }) => transfer.classifierId === "knots_bip110",
    );
    const assessmentCode = policy?.assessmentCodes?.at(row) ?? 0;
    const transaction = {
      txid: hashAt(this.txids, row),
      vsize: this.vsize.at(row),
      classifications: this.classifiers
        .map((classifier) => classifier.result(row))
        .filter((result): result is ClassificationResult => result !== null),
      bip110:
        assessmentCode === 0
          ? null
          : (policy?.transfer.assessmentDictionary?.[assessmentCode - 1] ??
            null),
    } as Pick<
      MempoolTransaction,
      "txid" | "vsize" | "classifications" | "bip110"
    > &
      Partial<MempoolTransaction>;
    const unavailable = (): never => {
      throw new Error("Membership stage is still loading");
    };
    for (const field of [
      "wtxid",
      "weight",
      "fee_sats",
      "entered_at_ms",
      "ancestor_count",
      "ancestor_vsize",
      "ancestor_fee_sats",
      "descendant_count",
      "descendant_vsize",
      "replaceable",
      "structure",
    ] as const) {
      Object.defineProperty(transaction, field, {
        configurable: false,
        enumerable: false,
        get: unavailable,
      });
    }
    const view = transaction as MempoolTransaction;
    this.rowCache.set(row, view);
    if (this.rowCache.size > ROW_CACHE_LIMIT) {
      const oldest = this.rowCache.keys().next().value;
      if (oldest !== undefined) this.rowCache.delete(oldest);
    }
    return view;
  }

  find(txid: string): MempoolTransaction | undefined {
    const target = parseHash(txid);
    if (target === null) return undefined;
    let low = 0;
    let high = this.publication.manifest.row_count;
    while (low < high) {
      const middle = low + Math.floor((high - low) / 2);
      let order = 0;
      for (let index = 0; index < 32; index += 1) {
        const left = this.txids[middle * 32 + index] ?? 0;
        const right = target[index] ?? 0;
        if (left !== right) {
          order = left < right ? -1 : 1;
          break;
        }
      }
      if (order < 0) low = middle + 1;
      else high = middle;
    }
    const candidate = this.transaction(low);
    return candidate?.txid === txid ? candidate : undefined;
  }

  private transactionView(): MempoolTransaction[] {
    const store = this;
    return new Proxy([] as MempoolTransaction[], {
      get(array, property, receiver) {
        if (property === "length") return store.publication.manifest.row_count;
        if (property === Symbol.iterator) {
          return function* (): IterableIterator<MempoolTransaction> {
            for (
              let row = 0;
              row < store.publication.manifest.row_count;
              row += 1
            ) {
              const transaction = store.transaction(row);
              if (transaction !== undefined) yield transaction;
            }
          };
        }
        const row = isArrayIndex(property);
        return row === null
          ? Reflect.get(array, property, receiver)
          : store.transaction(row);
      },
      has(array, property) {
        const row = isArrayIndex(property);
        return row === null
          ? Reflect.has(array, property)
          : row < store.publication.manifest.row_count;
      },
    });
  }
}

interface PackedLookup {
  readonly rowCount: number;
  find(txid: string): MempoolTransaction | undefined;
  rowVsize(row: number): number | undefined;
  rowWtxidByte(row: number, byte: number): number | undefined;
  transaction(row: number): MempoolTransaction | undefined;
}

const stores = new WeakMap<MempoolSnapshot, PackedLookup>();
const completeSnapshots = new WeakSet<MempoolSnapshot>();
const txidColumns = new WeakMap<PackedLookup, Uint8Array>();
const transactionStores = new WeakMap<
  readonly MempoolTransaction[],
  PackedLookup
>();

export const createLoadedSourcePublication = (
  publication: PackedPublicationTransfer,
): LoadedSourcePublication => {
  const store = new PackedPublicationStore(publication);
  return {
    source: publication.manifest.source,
    publication_id: publication.manifest.publication_id,
    publication: store.snapshot,
  };
};

export const createPrimarySourcePublication = (
  publication: PackedPrimaryPublicationTransfer,
): LoadedSourcePublication => {
  const store = new PackedPrimaryPublicationStore(publication);
  return {
    source: publication.manifest.source,
    publication_id: publication.manifest.publication_id,
    publication: store.snapshot,
  };
};

export const snapshotIsComplete = (snapshot: MempoolSnapshot): boolean =>
  completeSnapshots.has(snapshot);

export const packedSnapshotRowCount = (snapshot: MempoolSnapshot): number => {
  const rowCount = stores.get(snapshot)?.rowCount;
  if (
    rowCount === undefined ||
    !Number.isSafeInteger(rowCount) ||
    rowCount < 0
  ) {
    throw new TypeError("Snapshot is not a packed v2 publication");
  }
  return rowCount;
};

export const packedSnapshotRowVsize = (
  snapshot: MempoolSnapshot,
  row: number,
): number | undefined => stores.get(snapshot)?.rowVsize(row);

export const packedSnapshotTransaction = (
  snapshot: MempoolSnapshot,
  row: number,
): MempoolTransaction | undefined => stores.get(snapshot)?.transaction(row);

export const packedSnapshotRowsShareWtxid = (
  left: MempoolSnapshot,
  leftRow: number,
  right: MempoolSnapshot,
  rightRow: number,
): boolean | null => {
  if (!completeSnapshots.has(left) || !completeSnapshots.has(right))
    return null;
  const leftStore = stores.get(left);
  const rightStore = stores.get(right);
  if (leftStore === undefined || rightStore === undefined) return null;
  for (let byte = 0; byte < 32; byte += 1) {
    const leftValue = leftStore.rowWtxidByte(leftRow, byte);
    const rightValue = rightStore.rowWtxidByte(rightRow, byte);
    if (leftValue === undefined || rightValue === undefined) return null;
    if (leftValue !== rightValue) return false;
  }
  return true;
};

/**
 * Compare two rows in one packed transaction view by descending virtual size,
 * then ascending raw txid bytes. Null means the view is not backed by a packed
 * v2 publication or either row is out of range.
 */
export const comparePackedTransactionRowsByVsize = (
  transactions: readonly MempoolTransaction[],
  leftRow: number,
  rightRow: number,
): number | null => {
  const store = transactionStores.get(transactions);
  const txids = store === undefined ? undefined : txidColumns.get(store);
  if (
    store === undefined ||
    txids === undefined ||
    !isValidRow(leftRow, store.rowCount) ||
    !isValidRow(rightRow, store.rowCount)
  ) {
    return null;
  }
  const leftVsize = store.rowVsize(leftRow);
  const rightVsize = store.rowVsize(rightRow);
  if (leftVsize === undefined || rightVsize === undefined) return null;
  if (leftVsize !== rightVsize) return leftVsize > rightVsize ? -1 : 1;
  const leftOffset = leftRow * 32;
  const rightOffset = rightRow * 32;
  for (let byte = 0; byte < 32; byte += 1) {
    const left = txids[leftOffset + byte];
    const right = txids[rightOffset + byte];
    if (left === undefined || right === undefined) return null;
    if (left !== right) return left < right ? -1 : 1;
  }
  return 0;
};

/**
 * Compare two packed txids without decoding either hash or transaction row.
 * Both snapshots and rows must belong to validated packed v2 publications.
 */
export const comparePackedSnapshotRows = (
  left: MempoolSnapshot,
  leftRow: number,
  right: MempoolSnapshot,
  rightRow: number,
): -1 | 0 | 1 => {
  const leftStore = stores.get(left);
  const rightStore = stores.get(right);
  const leftTxids =
    leftStore === undefined ? undefined : txidColumns.get(leftStore);
  const rightTxids =
    rightStore === undefined ? undefined : txidColumns.get(rightStore);
  if (
    leftStore === undefined ||
    rightStore === undefined ||
    leftTxids === undefined ||
    rightTxids === undefined ||
    !isValidRow(leftRow, leftStore.rowCount) ||
    !isValidRow(rightRow, rightStore.rowCount)
  ) {
    throw new TypeError("Packed comparison row is unavailable");
  }
  const leftOffset = leftRow * 32;
  const rightOffset = rightRow * 32;
  if (
    leftOffset + 32 > leftTxids.length ||
    rightOffset + 32 > rightTxids.length
  ) {
    throw new TypeError("Packed comparison row is unavailable");
  }
  for (let byte = 0; byte < 32; byte += 1) {
    const leftValue = leftTxids[leftOffset + byte]!;
    const rightValue = rightTxids[rightOffset + byte]!;
    if (leftValue !== rightValue) return leftValue < rightValue ? -1 : 1;
  }
  return 0;
};

export const findSnapshotTransaction = (
  snapshot: MempoolSnapshot,
  txid: string,
): MempoolTransaction | undefined => {
  const store = stores.get(snapshot);
  if (store === undefined) {
    throw new TypeError("Snapshot is not a packed v2 publication");
  }
  return store.find(txid);
};
