import type {
  ClassifierResultTupleTransfer,
  PackedClassifierTransfer,
  PackedPublicationTransfer,
  PackedSignedColumnTransfer,
  PackedUnsignedColumnTransfer,
} from "./atlas-worker-protocol";
import { PackedPublicationStore } from "./packed-store";
import type {
  Bip110Assessment,
  Bip110Summary,
  MempoolTransaction,
  RuleId,
  SourceSummary,
} from "./types";
import type { LoadedSourceSnapshot } from "./comparison-model";

export const violating = (
  violatedRules: RuleId[],
  unknownRules: RuleId[] = [],
): Bip110Assessment => ({
  status: "violating",
  primary_rule: violatedRules[0] ?? null,
  violated_rules: violatedRules,
  unknown_rules: unknownRules,
});

const summary = (
  transactions: readonly MempoolTransaction[],
): Bip110Summary => ({
  evaluator_id: "rdts-rules",
  evaluator_version: "0.1.0",
  scope: "knots_mempool_policy",
  compatible_count: transactions.filter(
    ({ bip110 }) => bip110?.status === "compatible",
  ).length,
  violating_count: transactions.filter(
    ({ bip110 }) => bip110?.status === "violating",
  ).length,
  indeterminate_count: transactions.filter(
    ({ bip110 }) => bip110?.status === "indeterminate",
  ).length,
  unclassified_count: transactions.filter(({ bip110 }) => bip110 === null)
    .length,
});

const packedColumn = (
  values: readonly number[],
): PackedUnsignedColumnTransfer => {
  const width = 7;
  const bytes = new Uint8Array(values.length * width);
  values.forEach((value, row) => {
    let packed = BigInt(value);
    for (let byte = 0; byte < width; byte += 1) {
      bytes[row * width + byte] = Number(packed & 0xffn);
      packed >>= 8n;
    }
  });
  return { width, values: bytes.buffer };
};

const packedSignedColumn = (
  values: readonly number[],
): PackedSignedColumnTransfer => {
  const width = 7;
  const bytes = new Uint8Array(values.length * width);
  values.forEach((value, row) => {
    let packed = BigInt.asUintN(width * 8, BigInt(value));
    for (let byte = 0; byte < width; byte += 1) {
      bytes[row * width + byte] = Number(packed & 0xffn);
      packed >>= 8n;
    }
  });
  return { width, values: bytes.buffer };
};

const hashBytes = (hash: string): number[] =>
  Array.from({ length: 32 }, (_, index) =>
    Number.parseInt(hash.slice(index * 2, index * 2 + 2), 16),
  );

const bitset = (values: readonly boolean[]): ArrayBuffer => {
  const bytes = new Uint8Array(Math.ceil(values.length / 8));
  values.forEach((value, index) => {
    if (value) {
      const byte = index >> 3;
      bytes[byte] = (bytes[byte] ?? 0) | (1 << (index & 7));
    }
  });
  return bytes.buffer;
};

const sparseRanks = (values: readonly boolean[]): Uint32Array => {
  const ranks = new Uint32Array(values.length + 1);
  let count = 0;
  values.forEach((value, index) => {
    ranks[index] = count;
    if (value) count += 1;
  });
  ranks[values.length] = count;
  return ranks;
};

const policyTransfer = (
  transactions: readonly MempoolTransaction[],
): PackedClassifierTransfer | null => {
  if (!transactions.some(({ bip110 }) => bip110 !== null)) return null;
  const resultDictionary: PackedClassifierTransfer["resultDictionary"] = [];
  const assessmentDictionary: Bip110Assessment[] = [];
  const resultIndex = new Map<string, number>();
  const assessmentIndex = new Map<string, number>();
  const resultCodes: number[] = [];
  const assessmentCodes: number[] = [];
  for (const { bip110 } of transactions) {
    if (bip110 === null) {
      resultCodes.push(0);
      assessmentCodes.push(0);
      continue;
    }
    const result: ClassifierResultTupleTransfer = {
      state: bip110.unknown_rules.length === 0 ? "complete" : "partial",
      primary_label: bip110.status,
      labels: [bip110.status],
      missing_facts: bip110.unknown_rules.length === 0 ? [] : ["policy_facts"],
    };
    const resultKey = JSON.stringify(result);
    let resultCode = resultIndex.get(resultKey);
    if (resultCode === undefined) {
      resultDictionary.push(result);
      resultCode = resultDictionary.length;
      resultIndex.set(resultKey, resultCode);
    }
    const assessmentKey = JSON.stringify(bip110);
    let assessmentCode = assessmentIndex.get(assessmentKey);
    if (assessmentCode === undefined) {
      assessmentDictionary.push(bip110);
      assessmentCode = assessmentDictionary.length;
      assessmentIndex.set(assessmentKey, assessmentCode);
    }
    resultCodes.push(resultCode);
    assessmentCodes.push(assessmentCode);
  }
  return {
    contentId: "13".repeat(32),
    classifierId: "knots_bip110",
    resultDictionary,
    resultCodes: packedColumn(resultCodes),
    assessmentDictionary,
    assessmentCodes: packedColumn(assessmentCodes),
  };
};

