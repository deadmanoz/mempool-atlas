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
//!
//! The data class is constructed as a real Taproot script-path inscription
//! reveal (envelope tapscript plus control block), so its transactions
//! honestly classify `data` in the behavior taxonomy,
//! `conditional_in_tapscript` in the `bip110` taxonomy, and `inscription` —
//! or, for the roughly 15% carrying a BRC-20 JSON payload, `brc20` — in the
//! `data_protocol` taxonomy.
//!
//! Every [`BIP110_SPECIAL_INTERVAL`]-th transaction (2% of the population)
//! is a dedicated BIP-110 construction cycling through [`Bip110Kind`], and
//! every [`PROTOCOL_SPECIAL_INTERVAL`]-th transaction offset by
//! [`PROTOCOL_SPECIAL_OFFSET`] (another 2%, disjoint from the BIP-110 cycle
//! by construction: multiples of 50 versus odd multiples of 25) is a
//! dedicated data-protocol construction cycling through [`ProtocolKind`],
//! so every verdict bin of the `bip110` and `data_protocol` taxonomies
//! renders locally. Specials are behavior-classified however they honestly
//! fall out: the data-carrying kinds land in `data`, the rest in `payment`.

use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, bail};
use atlas_classifiers::arc4;
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

/// Every this-many-th transaction is a dedicated BIP-110 construction.
const BIP110_SPECIAL_INTERVAL: u64 = 50;

/// Every this-many-th transaction, offset by
/// [`PROTOCOL_SPECIAL_OFFSET`], is a dedicated data-protocol construction.
/// The offset keeps the cycle disjoint from the BIP-110 cycle: protocol
/// specials sit at odd multiples of 25, BIP-110 specials at multiples of 50.
const PROTOCOL_SPECIAL_INTERVAL: u64 = 50;
const PROTOCOL_SPECIAL_OFFSET: u64 = 25;

/// Fraction of data-class inscriptions carrying a BRC-20 JSON payload.
const BRC20_FRACTION: f64 = 0.15;

/// The dedicated BIP-110 constructions, one per checkable-rule verdict of
/// the `bip110` taxonomy plus one ambiguity that classifies `indeterminate`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Bip110Kind {
    /// Rule 1: an `OP_RETURN` scriptPubKey beyond 83 bytes.
    OversizedOpReturn,
    /// Rule 1: a 67-byte bare P2PK-style scriptPubKey.
    OversizedBareScriptPubkey,
    /// Rule 2: a 300-byte script argument under a Taproot script path.
    OversizedWitnessArgument,
    /// Rule 4: an annex on a key-path-shaped spend.
    KeyPathAnnex,
    /// Rule 5: a 289-byte control block (eight merkle-path steps).
    OversizedControlBlock,
    /// Rule 6: a tapscript that is one `OP_SUCCESS187` opcode.
    OpSuccessTapscript,
    /// Rule 7: an inscription-style `OP_IF` envelope tapscript.
    ConditionalTapscript,
    /// A `0x50`-led last witness element on a non-Taproot-shaped witness:
    /// ambiguous, so the honest verdict is `indeterminate`.
    AmbiguousAnnex,
}

impl Bip110Kind {
    const ALL: [Self; 8] = [
        Self::OversizedOpReturn,
        Self::OversizedBareScriptPubkey,
        Self::OversizedWitnessArgument,
        Self::KeyPathAnnex,
        Self::OversizedControlBlock,
        Self::OpSuccessTapscript,
        Self::ConditionalTapscript,
        Self::AmbiguousAnnex,
    ];

    /// The behavior classification the construction honestly lands in.
    const fn behavior_class(self) -> TxClass {
        match self {
            Self::OversizedOpReturn | Self::ConditionalTapscript => TxClass::Data,
            _ => TxClass::Payment,
        }
    }

