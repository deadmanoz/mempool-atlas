import type {
  AtlasWorkerRequest,
  AtlasWorkerResponse,
  ClassifierResultTupleTransfer,
  PackedClassifierTransfer,
  PackedMembershipTransfer,
  PackedPopulationTransfer,
  PackedPrimaryPublicationTransfer,
  PackedPublicationTransfer,
  PackedSignedColumnTransfer,
  PackedStructureTransfer,
  PackedUnsignedColumnTransfer,
  WorkerQuorumTiming,
  WorkerStageTiming,
} from "./atlas-worker-protocol";
import type {
  Bip110Assessment,
  ClassifierDescriptor,
  StageDescriptor,
  StagedSnapshotManifest,
} from "./types";
import { RULE_IDS } from "./types";

const DIGEST = /^[0-9a-f]{64}$/;
const SOURCE_ID = /^[A-Za-z0-9._-]{1,64}$/;
const CLASSIFIER_ID = /^[a-z][a-z0-9_]*$/;
const MAX_SUPERSESSION_RESTARTS = 3;
const MAX_SUPPORTED_ROWS = 200_000;
const MAX_STAGE_BYTES = 64 * 1024 * 1024;
const MAX_PUBLICATION_BYTES = 192 * 1024 * 1024;
const MAX_ERROR_BODY_BYTES = 64 * 1024;
const DEFAULT_PRIMARY_CLASSIFIER_ID = "transaction_properties";

const isRecord = (value: unknown): value is Record<string, unknown> =>
  typeof value === "object" && value !== null;

const hasOnlyKeys = (
  value: Record<string, unknown>,
  expected: readonly string[],
): boolean => {
  const keys = Object.keys(value);
  return (
    keys.length === expected.length &&
    keys.every((key) => expected.includes(key))
  );
};

const isNonNegativeSafeInteger = (value: unknown): value is number =>
  typeof value === "number" && Number.isSafeInteger(value) && value >= 0;

const integer = (value: unknown, field: string): number => {
  if (!isNonNegativeSafeInteger(value)) {
    throw new TypeError(`Invalid ${field}`);
  }
  return value;
};

const string = (value: unknown, field: string): string => {
  if (typeof value !== "string" || value.length === 0) {
    throw new TypeError(`Invalid ${field}`);
  }
  return value;
};

const digest = (value: unknown, field: string): string => {
  if (typeof value !== "string" || !DIGEST.test(value)) {
    throw new TypeError(`Invalid ${field}`);
  }
  return value;
};

const stringArray = (value: unknown, field: string): string[] => {
  if (
    !Array.isArray(value) ||
    !value.every((item) => typeof item === "string")
  ) {
    throw new TypeError(`Invalid ${field}`);
  }
  return value as string[];
};

const exactStringArray = (
  value: unknown,
  expected: readonly string[],
  field: string,
): string[] => {
  const parsed = stringArray(value, field);
  if (
    parsed.length !== expected.length ||
    parsed.some((item, index) => item !== expected[index])
  ) {
    throw new TypeError(`${field} does not match its descriptor`);
  }
  return parsed;
};

const parseDescriptor = (value: unknown, rowCount: number): StageDescriptor => {
  if (!isRecord(value)) {
    throw new TypeError("Invalid stage descriptor");
  }
  const classifier = value.kind === "classifier";
  const uncompressedBytes = integer(
    value.uncompressed_bytes,
    "stage byte length",
  );
  const keys = classifier
    ? [
        "kind",
        "classifier_id",
        "content_id",
        "uncompressed_bytes",
        "row_count",
        "dependency_ids",
      ]
    : [
        "kind",
        "content_id",
        "uncompressed_bytes",
        "row_count",
        "dependency_ids",
      ];
  if (
    !hasOnlyKeys(value, keys) ||
    (value.kind !== "population" &&
      value.kind !== "membership" &&
      value.kind !== "structure" &&
      value.kind !== "classifier") ||
    integer(value.row_count, "stage row count") !== rowCount ||
    uncompressedBytes === 0 ||
    uncompressedBytes > MAX_STAGE_BYTES
  ) {
    throw new TypeError("Invalid stage descriptor");
  }
  const classifierId = classifier
    ? string(value.classifier_id, "classifier stage ID")
    : undefined;
  if (classifierId !== undefined && !CLASSIFIER_ID.test(classifierId)) {
    throw new TypeError("Invalid classifier stage ID");
  }
  const dependencyIds = stringArray(
    value.dependency_ids,
    "stage dependency IDs",
  );
  dependencyIds.forEach((id) => digest(id, "stage dependency ID"));
  return {
    kind: value.kind,
    ...(classifierId === undefined ? {} : { classifier_id: classifierId }),
    content_id: digest(value.content_id, "stage content ID"),
    uncompressed_bytes: uncompressedBytes,
    row_count: rowCount,
    dependency_ids: dependencyIds,
  };
};

