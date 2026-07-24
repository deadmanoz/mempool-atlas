import { describe, expect, it } from "vitest";

import {
  MEMBERSHIP_PAGE_SIZE,
  formatMembershipAge,
  membershipPage,
} from "./membership-table";
import type { MempoolTransaction } from "./types";

const memberships = Array.from(
  { length: 205 },
  (_, index): MempoolTransaction => ({
    txid: index.toString(16).padStart(64, "0"),
    wtxid: index.toString(16).padStart(64, "0"),
    vsize: 141,
    fee_sats: 423,
    entered_at_ms: 1_700_000_000_000,
    bip110: {
      status: "compatible",
      primary_rule: null,
      violated_rules: [],
      unknown_rules: [],
    },
  }),
);

describe("membershipPage", () => {
  it("caps each page at 100 memberships", () => {
    const first = membershipPage(memberships, "", 0);
    const second = membershipPage(memberships, "", 1);
    const third = membershipPage(memberships, "", 2);

    expect(MEMBERSHIP_PAGE_SIZE).toBe(100);
    expect(first.entries).toHaveLength(100);
    expect(first.firstMatchNumber).toBe(1);
    expect(first.lastMatchNumber).toBe(100);
    expect(second.entries).toHaveLength(100);
    expect(third.entries).toHaveLength(5);
    expect(third.lastMatchNumber).toBe(205);
  });

  it("clamps a requested page to the available range", () => {
    const page = membershipPage(memberships, "", 99);

    expect(page.pageIndex).toBe(2);
    expect(page.pageCount).toBe(3);
    expect(page.entries).toHaveLength(5);
  });

  it("matches a case-insensitive transaction ID prefix", () => {
    const expectedTxid = memberships[204]?.txid;
    const page = membershipPage(
      memberships,
      `  ${expectedTxid?.toUpperCase()}  `,
      0,
    );

    expect(page.matchCount).toBe(1);
    expect(page.entries[0]?.txid).toBe(expectedTxid);
  });

  it("returns an empty page when the prefix does not match", () => {
    const page = membershipPage(memberships, "ffff", 0);

    expect(page).toMatchObject({
      entries: [],
      matchCount: 0,
      pageIndex: 0,
      pageCount: 0,
      firstMatchNumber: 0,
      lastMatchNumber: 0,
    });
  });
});

describe("formatMembershipAge", () => {
  it.each([
    [60 * 60_000 + 1_000, "1 hour"],
    [24 * 60 * 60_000 + 1_000, "1 day"],
    [2 * 60 * 60_000, "2 hours"],
    [2 * 24 * 60 * 60_000, "2 days"],
  ])("formats %s ms as %s", (ageMs, expected) => {
    expect(formatMembershipAge(ageMs)).toBe(expected);
  });
});