    /// The `bip110` verdict the construction is built to receive.
    #[cfg(test)]
    const fn expected_verdict(self) -> &'static str {
        match self {
            Self::OversizedOpReturn | Self::OversizedBareScriptPubkey => "oversized_script_pubkey",
            Self::OversizedWitnessArgument => "oversized_data_push",
            Self::KeyPathAnnex => "annex_present",
            Self::OversizedControlBlock => "oversized_control_block",
            Self::OpSuccessTapscript => "op_success_in_tapscript",
            Self::ConditionalTapscript => "conditional_in_tapscript",
            Self::AmbiguousAnnex => "indeterminate",
        }
    }

    /// The `data_protocol` verdict the construction honestly lands in: the
    /// oversized OP_RETURN is an unrecognized data carrier, and the
    /// conditional tapscript is the inscription-style envelope.
    #[cfg(test)]
    const fn expected_data_protocol_verdict(self) -> &'static str {
        match self {
            Self::OversizedOpReturn => "op_return_other",
            Self::ConditionalTapscript => "inscription",
            _ => "none",
        }
    }
}

/// The dedicated BIP-110 construction for one transaction index, if any.
fn bip110_kind_for_index(index: u64) -> Option<Bip110Kind> {
    index.is_multiple_of(BIP110_SPECIAL_INTERVAL).then(|| {
        let cycle = (index / BIP110_SPECIAL_INTERVAL) % Bip110Kind::ALL.len() as u64;
        Bip110Kind::ALL[cycle as usize]
    })
}

/// The dedicated data-protocol constructions, one per carrier verdict of
/// the `data_protocol` taxonomy that the ordinary population does not
/// already render (the data class renders `inscription` and `brc20`).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ProtocolKind {
    /// `OP_RETURN OP_13 <push>`: the runestone marker.
    Runestone,
    /// A 1-of-3 bare multisig whose first two "pubkeys" carry data bytes
    /// behind implausible prefixes.
    Stamps,
    /// An `OP_RETURN` whose payload is `CNTRPRTY`-prefixed and
    /// ARC4-encrypted with the display-order bytes of the first input's
    /// previous txid, mirroring real Counterparty keying.
    CounterpartyArc4,
    /// An `OP_RETURN` whose payload opens with ASCII `omni`.
    Omni,
    /// An `OP_RETURN` with an unrecognized payload.
    OpReturnOther,
}

impl ProtocolKind {
    const ALL: [Self; 5] = [
        Self::Runestone,
        Self::Stamps,
        Self::CounterpartyArc4,
        Self::Omni,
        Self::OpReturnOther,
    ];

    /// The behavior classification the construction honestly lands in.
    const fn behavior_class(self) -> TxClass {
        match self {
            Self::Stamps => TxClass::Payment,
            _ => TxClass::Data,
        }
    }

    /// The `data_protocol` verdict the construction is built to receive.
    #[cfg(test)]
    const fn expected_verdict(self) -> &'static str {
        match self {
            Self::Runestone => "runes",
            Self::Stamps => "stamps",
            Self::CounterpartyArc4 => "counterparty",
            Self::Omni => "omni",
            Self::OpReturnOther => "op_return_other",
        }
    }

    /// The `bip110` verdict the construction honestly lands in: the Stamps
    /// bare multisig is a 105-byte non-OP_RETURN scriptPubKey.
    #[cfg(test)]
    const fn expected_bip110_verdict(self) -> &'static str {
        match self {
            Self::Stamps => "oversized_script_pubkey",
            _ => "conforming",
        }
    }
}

/// The dedicated data-protocol construction for one transaction index, if
/// any.
fn protocol_kind_for_index(index: u64) -> Option<ProtocolKind> {
    (index % PROTOCOL_SPECIAL_INTERVAL == PROTOCOL_SPECIAL_OFFSET).then(|| {
        let cycle = (index / PROTOCOL_SPECIAL_INTERVAL) % ProtocolKind::ALL.len() as u64;
        ProtocolKind::ALL[cycle as usize]
    })
}

/// A transaction index's dedicated construction, if any. The two cycles are
/// disjoint by construction, so at most one can claim an index.
#[derive(Clone, Copy, Debug)]
enum SpecialKind {
    Bip110(Bip110Kind),
    Protocol(ProtocolKind),
}