const parseManifest = (value: unknown): StagedSnapshotManifest => {
  if (
    !isRecord(value) ||
    !hasOnlyKeys(value, [
      "schema_version",
      "source",
      "source_id",
      "source_label",
      "collection_started_at_ms",
      "collection_completed_at_ms",
      "collection_duration_ms",
      "observed_at_ms",
      "classification_revision",
      "chain_tip",
      "transaction_count",
      "total_vsize",
      "classifier_catalog",
      "classification_summaries",
      "bip110_summary",
      "row_count",
      "population_id",
      "classification_set_id",
      "publication_id",
      "stages",
    ]) ||
    value.schema_version !== 2 ||
    !isRecord(value.source) ||
    !isRecord(value.chain_tip) ||
    !Array.isArray(value.classifier_catalog) ||
    value.classifier_catalog.length === 0 ||
    !Array.isArray(value.classification_summaries) ||
    !isRecord(value.bip110_summary) ||
    !Array.isArray(value.stages)
  ) {
    throw new TypeError("Invalid v2 snapshot manifest");
  }
  const sourceId = string(value.source_id, "manifest source ID");
  const sourceLabel = string(value.source_label, "manifest source label");
  if (!SOURCE_ID.test(sourceId)) {
    throw new TypeError("Invalid manifest source ID");
  }
  const rowCount = integer(value.row_count, "manifest row count");
  if (rowCount > MAX_SUPPORTED_ROWS) {
    throw new TypeError("Manifest row count exceeds the supported maximum");
  }
  const transactionCount = integer(
    value.transaction_count,
    "manifest transaction count",
  );
  if (rowCount !== transactionCount) {
    throw new TypeError("Manifest row count does not match transaction count");
  }
  const catalog = value.classifier_catalog as unknown as ClassifierDescriptor[];
  const catalogIds = catalog.map((entry) => {
    if (
      !isRecord(entry) ||
      !hasOnlyKeys(entry, [
        "id",
        "version",
        "title",
        "methodology",
        "semantics",
        "required_facts",
        "labels",
      ]) ||
      !CLASSIFIER_ID.test(string(entry.id, "classifier ID")) ||
      typeof entry.version !== "string" ||
      typeof entry.title !== "string" ||
      (entry.methodology !== "exact" &&
        entry.methodology !== "heuristic" &&
        entry.methodology !== "fingerprint" &&
        entry.methodology !== "policy") ||
      (entry.semantics !== "multi_label" && entry.semantics !== "rule_set") ||
      !Array.isArray(entry.required_facts) ||
      !Array.isArray(entry.labels)
    ) {
      throw new TypeError("Invalid classifier catalog");
    }
    const labels = entry.labels as unknown[];
    const labelIds = labels.map((label) => {
      if (
        !isRecord(label) ||
        !hasOnlyKeys(label, ["key", "label", "description"]) ||
        !CLASSIFIER_ID.test(string(label.key, "classifier label key")) ||
        typeof label.label !== "string" ||
        typeof label.description !== "string"
      ) {
        throw new TypeError("Invalid classifier label catalog");
      }
      return label.key;
    });
    if (new Set(labelIds).size !== labelIds.length) {
      throw new TypeError("Classifier label catalog repeats a key");
    }
    return entry.id;
  });
  if (new Set(catalogIds).size !== catalogIds.length) {
    throw new TypeError("Classifier catalog repeats an ID");
  }
  const descriptors = value.stages.map((entry) =>
    parseDescriptor(entry, rowCount),
  );
  const publicationBytes = descriptors.reduce(
    (total, descriptor) => total + descriptor.uncompressed_bytes,
    0,
  );
  if (
    !Number.isSafeInteger(publicationBytes) ||
    publicationBytes > MAX_PUBLICATION_BYTES
  ) {
    throw new TypeError("Manifest publication exceeds the supported maximum");
  }
  const expectedKinds = ["population", "membership", "structure"];
  if (
    descriptors.length !== catalog.length + 3 ||
    expectedKinds.some((kind, index) => descriptors[index]?.kind !== kind) ||
    catalogIds.some((id, index) => {
      const descriptor = descriptors[index + 3];
      return (
        descriptor?.kind !== "classifier" || descriptor.classifier_id !== id
      );
    })
  ) {
    throw new TypeError("Manifest stage order does not match its catalog");
  }
  const populationId = digest(value.population_id, "population ID");
  const classificationSetId = digest(
    value.classification_set_id,
    "classification set ID",
  );
  if (
    descriptors[0]?.content_id !== populationId ||
    descriptors[0].dependency_ids.length !== 0 ||
    descriptors[1]?.dependency_ids[0] !== populationId ||
    descriptors[1]?.dependency_ids.length !== 1 ||
    descriptors[2]?.dependency_ids[0] !== populationId ||
    descriptors[2]?.dependency_ids[1] !== classificationSetId ||
    descriptors[2]?.dependency_ids.length !== 2 ||
    descriptors
      .slice(3)
      .some(
        (descriptor) =>
          descriptor.dependency_ids.length !== 1 ||
          descriptor.dependency_ids[0] !== populationId,
      )
  ) {
    throw new TypeError("Manifest contains an invalid dependency graph");
  }
  if (
    value.source.availability !== "ready" &&
    value.source.availability !== "stale"
  ) {
    throw new TypeError("Published manifest has no available snapshot");
  }
  if (
    value.source.source_id !== sourceId ||
    value.source.source_label !== sourceLabel ||
    value.source.snapshot_observed_at_ms !== value.observed_at_ms ||
    value.source.transaction_count !== transactionCount ||
    value.source.total_vsize !== value.total_vsize
  ) {
    throw new TypeError("Manifest source summary does not match its snapshot");
  }
  integer(value.collection_started_at_ms, "collection start");
  integer(value.collection_completed_at_ms, "collection completion");
  integer(value.collection_duration_ms, "collection duration");
  integer(value.observed_at_ms, "observation time");
  integer(value.classification_revision, "classification revision");
  integer(value.chain_tip.height, "chain height");
  digest(value.chain_tip.hash, "chain hash");
  integer(value.total_vsize, "total vsize");
  const sourceChainTip = value.source.chain_tip;
  const sourceClassification = value.source.classification;
  const bip110Summary = value.bip110_summary;
  if (
    !isRecord(sourceChainTip) ||
    sourceChainTip.height !== value.chain_tip.height ||
    sourceChainTip.hash !== value.chain_tip.hash ||
    !isRecord(sourceClassification) ||
    (sourceClassification.state !== "classifying" &&
      sourceClassification.state !== "complete" &&
      sourceClassification.state !== "paused") ||
    sourceClassification.revision !== value.classification_revision ||
    !hasOnlyKeys(bip110Summary, [
      "evaluator_id",
      "evaluator_version",
      "scope",
      "compatible_count",
      "violating_count",
      "indeterminate_count",
      "unclassified_count",
    ]) ||
    typeof bip110Summary.evaluator_id !== "string" ||
    bip110Summary.evaluator_id.length === 0 ||
    typeof bip110Summary.evaluator_version !== "string" ||
    bip110Summary.evaluator_version.length === 0 ||
    bip110Summary.scope !== "knots_mempool_policy" ||
    !Array.isArray(value.classification_summaries) ||
    value.classification_summaries.length !== catalog.length
  ) {
    throw new TypeError("Manifest classification metadata is inconsistent");
  }
  const compatibleCount = integer(
    bip110Summary.compatible_count,
    "compatible count",
  );
  const violatingCount = integer(
    bip110Summary.violating_count,
    "violating count",
  );
  const indeterminateCount = integer(
    bip110Summary.indeterminate_count,
    "indeterminate count",
  );
  const unclassifiedCount = integer(
    bip110Summary.unclassified_count,
    "BIP-110 unclassified count",
  );
  const classifiedCount = integer(
    sourceClassification.classified_count,
    "classified count",
  );
  if (
    classifiedCount !== compatibleCount + violatingCount + indeterminateCount ||
    integer(sourceClassification.unclassified_count, "unclassified count") !==
      unclassifiedCount ||
    classifiedCount + unclassifiedCount !== transactionCount
  ) {
    throw new TypeError("Manifest classification counts are inconsistent");
  }
  value.classification_summaries.forEach((summary, index) => {
    const descriptor = catalog[index];
    if (
      descriptor === undefined ||
      !isRecord(summary) ||
      !hasOnlyKeys(summary, [
        "classifier_id",
        "complete_count",
        "partial_count",
        "unclassified_count",
        "label_counts",
      ]) ||
      summary.classifier_id !== descriptor.id ||
      !isRecord(summary.label_counts)
    ) {
      throw new TypeError("Invalid classifier summary");
    }
    const complete = integer(summary.complete_count, "complete count");
    const partial = integer(summary.partial_count, "partial count");
    const unavailable = integer(
      summary.unclassified_count,
      "classifier unclassified count",
    );
    const labelCounts = summary.label_counts as Record<string, unknown>;
    const declaredLabels = descriptor.labels.map(({ key }) => key);
    if (
      complete + partial + unavailable !== transactionCount ||
      Object.keys(labelCounts).length !== declaredLabels.length ||
      declaredLabels.some(
        (label) =>
          !Object.hasOwn(labelCounts, label) ||
          !isNonNegativeSafeInteger(labelCounts[label]),
      )
    ) {
      throw new TypeError("Classifier summary is inconsistent");
    }
  });
  return {
    ...(value as unknown as StagedSnapshotManifest),
    classifier_catalog: catalog,
    stages: descriptors,
    population_id: populationId,
    classification_set_id: classificationSetId,
    publication_id: digest(value.publication_id, "publication ID"),
  };
};

const bytesToHex = (bytes: Uint8Array): string =>
  Array.from(bytes, (value) => value.toString(16).padStart(2, "0")).join("");

const hexToBytes = (value: string): Uint8Array => {
  digest(value, "digest");
  const result = new Uint8Array(32);
  for (let index = 0; index < result.length; index += 1) {
    result[index] = Number.parseInt(value.slice(index * 2, index * 2 + 2), 16);
  }
  return result;
};

const sha256 = async (bytes: Uint8Array): Promise<string> =>
  bytesToHex(
    new Uint8Array(
      await crypto.subtle.digest("SHA-256", Uint8Array.from(bytes).buffer),
    ),
  );

class Preimage {
  private readonly parts: Uint8Array[] = [];

  raw(value: Uint8Array): void {
    this.parts.push(value);
  }

  byte(value: number): void {
    this.raw(Uint8Array.of(value));
  }

  u64(value: number): void {
    if (!Number.isSafeInteger(value) || value < 0) {
      throw new TypeError("Publication integer is outside the safe range");
    }
    const bytes = new Uint8Array(8);
    new DataView(bytes.buffer).setBigUint64(0, BigInt(value), true);
    this.raw(bytes);
  }

  text(value: string): void {
    const bytes = new TextEncoder().encode(value);
    this.u64(bytes.byteLength);
    this.raw(bytes);
  }

  optionalText(value: string | null): void {
    this.byte(value === null ? 0 : 1);
    if (value !== null) this.text(value);
  }

  optionalU64(value: number | null): void {
    this.byte(value === null ? 0 : 1);
    if (value !== null) this.u64(value);
  }

