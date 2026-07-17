//! Dev-seed generator: builds a deterministic synthetic mempool from
//! per-classification distribution presets and delivers it through the real
//! `POST /api/v1/events/batch` ingest path of a running atlas-server, so local
//! development exercises production ingest instead of writing SQL directly.
//!
//! Every synthetic transaction is a real, consensus-serializable
//! [`bitcoin::Transaction`] whose input and output shape is constructed to
//! satisfy its class's server-side heuristic rule. Each transaction emits two
//! events in order: a `p2p_transaction` observation carrying the computed
//! txid, wtxid, and raw transaction hex, then the RPC-style membership
//! evidence — a reconciled `present` fact triple whose vsize is the
//! constructed transaction's real vsize, or `mempool_added` for the
//! awaiting-RPC fraction. Server-side enrichment can therefore derive shape
//! facts and classifier verdicts from the raw bytes instead of trusting a
//! generator-side label.
//!
//! Txids derive deterministically from the seed while each run uses a fresh
//! source session, so re-running with the same seed refreshes the facts and
//! ages of the same transaction population instead of growing the mempool.
//!
//! The class mixture and log-normal fee presets are ported from the design
//! exploration (`Mempool Visualisations.dc.html`); output-value presets and
//! per-class script mixes shape the constructed transactions.

use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, bail};
use atlas_model::{
    CaptureGapCertainty, Evidence, IngestBatchRequest, IngestBatchResponse, IngestStatus,
    MAX_INGEST_BATCH_EVENTS, MempoolEntryFacts, NormalizedEvent, ReconciledMembership, SourceId,
    SourceSessionId,
};
use bitcoin::absolute::LockTime;
use bitcoin::consensus::encode::serialize_hex;
use bitcoin::hashes::Hash as _;
use bitcoin::transaction::Version;
use bitcoin::{
    Amount, OutPoint, PubkeyHash, ScriptBuf, ScriptHash, Sequence, Transaction, TxIn, TxOut, Txid,
    WPubkeyHash, WScriptHash, Witness, WitnessProgram, WitnessVersion,
};
use clap::Parser;

#[derive(Debug, Parser)]
#[command(name = "atlas-seed")]
#[command(about = "Seed a running atlas-server with a deterministic synthetic mempool")]
struct Cli {
    /// Base URL of the running atlas-server.
    #[arg(long, default_value = "http://127.0.0.1:3101")]
    server: String,
    /// Source ID to seed; must not collide with a real agent-owned source.
    #[arg(long)]
    source: String,
    /// Number of transactions to generate; each emits two events.
    #[arg(long, default_value_t = 20_000)]
    count: u64,
    /// Fraction of transactions left awaiting RPC facts.
    #[arg(long, default_value_t = 0.03)]
    awaiting_fraction: f64,
    /// Deterministic generator seed; also part of the session identity.
    #[arg(long, default_value_t = 1)]
    seed: u64,
    /// Also record one possible-loss capture gap so honesty surfaces render.
    #[arg(long, default_value_t = false)]
    capture_gap: bool,
}

/// Classification a constructed transaction is shaped to match. Ordering
/// mirrors the server-side first-match-wins heuristic rules.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
enum TxClass {
    Payment,
    Consolidation,
    Batch,
    Coinjoin,
    Data,
    Lightning,
    Unknown,
}

/// Script forms drawn for non-special outputs.
#[derive(Clone, Copy, Debug)]
enum ScriptKind {
    P2wpkh,
    P2tr,
    P2pkh,
    P2sh,
    P2wsh,
}

/// Log-normal medians and sigmas per classification. Fee presets are ported
/// from the design exploration's sample presets; total-output-value presets
/// (in BTC) shape the constructed outputs. Vsize is no longer a preset: it is
/// the real vsize of the constructed transaction.
struct ClassPreset {
    class: TxClass,
    weight: f64,
    fee_median: f64,
    fee_sigma: f64,
    value_median_btc: f64,
    value_sigma: f64,
    age_exponent: f64,
}