fn special_for_index(index: u64) -> Option<SpecialKind> {
    if let Some(kind) = bip110_kind_for_index(index) {
        return Some(SpecialKind::Bip110(kind));
    }
    protocol_kind_for_index(index).map(SpecialKind::Protocol)
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
/// marker-scrubbed witness element makes the wtxid differ from the txid.
fn dummy_input(rng: &mut Rng) -> TxIn {
    let previous_txid = Txid::from_byte_array(rng.byte_array());
    let mut element = [0_u8; WITNESS_ELEMENT_LEN];
    rng.fill_bytes(&mut element);
    scrub_marker(&mut element);
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
    (0..count).map(|_| dummy_input(rng)).collect()
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
            // A real Taproot script-path inscription reveal: one 64-byte
            // dummy signature argument, the envelope tapscript, and a
            // 33-byte control block (leaf version 0xc0), matching the
            // structural inference the bip110 and data_protocol packs
            // share. The envelope carries the inscription marker bytes, so
            // behavior lands on data; the tapscript's OP_IF lands bip110 on
            // conditional_in_tapscript; and data_protocol reads the
            // envelope as inscription — or brc20 for the JSON fraction.
            let tapscript = if rng.uniform() < BRC20_FRACTION {
                let body = format!(
                    r#"{{"p":"brc-20","op":"transfer","tick":"atls","amt":"{}"}}"#,
                    rng.range_inclusive(1, 100_000)
                );
                inscription_envelope_tapscript(rng, b"application/json", body.as_bytes())
            } else {
                let body_len = rng.range_inclusive(24, 72) as usize;
                let body = scrubbed_element(rng, body_len);
                inscription_envelope_tapscript(rng, b"image/png", &body)
            };
            let elements = vec![scrubbed_element(rng, 64), tapscript, control_block(rng, 0)];
            (
                vec![witness_input(rng, &elements)],
                ordinary_outputs(rng, class, 1, total_sats),
            )
        }
        TxClass::Lightning => {
            // One exact 330-sat P2TR anchor among ordinary outputs; no
            // OP_RETURN and no marker, so the data rule cannot fire first.
            let inputs = vec![dummy_input(rng)];
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

/// A synthetic input spending a deterministic dummy outpoint through the
/// given witness elements.
fn witness_input(rng: &mut Rng, elements: &[Vec<u8>]) -> TxIn {
    TxIn {
        previous_output: OutPoint {
            txid: Txid::from_byte_array(rng.byte_array()),
            vout: 0,
        },
        script_sig: ScriptBuf::new(),
        sequence: Sequence::MAX,
        witness: Witness::from_slice(elements),
    }
}

/// A marker-free random witness element of the given length.
fn scrubbed_element(rng: &mut Rng, len: usize) -> Vec<u8> {
    let mut element = vec![0_u8; len];
    rng.fill_bytes(&mut element);
    scrub_marker(&mut element);
    element
}

/// A clean 34-byte tapscript: `OP_PUSHBYTES_32 <key> OP_CHECKSIG`.
fn clean_tapscript(rng: &mut Rng) -> Vec<u8> {
    let mut script = vec![0x20];
    script.extend_from_slice(&scrubbed_element(rng, 32));
    script.push(0xac);
    script
}

/// A control-block-shaped element (leaf version `0xc0`) with `depth`
/// merkle-path steps: `33 + 32 * depth` bytes.
fn control_block(rng: &mut Rng, depth: usize) -> Vec<u8> {
    let mut block = vec![0xc0];
    block.extend_from_slice(&scrubbed_element(rng, 32 + 32 * depth));
    block
}

/// Appends one single-length-byte data push. Every seed push stays below
/// `OP_PUSHDATA1`, so pushes never trip BIP-110's 256-byte rule 2.
fn push_bytes(script: &mut Vec<u8>, bytes: &[u8]) {
    let len = u8::try_from(bytes.len()).expect("push fits one length byte");
    assert!(len < 0x4c, "seed pushes stay below OP_PUSHDATA1");
    script.push(len);
    script.extend_from_slice(bytes);
}

/// A full inscription-envelope tapscript: `<key> OP_CHECKSIG OP_FALSE OP_IF
/// <push "ord"> <tag 1> <content-type> OP_0 <body> OP_ENDIF`.
fn inscription_envelope_tapscript(rng: &mut Rng, content_type: &[u8], body: &[u8]) -> Vec<u8> {
    let mut script = vec![0x20];
    script.extend_from_slice(&scrubbed_element(rng, 32));
    script.push(0xac);
    script.extend_from_slice(&INSCRIPTION_MARKER);
    push_bytes(&mut script, &[0x01]);
    push_bytes(&mut script, content_type);
    script.push(0x00);
    push_bytes(&mut script, body);
    script.push(0x68);
    script
}

/// Constructs a real transaction that definitively violates (or, for
/// [`Bip110Kind::AmbiguousAnnex`], honestly clouds) exactly one checkable
/// BIP-110 rule, while its behavior class falls out of the ordinary
/// heuristics: `data` for the data-carrying kinds, `payment` otherwise.
fn construct_bip110_transaction(rng: &mut Rng, kind: Bip110Kind, total_sats: u64) -> Transaction {
    let class = kind.behavior_class();
    let (input, output) = match kind {
        Bip110Kind::OversizedOpReturn => {
            // 99-byte OP_RETURN scriptPubKey: OP_RETURN OP_PUSHDATA1 96 <96>.
            let mut script_pubkey = vec![0x6a, 0x4c, 96];
            script_pubkey.extend_from_slice(&rng.byte_array::<32>());
            script_pubkey.extend_from_slice(&rng.byte_array::<32>());
            script_pubkey.extend_from_slice(&rng.byte_array::<32>());
            let mut outputs = vec![TxOut {
                value: Amount::from_sat(0),
                script_pubkey: ScriptBuf::from_bytes(script_pubkey),
            }];
            outputs.extend(ordinary_outputs(rng, class, 1, total_sats));
            (vec![dummy_input(rng)], outputs)
        }
        Bip110Kind::OversizedBareScriptPubkey => {
            // 67-byte bare P2PK-style scriptPubKey: push65 <key> OP_CHECKSIG.
            let mut script_pubkey = vec![0x41];
            script_pubkey.extend_from_slice(&scrubbed_element(rng, 65));
            script_pubkey.push(0xac);
            let outputs = vec![TxOut {
                value: Amount::from_sat(total_sats.max(MIN_OUTPUT_SATS)),
                script_pubkey: ScriptBuf::from_bytes(script_pubkey),
            }];
            (vec![dummy_input(rng)], outputs)
        }
        Bip110Kind::OversizedWitnessArgument => {
            let elements = vec![
                scrubbed_element(rng, 300),
                clean_tapscript(rng),
                control_block(rng, 1),
            ];
            (
                vec![witness_input(rng, &elements)],
                ordinary_outputs(rng, class, 2, total_sats),
            )
        }
        Bip110Kind::KeyPathAnnex => {
            let elements = vec![scrubbed_element(rng, 64), vec![0x50, 0xa7, 0x1a, 0x5f]];
            (
                vec![witness_input(rng, &elements)],
                ordinary_outputs(rng, class, 2, total_sats),
            )
        }
        Bip110Kind::OversizedControlBlock => {
            // 289 bytes: 33 + 32 * 8 merkle-path steps, one past the
            // 257-byte (7-step) BIP-110 limit.
            let elements = vec![clean_tapscript(rng), control_block(rng, 8)];
            (
                vec![witness_input(rng, &elements)],
                ordinary_outputs(rng, class, 2, total_sats),
            )
        }
        Bip110Kind::OpSuccessTapscript => {
            // The tapscript is the single opcode OP_SUCCESS187 (0xbb).
            let elements = vec![vec![0xbb], control_block(rng, 1)];
            (
                vec![witness_input(rng, &elements)],
                ordinary_outputs(rng, class, 2, total_sats),
            )
        }
        Bip110Kind::ConditionalTapscript => {
            // OP_FALSE OP_IF push3 "ord" OP_ENDIF: the inscription-style
            // envelope, carrying the marker so behavior lands on data.
            let mut envelope = INSCRIPTION_MARKER.to_vec();
            envelope.push(0x68);
            let elements = vec![envelope, control_block(rng, 1)];
            (
                vec![witness_input(rng, &elements)],
                ordinary_outputs(rng, class, 1, total_sats),
            )
        }
        Bip110Kind::AmbiguousAnnex => {
            // The 0x50-led last element cannot be claimed as an annex: after
            // stripping it, one 107-byte element is neither a key-path
            // signature nor a script-path stack.
            let mut candidate = vec![0x50];
            candidate.extend_from_slice(&scrubbed_element(rng, 106));
            let elements = vec![scrubbed_element(rng, 107), candidate];
            (
                vec![witness_input(rng, &elements)],
                ordinary_outputs(rng, class, 2, total_sats),
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

/// Constructs a real transaction carrying exactly one dedicated
/// data-protocol fingerprint, while its behavior class falls out of the
/// ordinary heuristics: `data` for the OP_RETURN carriers, `payment` for
/// the Stamps bare multisig.
fn construct_protocol_transaction(
    rng: &mut Rng,
    kind: ProtocolKind,
    total_sats: u64,
) -> Transaction {
    let class = kind.behavior_class();
    let (input, output) = match kind {
        ProtocolKind::Runestone => {
            // OP_RETURN OP_13 <payload push>: the runestone marker.
            let mut script = vec![0x6a, 0x5d];
            push_bytes(&mut script, &scrubbed_element(rng, 12));
            let mut outputs = vec![TxOut {
                value: Amount::from_sat(0),
                script_pubkey: ScriptBuf::from_bytes(script),
            }];
            outputs.extend(ordinary_outputs(rng, class, 1, total_sats));
            (vec![dummy_input(rng)], outputs)
        }
        ProtocolKind::Stamps => {
            // OP_1 <data key> <data key> <plausible key> OP_3
            // OP_CHECKMULTISIG: 105 bytes, so bip110 honestly reads it as
            // an oversized non-OP_RETURN scriptPubKey.
            let mut script = vec![0x51];
            for _ in 0..2 {
                let mut key = vec![0x00];
                key.extend_from_slice(&scrubbed_element(rng, 32));
                push_bytes(&mut script, &key);
            }
            let mut signer = vec![0x02];
            signer.extend_from_slice(&scrubbed_element(rng, 32));
            push_bytes(&mut script, &signer);
            script.extend_from_slice(&[0x53, 0xae]);
            let mut outputs = vec![TxOut {
                value: Amount::from_sat(MIN_OUTPUT_SATS),
                script_pubkey: ScriptBuf::from_bytes(script),
            }];
            outputs.extend(ordinary_outputs(rng, class, 1, total_sats));
            (vec![dummy_input(rng)], outputs)
        }
        ProtocolKind::CounterpartyArc4 => {
            // Counterparty keys ARC4 with the display-order (reversed-hex)
            // bytes of the first input's previous txid; the classifier
            // must recover the CNTRPRTY prefix through the same keying.
            let input = dummy_input(rng);
            let mut key = input.previous_output.txid.to_byte_array();
            key.reverse();
            let mut plaintext = b"CNTRPRTY".to_vec();
            plaintext.extend_from_slice(&scrubbed_element(rng, 22));
            let mut script = vec![0x6a];
            push_bytes(&mut script, &arc4(&key, &plaintext));
            let mut outputs = vec![TxOut {
                value: Amount::from_sat(0),
                script_pubkey: ScriptBuf::from_bytes(script),
            }];
            outputs.extend(ordinary_outputs(rng, class, 1, total_sats));
            (vec![input], outputs)
        }
        ProtocolKind::Omni => {
            let mut payload = b"omni".to_vec();
            payload.extend_from_slice(&[0x00, 0x00, 0x00, 0x32]);
            payload.extend_from_slice(&scrubbed_element(rng, 16));
            let mut script = vec![0x6a];
            push_bytes(&mut script, &payload);
            let mut outputs = vec![TxOut {
                value: Amount::from_sat(0),
                script_pubkey: ScriptBuf::from_bytes(script),
            }];
            outputs.extend(ordinary_outputs(rng, class, 1, total_sats));
            (vec![dummy_input(rng)], outputs)
        }
        ProtocolKind::OpReturnOther => {
            // The fixed 0xdead head keeps the payload from ever opening
            // with a recognized plaintext protocol prefix.
            let mut payload = vec![0xde, 0xad];
            payload.extend_from_slice(&scrubbed_element(rng, 18));
            let mut script = vec![0x6a];
            push_bytes(&mut script, &payload);
            let mut outputs = vec![TxOut {
                value: Amount::from_sat(0),
                script_pubkey: ScriptBuf::from_bytes(script),
            }];
            outputs.extend(ordinary_outputs(rng, class, 1, total_sats));
            (vec![dummy_input(rng)], outputs)
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

/// The preset for one class; every class has exactly one preset.
fn preset_for(class: TxClass) -> &'static ClassPreset {
    PRESETS
        .iter()
        .find(|preset| preset.class == class)
        .expect("every class has a preset")
}

fn draw_total_sats(rng: &mut Rng, preset: &ClassPreset) -> u64 {
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let total_sats = ((log_normal(rng, preset.value_median_btc, preset.value_sigma) * SATS_PER_BTC)
        .round() as u64)
        .max(MIN_TOTAL_SATS);
    total_sats
}

fn synthesize(
    rng: &mut Rng,
    awaiting_fraction: f64,
    now_ms: u64,
    special: Option<SpecialKind>,
) -> SyntheticTransaction {
    let (preset, transaction) = match special {
        // Facts (fee, value, age) of a special come from the preset of the
        // behavior class the construction honestly lands in.
        Some(SpecialKind::Bip110(kind)) => {
            let preset = preset_for(kind.behavior_class());
            let total_sats = draw_total_sats(rng, preset);
            (preset, construct_bip110_transaction(rng, kind, total_sats))
        }
        Some(SpecialKind::Protocol(kind)) => {
            let preset = preset_for(kind.behavior_class());
            let total_sats = draw_total_sats(rng, preset);
            (
                preset,
                construct_protocol_transaction(rng, kind, total_sats),
            )
        }
        None => {
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
            let total_sats = draw_total_sats(rng, preset);
            (preset, construct_transaction(rng, preset.class, total_sats))
        }
    };
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
    for index in 0..cli.count {
        let transaction = synthesize(
            &mut rng,
            cli.awaiting_fraction,
            now_ms,
            special_for_index(index),
        );
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
        for index in 0..700 {
            let synthetic = synthesize(&mut rng, 0.0, NOW_MS, special_for_index(index));
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
    fn bip110_specials_cycle_at_the_expected_interval() {
        assert_eq!(
            bip110_kind_for_index(0),
            Some(Bip110Kind::OversizedOpReturn)
        );
        assert_eq!(bip110_kind_for_index(1), None);
        assert_eq!(bip110_kind_for_index(49), None);
        assert_eq!(
            bip110_kind_for_index(50),
            Some(Bip110Kind::OversizedBareScriptPubkey)
        );
        assert_eq!(
            bip110_kind_for_index(50 * 7),
            Some(Bip110Kind::AmbiguousAnnex)
        );
        assert_eq!(
            bip110_kind_for_index(50 * 8),
            Some(Bip110Kind::OversizedOpReturn),
            "the cycle wraps"
        );
    }

    #[test]
    fn protocol_specials_cycle_disjointly_from_the_bip110_specials() {
        assert_eq!(protocol_kind_for_index(0), None);
        assert_eq!(protocol_kind_for_index(25), Some(ProtocolKind::Runestone));
        assert_eq!(protocol_kind_for_index(50), None, "a bip110 slot");
        assert_eq!(protocol_kind_for_index(75), Some(ProtocolKind::Stamps));
        assert_eq!(
            protocol_kind_for_index(25 + 50 * 4),
            Some(ProtocolKind::OpReturnOther)
        );
        assert_eq!(
            protocol_kind_for_index(25 + 50 * 5),
            Some(ProtocolKind::Runestone),
            "the cycle wraps"
        );
        for index in 0..10_000 {
            assert!(
                !(bip110_kind_for_index(index).is_some()
                    && protocol_kind_for_index(index).is_some()),
                "index {index} claimed by both special cycles",
            );
        }
    }

    /// Every dedicated BIP-110 construction must land in its intended
    /// `bip110` verdict bin, its honest behavior class, and its honest
    /// `data_protocol` verdict, asserted by running the real classifier
    /// packs over the consensus-round-tripped bytes exactly as server-side
    /// enrichment does.
    #[test]
    fn bip110_constructions_yield_their_intended_verdicts() {
        use atlas_classifiers::{
            Bip110Conformance, ClassificationInput, Classifier, DataProtocolFingerprints,
        };

        let mut rng = Rng::new(13);
        for kind in Bip110Kind::ALL {
            for _ in 0..25 {
                let transaction = construct_bip110_transaction(&mut rng, kind, 750_000);
                let parsed: Transaction =
                    deserialize_hex(&serialize_hex(&transaction)).expect("raw hex round-trips");
                let txid = parsed.compute_txid().to_string();
                let input = ClassificationInput {
                    txid: &txid,
                    wtxid: None,
                    transaction: Some(&parsed),
                };
                assert_eq!(
                    Bip110Conformance.classify(&input).verdict.as_deref(),
                    Some(kind.expected_verdict()),
                    "{kind:?} produced the wrong bip110 verdict",
                );
                assert_eq!(
                    DataProtocolFingerprints.classify(&input).verdict.as_deref(),
                    Some(kind.expected_data_protocol_verdict()),
                    "{kind:?} produced the wrong data_protocol verdict",
                );
                assert_eq!(
                    heuristic_class(&parsed),
                    kind.behavior_class(),
                    "{kind:?} landed in the wrong behavior class",
                );
            }
        }
    }

    /// Every dedicated protocol construction must land in its intended
    /// `data_protocol` verdict bin, its honest `bip110` verdict, and its
    /// honest behavior class.
    #[test]
    fn protocol_constructions_yield_their_intended_verdicts() {
        use atlas_classifiers::{
            Bip110Conformance, ClassificationInput, Classifier, DataProtocolFingerprints,
        };

        let mut rng = Rng::new(17);
        for kind in ProtocolKind::ALL {
            for _ in 0..25 {
                let transaction = construct_protocol_transaction(&mut rng, kind, 750_000);
                let parsed: Transaction =
                    deserialize_hex(&serialize_hex(&transaction)).expect("raw hex round-trips");
                let txid = parsed.compute_txid().to_string();
                let input = ClassificationInput {
                    txid: &txid,
                    wtxid: None,
                    transaction: Some(&parsed),
                };
                assert_eq!(
                    DataProtocolFingerprints.classify(&input).verdict.as_deref(),
                    Some(kind.expected_verdict()),
                    "{kind:?} produced the wrong data_protocol verdict",
                );
                assert_eq!(
                    Bip110Conformance.classify(&input).verdict.as_deref(),
                    Some(kind.expected_bip110_verdict()),
                    "{kind:?} produced the wrong bip110 verdict",
                );
                assert_eq!(
                    heuristic_class(&parsed),
                    kind.behavior_class(),
                    "{kind:?} landed in the wrong behavior class",
                );
            }
        }
    }

    /// Data-class transactions are real inscription reveals: behavior
    /// `data`, bip110 `conditional_in_tapscript`, and data_protocol
    /// `inscription` with a `brc20` fraction that must actually appear.
    #[test]
    fn data_class_inscriptions_span_all_three_taxonomies() {
        use atlas_classifiers::{
            Bip110Conformance, ClassificationInput, Classifier, DataProtocolFingerprints,
        };

        let mut rng = Rng::new(19);
        let mut verdict_counts: HashMap<String, usize> = HashMap::new();
        for _ in 0..200 {
            let transaction = construct_transaction(&mut rng, TxClass::Data, 80_000);
            let parsed: Transaction =
                deserialize_hex(&serialize_hex(&transaction)).expect("raw hex round-trips");
            assert_eq!(heuristic_class(&parsed), TxClass::Data);
            let txid = parsed.compute_txid().to_string();
            let input = ClassificationInput {
                txid: &txid,
                wtxid: None,
                transaction: Some(&parsed),
            };
            assert_eq!(
                Bip110Conformance.classify(&input).verdict.as_deref(),
                Some("conditional_in_tapscript"),
                "the envelope's OP_IF must land bip110 on rule 7",
            );
            let verdict = DataProtocolFingerprints
                .classify(&input)
                .verdict
                .expect("verdict present");
            assert!(
                verdict == "inscription" || verdict == "brc20",
                "unexpected data_protocol verdict {verdict}",
            );
            *verdict_counts.entry(verdict).or_insert(0) += 1;
        }
        assert!(verdict_counts.get("inscription").copied().unwrap_or(0) > 0);
        assert!(
            verdict_counts.get("brc20").copied().unwrap_or(0) > 0,
            "the brc-20 fraction must appear",
        );
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