  json(value: unknown): void {
    const bytes = new TextEncoder().encode(JSON.stringify(value));
    this.u64(bytes.byteLength);
    this.raw(bytes);
  }

  finish(): Uint8Array {
    const length = this.parts.reduce((sum, part) => sum + part.byteLength, 0);
    const output = new Uint8Array(length);
    let offset = 0;
    for (const part of this.parts) {
      output.set(part, offset);
      offset += part.byteLength;
    }
    return output;
  }
}

const classificationSetId = async (
  manifest: StagedSnapshotManifest,
): Promise<string> => {
  const preimage = new Preimage();
  preimage.raw(
    new TextEncoder().encode("mempool-atlas/v2/classification-set\0"),
  );
  preimage.u64(manifest.classifier_catalog.length);
  manifest.classifier_catalog.forEach((classifier, index) => {
    const descriptor = manifest.stages[index + 3];
    if (descriptor === undefined)
      throw new TypeError("Missing classifier stage");
    preimage.text(classifier.id);
    preimage.text(classifier.version);
    preimage.raw(hexToBytes(descriptor.content_id));
  });
  return sha256(preimage.finish());
};

const publicationId = async (
  manifest: StagedSnapshotManifest,
): Promise<string> => {
  const source = manifest.source;
  const preimage = new Preimage();
  preimage.raw(new TextEncoder().encode("mempool-atlas/v2/publication\0"));
  preimage.text(source.source_id);
  preimage.text(source.source_label);
  preimage.byte(
    source.availability === "waiting"
      ? 0
      : source.availability === "ready"
        ? 1
        : source.availability === "stale"
          ? 2
          : 3,
  );
  preimage.u64(source.poll_interval_seconds);
  preimage.optionalU64(source.last_poll_started_at_ms);
  preimage.optionalU64(source.snapshot_observed_at_ms);
  preimage.byte(source.chain_tip === null ? 0 : 1);
  if (source.chain_tip !== null) {
    preimage.u64(source.chain_tip.height);
    preimage.text(source.chain_tip.hash);
  }
  preimage.optionalU64(source.transaction_count);
  preimage.optionalU64(source.total_vsize);
  preimage.byte(source.classification === null ? 0 : 1);
  if (source.classification !== null) {
    preimage.byte(
      source.classification.state === "classifying"
        ? 0
        : source.classification.state === "complete"
          ? 1
          : 2,
    );
    preimage.u64(source.classification.revision);
    preimage.u64(source.classification.classified_count);
    preimage.u64(source.classification.unclassified_count);
  }
  preimage.optionalText(source.last_error);
  preimage.text(manifest.source_id);
  preimage.text(manifest.source_label);
  [
    manifest.collection_started_at_ms,
    manifest.collection_completed_at_ms,
    manifest.collection_duration_ms,
    manifest.observed_at_ms,
    manifest.classification_revision,
    manifest.chain_tip.height,
    manifest.transaction_count,
    manifest.total_vsize,
  ].forEach((value) => preimage.u64(value));
  preimage.text(manifest.chain_tip.hash);
  preimage.json(manifest.classifier_catalog);
  preimage.json(manifest.classification_summaries);
  preimage.json(manifest.bip110_summary);
  preimage.raw(hexToBytes(manifest.classification_set_id));
  preimage.u64(manifest.stages.length);
  manifest.stages.forEach((descriptor) => {
    preimage.byte(
      descriptor.kind === "population"
        ? 1
        : descriptor.kind === "membership"
          ? 2
          : descriptor.kind === "structure"
            ? 3
            : 4,
    );
    preimage.optionalText(descriptor.classifier_id ?? null);
    preimage.raw(hexToBytes(descriptor.content_id));
    preimage.u64(descriptor.uncompressed_bytes);
    preimage.u64(descriptor.row_count);
    preimage.u64(descriptor.dependency_ids.length);
    descriptor.dependency_ids.forEach((id) => preimage.raw(hexToBytes(id)));
  });
  return sha256(preimage.finish());
};

const validateManifestRoots = async (
  manifest: StagedSnapshotManifest,
): Promise<void> => {
  if (
    (await classificationSetId(manifest)) !== manifest.classification_set_id
  ) {
    throw new TypeError("Classification-set digest mismatch");
  }
  if ((await publicationId(manifest)) !== manifest.publication_id) {
    throw new TypeError("Publication digest mismatch");
  }
};

const decodeBase64 = (value: unknown, field: string): Uint8Array => {
  if (typeof value !== "string") throw new TypeError(`Invalid ${field}`);
  let binary: string;
  try {
    binary = atob(value);
  } catch {
    throw new TypeError(`Invalid ${field}`);
  }
  if (btoa(binary) !== value) throw new TypeError(`Non-canonical ${field}`);
  return Uint8Array.from(binary, (character) => character.charCodeAt(0));
};

const asBuffer = (value: Uint8Array): ArrayBuffer =>
  value.buffer.slice(
    value.byteOffset,
    value.byteOffset + value.byteLength,
  ) as ArrayBuffer;

const readPackedInteger = (
  bytes: Uint8Array,
  width: number,
  row: number,
): bigint => {
  let value = 0n;
  const offset = row * width;
  for (let index = width - 1; index >= 0; index -= 1) {
    value = (value << 8n) | BigInt(bytes[offset + index] ?? 0);
  }
  return value;
};

const readUnsigned = (
  bytes: Uint8Array,
  width: number,
  row: number,
): number => {
  const value = readPackedInteger(bytes, width, row);
  const number = Number(value);
  if (!Number.isSafeInteger(number))
    throw new TypeError("Packed integer is unsafe");
  return number;
};

const unsignedColumn = (
  value: unknown,
  rows: number,
  field: string,
): PackedUnsignedColumnTransfer => {
  if (
    !isRecord(value) ||
    !hasOnlyKeys(value, ["width_bytes", "values_base64"])
  ) {
    throw new TypeError(`Invalid ${field}`);
  }
  const width = integer(value.width_bytes, `${field} width`);
  if (width < 1 || width > 8) throw new TypeError(`Invalid ${field} width`);
  const bytes = decodeBase64(value.values_base64, `${field} values`);
  if (bytes.byteLength !== rows * width) {
    throw new TypeError(`${field} has the wrong packed length`);
  }
  for (let row = 0; row < rows; row += 1) readUnsigned(bytes, width, row);
  return { width, values: asBuffer(bytes) };
};

const signedColumn = (
  value: unknown,
  rows: number,
  field: string,
): PackedSignedColumnTransfer => {
  if (
    !isRecord(value) ||
    !hasOnlyKeys(value, ["width_bytes", "values_base64"])
  ) {
    throw new TypeError(`Invalid ${field}`);
  }
  const width = integer(value.width_bytes, `${field} width`);
  if (width < 1 || width > 8) throw new TypeError(`Invalid ${field} width`);
  const bytes = decodeBase64(value.values_base64, `${field} values`);
  if (bytes.byteLength !== rows * width) {
    throw new TypeError(`${field} has the wrong packed length`);
  }
  for (let row = 0; row < rows; row += 1) {
    const unsigned = readPackedInteger(bytes, width, row);
    const bits = BigInt(width * 8);
    const signed =
      unsigned & (1n << (bits - 1n)) ? unsigned - (1n << bits) : unsigned;
    if (!Number.isSafeInteger(Number(signed))) {
      throw new TypeError(`${field} contains an unsafe integer`);
    }
  }
  return { width, values: asBuffer(bytes) };
};

const bitset = (value: unknown, rows: number, field: string): Uint8Array => {
  const bytes = decodeBase64(value, field);
  if (bytes.byteLength !== Math.ceil(rows / 8)) {
    throw new TypeError(`${field} has the wrong packed length`);
  }
  const remainder = rows % 8;
  if (remainder !== 0 && (bytes.at(-1) ?? 0) >> remainder !== 0) {
    throw new TypeError(`${field} has non-zero padding`);
  }
  return bytes;
};

const isSet = (bytes: Uint8Array, row: number): boolean =>
  ((bytes[row >> 3] ?? 0) & (1 << (row & 7))) !== 0;