const PRESETS: [ClassPreset; 7] = [
    ClassPreset {
        class: TxClass::Payment,
        weight: 0.30,
        fee_median: 14.0,
        fee_sigma: 0.8,
        value_median_btc: 0.015,
        value_sigma: 1.2,
        age_exponent: 2.4,
    },
    ClassPreset {
        class: TxClass::Consolidation,
        weight: 0.09,
        fee_median: 4.0,
        fee_sigma: 0.7,
        value_median_btc: 0.6,
        value_sigma: 1.0,
        age_exponent: 3.4,
    },
    ClassPreset {
        class: TxClass::Batch,
        weight: 0.07,
        fee_median: 9.0,
        fee_sigma: 0.6,
        value_median_btc: 1.2,
        value_sigma: 0.9,
        age_exponent: 2.6,
    },
    ClassPreset {
        class: TxClass::Coinjoin,
        weight: 0.03,
        fee_median: 7.0,
        fee_sigma: 0.5,
        value_median_btc: 2.5,
        value_sigma: 0.6,
        age_exponent: 2.2,
    },
    ClassPreset {
        class: TxClass::Data,
        weight: 0.13,
        fee_median: 22.0,
        fee_sigma: 1.1,
        value_median_btc: 0.0008,
        value_sigma: 1.0,
        age_exponent: 2.0,
    },
    ClassPreset {
        class: TxClass::Lightning,
        weight: 0.09,
        fee_median: 11.0,
        fee_sigma: 0.6,
        value_median_btc: 0.04,
        value_sigma: 0.9,
        age_exponent: 2.3,
    },
    ClassPreset {
        class: TxClass::Unknown,
        weight: 0.29,
        fee_median: 6.0,
        fee_sigma: 1.3,
        value_median_btc: 0.01,
        value_sigma: 1.3,
        age_exponent: 2.8,
    },
];

/// Weighted script mix for a class's non-special outputs.
const fn script_mix(class: TxClass) -> &'static [(ScriptKind, f64)] {
    match class {
        TxClass::Payment => &[
            (ScriptKind::P2wpkh, 0.60),
            (ScriptKind::P2tr, 0.25),
            (ScriptKind::P2sh, 0.10),
            (ScriptKind::P2pkh, 0.05),
        ],
        TxClass::Consolidation => &[
            (ScriptKind::P2wpkh, 0.70),
            (ScriptKind::P2sh, 0.15),
            (ScriptKind::P2pkh, 0.15),
        ],
        TxClass::Batch => &[
            (ScriptKind::P2wpkh, 0.50),
            (ScriptKind::P2tr, 0.20),
            (ScriptKind::P2sh, 0.15),
            (ScriptKind::P2pkh, 0.15),
        ],
        TxClass::Coinjoin => &[(ScriptKind::P2wpkh, 0.50), (ScriptKind::P2tr, 0.50)],
        TxClass::Data => &[(ScriptKind::P2tr, 0.70), (ScriptKind::P2wpkh, 0.30)],
        TxClass::Lightning => &[(ScriptKind::P2wpkh, 0.60), (ScriptKind::P2wsh, 0.40)],
        TxClass::Unknown => &[
            (ScriptKind::P2wpkh, 0.35),
            (ScriptKind::P2tr, 0.20),
            (ScriptKind::P2pkh, 0.15),
            (ScriptKind::P2sh, 0.15),
            (ScriptKind::P2wsh, 0.15),
        ],
    }
}

const MAX_AGE_MS: f64 = 5.0 * 86_400_000.0;
const SATS_PER_BTC: f64 = 100_000_000.0;
/// Minimum total output value drawn for one transaction.
const MIN_TOTAL_SATS: u64 = 1_000;
/// Minimum per-output value for ordinary outputs. Keeping this above the
/// 330-sat lightning anchor value guarantees ordinary outputs never trip the
/// lightning heuristic in another class.
const MIN_OUTPUT_SATS: u64 = 546;
/// Exact value of the synthetic lightning anchor output.
const ANCHOR_SATS: u64 = 330;
/// Length of the single dummy witness element on every input.
const WITNESS_ELEMENT_LEN: usize = 107;
/// `OP_FALSE OP_IF OP_PUSHBYTES_3 "ord"` — the inscription envelope marker
/// the server-side data heuristic searches witness elements for.
const INSCRIPTION_MARKER: [u8; 6] = [0x00, 0x63, 0x03, 0x6f, 0x72, 0x64];

