import type { MempoolTransaction } from "./types";

export const txid = (value: number): string =>
  value.toString(16).padStart(64, "0");

/** A schema-valid transaction baseline for focused web unit tests. */
export const mempoolTransaction = (
  value: number,
  overrides: Partial<MempoolTransaction> = {},
): MempoolTransaction => {
  const vsize = overrides.vsize ?? 200;
  const feeSats = overrides.fee_sats ?? 2_000;
  return {
    txid: txid(value),
    wtxid: txid(value),
    vsize,
    weight: overrides.weight ?? vsize * 4,
    fee_sats: feeSats,
    entered_at_ms: 1_700_000_000_000,
    ancestor_count: 1,
    ancestor_vsize: overrides.ancestor_vsize ?? vsize,
    ancestor_fee_sats: overrides.ancestor_fee_sats ?? feeSats,
    descendant_count: 1,
    descendant_vsize: overrides.descendant_vsize ?? vsize,
    replaceable: false,
    structure: null,
    classifications: [],
    bip110: null,
    ...overrides,
  };
};