const ranks = (bits: Uint8Array, rows: number): Uint32Array => {
  const result = new Uint32Array(rows + 1);
  let count = 0;
  for (let row = 0; row < rows; row += 1) {
    result[row] = count;
    if (((bits[row >> 3] ?? 0) & (1 << (row & 7))) !== 0) count += 1;
  }
  result[rows] = count;
  return result;
};

const commonStage = (
  value: unknown,
  descriptor: StageDescriptor,
): Record<string, unknown> => {
  if (!isRecord(value)) throw new TypeError(`Invalid ${descriptor.kind} stage`);
  if (
    value.schema_version !== 2 ||
    value.kind !== descriptor.kind ||
    value.row_count !== descriptor.row_count
  ) {
    throw new TypeError(`${descriptor.kind} stage header mismatch`);
  }
  exactStringArray(
    value.dependency_ids,
    descriptor.dependency_ids,
    `${descriptor.kind} dependencies`,
  );
  return value;
};

const populationStage = (
  value: unknown,
  descriptor: StageDescriptor,
): PackedPopulationTransfer => {
  const body = commonStage(value, descriptor);
  const txids = decodeBase64(body.txids_base64, "population txids");
  if (txids.byteLength !== descriptor.row_count * 32) {
    throw new TypeError("Population txids have the wrong packed length");
  }
  for (let row = 1; row < descriptor.row_count; row += 1) {
    let order = 0;
    for (let index = 0; index < 32; index += 1) {
      const left = txids[(row - 1) * 32 + index] ?? 0;
      const right = txids[row * 32 + index] ?? 0;
      if (left !== right) {
        order = left < right ? -1 : 1;
        break;
      }
    }
    if (order >= 0)
      throw new TypeError("Population txids are not strictly ordered");
  }
  return {
    contentId: descriptor.content_id,
    txids: asBuffer(txids),
    vsize: unsignedColumn(body.vsize, descriptor.row_count, "vsize"),
  };
};

const membershipStage = (
  value: unknown,
  descriptor: StageDescriptor,
  populationId: string,
): PackedMembershipTransfer => {
  const body = commonStage(value, descriptor);
  if (body.population_id !== populationId) {
    throw new TypeError("Membership population dependency mismatch");
  }
  const bits = bitset(
    body.differing_wtxid_bitset_base64,
    descriptor.row_count,
    "differing-wtxid bitset",
  );
  const rank = ranks(bits, descriptor.row_count);
  const wtxids = decodeBase64(body.differing_wtxids_base64, "differing wtxids");
  if (wtxids.byteLength !== (rank.at(-1) ?? 0) * 32) {
    throw new TypeError("Differing wtxids do not match their presence bitset");
  }
  const replaceable = bitset(
    body.replaceable_bitset_base64,
    descriptor.row_count,
    "replaceable bitset",
  );
  return {
    contentId: descriptor.content_id,
    differingWtxidBits: asBuffer(bits),
    differingWtxidRanks: asBuffer(new Uint8Array(rank.buffer)),
    differingWtxids: asBuffer(wtxids),
    weight: unsignedColumn(body.weight, descriptor.row_count, "weight"),
    feeSats: unsignedColumn(body.fee_sats, descriptor.row_count, "fee_sats"),
    enteredAtMs: unsignedColumn(
      body.entered_at_ms,
      descriptor.row_count,
      "entered_at_ms",
    ),
    ancestorCount: unsignedColumn(
      body.ancestor_count,
      descriptor.row_count,
      "ancestor_count",
    ),
    ancestorVsize: unsignedColumn(
      body.ancestor_vsize,
      descriptor.row_count,
      "ancestor_vsize",
    ),
    ancestorFeeSats: signedColumn(
      body.ancestor_fee_sats,
      descriptor.row_count,
      "ancestor_fee_sats",
    ),
    descendantCount: unsignedColumn(
      body.descendant_count,
      descriptor.row_count,
      "descendant_count",
    ),
    descendantVsize: unsignedColumn(
      body.descendant_vsize,
      descriptor.row_count,
      "descendant_vsize",
    ),
    replaceableBits: asBuffer(replaceable),
  };
};

const structureStage = (
  value: unknown,
  descriptor: StageDescriptor,
  manifest: StagedSnapshotManifest,
): PackedStructureTransfer => {
  const body = commonStage(value, descriptor);
  if (
    body.population_id !== manifest.population_id ||
    body.classification_set_id !== manifest.classification_set_id
  ) {
    throw new TypeError("Structure dependency mismatch");
  }
  const bits = bitset(
    body.presence_bitset_base64,
    descriptor.row_count,
    "structure presence bitset",
  );
  const rank = ranks(bits, descriptor.row_count);
  const rows = rank.at(-1) ?? 0;
  return {
    contentId: descriptor.content_id,
    presenceBits: asBuffer(bits),
    presenceRanks: asBuffer(new Uint8Array(rank.buffer)),
    inputCount: unsignedColumn(body.input_count, rows, "input_count"),
    outputCount: unsignedColumn(body.output_count, rows, "output_count"),
    opReturnBytes: unsignedColumn(
      body.op_return_bytes,
      rows,
      "op_return_bytes",
    ),
    outputSats: unsignedColumn(body.output_sats, rows, "output_sats"),
    witnessBytes: unsignedColumn(body.witness_bytes, rows, "witness_bytes"),
  };
};

const resultTuple = (value: unknown): ClassifierResultTupleTransfer => {
  if (
    !isRecord(value) ||
    !hasOnlyKeys(value, [
      "state",
      "primary_label",
      "labels",
      "missing_facts",
    ]) ||
    (value.state !== "complete" && value.state !== "partial") ||
    (value.primary_label !== null && typeof value.primary_label !== "string")
  ) {
    throw new TypeError("Invalid classifier result dictionary entry");
  }
  const labels = stringArray(value.labels, "classifier labels");
  const missingFacts = stringArray(
    value.missing_facts,
    "classifier missing facts",
  );
  if (
    (value.state === "complete" && labels.length === 0) ||
    labels.some((label) => !CLASSIFIER_ID.test(label)) ||
    missingFacts.some((fact) => !CLASSIFIER_ID.test(fact)) ||
    new Set(labels).size !== labels.length ||
    new Set(missingFacts).size !== missingFacts.length ||
    (value.state === "complete" && missingFacts.length !== 0) ||
    (value.state === "partial" && missingFacts.length === 0) ||
    (value.primary_label !== null && !labels.includes(value.primary_label))
  ) {
    throw new TypeError("Inconsistent classifier result dictionary entry");
  }
  return {
    state: value.state,
    primary_label: value.primary_label,
    labels,
    missing_facts: missingFacts,
  };
};

const assessment = (value: unknown): Bip110Assessment => {
  if (
    !isRecord(value) ||
    !hasOnlyKeys(value, [
      "status",
      "primary_rule",
      "violated_rules",
      "unknown_rules",
    ]) ||
    (value.status !== "compatible" &&
      value.status !== "violating" &&
      value.status !== "indeterminate")
  ) {
    throw new TypeError("Invalid BIP-110 assessment dictionary entry");
  }
  if (
    value.primary_rule !== null &&
    (typeof value.primary_rule !== "string" ||
      !(RULE_IDS as readonly string[]).includes(value.primary_rule))
  ) {
    throw new TypeError("Invalid BIP-110 primary rule");
  }
  const violated = stringArray(value.violated_rules, "violated rules");
  const unknown = stringArray(value.unknown_rules, "unknown rules");
  if (
    violated.some((rule) => !(RULE_IDS as readonly string[]).includes(rule)) ||
    unknown.some((rule) => !(RULE_IDS as readonly string[]).includes(rule)) ||
    new Set(violated).size !== violated.length ||
    new Set(unknown).size !== unknown.length ||
    (value.status === "compatible" &&
      (value.primary_rule !== null ||
        violated.length !== 0 ||
        unknown.length !== 0)) ||
    (value.status === "violating" &&
      (violated.length === 0 ||
        (value.primary_rule !== null &&
          !violated.includes(value.primary_rule)))) ||
    (value.status === "indeterminate" &&
      (value.primary_rule !== null ||
        violated.length !== 0 ||
        unknown.length === 0))
  ) {
    throw new TypeError("Inconsistent BIP-110 assessment dictionary entry");
  }
  return value as unknown as Bip110Assessment;
};

