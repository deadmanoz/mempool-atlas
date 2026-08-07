export interface BoundedSampleEntry {
  txid: string;
}

export interface BoundedSample<T extends BoundedSampleEntry> {
  entries: T[];
  pinsSelected: boolean;
}

export const pinSelectedInBoundedSample = <T extends BoundedSampleEntry>(
  largest: readonly T[],
  selected: T | undefined,
  limit: number,
  isEligible: (entry: T) => boolean = () => true,
): BoundedSample<T> => {
  const bounded = largest.slice(0, limit);
  if (selected === undefined) {
    return { entries: bounded, pinsSelected: false };
  }
  if (bounded.some(({ txid }) => txid === selected.txid)) {
    return { entries: bounded, pinsSelected: false };
  }
  if (!isEligible(selected)) {
    return { entries: bounded, pinsSelected: false };
  }
  return {
    entries: [selected, ...bounded.slice(0, Math.max(0, limit - 1))],
    pinsSelected: true,
  };
};
