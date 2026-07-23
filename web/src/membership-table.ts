import type { MempoolTransaction } from "./types";

export const MEMBERSHIP_PAGE_SIZE = 100;

const ageDecimalFormat = new Intl.NumberFormat(undefined, {
  maximumFractionDigits: 2,
});

const formatRoundedUnit = (value: number, unit: string): string => {
  const roundedValue = Math.round(value * 100) / 100;
  return `${ageDecimalFormat.format(roundedValue)} ${unit}${roundedValue === 1 ? "" : "s"}`;
};

export const formatMembershipAge = (ageMs: number): string => {
  const minutes = Math.max(0, ageMs) / 60_000;
  if (minutes < 1) {
    return "<1 minute";
  }
  if (minutes < 60) {
    const wholeMinutes = Math.floor(minutes);
    return `${wholeMinutes} minute${wholeMinutes === 1 ? "" : "s"}`;
  }
  const hours = minutes / 60;
  if (hours < 24) {
    return formatRoundedUnit(hours, "hour");
  }
  return formatRoundedUnit(hours / 24, "day");
};

export interface MembershipPage {
  entries: MempoolTransaction[];
  matchCount: number;
  pageIndex: number;
  pageCount: number;
  firstMatchNumber: number;
  lastMatchNumber: number;
}

export const membershipPage = (
  memberships: readonly MempoolTransaction[],
  txidPrefix: string,
  requestedPageIndex: number,
): MembershipPage => {
  const normalizedPrefix = txidPrefix.trim().toLowerCase();
  const matches: readonly MempoolTransaction[] =
    normalizedPrefix.length === 0
      ? memberships
      : memberships.filter((membership) =>
          membership.txid.startsWith(normalizedPrefix),
        );
  const pageCount = Math.ceil(matches.length / MEMBERSHIP_PAGE_SIZE);
  const pageIndex =
    pageCount === 0
      ? 0
      : Math.min(Math.max(0, requestedPageIndex), pageCount - 1);
  const start = pageIndex * MEMBERSHIP_PAGE_SIZE;
  const entries = matches.slice(start, start + MEMBERSHIP_PAGE_SIZE);

  return {
    entries,
    matchCount: matches.length,
    pageIndex,
    pageCount,
    firstMatchNumber: entries.length === 0 ? 0 : start + 1,
    lastMatchNumber: start + entries.length,
  };
};