const validatePolicyResult = (
  result: ClassifierResultTupleTransfer,
  value: Bip110Assessment,
): void => {
  const partial = value.unknown_rules.length > 0;
  if (
    result.state !== (partial ? "partial" : "complete") ||
    result.primary_label !== value.status ||
    result.labels.length !== 1 ||
    result.labels[0] !== value.status ||
    result.missing_facts.length !== (partial ? 1 : 0) ||
    (partial && result.missing_facts[0] !== "policy_facts")
  ) {
    throw new TypeError("BIP-110 result does not match its assessment");
  }
};

const validateCodes = (
  column: PackedUnsignedColumnTransfer,
  rows: number,
  dictionaryLength: number,
  field: string,
): void => {
  const bytes = new Uint8Array(column.values);
  for (let row = 0; row < rows; row += 1) {
    if (readUnsigned(bytes, column.width, row) > dictionaryLength) {
      throw new TypeError(`${field} references a missing dictionary entry`);
    }
  }
};

const classifierStage = (
  value: unknown,
  descriptor: StageDescriptor,
  manifest: StagedSnapshotManifest,
): PackedClassifierTransfer => {
  const body = commonStage(value, descriptor);
  if (
    body.population_id !== manifest.population_id ||
    body.classifier_id !== descriptor.classifier_id ||
    !Array.isArray(body.result_dictionary)
  ) {
    throw new TypeError("Classifier stage dependency or identity mismatch");
  }
  const dictionary = body.result_dictionary.map(resultTuple);
  const resultCodes = unsignedColumn(
    body.result_codes,
    descriptor.row_count,
    "classifier result codes",
  );
  validateCodes(
    resultCodes,
    descriptor.row_count,
    dictionary.length,
    "classifier result codes",
  );
  let assessmentDictionary: Bip110Assessment[] | null = null;
  let assessmentCodes: PackedUnsignedColumnTransfer | null = null;
  if (descriptor.classifier_id === "knots_bip110") {
    if (
      !isRecord(body.bip110) ||
      !hasOnlyKeys(body.bip110, [
        "assessment_dictionary",
        "assessment_codes",
      ]) ||
      !Array.isArray(body.bip110.assessment_dictionary)
    ) {
      throw new TypeError("BIP-110 classifier is missing assessment columns");
    }
    assessmentDictionary = body.bip110.assessment_dictionary.map(assessment);
    assessmentCodes = unsignedColumn(
      body.bip110.assessment_codes,
      descriptor.row_count,
      "assessment codes",
    );
    validateCodes(
      assessmentCodes,
      descriptor.row_count,
      assessmentDictionary.length,
      "assessment codes",
    );
  } else if (body.bip110 !== undefined) {
    throw new TypeError("Non-policy classifier contains BIP-110 columns");
  }
  return {
    contentId: descriptor.content_id,
    classifierId: descriptor.classifier_id ?? "",
    resultDictionary: dictionary,
    resultCodes,
    assessmentDictionary,
    assessmentCodes,
  };
};

class SupersededStageError extends Error {}

class WorkerHttpError extends Error {
  constructor(
    readonly status: number,
    message: string,
  ) {
    super(message);
  }
}

const wait = (milliseconds: number, signal: AbortSignal): Promise<void> =>
  new Promise((resolve, reject) => {
    if (signal.aborted) {
      reject(new DOMException("Aborted", "AbortError"));
      return;
    }
    const abort = (): void => {
      clearTimeout(timer);
      reject(new DOMException("Aborted", "AbortError"));
    };
    const timer = setTimeout(() => {
      signal.removeEventListener("abort", abort);
      resolve();
    }, milliseconds);
    signal.addEventListener("abort", abort, { once: true });
  });

const retryDelay = (response: Response, attempt: number): number => {
  const header = response.headers.get("Retry-After");
  if (header !== null) {
    const seconds = Number(header);
    if (Number.isFinite(seconds) && seconds >= 0) {
      return Math.min(5_000, seconds * 1_000);
    }
    const date = Date.parse(header);
    if (Number.isFinite(date)) {
      return Math.min(5_000, Math.max(0, date - Date.now()));
    }
  }
  return 250 * 2 ** attempt;
};

const readBoundedResponse = async (
  response: Response,
  maximumBytes: number,
  exactBytes: number | null,
): Promise<Uint8Array> => {
  if (response.body === null) {
    throw new TypeError("Atlas response has no readable body");
  }
  const reader = response.body.getReader();
  const chunks: Uint8Array[] = [];
  let length = 0;
  try {
    while (true) {
      const { done, value } = await reader.read();
      if (done) break;
      length += value.byteLength;
      if (
        length > maximumBytes ||
        (exactBytes !== null && length > exactBytes)
      ) {
        await reader.cancel("Atlas response exceeded its declared limit");
        throw new TypeError("Atlas response exceeded its declared limit");
      }
      chunks.push(value);
    }
  } finally {
    reader.releaseLock();
  }
  if (exactBytes !== null && length !== exactBytes) {
    throw new TypeError("Atlas response length does not match its descriptor");
  }
  const bytes = new Uint8Array(length);
  let offset = 0;
  for (const chunk of chunks) {
    bytes.set(chunk, offset);
    offset += chunk.byteLength;
  }
  return bytes;
};

const fetchBytes = async (
  path: string,
  signal: AbortSignal,
  maximumBytes: number,
  exactBytes: number | null = null,
): Promise<Uint8Array> => {
  for (let attempt = 0; attempt < 4; attempt += 1) {
    const response = await fetch(path, {
      signal,
      headers: { Accept: "application/json" },
    });
    if (response.status === 409) throw new SupersededStageError();
    if (response.status === 429) {
      if (attempt < 3) {
        await wait(retryDelay(response, attempt), signal);
        continue;
      }
      throw new WorkerHttpError(
        429,
        "Atlas rate limit retries exhausted after 4 attempts",
      );
    }
    if (!response.ok) {
      let detail = "";
      try {
        const errorBytes = await readBoundedResponse(
          response,
          MAX_ERROR_BODY_BYTES,
          null,
        );
        const body: unknown = JSON.parse(new TextDecoder().decode(errorBytes));
        if (isRecord(body)) {
          const message = body.title ?? body.error;
          if (typeof message === "string") detail = `: ${message}`;
        }
      } catch {
        // Status and route remain enough to diagnose a malformed error body.
      }
      throw new WorkerHttpError(
        response.status,
        `Atlas request failed (${response.status})${detail}`,
      );
    }
    return readBoundedResponse(response, maximumBytes, exactBytes);
  }
  throw new WorkerHttpError(429, "Atlas rate limit retry loop exhausted");
};

const fetchManifest = async (
  sourceId: string,
  signal: AbortSignal,
): Promise<StagedSnapshotManifest> => {
  const bytes = await fetchBytes(
    `/api/v2/sources/${encodeURIComponent(sourceId)}/mempool`,
    signal,
    MAX_STAGE_BYTES,
  );
  let value: unknown;
  try {
    value = JSON.parse(new TextDecoder().decode(bytes));
  } catch {
    throw new TypeError("Malformed v2 snapshot manifest JSON");
  }
  const manifest = parseManifest(value);
  if (manifest.source_id !== sourceId) {
    throw new TypeError("Manifest identifies a different source");
  }
  await validateManifestRoots(manifest);
  return manifest;
};

const stagePath = (sourceId: string, descriptor: StageDescriptor): string => {
  const base = `/api/v2/sources/${encodeURIComponent(sourceId)}/mempool/stages`;
  return descriptor.kind === "classifier"
    ? `${base}/classifier/${encodeURIComponent(descriptor.classifier_id ?? "")}/${descriptor.content_id}`
    : `${base}/${descriptor.kind}/${descriptor.content_id}`;
};