/// xorshift64* — small, deterministic, and dependency-free.
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Self(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1)
    }

    fn next_u64(&mut self) -> u64 {
        let mut state = self.0;
        state ^= state >> 12;
        state ^= state << 25;
        state ^= state >> 27;
        self.0 = state;
        state.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    fn uniform(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1_u64 << 53) as f64
    }

    /// Approximate standard normal, matching the design generator's shape.
    fn normal(&mut self) -> f64 {
        self.uniform() + self.uniform() + self.uniform() - 1.5
    }

    /// Uniform draw from the inclusive range. Modulo bias is irrelevant here.
    fn range_inclusive(&mut self, low: u64, high: u64) -> u64 {
        low + self.next_u64() % (high - low + 1)
    }

    fn fill_bytes(&mut self, out: &mut [u8]) {
        for chunk in out.chunks_mut(8) {
            let word = self.next_u64().to_le_bytes();
            chunk.copy_from_slice(&word[..chunk.len()]);
        }
    }

    fn byte_array<const N: usize>(&mut self) -> [u8; N] {
        let mut bytes = [0_u8; N];
        self.fill_bytes(&mut bytes);
        bytes
    }
}

fn log_normal(rng: &mut Rng, median: f64, sigma: f64) -> f64 {
    (median.ln() + rng.normal() * sigma).exp()
}

/// Rewrites any accidental inscription-marker occurrence in random witness
/// bytes so only the data class carries the marker. Flipping the first byte
/// of a match cannot create a new match because `0xff` appears nowhere in
/// the marker.
fn scrub_marker(bytes: &mut [u8]) {
    loop {
        let Some(position) = bytes
            .windows(INSCRIPTION_MARKER.len())
            .position(|window| window == INSCRIPTION_MARKER)
        else {
            return;
        };
        bytes[position] ^= 0xff;
    }
}

/// A synthetic input spending a deterministic dummy outpoint. The single
/// witness element makes the wtxid differ from the txid; the data class
/// embeds the inscription marker in it.
fn dummy_input(rng: &mut Rng, embed_marker: bool) -> TxIn {
    let previous_txid = Txid::from_byte_array(rng.byte_array());
    let mut element = [0_u8; WITNESS_ELEMENT_LEN];
    rng.fill_bytes(&mut element);
    if embed_marker {
        element[20..20 + INSCRIPTION_MARKER.len()].copy_from_slice(&INSCRIPTION_MARKER);
    } else {
        scrub_marker(&mut element);
    }
    TxIn {
        previous_output: OutPoint {
            txid: previous_txid,
            vout: 0,
        },
        script_sig: ScriptBuf::new(),
        sequence: Sequence::MAX,
        witness: Witness::from_slice(&[element]),
    }
}

fn dummy_inputs(rng: &mut Rng, count: usize) -> Vec<TxIn> {
    (0..count).map(|_| dummy_input(rng, false)).collect()
}

fn p2tr_script(rng: &mut Rng) -> ScriptBuf {
    let program = WitnessProgram::new(WitnessVersion::V1, &rng.byte_array::<32>())
        .expect("a 32-byte v1 witness program is always valid");
    ScriptBuf::new_witness_program(&program)
}

fn draw_script(rng: &mut Rng, class: TxClass) -> ScriptBuf {
    let mix = script_mix(class);
    let draw = rng.uniform();
    let mut cumulative = 0.0;
    let mut kind = mix[mix.len() - 1].0;
    for (candidate, weight) in mix {
        cumulative += weight;
        if draw <= cumulative {
            kind = *candidate;
            break;
        }
    }
    match kind {
        ScriptKind::P2wpkh => {
            ScriptBuf::new_p2wpkh(&WPubkeyHash::from_byte_array(rng.byte_array()))
        }
        ScriptKind::P2tr => p2tr_script(rng),
        ScriptKind::P2pkh => ScriptBuf::new_p2pkh(&PubkeyHash::from_byte_array(rng.byte_array())),
        ScriptKind::P2sh => ScriptBuf::new_p2sh(&ScriptHash::from_byte_array(rng.byte_array())),
        ScriptKind::P2wsh => ScriptBuf::new_p2wsh(&WScriptHash::from_byte_array(rng.byte_array())),
    }
}

