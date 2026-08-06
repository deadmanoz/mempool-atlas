export const countFormat = new Intl.NumberFormat();
export const decimalFormat = new Intl.NumberFormat(undefined, {
  maximumFractionDigits: 2,
});
export const percentageFormat = new Intl.NumberFormat(undefined, {
  maximumFractionDigits: 2,
  style: "percent",
});

export const formatVsize = (value: number): string => {
  if (value >= 1_000_000_000) {
    return `${decimalFormat.format(value / 1_000_000_000)} GvB`;
  }
  if (value >= 1_000_000) {
    return `${decimalFormat.format(value / 1_000_000)} MvB`;
  }
  if (value >= 1_000) {
    return `${decimalFormat.format(value / 1_000)} kvB`;
  }
  return `${countFormat.format(value)} vB`;
};

const SATS_PER_BTC = 100_000_000;

/**
 * Bitcoin amounts are assembled from two exact integers rather than one
 * fractional division, so no satoshi is ever lost to floating point. Only the
 * decimal mark is borrowed from the runtime locale.
 */
const decimalSeparator =
  new Intl.NumberFormat()
    .formatToParts(1.5)
    .find(({ type }) => type === "decimal")?.value ?? ".";

/**
 * Renders an exact satoshi amount. Amounts below one bitcoin stay in satoshis;
 * larger amounts become bitcoin carrying every satoshi as fraction digits, so
 * 100,000,001 sats reads as 1.00000001 BTC and never as 1 BTC. Delta-adjusted
 * amounts may be negative, which the sign split preserves.
 */
export const formatSats = (value: number): string => {
  const magnitude = Math.abs(value);
  const sign = value < 0 ? "-" : "";
  if (magnitude < SATS_PER_BTC) {
    return `${sign}${countFormat.format(magnitude)} ${magnitude === 1 ? "sat" : "sats"}`;
  }
  // Division rounds; the remainder is repaired so both parts stay exact.
  let whole = Math.floor(magnitude / SATS_PER_BTC);
  let fraction = magnitude - whole * SATS_PER_BTC;
  if (fraction < 0) {
    whole -= 1;
    fraction += SATS_PER_BTC;
  } else if (fraction >= SATS_PER_BTC) {
    whole += 1;
    fraction -= SATS_PER_BTC;
  }
  const digits = String(fraction).padStart(8, "0").replace(/0+$/, "");
  const exact =
    digits === ""
      ? countFormat.format(whole)
      : `${countFormat.format(whole)}${decimalSeparator}${digits}`;
  return `${sign}${exact} BTC`;
};