const fetchStage = async (
  sourceId: string,
  descriptor: StageDescriptor,
  signal: AbortSignal,
): Promise<unknown> => {
  const bytes = await fetchBytes(
    stagePath(sourceId, descriptor),
    signal,
    descriptor.uncompressed_bytes,
    descriptor.uncompressed_bytes,
  );
  if (
    bytes.byteLength !== descriptor.uncompressed_bytes ||
    (await sha256(bytes)) !== descriptor.content_id
  ) {
    throw new TypeError(`${descriptor.kind} stage content digest mismatch`);
  }
  try {
    return JSON.parse(new TextDecoder().decode(bytes));
  } catch {
    throw new TypeError(`Malformed ${descriptor.kind} stage JSON`);
  }
};

type DecodedStage =
  | PackedPopulationTransfer
  | PackedMembershipTransfer
  | PackedStructureTransfer
  | PackedClassifierTransfer;

const decodeStage = (
  value: unknown,
  descriptor: StageDescriptor,
  manifest: StagedSnapshotManifest,
): DecodedStage => {
  switch (descriptor.kind) {
    case "population":
      return populationStage(value, descriptor);
    case "membership":
      return membershipStage(value, descriptor, manifest.population_id);
    case "structure":
      return structureStage(value, descriptor, manifest);
    case "classifier":
      return classifierStage(value, descriptor, manifest);
  }
};

interface TimedStage {
  decoded: DecodedStage;
  timing: WorkerStageTiming;
}

interface CachedStage {
  decoded: DecodedStage;
}

const descriptorKey = (descriptor: StageDescriptor): string =>
  JSON.stringify([
    descriptor.kind,
    descriptor.classifier_id ?? null,
    descriptor.content_id,
    descriptor.uncompressed_bytes,
    descriptor.row_count,
    descriptor.dependency_ids,
  ]);

const loadStage = async (
  sourceId: string,
  descriptor: StageDescriptor,
  manifest: StagedSnapshotManifest,
  signal: AbortSignal,
): Promise<TimedStage> => {
  const fetchStarted = performance.now();
  const value = await fetchStage(sourceId, descriptor, signal);
  const fetched = performance.now();
  const decoded = decodeStage(value, descriptor, manifest);
  const finished = performance.now();
  return {
    decoded,
    timing: {
      kind: descriptor.kind,
      classifierId: descriptor.classifier_id ?? null,
      reused: false,
      fetchDigestParseMs: fetched - fetchStarted,
      decodeValidatePackMs: finished - fetched,
    },
  };
};

const loadCachedStage = async (
  sourceId: string,
  descriptor: StageDescriptor,
  manifest: StagedSnapshotManifest,
  signal: AbortSignal,
  cache: Map<string, CachedStage>,
): Promise<TimedStage> => {
  const key = descriptorKey(descriptor);
  const cached = cache.get(key);
  if (cached !== undefined) {
    return {
      decoded: cached.decoded,
      timing: {
        kind: descriptor.kind,
        classifierId: descriptor.classifier_id ?? null,
        reused: true,
        fetchDigestParseMs: 0,
        decodeValidatePackMs: 0,
      },
    };
  }
  const loaded = await loadStage(sourceId, descriptor, manifest, signal);
  cache.set(key, { decoded: loaded.decoded });
  return loaded;
};

const loadStageSet = async (
  sourceId: string,
  descriptors: readonly StageDescriptor[],
  manifest: StagedSnapshotManifest,
  parentSignal: AbortSignal,
  cache: Map<string, CachedStage>,
): Promise<TimedStage[]> => {
  const controller = new AbortController();
  const abort = (): void => controller.abort();
  if (parentSignal.aborted) controller.abort();
  else parentSignal.addEventListener("abort", abort, { once: true });
  const requests = descriptors.map((descriptor) =>
    loadCachedStage(sourceId, descriptor, manifest, controller.signal, cache),
  );
  try {
    return await Promise.all(requests);
  } catch (error) {
    if (!(error instanceof SupersededStageError)) controller.abort();
    await Promise.allSettled(requests);
    throw error;
  } finally {
    parentSignal.removeEventListener("abort", abort);
  }
};

const retainCachedDescriptors = (
  cache: Map<string, CachedStage>,
  descriptors: readonly StageDescriptor[],
): void => {
  const retained = new Set(descriptors.map(descriptorKey));
  for (const key of cache.keys()) {
    if (!retained.has(key)) cache.delete(key);
  }
};

const columnValue = (
  column: PackedUnsignedColumnTransfer,
  row: number,
): number => readUnsigned(new Uint8Array(column.values), column.width, row);

const validatePrimaryPublicationSemantics = (
  publication: PackedPrimaryPublicationTransfer,
): void => {
  const { manifest, population, classifiers } = publication;
  const classifier = classifiers[0];
  const classifierIndex = manifest.classifier_catalog.findIndex(
    ({ id }) => id === classifier?.classifierId,
  );
  const summary = manifest.classification_summaries[classifierIndex];
  const descriptor = manifest.classifier_catalog[classifierIndex];
  if (
    classifiers.length !== 1 ||
    classifier === undefined ||
    classifierIndex < 0 ||
    descriptor === undefined ||
    summary === undefined ||
    summary.classifier_id !== classifier.classifierId
  ) {
    throw new TypeError(
      "Primary classifier does not match the manifest catalog",
    );
  }
  const counts = {
    complete: 0,
    partial: 0,
    unclassified: 0,
    labels: Object.fromEntries(
      descriptor.labels.map(({ key }) => [key, 0]),
    ) as Record<string, number>,
  };
  const statusCounts = {
    compatible: 0,
    violating: 0,
    indeterminate: 0,
    unclassified: 0,
  };
  const policy = classifier.classifierId === "knots_bip110";
  if (
    policy !==
    (classifier.assessmentCodes !== null &&
      classifier.assessmentDictionary !== null)
  ) {
    throw new TypeError("Primary BIP-110 assessment columns are inconsistent");
  }
  let totalVsize = 0;
  for (let row = 0; row < manifest.row_count; row += 1) {
    const vsize = columnValue(population.vsize, row);
    if (vsize === 0) {
      throw new TypeError(`Packed population invariant failed at row ${row}`);
    }
    totalVsize += vsize;
    if (!Number.isSafeInteger(totalVsize)) {
      throw new TypeError("Packed total vsize is unsafe");
    }
    const resultCode = columnValue(classifier.resultCodes, row);
    let policyResult: ClassifierResultTupleTransfer | null = null;
    if (resultCode === 0) {
      counts.unclassified += 1;
    } else {
      const tuple = classifier.resultDictionary[resultCode - 1];
      if (tuple === undefined) {
        throw new TypeError("Missing primary classifier result tuple");
      }
      counts[tuple.state] += 1;
      tuple.labels.forEach((label) => {
        if (!(label in counts.labels)) {
          throw new TypeError(
            `Classifier result contains undeclared label ${label}`,
          );
        }
        counts.labels[label] = (counts.labels[label] ?? 0) + 1;
      });
      if (policy) policyResult = tuple;
    }
    if (policy) {
      const assessmentCodes = classifier.assessmentCodes;
      const assessmentDictionary = classifier.assessmentDictionary;
      if (assessmentCodes === null || assessmentDictionary === null) {
        throw new TypeError("Primary BIP-110 assessment columns are missing");
      }
      const assessmentCode = columnValue(assessmentCodes, row);
      if ((assessmentCode !== 0) !== (resultCode !== 0)) {
        throw new TypeError(
          `BIP-110 assessment presence differs at row ${row}`,
        );
      }
      if (assessmentCode === 0) {
        statusCounts.unclassified += 1;
      } else {
        const entry = assessmentDictionary[assessmentCode - 1];
        if (entry === undefined) {
          throw new TypeError("Missing primary BIP-110 assessment tuple");
        }
        if (policyResult === null) {
          throw new TypeError("Missing primary BIP-110 result tuple");
        }
        validatePolicyResult(policyResult, entry);
        statusCounts[entry.status] += 1;
      }
    }
  }
  if (totalVsize !== manifest.total_vsize) {
    throw new TypeError("Packed total vsize does not match the manifest");
  }
  if (
    summary.complete_count !== counts.complete ||
    summary.partial_count !== counts.partial ||
    summary.unclassified_count !== counts.unclassified ||
    Object.keys(counts.labels).some(
      (label) => summary.label_counts[label] !== counts.labels[label],
    ) ||
    Object.keys(summary.label_counts).length !==
      Object.keys(counts.labels).length
  ) {
    throw new TypeError(
      "Primary classifier summary does not match packed rows",
    );
  }
  if (
    policy &&
    (manifest.bip110_summary.compatible_count !== statusCounts.compatible ||
      manifest.bip110_summary.violating_count !== statusCounts.violating ||
      manifest.bip110_summary.indeterminate_count !==
        statusCounts.indeterminate ||
      manifest.bip110_summary.unclassified_count !== statusCounts.unclassified)
  ) {
    throw new TypeError("Primary BIP-110 summary does not match packed rows");
  }
};

