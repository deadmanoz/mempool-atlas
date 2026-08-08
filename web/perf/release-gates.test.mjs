import { readFileSync } from "node:fs";
import { resolve } from "node:path";

import { describe, expect, it } from "vitest";

import {
  PINNED_THROUGHPUT_BYTES_PER_SECOND,
  RELEASE_GATES,
  STAGED_PROJECTION_GATES,
} from "./release-gates.mjs";

const performanceDoc = readFileSync(
  resolve(import.meta.dirname, "..", "..", "docs", "client-performance.md"),
  "utf8",
);
const stagedProjection = JSON.parse(
  readFileSync(
    resolve(
      import.meta.dirname,
      "..",
      "..",
      "docs",
      "client-performance-staged-projection.json",
    ),
    "utf8",
  ),
);
const formattedBytes = (value) => value.toLocaleString("en-US");
const documentedReleaseGates = () => {
  const match = performanceDoc.match(
    /<!-- release-gates:start -->\s*```json\s*([\s\S]*?)\s*```\s*<!-- release-gates:end -->/,
  );
  if (!match) {
    throw new Error("documented release gate inventory is missing");
  }
  return JSON.parse(match[1]);
};

const projectionRows = () => {
  const rows = new Map();
  for (const match of performanceDoc.matchAll(
    /^\| (Node primary|Node complete|Comparison primary|Comparison complete)\s+\|\s+([\d,]+) bytes \|\s+([\d,]+) bytes \|\s+(pass|fail)\s+\|$/gm,
  )) {
    rows.set(match[1], {
      exactBytes: Number(match[2].replaceAll(",", "")),
      ceilingBytes: Number(match[3].replaceAll(",", "")),
      result: match[4],
    });
  }
  return rows;
};

const documentedSeconds = (label, pattern) => {
  const match = performanceDoc.match(pattern);
  if (!match) throw new Error(`documented ${label} values are missing`);
  return match.slice(1).map(Number);
};

describe("release gate source of truth", () => {
  it("drives the browser byte gates from the staged projection gates", () => {
    expect(RELEASE_GATES.node.primary_encoded_body_bytes).toBe(
      STAGED_PROJECTION_GATES.node_primary_bytes,
    );
    expect(RELEASE_GATES.node.complete_encoded_body_bytes).toBe(
      STAGED_PROJECTION_GATES.node_complete_target_bytes,
    );
    expect(RELEASE_GATES.comparison.primary_encoded_body_bytes).toBe(
      STAGED_PROJECTION_GATES.comparison_primary_bytes,
    );
    expect(RELEASE_GATES.comparison.complete_encoded_body_bytes).toBe(
      STAGED_PROJECTION_GATES.comparison_complete_target_bytes,
    );
  });

  it("keeps every documented executable release gate synchronized", () => {
    expect(documentedReleaseGates()).toEqual(RELEASE_GATES);
  });

  it("keeps the documented throughput and projection ceilings synchronized", () => {
    expect(stagedProjection.throughput_bytes_per_second).toBe(
      PINNED_THROUGHPUT_BYTES_PER_SECOND,
    );
    expect(performanceDoc).toContain(
      `\`${PINNED_THROUGHPUT_BYTES_PER_SECOND}\` bytes per second`,
    );
    for (const ceiling of Object.values(STAGED_PROJECTION_GATES)) {
      expect(performanceDoc).toContain(`${formattedBytes(ceiling)} bytes`);
    }
  });

  it("keeps measured projection bytes and wall clocks synchronized", () => {
    expect(projectionRows()).toEqual(
      new Map([
        [
          "Node primary",
          {
            exactBytes:
              stagedProjection.worst_case.node_primary.primary_node_bytes,
            ceilingBytes: stagedProjection.gates.node_primary_bytes,
            result: stagedProjection.checks.node_primary ? "pass" : "fail",
          },
        ],
        [
          "Node complete",
          {
            exactBytes:
              stagedProjection.worst_case.node_complete
                .cold_progressive_complete_bytes,
            ceilingBytes: stagedProjection.gates.node_complete_target_bytes,
            result: stagedProjection.checks.node_complete_target
              ? "pass"
              : "fail",
          },
        ],
        [
          "Comparison primary",
          {
            exactBytes:
              stagedProjection.worst_case.comparison_primary
                .primary_comparison_bytes,
            ceilingBytes: stagedProjection.gates.comparison_primary_bytes,
            result: stagedProjection.checks.comparison_primary
              ? "pass"
              : "fail",
          },
        ],
        [
          "Comparison complete",
          {
            exactBytes:
              stagedProjection.worst_case.comparison_complete
                .cold_progressive_complete_bytes,
            ceilingBytes:
              stagedProjection.gates.comparison_complete_target_bytes,
            result: stagedProjection.checks.comparison_complete_target
              ? "pass"
              : "fail",
          },
        ],
      ]),
    );

    const wallClocks = [
      stagedProjection.worst_case.node_primary.wall_clock_ms,
      stagedProjection.worst_case.node_complete.wall_clock_ms,
      stagedProjection.worst_case.comparison_primary.wall_clock_ms,
      stagedProjection.worst_case.comparison_complete.wall_clock_ms,
    ].map((milliseconds) => milliseconds / 1_000);
    expect(
      documentedSeconds(
        "projected wall-clock",
        /corresponding projected wall clocks are ([\d.]+),\s*([\d.]+), ([\d.]+), and ([\d.]+) seconds/,
      ),
    ).toEqual(wallClocks);
    const margins = [
      RELEASE_GATES.node.primary_interaction_ms / 1_000 - wallClocks[0],
      RELEASE_GATES.node.complete_feature_ready_ms / 1_000 - wallClocks[1],
      RELEASE_GATES.comparison.primary_interaction_ms / 1_000 - wallClocks[2],
      RELEASE_GATES.comparison.complete_feature_ready_ms / 1_000 -
        wallClocks[3],
    ].map((seconds) => Number(seconds.toFixed(3)));
    expect(
      documentedSeconds(
        "projected wall-clock margin",
        /remaining margins against the release gates are ([\d.]+), ([\d.]+), ([\d.]+), and\s*([\d.]+) seconds/,
      ),
    ).toEqual(margins);
  });
});
