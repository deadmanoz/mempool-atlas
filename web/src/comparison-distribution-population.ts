import type {
  ComparedTransaction,
  ComparisonSide,
  CurrentComparison,
} from "./comparison-model";
import type { MempoolTransaction } from "./types";

export type ComparisonDistributionScope =
  "all" | "common" | "left_only" | "right_only";

const arrayIndex = (property: PropertyKey): number | null => {
  if (typeof property !== "string" || !/^(0|[1-9][0-9]*)$/.test(property)) {
    return null;
  }
  const index = Number(property);
  return Number.isSafeInteger(index) ? index : null;
};

const presentTransactionView = (
  entries: readonly ComparedTransaction[],
  side: ComparisonSide,
): MempoolTransaction[] => {
  const transactionAt = (index: number): MempoolTransaction | undefined => {
    const entry = entries[index];
    if (entry === undefined) return undefined;
    return (side === "left" ? entry.left : entry.right) ?? undefined;
  };
  return new Proxy([] as MempoolTransaction[], {
    get(array, property, receiver) {
      if (property === "length") return entries.length;
      if (property === Symbol.iterator) {
        return function* (): IterableIterator<MempoolTransaction> {
          for (let index = 0; index < entries.length; index += 1) {
            const transaction = transactionAt(index);
            if (transaction !== undefined) yield transaction;
          }
        };
      }
      const index = arrayIndex(property);
      return index === null
        ? Reflect.get(array, property, receiver)
        : transactionAt(index);
    },
    has(array, property) {
      const index = arrayIndex(property);
      return index === null
        ? Reflect.has(array, property)
        : index >= 0 && index < entries.length;
    },
    getOwnPropertyDescriptor(array, property) {
      const index = arrayIndex(property);
      if (index !== null && index < entries.length) {
        return {
          configurable: true,
          enumerable: true,
          writable: false,
          value: transactionAt(index),
        };
      }
      return Reflect.getOwnPropertyDescriptor(array, property);
    },
  });
};

export const comparisonDistributionTransactions = (
  current: CurrentComparison,
  side: ComparisonSide,
  scope: ComparisonDistributionScope,
): MempoolTransaction[] => {
  if (scope === "all") return current[side].snapshot.transactions;
  if (scope === "common") return presentTransactionView(current.common, side);
  if (scope === "left_only") {
    return side === "left"
      ? presentTransactionView(current.left_only, side)
      : [];
  }
  return side === "right"
    ? presentTransactionView(current.right_only, side)
    : [];
};

export const comparisonDistributionScopeSuffix = (
  scope: ComparisonDistributionScope,
): string =>
  scope === "all"
    ? ""
    : scope === "common"
      ? " · present in both"
      : scope === "left_only"
        ? " · only in Source A"
        : " · only in Source B";
