import { createHash } from "node:crypto";
import { mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { gzipSync } from "node:zlib";

const WEB_ROOT = resolve(import.meta.dirname, "..");
const FIXTURE_ROOT = join(WEB_ROOT, ".perf-fixtures", "performance");
const MANIFEST_PATH = join(FIXTURE_ROOT, "manifest.json");
const OUTPUT_PATH = resolve(
  WEB_ROOT,
  "..",
  "docs",
  "client-performance-staged-projection.json",
);

const THROUGHPUT_BYTES_PER_SECOND = 159_461.45607954692;
const PERFORMANCE_SOURCE_COUNT = 2;
const PERFORMANCE_TRANSACTION_COUNT = 70_000;
const GZIP_LEVEL = 6;
const GATES = Object.freeze({
  node_primary_bytes: 3_189_229,
  node_complete_target_bytes: 6_059_535,
  node_complete_maximum_bytes: 6_537_919,
  comparison_primary_bytes: 5_740_612,
  comparison_complete_target_bytes: 12_119_070,
  comparison_complete_maximum_bytes: 13_075_839,
});

const fixture = JSON.parse(readFileSync(MANIFEST_PATH, "utf8"));
if (
  fixture.manifest_version !== 2 ||
  fixture.profile !== "performance" ||
  !Array.isArray(fixture.snapshots) ||
  fixture.snapshots.length !== PERFORMANCE_SOURCE_COUNT ||
  fixture.configuration?.requested_transaction_count_per_source !==
    PERFORMANCE_TRANSACTION_COUNT ||
  fixture.snapshots.some(
    (snapshot) => snapshot.transaction_count !== PERFORMANCE_TRANSACTION_COUNT,
  )
) {
  throw new Error(
    "v2 performance fixtures are missing or incompatible; run just perf-fixtures",
  );
}

const measuredBody = (descriptor) => {
  const bytes = readFileSync(join(FIXTURE_ROOT, descriptor.path));
  const contentId = createHash("sha256").update(bytes).digest("hex");
  if (
    bytes.byteLength !== descriptor.uncompressed_bytes ||
    contentId !== descriptor.content_id
  ) {
    throw new Error(
      `fixture body does not match descriptor ${descriptor.path}`,
    );
  }
  return Object.freeze({
    identity_bytes: bytes.byteLength,
    gzip_bytes: gzipSync(bytes, { level: GZIP_LEVEL }).byteLength,
  });
};

const sources = fixture.snapshots.map((snapshot) => {
  const manifest = measuredBody(snapshot.manifest.body);
  const stages = snapshot.stages.map((stage) => ({
    kind: stage.kind,
    classifier_id: stage.classifier_id,
    ...measuredBody(stage.route.body),
  }));
  const population = stages.find((stage) => stage.kind === "population");
  if (population === undefined)
    throw new Error("fixture has no population stage");
  const classifierStages = stages.filter(
    (stage) => stage.kind === "classifier",
  );
  const bip110 = classifierStages.find(
    (stage) => stage.classifier_id === "knots_bip110",
  );
  if (bip110 === undefined) throw new Error("fixture has no BIP-110 stage");
  const staticCompleteBytes =
    manifest.gzip_bytes +
    stages.reduce((total, stage) => total + stage.gzip_bytes, 0);
  const selected = Object.fromEntries(
    classifierStages.map((stage) => [
      stage.classifier_id,
      {
        primary_node_bytes:
          manifest.gzip_bytes + population.gzip_bytes + stage.gzip_bytes,
        primary_comparison_source_bytes:
          manifest.gzip_bytes +
          population.gzip_bytes +
          stage.gzip_bytes +
          (stage.classifier_id === "knots_bip110" ? 0 : bip110.gzip_bytes),
        cold_progressive_complete_bytes:
          staticCompleteBytes + manifest.gzip_bytes,
      },
    ]),
  );
  return {
    source_id: snapshot.source_id,
    transaction_count: snapshot.transaction_count,
    manifest,
    stages,
    static_complete_bytes: staticCompleteBytes,
    selected,
  };
});

const classifierIds = Object.keys(sources[0].selected);
if (
  classifierIds.length === 0 ||
  sources.some(
    (source) =>
      JSON.stringify(Object.keys(source.selected)) !==
      JSON.stringify(classifierIds),
  )
) {
  throw new Error("performance sources have inconsistent classifier lanes");
}

const maximumCandidate = (candidates, field) =>
  candidates.reduce((maximum, candidate) =>
    candidate[field] > maximum[field] ? candidate : maximum,
  );

const nodeCandidates = sources.flatMap((source) =>
  classifierIds.map((classifier_id) => ({
    source_id: source.source_id,
    classifier_id,
    ...source.selected[classifier_id],
  })),
);
const comparisonCandidates = classifierIds.map((classifier_id) => ({
  classifier_id,
  primary_comparison_bytes: sources.reduce(
    (total, source) =>
      total + source.selected[classifier_id].primary_comparison_source_bytes,
    0,
  ),
  cold_progressive_complete_bytes: sources.reduce(
    (total, source) =>
      total + source.selected[classifier_id].cold_progressive_complete_bytes,
    0,
  ),
}));

const nodePrimary = maximumCandidate(nodeCandidates, "primary_node_bytes");
const nodeComplete = maximumCandidate(
  nodeCandidates,
  "cold_progressive_complete_bytes",
);
const comparisonPrimary = maximumCandidate(
  comparisonCandidates,
  "primary_comparison_bytes",
);
const comparisonComplete = maximumCandidate(
  comparisonCandidates,
  "cold_progressive_complete_bytes",
);

const wallClockMs = (bytes, requestSeconds, processingSeconds) =>
  Math.ceil(
    (requestSeconds + bytes / THROUGHPUT_BYTES_PER_SECOND + processingSeconds) *
      1_000,
  );

const checks = {
  node_primary: nodePrimary.primary_node_bytes <= GATES.node_primary_bytes,
  node_complete_target:
    nodeComplete.cold_progressive_complete_bytes <=
    GATES.node_complete_target_bytes,
  node_complete_maximum:
    nodeComplete.cold_progressive_complete_bytes <=
    GATES.node_complete_maximum_bytes,
  comparison_primary:
    comparisonPrimary.primary_comparison_bytes <=
    GATES.comparison_primary_bytes,
  comparison_complete_target:
    comparisonComplete.cold_progressive_complete_bytes <=
    GATES.comparison_complete_target_bytes,
  comparison_complete_maximum:
    comparisonComplete.cold_progressive_complete_bytes <=
    GATES.comparison_complete_maximum_bytes,
};

const output = {
  schema_version: 2,
  fixture_manifest_version: fixture.manifest_version,
  fixture_profile: fixture.profile,
  throughput_bytes_per_second: THROUGHPUT_BYTES_PER_SECOND,
  gates: GATES,
  checks,
  release_gate_passed:
    checks.node_primary &&
    checks.node_complete_maximum &&
    checks.comparison_primary &&
    checks.comparison_complete_maximum,
  requires_pre_authorized_rederivation:
    !checks.node_complete_target || !checks.comparison_complete_target,
  worst_case: {
    node_primary: {
      ...nodePrimary,
      wall_clock_ms: wallClockMs(nodePrimary.primary_node_bytes, 2, 8),
    },
    node_complete: {
      ...nodeComplete,
      wall_clock_ms: wallClockMs(
        nodeComplete.cold_progressive_complete_bytes,
        2,
        12,
      ),
    },
    comparison_primary: {
      ...comparisonPrimary,
      wall_clock_ms: wallClockMs(
        comparisonPrimary.primary_comparison_bytes,
        2,
        12,
      ),
    },
    comparison_complete: {
      ...comparisonComplete,
      wall_clock_ms: wallClockMs(
        comparisonComplete.cold_progressive_complete_bytes,
        2,
        16,
      ),
    },
  },
  sources,
  comparison_candidates: comparisonCandidates,
};

mkdirSync(dirname(OUTPUT_PATH), { recursive: true });
writeFileSync(OUTPUT_PATH, `${JSON.stringify(output, null, 2)}\n`);
console.log(`staged projection: ${OUTPUT_PATH}`);
console.log(
  `node ${nodePrimary.primary_node_bytes}/${nodeComplete.cold_progressive_complete_bytes} bytes; comparison ${comparisonPrimary.primary_comparison_bytes}/${comparisonComplete.cold_progressive_complete_bytes} bytes`,
);
if (!output.release_gate_passed) process.exitCode = 1;