/// Ordinary outputs with pairwise-distinct values (base plus index), all at
/// least [`MIN_OUTPUT_SATS`] so none can be a 330-sat lightning anchor.
fn ordinary_outputs(rng: &mut Rng, class: TxClass, count: usize, total_sats: u64) -> Vec<TxOut> {
    let base = (total_sats / count.max(1) as u64).max(MIN_OUTPUT_SATS);
    (0..count)
        .map(|index| TxOut {
            value: Amount::from_sat(base + index as u64),
            script_pubkey: draw_script(rng, class),
        })
        .collect()
}

/// Constructs a real transaction whose shape satisfies exactly its class's
/// server-side heuristic rule and no earlier rule in the first-match-wins
/// order (coinjoin, consolidation, batch, data, lightning, payment).
fn construct_transaction(rng: &mut Rng, class: TxClass, total_sats: u64) -> Transaction {
    let (input, output) = match class {
        TxClass::Payment => {
            // <=3 inputs and 2 outputs: matches only the payment rule.
            let input_count = rng.range_inclusive(1, 3) as usize;
            (
                dummy_inputs(rng, input_count),
                ordinary_outputs(rng, class, 2, total_sats),
            )
        }
        TxClass::Consolidation => {
            // >=10 inputs into one output; too few outputs for coinjoin.
            let input_count = rng.range_inclusive(10, 80) as usize;
            (
                dummy_inputs(rng, input_count),
                ordinary_outputs(rng, class, 1, total_sats),
            )
        }
        TxClass::Batch => {
            // >=20 outputs with pairwise-distinct values, so even a 5-input
            // draw cannot satisfy the coinjoin equal-value requirement.
            let input_count = rng.range_inclusive(2, 5) as usize;
            let output_count = rng.range_inclusive(20, 120) as usize;
            (
                dummy_inputs(rng, input_count),
                ordinary_outputs(rng, class, output_count, total_sats),
            )
        }
        TxClass::Coinjoin => {
            // n >= 5 inputs, n outputs, ceil(60%) of them at one shared
            // non-zero denomination; the change values stay strictly above
            // the denomination so they never widen the equal-value set.
            let participant_count = rng.range_inclusive(5, 60) as usize;
            let inputs = dummy_inputs(rng, participant_count);
            let denomination_count = (participant_count * 6).div_ceil(10);
            let denomination = (total_sats / participant_count as u64).max(MIN_OUTPUT_SATS);
            let outputs = (0..participant_count)
                .map(|index| {
                    let value = if index < denomination_count {
                        denomination
                    } else {
                        denomination + 1 + (index - denomination_count) as u64
                    };
                    TxOut {
                        value: Amount::from_sat(value),
                        script_pubkey: draw_script(rng, class),
                    }
                })
                .collect();
            (inputs, outputs)
        }
        TxClass::Data => {
            // Both data signals: an OP_RETURN output and an input witness
            // element embedding the inscription marker.
            let inputs = vec![dummy_input(rng, true)];
            let mut outputs = vec![TxOut {
                value: Amount::from_sat(0),
                script_pubkey: ScriptBuf::new_op_return(rng.byte_array::<32>()),
            }];
            if rng.uniform() < 0.5 {
                outputs.extend(ordinary_outputs(rng, class, 1, total_sats));
            }
            (inputs, outputs)
        }
        TxClass::Lightning => {
            // One exact 330-sat P2TR anchor among ordinary outputs; no
            // OP_RETURN and no marker, so the data rule cannot fire first.
            let inputs = vec![dummy_input(rng, false)];
            let output_count = rng.range_inclusive(3, 4) as usize;
            let anchor_index = rng.range_inclusive(0, output_count as u64 - 1) as usize;
            let mut outputs = ordinary_outputs(rng, class, output_count - 1, total_sats);
            outputs.insert(
                anchor_index,
                TxOut {
                    value: Amount::from_sat(ANCHOR_SATS),
                    script_pubkey: p2tr_script(rng),
                },
            );
            (inputs, outputs)
        }
        TxClass::Unknown => {
            // 4 inputs and 4 distinct-value outputs match no rule: too many
            // inputs and outputs for payment, too few for everything else.
            (
                dummy_inputs(rng, 4),
                ordinary_outputs(rng, class, 4, total_sats),
            )
        }
    };
    Transaction {
        version: Version::TWO,
        lock_time: LockTime::ZERO,
        input,
        output,
    }
}

