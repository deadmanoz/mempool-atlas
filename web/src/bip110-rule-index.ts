import type { MempoolTransaction, RuleId } from "./types";
import { RULE_IDS } from "./types";
import {
  sortTransactionViewByVsize,
  transactionIndexView,
} from "./transaction-view";

export interface Bip110RulePopulationSummary {
  rule: RuleId;
  count: number;
  vsize: number;
  totalShare: number;
}

export interface Bip110RulePopulation extends Bip110RulePopulationSummary {
  transactions: MempoolTransaction[];
}

interface Bip110RuleAccumulator {
  rows: number[];
  vsize: number;
}

interface Bip110RulePopulationIndex {
  rows: Uint32Array | null;
  count: number;
  vsize: number;
  population: Bip110RulePopulation | null;
}

export type Bip110RuleIndexBuilder = Map<RuleId, Bip110RuleAccumulator>;

const ruleIndexes = new WeakMap<
  readonly MempoolTransaction[],
  Map<RuleId, Bip110RulePopulationIndex>
>();

export const createBip110RuleIndexBuilder = (): Bip110RuleIndexBuilder =>
  new Map(RULE_IDS.map((rule) => [rule, { rows: [], vsize: 0 }] as const));

export const addTransactionToBip110RuleIndex = (
  builder: Bip110RuleIndexBuilder,
  transaction: MempoolTransaction,
  row: number,
): void => {
  const violatedRules = transaction.bip110?.violated_rules ?? [];
  for (let index = 0; index < violatedRules.length; index += 1) {
    const rule = violatedRules[index];
    if (rule === undefined || violatedRules.indexOf(rule) !== index) continue;
    const accumulator = builder.get(rule);
    if (accumulator === undefined) continue;
    accumulator.rows.push(row);
    accumulator.vsize += transaction.vsize;
  }
};

export const cacheBip110RuleIndex = (
  transactions: readonly MempoolTransaction[],
  builder: Bip110RuleIndexBuilder,
): void => {
  ruleIndexes.set(
    transactions,
    new Map(
      [...builder].map(([rule, { rows, vsize }]) => [
        rule,
        {
          rows: Uint32Array.from(rows),
          count: rows.length,
          vsize,
          population: null,
        },
      ]),
    ),
  );
};

const ruleIndex = (
  transactions: readonly MempoolTransaction[],
): Map<RuleId, Bip110RulePopulationIndex> => {
  const cached = ruleIndexes.get(transactions);
  if (cached !== undefined) return cached;

  const builder = createBip110RuleIndexBuilder();
  for (let row = 0; row < transactions.length; row += 1) {
    const transaction = transactions[row];
    if (transaction !== undefined) {
      addTransactionToBip110RuleIndex(builder, transaction, row);
    }
  }
  cacheBip110RuleIndex(transactions, builder);
  return ruleIndexes.get(transactions)!;
};

export const bip110RulePopulationSummary = (
  transactions: readonly MempoolTransaction[],
  rule: RuleId,
): Bip110RulePopulationSummary => {
  const index = ruleIndex(transactions).get(rule);
  const count = index?.count ?? 0;
  return {
    rule,
    count,
    vsize: index?.vsize ?? 0,
    totalShare: transactions.length === 0 ? 0 : count / transactions.length,
  };
};

export const bip110RulePopulation = (
  transactions: readonly MempoolTransaction[],
  rule: RuleId,
): Bip110RulePopulation => {
  const index = ruleIndex(transactions).get(rule);
  if (index === undefined) {
    return { rule, transactions: [], count: 0, vsize: 0, totalShare: 0 };
  }
  if (index.population !== null) return index.population;
  if (index.rows === null) {
    throw new Error(`BIP-110 rule population ${rule} is unavailable`);
  }
  index.population = {
    ...bip110RulePopulationSummary(transactions, rule),
    transactions: sortTransactionViewByVsize(
      transactionIndexView(transactions, index.rows),
    ),
  };
  index.rows = null;
  return index.population;
};