const packedPublication = (
  sourceId: string,
  sourceLabel: string,
  transactions: readonly MempoolTransaction[],
  observationTime: number,
): PackedPublicationTransfer => {
  const bip110Summary = summary(transactions);
  const policy = policyTransfer(transactions);
  const classifiedCount =
    transactions.length - bip110Summary.unclassified_count;
  const source: SourceSummary = {
    source_id: sourceId,
    source_label: sourceLabel,
    availability: "ready",
    poll_interval_seconds: 300,
    last_poll_started_at_ms: observationTime - 1_000,
    snapshot_observed_at_ms: observationTime,
    chain_tip: { height: 900_000, hash: "00".repeat(32) },
    transaction_count: transactions.length,
    total_vsize: transactions.reduce((total, entry) => total + entry.vsize, 0),
    classification: {
      state: "complete",
      revision: 1,
      classified_count: classifiedCount,
      unclassified_count: bip110Summary.unclassified_count,
    },
    last_error: null,
  };
  const differingWtxids = transactions.map(({ txid, wtxid }) => txid !== wtxid);
  const structures = transactions.map(({ structure }) => structure !== null);
  const classifierCatalog =
    policy === null
      ? []
      : [
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
              {
                key: "violating",
                label: "Violating",
                description: "Violating",
              },
              {
                key: "indeterminate",
                label: "Indeterminate",
                description: "Indeterminate",
              },
            ],
          },
        ];
  const classificationSummaries =
    policy === null
      ? []
      : [
          {
            classifier_id: "knots_bip110",
            complete_count: transactions.filter(
              ({ bip110 }) =>
                bip110 !== null && bip110.unknown_rules.length === 0,
            ).length,
            partial_count: transactions.filter(
              ({ bip110 }) =>
                bip110 !== null && bip110.unknown_rules.length > 0,
            ).length,
            unclassified_count: bip110Summary.unclassified_count,
            label_counts: {
              compatible: bip110Summary.compatible_count,
              violating: bip110Summary.violating_count,
              indeterminate: bip110Summary.indeterminate_count,
            },
          },
        ];
  return {
    manifest: {
      schema_version: 2,
      source,
      source_id: sourceId,
      source_label: sourceLabel,
      collection_started_at_ms: observationTime - 1_000,
      collection_completed_at_ms: observationTime,
      collection_duration_ms: 1_000,
      observed_at_ms: observationTime,
      classification_revision: 1,
      chain_tip: source.chain_tip!,
      transaction_count: transactions.length,
      total_vsize: source.total_vsize!,
      classifier_catalog: classifierCatalog,
      classification_summaries: classificationSummaries,
      bip110_summary: bip110Summary,
      row_count: transactions.length,
      population_id: "10".repeat(32),
      classification_set_id: "14".repeat(32),
      publication_id: "15".repeat(32),
      stages: [],
    },
    population: {
      contentId: "10".repeat(32),
      txids: Uint8Array.from(
        transactions.flatMap(({ txid }) => hashBytes(txid)),
      ).buffer,
      vsize: packedColumn(transactions.map(({ vsize }) => vsize)),
    },
    membership: {
      contentId: "11".repeat(32),
      differingWtxidBits: bitset(differingWtxids),
      differingWtxidRanks: sparseRanks(differingWtxids).buffer as ArrayBuffer,
      differingWtxids: Uint8Array.from(
        transactions.flatMap(({ txid, wtxid }) =>
          txid === wtxid ? [] : hashBytes(wtxid),
        ),
      ).buffer,
      weight: packedColumn(transactions.map(({ weight }) => weight)),
      feeSats: packedColumn(transactions.map(({ fee_sats }) => fee_sats)),
      enteredAtMs: packedColumn(
        transactions.map(({ entered_at_ms }) => entered_at_ms),
      ),
      ancestorCount: packedColumn(
        transactions.map(({ ancestor_count }) => ancestor_count),
      ),
      ancestorVsize: packedColumn(
        transactions.map(({ ancestor_vsize }) => ancestor_vsize),
      ),
      ancestorFeeSats: packedSignedColumn(
        transactions.map(({ ancestor_fee_sats }) => ancestor_fee_sats),
      ),
      descendantCount: packedColumn(
        transactions.map(({ descendant_count }) => descendant_count),
      ),
      descendantVsize: packedColumn(
        transactions.map(({ descendant_vsize }) => descendant_vsize),
      ),
      replaceableBits: bitset(
        transactions.map(({ replaceable }) => replaceable),
      ),
    },
    structure: {
      contentId: "12".repeat(32),
      presenceBits: bitset(structures),
      presenceRanks: sparseRanks(structures).buffer as ArrayBuffer,
      inputCount: packedColumn(
        transactions.flatMap(({ structure }) =>
          structure === null ? [] : [structure.input_count],
        ),
      ),
      outputCount: packedColumn(
        transactions.flatMap(({ structure }) =>
          structure === null ? [] : [structure.output_count],
        ),
      ),
      opReturnBytes: packedColumn(
        transactions.flatMap(({ structure }) =>
          structure === null ? [] : [structure.op_return_bytes],
        ),
      ),
      recognizedNonOpReturnBytes: packedColumn(
        transactions.flatMap(({ structure }) =>
          structure === null
            ? []
            : [structure.recognized_carried_bytes - structure.op_return_bytes],
        ),
      ),
      outputSats: packedColumn(
        transactions.flatMap(({ structure }) =>
          structure === null ? [] : [structure.output_sats],
        ),
      ),
      witnessBytes: packedColumn(
        transactions.flatMap(({ structure }) =>
          structure === null ? [] : [structure.witness_bytes],
        ),
      ),
    },
    classifiers: policy === null ? [] : [policy],
  };
};

export const loadedSource = (
  sourceId: string,
  transactions: MempoolTransaction[],
  observedAtMs?: number,
): LoadedSourceSnapshot => {
  const observationTime = observedAtMs ?? 1_700_000_001_000;
  const sourceLabel =
    observedAtMs === undefined
      ? `${sourceId.toUpperCase()} node`
      : sourceId.toUpperCase();
  const store = new PackedPublicationStore(
    packedPublication(sourceId, sourceLabel, transactions, observationTime),
  );
  return {
    source: store.publication.manifest.source,
    snapshot: store.snapshot,
  };
};