const validatePublicationSemantics = (
  publication: PackedPublicationTransfer,
): void => {
  const { manifest, population, membership, structure, classifiers } =
    publication;
  if (
    classifiers.length !== manifest.classifier_catalog.length ||
    classifiers.some(
      (classifier, index) =>
        classifier.classifierId !== manifest.classifier_catalog[index]?.id,
    )
  ) {
    throw new TypeError("Decoded classifier order does not match the manifest");
  }
  const resultCounts = classifiers.map((classifier, index) => ({
    complete: 0,
    partial: 0,
    unclassified: 0,
    labels: Object.fromEntries(
      (manifest.classifier_catalog[index]?.labels ?? []).map(({ key }) => [
        key,
        0,
      ]),
    ) as Record<string, number>,
    classifier,
  }));
  const policy = classifiers.find(
    ({ classifierId }) => classifierId === "knots_bip110",
  );
  if (
    policy === undefined ||
    policy.assessmentCodes === null ||
    policy.assessmentDictionary === null
  ) {
    throw new TypeError("BIP-110 assessment columns are missing");
  }
  const assessmentCodes = policy.assessmentCodes;
  const assessmentDictionary = policy.assessmentDictionary;
  const statusCounts = {
    compatible: 0,
    violating: 0,
    indeterminate: 0,
    unclassified: 0,
  };
  const structureBits = new Uint8Array(structure.presenceBits);
  let totalVsize = 0;
  for (let row = 0; row < manifest.row_count; row += 1) {
    const vsize = columnValue(population.vsize, row);
    const weight = columnValue(membership.weight, row);
    if (
      vsize === 0 ||
      weight === 0 ||
      weight > vsize * 4 ||
      columnValue(membership.ancestorCount, row) === 0 ||
      columnValue(membership.descendantCount, row) === 0 ||
      columnValue(membership.ancestorVsize, row) < vsize ||
      columnValue(membership.descendantVsize, row) < vsize
    ) {
      throw new TypeError(`Packed membership invariant failed at row ${row}`);
    }
    totalVsize += vsize;
    if (!Number.isSafeInteger(totalVsize)) {
      throw new TypeError("Packed total vsize is unsafe");
    }
    let resultPresence: boolean | null = null;
    let policyResult: ClassifierResultTupleTransfer | null = null;
    resultCounts.forEach((counts) => {
      const code = columnValue(counts.classifier.resultCodes, row);
      const present = code !== 0;
      if (resultPresence === null) resultPresence = present;
      else if (resultPresence !== present) {
        throw new TypeError(
          `Classifier presence is inconsistent at row ${row}`,
        );
      }
      if (!present) {
        counts.unclassified += 1;
        return;
      }
      const tuple = counts.classifier.resultDictionary[code - 1];
      if (tuple === undefined) {
        throw new TypeError("Missing classifier result tuple");
      }
      counts[tuple.state] += 1;
      tuple.labels.forEach((label) => {
        if (!(label in counts.labels)) {
          throw new TypeError(
            `Classifier result contains undeclared label ${label}`,
          );
        }
        counts.labels[label] = (counts.labels[label] ?? 0) + 1;
      });
      if (counts.classifier.classifierId === "knots_bip110") {
        policyResult = tuple;
      }
    });
    const assessmentCode = columnValue(assessmentCodes, row);
    if ((assessmentCode !== 0) !== resultPresence) {
      throw new TypeError(`BIP-110 assessment presence differs at row ${row}`);
    }
    if (isSet(structureBits, row) !== resultPresence) {
      throw new TypeError(`Structure presence differs at row ${row}`);
    }
    if (assessmentCode === 0) {
      statusCounts.unclassified += 1;
    } else {
      const entry = assessmentDictionary[assessmentCode - 1];
      if (entry === undefined) {
        throw new TypeError("Missing BIP-110 assessment tuple");
      }
      if (policyResult === null) {
        throw new TypeError("Missing BIP-110 result tuple");
      }
      validatePolicyResult(policyResult, entry);
      statusCounts[entry.status] += 1;
    }
  }
  if (totalVsize !== manifest.total_vsize) {
    throw new TypeError("Packed total vsize does not match the manifest");
  }
  resultCounts.forEach((counts, index) => {
    const summary = manifest.classification_summaries[index];
    if (
      summary === undefined ||
      summary.classifier_id !== counts.classifier.classifierId ||
      summary.complete_count !== counts.complete ||
      summary.partial_count !== counts.partial ||
      summary.unclassified_count !== counts.unclassified ||
      Object.keys(counts.labels).some(
        (label) => summary.label_counts[label] !== counts.labels[label],
      ) ||
      Object.keys(summary.label_counts).length !==
        Object.keys(counts.labels).length
    ) {
      throw new TypeError("Classifier summary does not match packed rows");
    }
  });
  if (
    manifest.bip110_summary.compatible_count !== statusCounts.compatible ||
    manifest.bip110_summary.violating_count !== statusCounts.violating ||
    manifest.bip110_summary.indeterminate_count !==
      statusCounts.indeterminate ||
    manifest.bip110_summary.unclassified_count !== statusCounts.unclassified
  ) {
    throw new TypeError("BIP-110 summary does not match packed rows");
  }
};

const transferables = (
  publication: PackedPublicationTransfer,
): Transferable[] => {
  const result: Transferable[] = [
    publication.population.txids,
    publication.population.vsize.values,
    publication.membership.differingWtxidBits,
    publication.membership.differingWtxidRanks,
    publication.membership.differingWtxids,
    publication.membership.weight.values,
    publication.membership.feeSats.values,
    publication.membership.enteredAtMs.values,
    publication.membership.ancestorCount.values,
    publication.membership.ancestorVsize.values,
    publication.membership.ancestorFeeSats.values,
    publication.membership.descendantCount.values,
    publication.membership.descendantVsize.values,
    publication.membership.replaceableBits,
    publication.structure.presenceBits,
    publication.structure.presenceRanks,
    publication.structure.inputCount.values,
    publication.structure.outputCount.values,
    publication.structure.opReturnBytes.values,
    publication.structure.outputSats.values,
    publication.structure.witnessBytes.values,
  ];
  for (const classifier of publication.classifiers) {
    result.push(classifier.resultCodes.values);
    if (classifier.assessmentCodes !== null) {
      result.push(classifier.assessmentCodes.values);
    }
  }
  return result;
};

const post = (
  response: AtlasWorkerResponse,
  transfers: Transferable[] = [],
): void => {
  globalThis.postMessage(response, { transfer: transfers });
};

const controllers = new Map<number, AbortController>();

