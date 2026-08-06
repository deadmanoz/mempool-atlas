import { describe, expect, it } from "vitest";

import { formatSats, formatVsize } from "./format";

describe("formatSats", () => {
  it("keeps sub-bitcoin amounts in exact satoshis", () => {
    expect(formatSats(0)).toBe("0 sats");
    expect(formatSats(1)).toBe("1 sat");
    expect(formatSats(2)).toBe("2 sats");
    expect(formatSats(50_000)).toBe("50,000 sats");
    expect(formatSats(99_999_999)).toBe("99,999,999 sats");
  });

  it("renders bitcoin amounts without dropping a satoshi", () => {
    expect(formatSats(100_000_000)).toBe("1 BTC");
    expect(formatSats(100_000_001)).toBe("1.00000001 BTC");
    expect(formatSats(199_999_999)).toBe("1.99999999 BTC");
    expect(formatSats(250_000_000)).toBe("2.5 BTC");
    expect(formatSats(100_000_010)).toBe("1.0000001 BTC");
  });

  it("stays exact across large amounts", () => {
    expect(formatSats(2_100_000_000_000_000)).toBe("21,000,000 BTC");
    expect(formatSats(2_099_999_999_999_999)).toBe("20,999,999.99999999 BTC");
    expect(formatSats(Number.MAX_SAFE_INTEGER)).toBe("90,071,992.54740991 BTC");
  });

  it("preserves the sign of delta-adjusted amounts", () => {
    expect(formatSats(-1)).toBe("-1 sat");
    expect(formatSats(-5_000)).toBe("-5,000 sats");
    expect(formatSats(-100_000_001)).toBe("-1.00000001 BTC");
  });
});

describe("formatVsize", () => {
  it("scales virtual size by magnitude", () => {
    expect(formatVsize(999)).toBe("999 vB");
    expect(formatVsize(1_500)).toBe("1.5 kvB");
    expect(formatVsize(2_500_000)).toBe("2.5 MvB");
  });
});
