import type { StagedSnapshotManifest } from "./types";

const DIGEST = /^[0-9a-f]{64}$/;

const bytesToHex = (bytes: Uint8Array): string =>
  Array.from(bytes, (value) => value.toString(16).padStart(2, "0")).join("");

const hexToBytes = (value: unknown): Uint8Array => {
  if (typeof value !== "string" || !DIGEST.test(value)) {
    throw new TypeError("Invalid digest");
  }
  const result = new Uint8Array(32);
  for (let index = 0; index < result.length; index += 1) {
    result[index] = Number.parseInt(value.slice(index * 2, index * 2 + 2), 16);
  }
  return result;
};

export const sha256 = async (bytes: Uint8Array): Promise<string> =>
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

export const classificationSetId = async (
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

export const publicationId = async (
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

export const validateManifestRoots = async (
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
