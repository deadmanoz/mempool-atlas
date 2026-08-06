// Fixture Atlas API server for frontend development without a live Bitcoin
// node.
//
// This is a DEVELOPMENT-ONLY tool. It is never built, packaged, or served in
// production; nothing under web/dev/ is part of the Vite build or npm
// package. It serves `/api/v1/sources` and `/api/v1/sources/:id/mempool` on
// 127.0.0.1:3101 with three deterministic synthetic sources (alpha, beta,
// gamma), so `web/vite.config.ts`'s existing proxy from `/api` to that origin
// lets `npm --prefix web run dev` (or `just web-dev`) run the complete
// frontend without any Bitcoin RPC endpoint configured.
//
// Payloads are generated to satisfy the structural validation `web/src/api.ts`
// performs on every response, including: classification summary counts that
// reconcile against per-transaction classifier states and label sets, a
// `knots_bip110` projection whose `status`/`primary_rule`/`violated_rules`/
// `unknown_rules` stay consistent with the paired `knots_bip110` classifier
// result, tier-1 membership facts (weight, ancestor/descendant counts and
// virtual sizes, `ancestor_fee_sats`) that stay within the bounds the
// validator enforces, and `structure` being non-null exactly when
// classifier results are present (the structure/classifications coupling).
// Run with `just web-fixtures` or `node web/dev/fixture-server.mjs`, then run
// `just web-dev` (or `npm --prefix web run dev`) in a second terminal.
import { createServer } from "node:http";

