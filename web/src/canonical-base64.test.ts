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

  it.each(["AR==", "AQJ=", "AQ=", "AQID="])(
    "rejects non-canonical %s",
    (encoded) => {
      expect(() => decodeCanonicalBase64(encoded, "fixture")).toThrow(
        "Non-canonical fixture",
      );
    },
  );
});
