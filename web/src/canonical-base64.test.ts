import { describe, expect, it } from "vitest";

import { decodeCanonicalBase64 } from "./canonical-base64";

describe("canonical base64", () => {
  it.each([
    ["", []],
    ["AQ==", [1]],
    ["AQI=", [1, 2]],
    ["AQID", [1, 2, 3]],
  ] as const)("decodes %j", (encoded, expected) => {
    expect([...decodeCanonicalBase64(encoded, "fixture")]).toEqual(expected);
  });

  it.each(["AR==", "AQJ=", "AQ=", "AQID=", "AA=A", "A===", "AA A"])(
    "rejects non-canonical %s",
    (encoded) => {
      expect(() => decodeCanonicalBase64(encoded, "fixture")).toThrow(
        "Non-canonical fixture",
      );
    },
  );

  it("decodes a packed 120,000-transaction hash column without recursion", () => {
    const encoded = "AAAA".repeat(1_280_000);
    const decoded = decodeCanonicalBase64(encoded, "population txids");
    expect(decoded).toHaveLength(120_000 * 32);
    expect(decoded[0]).toBe(0);
    expect(decoded.at(-1)).toBe(0);
  });
});
