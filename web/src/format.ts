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

export const formatSats = (value: number): string =>
  value >= 100_000_000
    ? `${decimalFormat.format(value / 100_000_000)} BTC`
    : `${countFormat.format(value)} sats`;