struct SyntheticTransaction {
    #[cfg_attr(not(test), allow(dead_code))]
    class: TxClass,
    txid: String,
    wtxid: String,
    raw_transaction_hex: String,
    peer_id: u64,
    awaiting_rpc: bool,
    facts: MempoolEntryFacts,
}

fn synthesize(rng: &mut Rng, awaiting_fraction: f64, now_ms: u64) -> SyntheticTransaction {
    let class_draw = rng.uniform();
    let mut cumulative = 0.0;
    let mut preset = &PRESETS[PRESETS.len() - 1];
    for candidate in &PRESETS {
        cumulative += candidate.weight;
        if class_draw <= cumulative {
            preset = candidate;
            break;
        }
    }

    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let total_sats = ((log_normal(rng, preset.value_median_btc, preset.value_sigma) * SATS_PER_BTC)
        .round() as u64)
        .max(MIN_TOTAL_SATS);
    let transaction = construct_transaction(rng, preset.class, total_sats);
    let vsize = u64::try_from(transaction.vsize()).expect("vsize fits in u64");

    let feerate = log_normal(rng, preset.fee_median, preset.fee_sigma).max(0.4);
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        clippy::cast_precision_loss
    )]
    let fee_sats = ((feerate * vsize as f64).round() as u64).max(1);
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let age_ms = (rng.uniform().powf(preset.age_exponent) * MAX_AGE_MS) as u64;
    let awaiting_rpc = rng.uniform() < awaiting_fraction;
    let peer_id = rng.next_u64() % 24;

    SyntheticTransaction {
        class: preset.class,
        txid: transaction.compute_txid().to_string(),
        wtxid: transaction.compute_wtxid().to_string(),
        raw_transaction_hex: serialize_hex(&transaction),
        peer_id,
        awaiting_rpc,
        facts: MempoolEntryFacts {
            vsize,
            fee_sats,
            entered_at_ms: now_ms.saturating_sub(age_ms),
        },
    }
}