const mulberry32 = (seed) => {
  let a = seed >>> 0;
  return () => {
    a |= 0;
    a = (a + 0x6d2b79f5) | 0;
    let t = Math.imul(a ^ (a >>> 15), 1 | a);
    t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t;
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
};

const hex64 = (rng) => {
  let out = "";
  for (let i = 0; i < 64; i += 1) {
    out += "0123456789abcdef"[Math.floor(rng() * 16)];
  }
  return out;
};

const label = (key, name, description) => ({ key, label: name, description });

const CATALOG = [
  {
    id: "transaction_properties",
    version: "1.2.0",
    title: "Transaction properties",
    methodology: "exact",
    semantics: "multi_label",
    required_facts: ["raw_transaction", "input_script_pubkeys"],
    labels: [
      label("version_1", "Version 1", "Transaction version 1."),
      label("version_2", "Version 2", "Transaction version 2."),
      label("signals_rbf", "Signals RBF", "Signals BIP-125 replaceability."),
      label("has_witness", "Has witness", "Carries segwit witness data."),
      label("p2pkh", "P2PKH", "Pay-to-public-key-hash script."),
      label("p2sh", "P2SH", "Pay-to-script-hash script."),
      label("p2wpkh", "P2WPKH", "Native segwit v0 key path."),
      label("p2wsh", "P2WSH", "Native segwit v0 script path."),
      label("p2tr", "P2TR", "Taproot output."),
      label("p2a", "P2A", "Pay-to-anchor output."),
      label("op_return", "OP_RETURN", "Data-carrier output."),
      label("unknown_script", "Other script", "Unrecognized script family."),
    ],
  },
  {
    id: "transaction_shape",
    version: "0.9.0",
    title: "Transaction shape",
    methodology: "heuristic",
    semantics: "multi_label",
    required_facts: ["raw_transaction"],
    labels: [
      label("payment", "Payment", "Small in, small out."),
      label("consolidation", "Consolidation", "Many inputs, few outputs."),
      label("batch_payout", "Batch payout", "Few inputs, many outputs."),
      label("self_transfer", "Self transfer", "Single input, single output."),
    ],
  },
  {
    id: "data_protocols",
    version: "0.4.1",
    title: "Data protocols",
    methodology: "fingerprint",
    semantics: "multi_label",
    required_facts: ["raw_transaction"],
    labels: [
      label("none", "No known protocol", "No recognized data fingerprint."),
      label("op_return_data", "OP_RETURN data", "Generic data carrier."),
      label("runes", "Runes", "Runes protocol fingerprint."),
      label("ordinals", "Ordinals", "Ordinals inscription fingerprint."),
    ],
  },
  {
    id: "knots_bip110",
    version: "1.0.0",
    title: "Knots BIP-110 policy",
    methodology: "policy",
    semantics: "rule_set",
    required_facts: ["raw_transaction", "prevout_scripts"],
    labels: [
      label("compatible", "Compatible", "Compatible with deployed policy."),
      label("violating", "Would violate", "Would violate deployed policy."),
      label("indeterminate", "Indeterminate", "Unresolved policy checks."),
      label("output_size", "Output size", "Rule 1 output size."),
      label("element_size", "Element size", "Rule 2 element size."),
      label("undefined_version", "Undefined version", "Rule 3."),
      label("taproot_annex", "Taproot annex", "Rule 4."),
      label("control_block_size", "Control block size", "Rule 5."),
      label("op_success", "OP_SUCCESS", "Rule 6."),
      label("tapscript_op_if", "Tapscript OP_IF", "Rule 7."),
    ],
  },
];

const PROFILES = [
  {
    weight: 0.34,
    labels: ["version_2", "has_witness", "p2wpkh"],
    vs: [110, 400],
  },
  {
    weight: 0.18,
    labels: ["version_2", "has_witness", "p2wsh"],
    vs: [150, 900],
  },
  {
    weight: 0.22,
    labels: ["version_2", "has_witness", "p2tr", "signals_rbf"],
    vs: [111, 650],
  },
  { weight: 0.1, labels: ["version_1", "p2pkh"], vs: [190, 1200] },
  { weight: 0.05, labels: ["version_2", "p2sh"], vs: [220, 2500] },
  {
    weight: 0.06,
    labels: ["version_2", "has_witness", "p2wpkh", "p2tr"],
    vs: [140, 3000],
  },
  {
    weight: 0.03,
    labels: ["version_2", "has_witness", "p2tr", "op_return"],
    vs: [130, 40000],
  },
  { weight: 0.02, labels: ["version_2", "p2a"], vs: [65, 120] },
];

const pick = (rng, entries) => {
  const total = entries.reduce((sum, entry) => sum + entry.weight, 0);
  let roll = rng() * total;
  for (const entry of entries) {
    roll -= entry.weight;
    if (roll <= 0) {
      return entry;
    }
  }
  return entries[entries.length - 1];
};

const feeRate = (rng) => {
  const roll = rng();
  if (roll < 0.45) return 1 + rng() * 3;
  if (roll < 0.8) return 4 + rng() * 20;
  if (roll < 0.95) return 24 + rng() * 80;
  return 100 + rng() * 380;
};

const result = (classifierId, state, labels, primary, missing) => ({
  classifier_id: classifierId,
  state,
  primary_label: primary,
  labels,
  missing_facts: missing,
  evidence: null,
});

const makeBody = (rng, observedAt) => {
  const profile = pick(rng, PROFILES);
  const vsize = Math.max(
    65,
    Math.round(profile.vs[0] + (profile.vs[1] - profile.vs[0]) * rng() ** 2.2),
  );
  const rate = feeRate(rng);
  const fee_sats = Math.max(vsize, Math.round(vsize * rate));
  const entered_at_ms = observedAt - Math.round(rng() ** 1.6 * 172_800_000);

  const hasRelatives = rng() < 0.18;
  const ancestorExtra = hasRelatives ? Math.round(rng() * 3) : 0;
  const descendantExtra =
    hasRelatives && rng() < 0.5 ? Math.round(rng() * 4) : 0;
  const tier1 = {
    weight: Math.max(1, vsize * 4 - Math.floor(rng() * 3)),
    ancestor_count: 1 + ancestorExtra,
    ancestor_vsize: vsize + ancestorExtra * Math.round(120 + rng() * 800),
    ancestor_fee_sats:
      fee_sats + ancestorExtra * Math.round(200 + rng() * 4_000),
    descendant_count: 1 + descendantExtra,
    descendant_vsize: vsize + descendantExtra * Math.round(120 + rng() * 800),
    replaceable: rng() < 0.35,
  };

  const unavailable = rng() < 0.02;
  if (unavailable) {
    return {
      vsize,
      ...tier1,
      fee_sats,
      entered_at_ms,
      structure: null,
      classifications: [],
      bip110: null,
    };
  }

  const partialProps = rng() < 0.07;
  const props = result(
    "transaction_properties",
    partialProps ? "partial" : "complete",
    profile.labels,
    profile.labels[0],
    partialProps ? ["input_script_pubkeys"] : [],
  );

  const shapeRoll = rng();
  const shapeLabels =
    shapeRoll < 0.55
      ? ["payment"]
      : shapeRoll < 0.72
        ? ["consolidation"]
        : shapeRoll < 0.86
          ? ["batch_payout"]
          : shapeRoll < 0.95
            ? ["self_transfer"]
            : ["payment", "batch_payout"];
  const shape = result(
    "transaction_shape",
    "complete",
    shapeLabels,
    shapeLabels[0],
    [],
  );

  const dataRoll = rng();
  const dataLabels =
    dataRoll < 0.82
      ? ["none"]
      : dataRoll < 0.9
        ? ["op_return_data"]
        : dataRoll < 0.96
          ? ["runes"]
          : ["ordinals"];
  const data = result(
    "data_protocols",
    "complete",
    dataLabels,
    dataLabels[0],
    [],
  );

  const policyRoll = rng();
  let bip110;
  let policy;
  if (policyRoll < 0.9) {
    bip110 = {
      status: "compatible",
      primary_rule: null,
      violated_rules: [],
      unknown_rules: [],
    };
    policy = result(
      "knots_bip110",
      "complete",
      ["compatible"],
      "compatible",
      [],
    );
  } else if (policyRoll < 0.96) {
    const violated =
      rng() < 0.7 ? ["element_size"] : ["output_size", "element_size"];
    const unknown = rng() < 0.25 ? ["op_success"] : [];
    bip110 = {
      status: "violating",
      primary_rule: violated[0],
      violated_rules: violated,
      unknown_rules: unknown,
    };
    policy = result(
      "knots_bip110",
      unknown.length === 0 ? "complete" : "partial",
      ["violating", ...violated],
      "violating",
      unknown.length === 0 ? [] : ["policy_facts"],
    );
  } else {
    const unknown = rng() < 0.5 ? ["op_success"] : ["tapscript_op_if"];
    bip110 = {
      status: "indeterminate",
      primary_rule: null,
      violated_rules: [],
      unknown_rules: unknown,
    };
    policy = result(
      "knots_bip110",
      "partial",
      ["indeterminate"],
      "indeterminate",
      ["policy_facts"],
    );
  }

  const hasWitness = profile.labels.includes("has_witness");
  const inputCount =
    shapeLabels[0] === "consolidation"
      ? 5 + Math.floor(rng() * 40)
      : 1 + Math.floor(rng() * 3);
  const outputCount =
    shapeLabels[0] === "batch_payout"
      ? 5 + Math.floor(rng() * 120)
      : shapeLabels[0] === "self_transfer"
        ? 1
        : 1 + Math.floor(rng() * 3);
  const opReturnBytes =
    dataLabels[0] === "op_return_data"
      ? 8 + Math.floor(rng() * 220)
      : dataLabels[0] === "runes"
        ? 12 + Math.floor(rng() * 40)
        : dataLabels[0] === "ordinals"
          ? 0
          : rng() < 0.03
            ? 4 + Math.floor(rng() * 60)
            : 0;
  const outputSats = Math.round(10_000 * Math.exp(rng() * 11));
  const structure = {
    input_count: inputCount,
    output_count: outputCount,
    op_return_bytes: opReturnBytes,
    output_sats: outputSats,
    witness_bytes: hasWitness
      ? Math.round(vsize * (0.5 + rng() * 1.2))
      : dataLabels[0] === "ordinals"
        ? Math.round(vsize * 2.5)
        : 0,
  };

  return {
    vsize,
    ...tier1,
    fee_sats,
    entered_at_ms,
    structure,
    classifications: [props, shape, data, policy],
    bip110,
  };
};

const buildSnapshot = (sourceId, sourceLabel, txids, seed, observedAt, tip) => {
  const rng = mulberry32(seed);
  const transactions = txids.map((txid) => {
    const wtxid = hex64(rng);
    return { txid, wtxid, ...makeBody(rng, observedAt) };
  });
  transactions.sort((a, b) => (a.txid < b.txid ? -1 : 1));

  const total_vsize = transactions.reduce((sum, t) => sum + t.vsize, 0);
  const statusCounts = {
    compatible: 0,
    violating: 0,
    indeterminate: 0,
    unclassified: 0,
  };
  const summaries = CATALOG.map((descriptor) => ({
    classifier_id: descriptor.id,
    complete_count: 0,
    partial_count: 0,
    unclassified_count: 0,
    label_counts: Object.fromEntries(
      descriptor.labels.map(({ key }) => [key, 0]),
    ),
  }));
  for (const transaction of transactions) {
    if (transaction.bip110 === null) {
      statusCounts.unclassified += 1;
    } else {
      statusCounts[transaction.bip110.status] += 1;
    }
    CATALOG.forEach((descriptor, index) => {
      const summary = summaries[index];
      const entry = transaction.classifications[index];
      if (entry === undefined) {
        summary.unclassified_count += 1;
        return;
      }
      summary[
        entry.state === "complete" ? "complete_count" : "partial_count"
      ] += 1;
      for (const key of entry.labels) {
        summary.label_counts[key] += 1;
      }
    });
  }

  const started = observedAt - 1_800;
  return {
    source_id: sourceId,
    source_label: sourceLabel,
    collection_started_at_ms: started,
    collection_completed_at_ms: observedAt,
    collection_duration_ms: observedAt - started,
    observed_at_ms: observedAt,
    classification_revision: 7,
    chain_tip: tip,
    transaction_count: transactions.length,
    total_vsize,
    classifier_catalog: CATALOG,
    classification_summaries: summaries,
    bip110_summary: {
      evaluator_id: "rdts-rules",
      evaluator_version: "1.0.0",
      scope: "knots_mempool_policy",
      compatible_count: statusCounts.compatible,
      violating_count: statusCounts.violating,
      indeterminate_count: statusCounts.indeterminate,
      unclassified_count: statusCounts.unclassified,
    },
    transactions,
  };
};

const poolRng = mulberry32(0xa71a5);
const pool = [...new Set(Array.from({ length: 1000 }, () => hex64(poolRng)))];
pool.sort();
const common = pool.slice(0, 560);
const alphaOnly = pool.slice(560, 700);
const betaOnly = pool.slice(700, 840);
const gammaTxids = pool.slice(300, 720);

const NOW = Date.now();
const TIP = { height: 917_432, hash: hex64(mulberry32(0x7ea)) };
const STALE_TIP = { height: 917_429, hash: hex64(mulberry32(0x7eb)) };

const snapshots = {
  alpha: buildSnapshot(
    "alpha",
    "bitcoind-alpha",
    [...common, ...alphaOnly],
    0x51ee7,
    NOW - 42_000,
    TIP,
  ),
  beta: buildSnapshot(
    "beta",
    "knots-beta",
    [...common, ...betaOnly],
    0xb0a17,
    NOW - 21_000,
    TIP,
  ),
  gamma: buildSnapshot(
    "gamma",
    "bitcoind-gamma",
    gammaTxids,
    0x9a44a,
    NOW - 1_260_000,
    STALE_TIP,
  ),
};

const AVAILABILITY = { alpha: "ready", beta: "ready", gamma: "stale" };
const LAST_ERROR = {
  alpha: null,
  beta: null,
  gamma: "transport error: connection refused (os error 61)",
};

const sourceSummary = (id) => {
  const snapshot = snapshots[id];
  const unclassified = snapshot.bip110_summary.unclassified_count;
  return {
    source_id: id,
    source_label: snapshot.source_label,
    availability: AVAILABILITY[id],
    poll_interval_seconds: 60,
    last_poll_started_at_ms: NOW - 15_000,
    snapshot_observed_at_ms: snapshot.observed_at_ms,
    chain_tip: snapshot.chain_tip,
    transaction_count: snapshot.transaction_count,
    total_vsize: snapshot.total_vsize,
    classification: {
      state: id === "gamma" ? "paused" : "complete",
      revision: snapshot.classification_revision,
      classified_count: snapshot.transaction_count - unclassified,
      unclassified_count: unclassified,
    },
    last_error: LAST_ERROR[id],
  };
};

const server = createServer((request, response) => {
  const url = new URL(request.url, "http://127.0.0.1");
  const respond = (status, body) => {
    const payload = JSON.stringify(body);
    response.writeHead(status, {
      "content-type": "application/json",
      "cache-control": "no-store",
    });
    response.end(payload);
  };
  if (url.pathname === "/api/v1/sources") {
    respond(200, { sources: ["alpha", "beta", "gamma"].map(sourceSummary) });
    return;
  }
  const snapshotMatch = url.pathname.match(
    /^\/api\/v1\/sources\/([a-z]+)\/mempool$/,
  );
  if (snapshotMatch !== null && snapshots[snapshotMatch[1]] !== undefined) {
    const id = snapshotMatch[1];
    respond(200, { source: sourceSummary(id), snapshot: snapshots[id] });
    return;
  }
  respond(404, { error: "not found" });
});

server.listen(3101, "127.0.0.1", () => {
  console.log("fixture atlas api on 127.0.0.1:3101");
  for (const [id, snapshot] of Object.entries(snapshots)) {
    console.log(
      `  ${id}: ${snapshot.transaction_count} tx, ${snapshot.total_vsize} vB`,
    );
  }
});
