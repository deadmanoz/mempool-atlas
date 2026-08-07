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
const formattedBytes = (value) => value.toLocaleString("en-US");

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

  it("keeps the documented throughput and byte ceilings synchronized", () => {
    expect(performanceDoc).toContain(
      `\`${PINNED_THROUGHPUT_BYTES_PER_SECOND}\` bytes per second`,
    );
    for (const ceiling of Object.values(STAGED_PROJECTION_GATES)) {
      expect(performanceDoc).toContain(`${formattedBytes(ceiling)} bytes`);
    }
  });
});