fn seed_events(cli: &Cli, now_ms: u64) -> anyhow::Result<Vec<NormalizedEvent>> {
    let source = SourceId::new(cli.source.clone())?;
    // Facts embed run-time ages, so a re-run must be a new session: replaying
    // the previous session's event identities with fresh facts would be a
    // conflicting-event rejection, not a refresh.
    let session = SourceSessionId::new(format!("seed-{}-{now_ms}", cli.seed))?;
    let mut rng = Rng::new(cli.seed);
    let mut sequence = 0;
    let mut next_event = |evidence| -> anyhow::Result<NormalizedEvent> {
        sequence += 1;
        Ok(NormalizedEvent::new(
            source.clone(),
            session.clone(),
            sequence,
            now_ms,
            now_ms,
            evidence,
        )?)
    };

    let mut events = Vec::new();
    if cli.capture_gap {
        events.push(next_event(Evidence::CaptureGap {
            input: "atlas_seed".to_owned(),
            reason: "synthetic_gap".to_owned(),
            certainty: CaptureGapCertainty::PossibleLoss,
        })?);
    }
    for _ in 0..cli.count {
        let transaction = synthesize(&mut rng, cli.awaiting_fraction, now_ms);
        events.push(next_event(Evidence::P2pTransaction {
            txid: transaction.txid.clone(),
            wtxid: transaction.wtxid,
            raw_transaction_hex: Some(transaction.raw_transaction_hex),
            peer_id: Some(transaction.peer_id),
            inbound: Some(true),
        })?);
        let evidence = if transaction.awaiting_rpc {
            Evidence::MempoolAdded {
                txid: transaction.txid,
            }
        } else {
            Evidence::MempoolReconciled {
                txid: transaction.txid,
                membership: ReconciledMembership::Present {
                    facts: transaction.facts,
                },
            }
        };
        events.push(next_event(evidence)?);
    }
    Ok(events)
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    if !(0.0..=1.0).contains(&cli.awaiting_fraction) {
        bail!("--awaiting-fraction must be between 0 and 1");
    }
    let now_ms = u64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .context("system clock before unix epoch")?
            .as_millis(),
    )
    .context("system clock beyond representable milliseconds")?;

    let events = seed_events(&cli, now_ms)?;
    let client = reqwest::blocking::Client::new();
    let batch_url = format!("{}/api/v1/events/batch", cli.server.trim_end_matches('/'));
    let (mut applied, mut duplicate) = (0_u64, 0_u64);
    for chunk in events.chunks(MAX_INGEST_BATCH_EVENTS) {
        let response = client
            .post(&batch_url)
            .json(&IngestBatchRequest {
                events: chunk.to_vec(),
            })
            .send()
            .with_context(|| format!("delivering batch to {batch_url}"))?;
        if response.status().as_u16() != 202 {
            bail!(
                "batch ingest returned {}: {}",
                response.status(),
                response.text().unwrap_or_default()
            );
        }
        let acknowledgements: IngestBatchResponse =
            response.json().context("parsing batch acknowledgements")?;
        if acknowledgements.acknowledgements.len() != chunk.len() {
            bail!("server acknowledged a different number of events than delivered");
        }
        for acknowledgement in acknowledgements.acknowledgements {
            match acknowledgement.status {
                IngestStatus::Applied => applied += 1,
                IngestStatus::Duplicate => duplicate += 1,
            }
        }
    }
    println!(
        "seeded source {}: {applied} applied, {duplicate} duplicate",
        cli.source
    );

    let summary_url = format!(
        "{}/api/v1/sources/{}/mempool/summary",
        cli.server.trim_end_matches('/'),
        cli.source
    );
    let summary: serde_json::Value = client
        .get(&summary_url)
        .send()
        .with_context(|| format!("fetching {summary_url}"))?
        .json()
        .context("parsing summary")?;
    println!(
        "summary totals: {}",
        serde_json::to_string_pretty(&summary["totals"]).expect("totals are JSON")
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use bitcoin::consensus::encode::deserialize_hex;

    use super::*;

    const NOW_MS: u64 = 1_752_710_400_000;

    fn cli(count: u64, capture_gap: bool) -> Cli {
        Cli {
            server: "http://127.0.0.1:3101".to_owned(),
            source: "seed-node".to_owned(),
            count,
            awaiting_fraction: 0.03,
            seed: 7,
            capture_gap,
        }
    }

    /// Local mirror of the server-side first-match-wins heuristic rules.
    fn heuristic_class(transaction: &Transaction) -> TxClass {
        let input_count = transaction.input.len();
        let output_count = transaction.output.len();
        if input_count >= 5 && output_count >= 5 {
            let mut value_counts: HashMap<u64, usize> = HashMap::new();
            for output in &transaction.output {
                *value_counts.entry(output.value.to_sat()).or_insert(0) += 1;
            }
            if value_counts
                .iter()
                .any(|(value, count)| *value > 0 && *count >= 3)
            {
                return TxClass::Coinjoin;
            }
        }
        if input_count >= 10 && output_count <= 2 {
            return TxClass::Consolidation;
        }
        if output_count >= 20 {
            return TxClass::Batch;
        }
        let has_op_return = transaction
            .output
            .iter()
            .any(|output| output.script_pubkey.is_op_return());
        let has_marker = transaction.input.iter().any(|input| {
            input.witness.iter().any(|element| {
                element
                    .windows(INSCRIPTION_MARKER.len())
                    .any(|window| window == INSCRIPTION_MARKER)
            })
        });
        if has_op_return || has_marker {
            return TxClass::Data;
        }
        if transaction.output.iter().any(|output| {
            output.value.to_sat() == ANCHOR_SATS
                && (output.script_pubkey.is_p2wsh() || output.script_pubkey.is_p2tr())
        }) {
            return TxClass::Lightning;
        }
        if input_count <= 3 && output_count <= 2 {
            return TxClass::Payment;
        }
        TxClass::Unknown
    }

    #[test]
    fn generation_is_deterministic_and_valid() {
        let first = seed_events(&cli(300, true), NOW_MS).expect("events");
        let second = seed_events(&cli(300, true), NOW_MS).expect("events");
        assert_eq!(first, second);
        assert_eq!(first.len(), 601, "capture gap plus two events per tx");
        for event in &first {
            event.validate().expect("seed events must be valid");
        }
        assert!(matches!(first[0].evidence, Evidence::CaptureGap { .. }));
        let awaiting = first
            .iter()
            .filter(|event| matches!(event.evidence, Evidence::MempoolAdded { .. }))
            .count();
        assert!(awaiting > 0, "some entries should await RPC facts");
    }

    #[test]
    fn p2p_evidence_precedes_membership_evidence_for_each_transaction() {
        let events = seed_events(&cli(200, false), NOW_MS).expect("events");
        assert_eq!(events.len(), 400);
        for pair in events.chunks(2) {
            let Evidence::P2pTransaction {
                txid: p2p_txid,
                wtxid,
                raw_transaction_hex,
                peer_id,
                inbound,
            } = &pair[0].evidence
            else {
                panic!("expected p2p_transaction first, got {:?}", pair[0].evidence);
            };
            assert_ne!(p2p_txid, wtxid, "witnesses must distinguish the wtxid");
            assert!(raw_transaction_hex.is_some());
            assert!(peer_id.is_some());
            assert_eq!(*inbound, Some(true));
            match &pair[1].evidence {
                Evidence::MempoolAdded { txid } | Evidence::MempoolReconciled { txid, .. } => {
                    assert_eq!(txid, p2p_txid);
                }
                other => panic!("expected membership evidence second, got {other:?}"),
            }
        }
    }

    #[test]
    fn raw_transactions_round_trip_with_matching_identifiers_and_vsize() {
        let events = seed_events(&cli(150, false), NOW_MS).expect("events");
        for pair in events.chunks(2) {
            let Evidence::P2pTransaction {
                txid,
                wtxid,
                raw_transaction_hex,
                ..
            } = &pair[0].evidence
            else {
                panic!("expected p2p_transaction first");
            };
            let hex = raw_transaction_hex.as_deref().expect("raw hex present");
            let transaction: Transaction = deserialize_hex(hex).expect("raw hex round-trips");
            assert_eq!(&transaction.compute_txid().to_string(), txid);
            assert_eq!(&transaction.compute_wtxid().to_string(), wtxid);
            if let Evidence::MempoolReconciled {
                membership: ReconciledMembership::Present { facts },
                ..
            } = &pair[1].evidence
            {
                assert_eq!(
                    facts.vsize,
                    u64::try_from(transaction.vsize()).expect("vsize fits in u64"),
                    "reconciled vsize must be the constructed transaction's real vsize"
                );
                assert!(facts.fee_sats >= 1);
                assert!(facts.entered_at_ms <= NOW_MS);
            }
        }
    }

    #[test]
    fn constructed_transactions_match_their_class_heuristics() {
        let mut rng = Rng::new(11);
        let mut counts: HashMap<TxClass, usize> = HashMap::new();
        for _ in 0..700 {
            let synthetic = synthesize(&mut rng, 0.0, NOW_MS);
            let transaction: Transaction =
                deserialize_hex(&synthetic.raw_transaction_hex).expect("raw hex round-trips");
            assert_eq!(
                heuristic_class(&transaction),
                synthetic.class,
                "txid {} was constructed for {:?} but matches another rule first",
                synthetic.txid,
                synthetic.class,
            );
            *counts.entry(synthetic.class).or_insert(0) += 1;
        }
        for preset in &PRESETS {
            assert!(
                counts.get(&preset.class).copied().unwrap_or(0) > 0,
                "class {:?} never generated",
                preset.class
            );
        }
    }

    #[test]
    fn class_weights_cover_the_unit_interval() {
        let total: f64 = PRESETS.iter().map(|preset| preset.weight).sum();
        assert!((total - 1.0).abs() < 1e-9);
        for preset in &PRESETS {
            let mix_total: f64 = script_mix(preset.class)
                .iter()
                .map(|(_, weight)| weight)
                .sum();
            assert!(
                (mix_total - 1.0).abs() < 1e-9,
                "script mix for {:?} must sum to 1",
                preset.class
            );
        }
    }
}