export const loadPackedPublication = async (
  sourceId: string,
  selectedClassifierId: string,
  signal: AbortSignal,
  onPrimary: (
    publication: PackedPrimaryPublicationTransfer,
    timing: WorkerQuorumTiming,
  ) => void = () => undefined,
  onCompleteTiming: (timing: WorkerQuorumTiming) => void = () => undefined,
): Promise<PackedPublicationTransfer> => {
  let restarts = 0;
  const stageCache = new Map<string, CachedStage>();
  let primaryCommitted = false;
  let primaryPopulationId: string | null = null;
  let completeCandidatePopulationId: string | null = null;
  let supersededCandidatePopulationId: string | null = null;
  let rebasedPopulationId: string | null = null;
  let populationRebaseStartedAt: number | null = null;
  const recordSupersessionRestart = (): void => {
    restarts += 1;
    if (restarts > MAX_SUPERSESSION_RESTARTS) {
      throw new WorkerHttpError(
        409,
        "Source changed while loading; retry when it settles",
      );
    }
  };
  while (true) {
    try {
      if (!primaryCommitted) {
        const primaryStarted = performance.now();
        const primaryManifestStarted = performance.now();
        const primaryManifest = await fetchManifest(sourceId, signal);
        const primaryManifestFinished = performance.now();
        const selectedClassifier =
          primaryManifest.classifier_catalog.find(
            ({ id }) => id === selectedClassifierId,
          )?.id ??
          primaryManifest.classifier_catalog.find(
            ({ id }) => id === DEFAULT_PRIMARY_CLASSIFIER_ID,
          )?.id ??
          primaryManifest.classifier_catalog[0]?.id;
        const selectedDescriptor = primaryManifest.stages.find(
          (entry) =>
            entry.kind === "classifier" &&
            entry.classifier_id === selectedClassifier,
        );
        const populationDescriptor = primaryManifest.stages[0];
        if (
          populationDescriptor === undefined ||
          selectedDescriptor === undefined
        ) {
          throw new TypeError("Manifest does not publish a primary classifier");
        }
        retainCachedDescriptors(stageCache, [
          populationDescriptor,
          selectedDescriptor,
        ]);
        const [primaryPopulationTimed, primaryClassifierTimed] =
          await loadStageSet(
            sourceId,
            [populationDescriptor, selectedDescriptor],
            primaryManifest,
            signal,
            stageCache,
          );
        if (
          primaryPopulationTimed === undefined ||
          primaryClassifierTimed === undefined
        ) {
          throw new TypeError("Primary stage quorum is incomplete");
        }
        const primaryPopulation = primaryPopulationTimed.decoded;
        const primaryClassifier = primaryClassifierTimed.decoded;
        if (
          !("txids" in primaryPopulation) ||
          !("classifierId" in primaryClassifier)
        ) {
          throw new TypeError(
            "Primary stage quorum has unexpected stage kinds",
          );
        }
        const primaryPublication: PackedPrimaryPublicationTransfer = {
          manifest: primaryManifest,
          population: primaryPopulation,
          classifiers: [primaryClassifier],
        };
        const primaryValidationStarted = performance.now();
        validatePrimaryPublicationSemantics(primaryPublication);
        const primaryFinished = performance.now();
        primaryCommitted = true;
        primaryPopulationId = primaryManifest.population_id;
        onPrimary(primaryPublication, {
          manifestFetchValidateMs:
            primaryManifestFinished - primaryManifestStarted,
          stages: [
            primaryPopulationTimed.timing,
            primaryClassifierTimed.timing,
          ],
          semanticValidationMs: primaryFinished - primaryValidationStarted,
          quorumMs: primaryFinished - primaryStarted,
          supersessionRestarts: restarts,
          populationRebaseMs: null,
        });
      }

      completeCandidatePopulationId = null;
      const completeStarted = performance.now();
      const completeManifestStarted = performance.now();
      const manifest = await fetchManifest(sourceId, signal);
      const completeManifestFinished = performance.now();
      if (supersededCandidatePopulationId !== null) {
        if (manifest.population_id === supersededCandidatePopulationId) {
          recordSupersessionRestart();
        }
        supersededCandidatePopulationId = null;
      }
      if (primaryPopulationId === null) {
        throw new TypeError("Primary population identity is missing");
      }
      if (rebasedPopulationId !== null) {
        if (manifest.population_id !== rebasedPopulationId) {
          throw new WorkerHttpError(
            409,
            "Source changed repeatedly while loading; retry when it settles",
          );
        }
      } else if (manifest.population_id !== primaryPopulationId) {
        rebasedPopulationId = manifest.population_id;
        populationRebaseStartedAt = completeManifestStarted;
      }
      completeCandidatePopulationId = manifest.population_id;
      retainCachedDescriptors(stageCache, manifest.stages);
      const timedStages = await loadStageSet(
        sourceId,
        manifest.stages,
        manifest,
        signal,
        stageCache,
      );
      const decoded = timedStages.map(({ decoded: stage }) => stage);
      const population = decoded[0];
      const membership = decoded[1];
      const structure = decoded[2];
      if (
        population === undefined ||
        membership === undefined ||
        structure === undefined ||
        !("txids" in population) ||
        !("differingWtxids" in membership) ||
        !("presenceBits" in structure)
      ) {
        throw new TypeError(
          "Decoded publication does not match manifest order",
        );
      }
      const classifiers = decoded.slice(3);
      if (classifiers.some((entry) => !("classifierId" in entry))) {
        throw new TypeError("Decoded classifier set is incomplete");
      }
      const publication: PackedPublicationTransfer = {
        manifest,
        population,
        membership,
        structure,
        classifiers: classifiers as PackedClassifierTransfer[],
      };
      const completeValidationStarted = performance.now();
      validatePublicationSemantics(publication);
      const completeFinished = performance.now();
      onCompleteTiming({
        manifestFetchValidateMs:
          completeManifestFinished - completeManifestStarted,
        stages: timedStages.map(({ timing }) => timing),
        semanticValidationMs: completeFinished - completeValidationStarted,
        quorumMs: completeFinished - completeStarted,
        supersessionRestarts: restarts,
        populationRebaseMs:
          populationRebaseStartedAt === null
            ? null
            : completeFinished - populationRebaseStartedAt,
      });
      return publication;
    } catch (error) {
      if (!(error instanceof SupersededStageError)) throw error;
      if (primaryCommitted && completeCandidatePopulationId !== null) {
        supersededCandidatePopulationId = completeCandidatePopulationId;
      } else {
        supersededCandidatePopulationId = null;
        recordSupersessionRestart();
      }
      completeCandidatePopulationId = null;
    }
  }
};

const load = async (
  request: Extract<AtlasWorkerRequest, { type: "load" }>,
): Promise<void> => {
  const controller = new AbortController();
  controllers.set(request.requestId, controller);
  try {
    const timing: { complete: WorkerQuorumTiming | null } = { complete: null };
    const publication = await loadPackedPublication(
      request.sourceId,
      request.selectedClassifierId,
      controller.signal,
      (primary, primaryTiming) => {
        post({
          type: "primary",
          requestId: request.requestId,
          publication: primary,
          timing: primaryTiming,
        });
      },
      (completeTiming) => {
        timing.complete = completeTiming;
      },
    );
    if (timing.complete === null) {
      throw new TypeError("Complete worker timing is missing");
    }
    post(
      {
        type: "complete",
        requestId: request.requestId,
        publication,
        timing: timing.complete,
      },
      transferables(publication),
    );
  } catch (error) {
    if (controller.signal.aborted) return;
    post({
      type: "error",
      requestId: request.requestId,
      status: error instanceof WorkerHttpError ? error.status : null,
      message:
        error instanceof Error
          ? error.message
          : "Unable to load v2 publication",
      retryable:
        error instanceof SupersededStageError ||
        (error instanceof WorkerHttpError &&
          (error.status === 409 || error.status === 429)),
    });
  } finally {
    controllers.delete(request.requestId);
  }
};

const workerGlobal = globalThis as typeof globalThis & {
  WorkerGlobalScope?: unknown;
};

if (typeof workerGlobal.WorkerGlobalScope !== "undefined") {
  globalThis.addEventListener(
    "message",
    (event: MessageEvent<AtlasWorkerRequest>) => {
      if (event.data.type === "cancel") {
        controllers.get(event.data.requestId)?.abort();
        return;
      }
      void load(event.data);
    },
  );
}

export {
  classificationSetId,
  decodeStage,
  parseManifest,
  publicationId,
  resultTuple,
  signedColumn,
  validateManifestRoots,
  validatePolicyResult,
  validatePublicationSemantics,
};
