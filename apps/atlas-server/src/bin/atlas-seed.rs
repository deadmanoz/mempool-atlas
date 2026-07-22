//! Dev-seed generator: builds a deterministic synthetic mempool from
//! per-classification distribution presets and installs it through the real
//! atomic `POST /api/v1/state` checkpoint protocol of a running atlas-server.
//!
//! Every synthetic transaction is a real, consensus-serializable
//! [`bitcoin::Transaction`] whose input and output shape is constructed to
//! satisfy its class's server-side heuristic rule. Admitted transactions enter
//! the complete SourceReplica checkpoint with the constructed transaction's
//! real vsize. Transactions selected by the rejection cycle stay out of source
//! state. Synthetic evidence is retained only inside unit tests; the shipped
//! seed command targets the production state endpoint exclusively.
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
//!
//! Every index selected by [`is_rejection_index`] (1.5% of the population, on
//! offsets that are all 13 modulo 50 and therefore globally disjoint from the
//! BIP-110 and data-protocol cycles) is refused instead of admitted: it emits
//! a `mempool_rejected` event with a cycling realistic reason and never enters
//! membership. Half of the rejected transactions emit their `p2p_transaction`
//! sighting first, so their bytes classify and the rejection read surface's
//! attribution has both classified and unclassified refusals to render.

use std::collections::BTreeSet;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, bail};
use atlas_classifiers::arc4;
#[cfg(test)]
use atlas_model::{CaptureGapCertainty, Evidence, NormalizedEvent};
use atlas_model::{
    CheckpointBegin, CheckpointChunk, CheckpointCommit, CheckpointId, MAX_CHECKPOINT_CHUNK_ENTRIES,
    MempoolEntryFacts, ReplicaCursor, SourceEpochId, SourceId, SourceReplicaCommand,
    SourceReplicaEntry, SourceReplicaRequest, SourceReplicaResponse, SourceSessionId,
    SourcesResponse,
};
use bitcoin::absolute::LockTime;
#[cfg(test)]
use bitcoin::consensus::encode::serialize_hex;
use bitcoin::hashes::Hash as _;
use bitcoin::transaction::Version;
use bitcoin::{
    Amount, OutPoint, PubkeyHash, ScriptBuf, ScriptHash, Sequence, Transaction, TxIn, TxOut, Txid,
    WPubkeyHash, WScriptHash, Witness, WitnessProgram, WitnessVersion,
};
use clap::{Args, Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(name = "atlas-seed")]
#[command(about = "Seed a running atlas-server with a deterministic synthetic mempool")]
struct Cli {
    /// Base URL of the running atlas-server.
    #[arg(long, default_value = "http://127.0.0.1:3101", global = true)]
    server: String,
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Seed one source with a deterministic synthetic mempool.
    Single(SingleArgs),
    /// Seed three related sources (strictest to most permissive) sharing one
    /// base population with realistic fork-relay divergence.
    Forks(ForksArgs),
}

#[derive(Debug, Args)]
struct SingleArgs {
    /// Source ID to seed; must not collide with a real agent-owned source.
    #[arg(long)]
    source: String,
    /// Number of transactions to generate.
    #[arg(long, default_value_t = 20_000)]
    count: u64,
    /// Deterministic generator seed; also part of the session identity.
    #[arg(long, default_value_t = 1)]
    seed: u64,
}

#[derive(Debug, Args)]
struct ForksArgs {
    /// Strictest source ID (drops data carriers and BIP-110-nonconforming txs).
    #[arg(long, default_value = "knots")]
    knots_source: String,
    /// Middle source ID (drops the most-aggressive data carriers).
    #[arg(long, default_value = "core")]
    core_source: String,
    /// Most permissive source ID (relays the full base population).
    #[arg(long, default_value = "libre-relay")]
    libre_source: String,
    /// Number of base-population transactions the three memberships derive from.
    #[arg(long, default_value_t = FORKS_DEFAULT_COUNT)]
    count: u64,
    /// Deterministic generator seed; also part of every session identity.
    #[arg(long, default_value_t = 1)]
    seed: u64,
}

fn validate_count(count: u64) -> anyhow::Result<()> {
    if count == 0 {
        bail!("--count must be greater than zero");
    }
    Ok(())
}

fn validate_single_args(args: &SingleArgs) -> anyhow::Result<()> {
    validate_count(args.count)?;
    SourceId::new(args.source.clone())?;
    Ok(())
}

fn validate_fork_args(args: &ForksArgs) -> anyhow::Result<[SourceId; 3]> {
    validate_count(args.count)?;
    let sources = [
        SourceId::new(args.knots_source.clone())?,
        SourceId::new(args.core_source.clone())?,
        SourceId::new(args.libre_source.clone())?,
    ];
    let distinct: BTreeSet<&str> = sources.iter().map(SourceId::as_str).collect();
    if distinct.len() != sources.len() {
        bail!("fork source IDs must be distinct");
    }
    Ok(sources)
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

/// Modulus and offsets of the rejection cycle: three of every two hundred
/// transactions (1.5%) are refused instead of admitted. Each offset is 13
/// modulo 50, so the cycle is globally disjoint from the BIP-110 (multiples of
/// 50) and data-protocol (25 modulo 50) special cycles and can never steal a
/// transaction those cycles need to render.
const REJECTION_SPECIAL_INTERVAL: u64 = 200;
const REJECTION_SPECIAL_OFFSETS: [u64; 3] = [13, 63, 113];

/// Realistic node reason strings, cycled across rejections so the read
/// surface's by-reason breakdown renders more than one bucket.
const REJECTION_REASONS: [&str; 6] = [
    "min relay fee not met",
    "insufficient fee",
    "non-mandatory-script-verify-flag",
    "tx-size",
    "dust",
    "bad-txns-inputs-missingorspent",
];

/// Whether the transaction at `index` is refused by policy instead of
/// admitted to the mempool.
fn is_rejection_index(index: u64) -> bool {
    REJECTION_SPECIAL_OFFSETS.contains(&(index % REJECTION_SPECIAL_INTERVAL))
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
    class: TxClass,
    txid: String,
    #[cfg(test)]
    wtxid: String,
    #[cfg(test)]
    raw_transaction_hex: String,
    #[cfg(test)]
    peer_id: u64,
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

fn synthesize(rng: &mut Rng, now_ms: u64, special: Option<SpecialKind>) -> SyntheticTransaction {
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
    let peer_id = rng.next_u64() % 24;
    #[cfg(not(test))]
    let _ = peer_id;

    SyntheticTransaction {
        class: preset.class,
        txid: transaction.compute_txid().to_string(),
        #[cfg(test)]
        wtxid: transaction.compute_wtxid().to_string(),
        #[cfg(test)]
        raw_transaction_hex: serialize_hex(&transaction),
        #[cfg(test)]
        peer_id,
        facts: MempoolEntryFacts {
            vsize,
            fee_sats,
            entered_at_ms: now_ms.saturating_sub(age_ms),
        },
    }
}

fn single_source_seed(args: &SingleArgs, now_ms: u64) -> anyhow::Result<SourceSeed> {
    single_source_seed_inner(args, now_ms, false)
}

fn single_source_seed_inner(
    args: &SingleArgs,
    now_ms: u64,
    include_capture_gap: bool,
) -> anyhow::Result<SourceSeed> {
    validate_single_args(args)?;
    let source = SourceId::new(args.source.clone())?;
    // Facts embed run-time ages, so a re-run must be a new session: replaying
    // the previous session's event identities with fresh facts would be a
    // conflicting-event rejection, not a refresh.
    let mut source = SourceSeed::new(source, format!("seed-{}-{now_ms}", args.seed), now_ms)?;
    let mut rng = Rng::new(args.seed);

    #[cfg(test)]
    if include_capture_gap {
        source.push(Evidence::CaptureGap {
            input: "atlas_seed".to_owned(),
            reason: "synthetic_gap".to_owned(),
            certainty: CaptureGapCertainty::PossibleLoss,
        })?;
    }
    #[cfg(not(test))]
    let _ = include_capture_gap;
    #[cfg(test)]
    let mut rejection_ordinal = 0_usize;
    for index in 0..args.count {
        let transaction = synthesize(&mut rng, now_ms, special_for_index(index));
        if is_rejection_index(index) {
            // A policy refusal never enters membership. Half of the rejected
            // transactions emit their P2P sighting first, so their bytes
            // classify and exercise attribution; the other half are refused
            // before any bytes are seen and stay unclassified.
            #[cfg(test)]
            {
                let reason = REJECTION_REASONS[rejection_ordinal % REJECTION_REASONS.len()];
                let seen_first = rejection_ordinal.is_multiple_of(2);
                rejection_ordinal += 1;
                if seen_first {
                    source.push(Evidence::P2pTransaction {
                        txid: transaction.txid.clone(),
                        wtxid: transaction.wtxid,
                        raw_transaction_hex: Some(transaction.raw_transaction_hex),
                        peer_id: Some(transaction.peer_id),
                        inbound: Some(true),
                    })?;
                }
                source.push(Evidence::MempoolRejected {
                    txid: transaction.txid,
                    reason: reason.to_owned(),
                })?;
            }
            continue;
        }
        source.observe(&transaction)?;
        source.admit(&transaction)?;
    }
    Ok(source)
}

#[cfg(test)]
fn seed_events(args: &SingleArgs, now_ms: u64) -> anyhow::Result<Vec<NormalizedEvent>> {
    Ok(single_source_seed(args, now_ms)?.evidence_events)
}

#[cfg(test)]
fn single_source_seed_with_capture_gap(
    args: &SingleArgs,
    now_ms: u64,
) -> anyhow::Result<SourceSeed> {
    single_source_seed_inner(args, now_ms, true)
}

/// Forks-mode default base-population size. Smaller than the single-source
/// default because three source memberships derive from one base population.
const FORKS_DEFAULT_COUNT: u64 = 5_000;

/// Core drops one in this many data-class transactions (modeling its stricter
/// datacarrier policy), on top of every oversized-OP_RETURN construction.
const CORE_DATA_DROP_INTERVAL: u64 = 7;

/// Knots emits a `mempool_rejected` event for one in this many of the
/// transactions it filters out of Core's set, so the rejection panel shows
/// Knots refusing what Core relays. The remainder it drops silently.
const KNOTS_REJECT_INTERVAL: usize = 3;

/// A handful of transactions only Knots saw, so the reverse anomaly region
/// (present in the stricter source, absent from the more permissive one) is
/// non-empty and visibly exercised.
const FORKS_ANOMALY_COUNT: u64 = 8;

/// Whether a dedicated special construction is definitively BIP-110
/// nonconforming. An ambiguous annex classifies `indeterminate`, not
/// nonconforming, so Knots does not filter it on that basis.
fn is_bip110_nonconforming(special: Option<SpecialKind>) -> bool {
    match special {
        Some(SpecialKind::Bip110(kind)) => kind != Bip110Kind::AmbiguousAnnex,
        Some(SpecialKind::Protocol(kind)) => kind == ProtocolKind::Stamps,
        None => false,
    }
}

/// Whether Core relays a base transaction. Core drops the most-aggressive data
/// carriers its stricter datacarrier policy would refuse: every
/// oversized-OP_RETURN construction and a fraction of the data class.
fn core_admits(
    index: u64,
    transaction: &SyntheticTransaction,
    special: Option<SpecialKind>,
) -> bool {
    let core_drops = matches!(
        special,
        Some(SpecialKind::Bip110(Bip110Kind::OversizedOpReturn))
    ) || (transaction.class == TxClass::Data
        && index.is_multiple_of(CORE_DATA_DROP_INTERVAL));
    !core_drops
}

/// Whether Knots filters a transaction out of Core's set: it refuses data
/// carriers (inscriptions, oversized data) and BIP-110-nonconforming outputs.
fn knots_filters(transaction: &SyntheticTransaction, special: Option<SpecialKind>) -> bool {
    transaction.class == TxClass::Data || is_bip110_nonconforming(special)
}

/// Accumulates one source's ordered event stream under a single fresh session.
#[derive(Clone, Debug, Eq, PartialEq)]
struct SourceSeed {
    source: SourceId,
    session: SourceSessionId,
    now_ms: u64,
    #[cfg(test)]
    sequence: u64,
    state_entries: Vec<SourceReplicaEntry>,
    #[cfg(test)]
    evidence_events: Vec<NormalizedEvent>,
}

impl SourceSeed {
    fn new(source: SourceId, session: String, now_ms: u64) -> anyhow::Result<Self> {
        Ok(Self {
            source,
            session: SourceSessionId::new(session)?,
            now_ms,
            #[cfg(test)]
            sequence: 0,
            state_entries: Vec::new(),
            #[cfg(test)]
            evidence_events: Vec::new(),
        })
    }

    #[cfg(test)]
    fn push(&mut self, evidence: Evidence) -> anyhow::Result<()> {
        self.sequence += 1;
        self.evidence_events.push(NormalizedEvent::new(
            self.source.clone(),
            self.session.clone(),
            self.sequence,
            self.now_ms,
            self.now_ms,
            evidence,
        )?);
        Ok(())
    }

    /// Emits the transaction's P2P sighting so server-side enrichment derives
    /// its shape facts and classifier verdicts once, globally by txid.
    fn observe(&mut self, transaction: &SyntheticTransaction) -> anyhow::Result<()> {
        #[cfg(test)]
        self.push(Evidence::P2pTransaction {
            txid: transaction.txid.clone(),
            wtxid: transaction.wtxid.clone(),
            raw_transaction_hex: Some(transaction.raw_transaction_hex.clone()),
            peer_id: Some(transaction.peer_id),
            inbound: Some(true),
        })?;
        #[cfg(not(test))]
        let _ = transaction;
        Ok(())
    }

    /// Adds one complete RPC fact triple to the source checkpoint. Evidence
    /// ordering is independent and does not establish membership.
    fn admit(&mut self, transaction: &SyntheticTransaction) -> anyhow::Result<()> {
        self.state_entries.push(SourceReplicaEntry::new(
            transaction.txid.clone(),
            transaction.facts.clone(),
        )?);
        Ok(())
    }

    fn reject(&mut self, transaction: &SyntheticTransaction, reason: &str) -> anyhow::Result<()> {
        #[cfg(test)]
        self.push(Evidence::MempoolRejected {
            txid: transaction.txid.clone(),
            reason: reason.to_owned(),
        })?;
        #[cfg(not(test))]
        let _ = (transaction, reason);
        Ok(())
    }
}

/// Builds the three per-source event streams for a staged fork seed. Every base
/// transaction is admitted by `libre-relay`; `core` omits the most-aggressive
/// data carriers; `knots` omits the data and BIP-110-nonconforming set and
/// additionally rejects a portion of what it omits. A handful of Knots-only
/// anomaly transactions keep the reverse anomaly region non-empty. Bytes are
/// observed once, via the most permissive source that carries each txid, so
/// classification derives for every txid regardless of the source a region
/// reads.
fn fork_seeds(args: &ForksArgs, now_ms: u64) -> anyhow::Result<Vec<SourceSeed>> {
    let [knots_source, core_source, libre_source] = validate_fork_args(args)?;
    let mut knots = SourceSeed::new(
        knots_source,
        format!("fork-knots-{}-{now_ms}", args.seed),
        now_ms,
    )?;
    let mut core = SourceSeed::new(
        core_source,
        format!("fork-core-{}-{now_ms}", args.seed),
        now_ms,
    )?;
    let mut libre = SourceSeed::new(
        libre_source,
        format!("fork-libre-{}-{now_ms}", args.seed),
        now_ms,
    )?;

    let mut rng = Rng::new(args.seed);
    let mut knots_reject_ordinal = 0_usize;
    for index in 0..args.count {
        let special = special_for_index(index);
        let transaction = synthesize(&mut rng, now_ms, special);
        // libre-relay carries the full base population and observes every txid's
        // bytes, so classification derives globally.
        libre.observe(&transaction)?;
        libre.admit(&transaction)?;
        if !core_admits(index, &transaction, special) {
            // Dropped by Core (and therefore Knots): a libre-only carrier that
            // surfaces as `added` at the core -> libre stage.
            continue;
        }
        core.admit(&transaction)?;
        if knots_filters(&transaction, special) {
            // In Core, filtered by Knots: `added` at the knots -> core stage.
            // Knots rejects a portion of what it filters, cycling reasons; the
            // remainder it drops silently.
            if knots_reject_ordinal.is_multiple_of(KNOTS_REJECT_INTERVAL) {
                let reason = REJECTION_REASONS
                    [(knots_reject_ordinal / KNOTS_REJECT_INTERVAL) % REJECTION_REASONS.len()];
                knots.reject(&transaction, reason)?;
            }
            knots_reject_ordinal += 1;
        } else {
            knots.admit(&transaction)?;
        }
    }

    // Knots-only anomalies: present transactions the more-permissive sources
    // never saw, so the knots -> core stage's reverse anomaly is non-empty.
    for _ in 0..FORKS_ANOMALY_COUNT {
        let transaction = synthesize(&mut rng, now_ms, None);
        knots.observe(&transaction)?;
        knots.admit(&transaction)?;
    }

    Ok(vec![knots, core, libre])
}

fn current_now_ms() -> anyhow::Result<u64> {
    u64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .context("system clock before unix epoch")?
            .as_millis(),
    )
    .context("system clock beyond representable milliseconds")
}

fn require_success(
    response: reqwest::blocking::Response,
    operation: &str,
) -> anyhow::Result<reqwest::blocking::Response> {
    if response.status().is_success() {
        return Ok(response);
    }
    let status = response.status();
    let body = response.text().unwrap_or_default();
    bail!("{operation} returned {status}: {body}")
}

/// Reads one target source's active cursor for replacement. An unknown source
/// has no cursor and is valid for a first seed run.
fn fetch_active_cursor(
    client: &reqwest::blocking::Client,
    server: &str,
    source: &SourceId,
) -> anyhow::Result<Option<ReplicaCursor>> {
    let url = format!("{server}/api/v1/sources");
    let response = client
        .get(&url)
        .send()
        .with_context(|| format!("fetching source cursors from {url}"))?;
    let response = require_success(response, "source cursor read")?;
    let sources: SourcesResponse = response.json().context("parsing source cursor response")?;
    Ok(sources
        .sources
        .into_iter()
        .find(|descriptor| descriptor.source_id == *source)
        .map(|descriptor| descriptor.state_cursor))
}

fn checkpoint_requests(
    seed: &SourceSeed,
    replaces: Option<ReplicaCursor>,
) -> anyhow::Result<Vec<SourceReplicaRequest>> {
    let mut entries = seed.state_entries.clone();
    entries.sort_by(|left, right| left.txid.cmp(&right.txid));
    let epoch_id = SourceEpochId::new(format!("epoch-{}", seed.session))?;
    let checkpoint_id = CheckpointId::new(format!("checkpoint-{}", seed.session))?;
    let expected_chunks = u32::try_from(entries.len().div_ceil(MAX_CHECKPOINT_CHUNK_ENTRIES))
        .context("checkpoint chunk count exceeds u32")?;
    let begin = CheckpointBegin::new(
        checkpoint_id.clone(),
        replaces,
        1,
        seed.now_ms,
        expected_chunks,
        &entries,
    )?;
    let mut requests = vec![SourceReplicaRequest::new(
        seed.source.clone(),
        epoch_id.clone(),
        SourceReplicaCommand::CheckpointBegin(begin.clone()),
    )?];
    for (chunk_index, entries) in entries.chunks(MAX_CHECKPOINT_CHUNK_ENTRIES).enumerate() {
        let chunk = CheckpointChunk::new(
            checkpoint_id.clone(),
            u32::try_from(chunk_index).context("checkpoint chunk index exceeds u32")?,
            entries.to_vec(),
        )?;
        requests.push(SourceReplicaRequest::new(
            seed.source.clone(),
            epoch_id.clone(),
            SourceReplicaCommand::CheckpointChunk(chunk),
        )?);
    }
    let commit = CheckpointCommit::new(checkpoint_id, 1, begin.content_sha256)?;
    requests.push(SourceReplicaRequest::new(
        seed.source.clone(),
        epoch_id,
        SourceReplicaCommand::CheckpointCommit(commit),
    )?);
    Ok(requests)
}

fn deliver_state(
    client: &reqwest::blocking::Client,
    state_url: &str,
    seed: &SourceSeed,
    replaces: Option<ReplicaCursor>,
) -> anyhow::Result<ReplicaCursor> {
    let mut active_cursor = None;
    for request in checkpoint_requests(seed, replaces)? {
        let response = client
            .post(state_url)
            .json(&request)
            .send()
            .with_context(|| format!("delivering state request to {state_url}"))?;
        if response.status().as_u16() != 202 {
            bail!(
                "state ingest returned {}: {}",
                response.status(),
                response.text().unwrap_or_default()
            );
        }
        let response: SourceReplicaResponse =
            response.json().context("parsing state acknowledgement")?;
        active_cursor = response.active_cursor().cloned().or(active_cursor);
    }
    active_cursor.context("checkpoint commit did not return an active cursor")
}

fn run_single(server: &str, args: &SingleArgs, now_ms: u64) -> anyhow::Result<()> {
    let source = single_source_seed(args, now_ms)?;
    let client = reqwest::blocking::Client::new();
    let server = server.trim_end_matches('/');
    let replaces = fetch_active_cursor(&client, server, &source.source)?;
    let state_url = format!("{server}/api/v1/state");
    let cursor = deliver_state(&client, &state_url, &source, replaces)?;
    println!(
        "seeded source {}: {} members at {}/{}",
        args.source,
        source.state_entries.len(),
        cursor.epoch_id,
        cursor.revision,
    );

    let summary_url = format!("{server}/api/v1/sources/{}/mempool/summary", args.source);
    let response = client
        .get(&summary_url)
        .send()
        .with_context(|| format!("fetching {summary_url}"))?;
    let summary: serde_json::Value = require_success(response, "summary smoke read")?
        .json()
        .context("parsing summary")?;
    println!(
        "summary totals: {}",
        serde_json::to_string_pretty(&summary["totals"]).expect("totals are JSON")
    );
    Ok(())
}

fn run_forks(server: &str, args: &ForksArgs, now_ms: u64) -> anyhow::Result<()> {
    let sources = fork_seeds(args, now_ms)?;
    let client = reqwest::blocking::Client::new();
    let server = server.trim_end_matches('/');
    // Read every target before the first write so a later read failure cannot
    // leave an avoidable partial replacement.
    let active_cursors = sources
        .iter()
        .map(|source| fetch_active_cursor(&client, server, &source.source))
        .collect::<anyhow::Result<Vec<_>>>()?;
    let state_url = format!("{server}/api/v1/state");
    for (source, replaces) in sources.iter().zip(active_cursors) {
        let cursor = deliver_state(&client, &state_url, source, replaces)?;
        println!(
            "seeded source {}: {} members at {}/{}",
            source.source,
            source.state_entries.len(),
            cursor.epoch_id,
            cursor.revision,
        );
    }

    // Fetch the derived comparison so the staged divergence is visible. The
    // order is least to most permissive, matching the fork presets.
    let compare_url = format!(
        "{server}/api/v1/sources/compare?sources={},{},{}",
        args.knots_source, args.core_source, args.libre_source
    );
    let response = client
        .get(&compare_url)
        .send()
        .with_context(|| format!("fetching {compare_url}"))?;
    let comparison: serde_json::Value = require_success(response, "comparison smoke read")?
        .json()
        .context("parsing comparison")?;
    println!(
        "comparison shared present {}",
        comparison["shared"]["present"]["count"],
    );
    if let Some(stages) = comparison["stages"].as_array() {
        for stage in stages {
            println!(
                "  {} -> {}: added present {}, anomaly present {}",
                stage["from"],
                stage["to"],
                stage["added"]["present"]["count"],
                stage["anomaly"]["present"]["count"],
            );
        }
    }
    Ok(())
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let now_ms = current_now_ms()?;
    match &cli.command {
        Command::Single(args) => run_single(&cli.server, args, now_ms),
        Command::Forks(args) => run_forks(&cli.server, args, now_ms),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{HashMap, HashSet};

    use bitcoin::consensus::encode::deserialize_hex;

    use super::*;

    const NOW_MS: u64 = 1_752_710_400_000;

    fn single_args(count: u64) -> SingleArgs {
        SingleArgs {
            source: "seed-node".to_owned(),
            count,
            seed: 7,
        }
    }

    fn fork_args(count: u64) -> ForksArgs {
        ForksArgs {
            knots_source: "knots".to_owned(),
            core_source: "core".to_owned(),
            libre_source: "libre-relay".to_owned(),
            count,
            seed: 7,
        }
    }

    #[test]
    fn seed_arguments_require_positive_counts_and_distinct_fork_sources() {
        let mut single = single_args(0);
        assert_eq!(
            validate_single_args(&single)
                .expect_err("zero single count")
                .to_string(),
            "--count must be greater than zero"
        );
        single.count = 1;
        validate_single_args(&single).expect("positive single count");

        let mut forks = fork_args(0);
        assert_eq!(
            validate_fork_args(&forks)
                .expect_err("zero fork count")
                .to_string(),
            "--count must be greater than zero"
        );
        forks.count = 1;
        forks.core_source = forks.knots_source.clone();
        assert_eq!(
            validate_fork_args(&forks)
                .expect_err("duplicate fork source")
                .to_string(),
            "fork source IDs must be distinct"
        );
    }

    #[test]
    fn production_cli_does_not_expose_experimental_evidence_delivery() {
        for arguments in [
            vec![
                "atlas-seed",
                "--experimental-evidence",
                "single",
                "--source",
                "demo",
            ],
            vec!["atlas-seed", "single", "--source", "demo", "--capture-gap"],
        ] {
            assert!(
                Cli::try_parse_from(arguments).is_err(),
                "removed evidence flags must not parse",
            );
        }
    }

    fn membership_txids(source: &SourceSeed) -> HashSet<String> {
        source
            .state_entries
            .iter()
            .map(|entry| entry.txid.clone())
            .collect()
    }

    fn rejected_txids(source: &SourceSeed) -> HashSet<String> {
        source
            .evidence_events
            .iter()
            .filter_map(|event| match &event.evidence {
                Evidence::MempoolRejected { txid, .. } => Some(txid.clone()),
                _ => None,
            })
            .collect()
    }

    fn observed_transactions(source: &SourceSeed) -> HashMap<String, Transaction> {
        source
            .evidence_events
            .iter()
            .filter_map(|event| match &event.evidence {
                Evidence::P2pTransaction {
                    txid,
                    raw_transaction_hex: Some(hex),
                    ..
                } => Some((
                    txid.clone(),
                    deserialize_hex(hex).expect("raw hex round-trips"),
                )),
                _ => None,
            })
            .collect()
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
        let first = single_source_seed_with_capture_gap(&single_args(300), NOW_MS).expect("seed");
        let second = single_source_seed_with_capture_gap(&single_args(300), NOW_MS).expect("seed");
        assert_eq!(first, second);

        // One capture gap, one P2P event per admitted transaction, and one or
        // two events per rejected transaction (half have a P2P sighting).
        let mut expected = 1;
        let mut rejection_ordinal = 0;
        for index in 0..300 {
            if is_rejection_index(index) {
                expected += if rejection_ordinal % 2 == 0 { 2 } else { 1 };
                rejection_ordinal += 1;
            } else {
                expected += 1;
            }
        }
        assert_eq!(first.evidence_events.len(), expected);
        assert_eq!(
            first.state_entries.len(),
            300 - rejection_ordinal,
            "only admitted transactions enter state",
        );

        for event in &first.evidence_events {
            event.validate().expect("seed events must be valid");
        }
        assert!(matches!(
            first.evidence_events[0].evidence,
            Evidence::CaptureGap { .. }
        ));
        let rejected = first
            .evidence_events
            .iter()
            .filter(|event| matches!(event.evidence, Evidence::MempoolRejected { .. }))
            .count();
        assert_eq!(
            rejected, rejection_ordinal,
            "one rejection event per rejection index",
        );
        assert!(rejected > 0, "some transactions should be rejected");
    }

    #[test]
    fn checkpoint_plan_is_a_bounded_atomic_replacement() {
        let seed = single_source_seed(&single_args(1_100), NOW_MS).expect("seed");
        let replaced =
            ReplicaCursor::new(SourceEpochId::new("old-epoch").expect("epoch"), 9).expect("cursor");
        let requests = checkpoint_requests(&seed, Some(replaced.clone())).expect("requests");
        let SourceReplicaCommand::CheckpointBegin(begin) = &requests[0].command else {
            panic!("first request must begin the checkpoint");
        };
        assert_eq!(begin.replaces, Some(replaced));
        assert_eq!(begin.expected_entries, seed.state_entries.len() as u64);
        let chunks: Vec<&CheckpointChunk> = requests
            .iter()
            .filter_map(|request| match &request.command {
                SourceReplicaCommand::CheckpointChunk(chunk) => Some(chunk),
                _ => None,
            })
            .collect();
        assert_eq!(chunks.len() as u32, begin.expected_chunks);
        assert!(
            chunks
                .iter()
                .all(|chunk| chunk.entries.len() <= MAX_CHECKPOINT_CHUNK_ENTRIES)
        );
        assert!(matches!(
            requests.last().expect("commit").command,
            SourceReplicaCommand::CheckpointCommit(_)
        ));
    }

    #[test]
    fn evidence_and_source_state_are_generated_separately() {
        let seed = single_source_seed(&single_args(200), NOW_MS).expect("seed");
        let observed: HashSet<&str> = seed
            .evidence_events
            .iter()
            .filter_map(|event| match &event.evidence {
                Evidence::P2pTransaction { txid, .. } => Some(txid.as_str()),
                _ => None,
            })
            .collect();
        let rejected = rejected_txids(&seed);
        assert!(
            seed.state_entries
                .iter()
                .all(|entry| observed.contains(entry.txid.as_str()))
        );
        assert!(
            seed.state_entries
                .iter()
                .all(|entry| !rejected.contains(&entry.txid))
        );
        assert!(seed.evidence_events.iter().all(|event| !matches!(
            event.evidence,
            Evidence::MempoolAdded { .. } | Evidence::MempoolReconciled { .. }
        )));
    }

    #[test]
    fn raw_transactions_round_trip_with_matching_identifiers_and_vsize() {
        let seed = single_source_seed(&single_args(150), NOW_MS).expect("seed");
        let state: HashMap<&str, &MempoolEntryFacts> = seed
            .state_entries
            .iter()
            .map(|entry| (entry.txid.as_str(), &entry.facts))
            .collect();
        for event in &seed.evidence_events {
            let Evidence::P2pTransaction {
                txid,
                wtxid,
                raw_transaction_hex: Some(raw_transaction_hex),
                ..
            } = &event.evidence
            else {
                continue;
            };
            let transaction: Transaction =
                deserialize_hex(raw_transaction_hex).expect("raw hex round-trips");
            assert_eq!(&transaction.compute_txid().to_string(), txid);
            assert_eq!(&transaction.compute_wtxid().to_string(), wtxid);
            if let Some(facts) = state.get(txid.as_str()) {
                assert_eq!(
                    facts.vsize,
                    u64::try_from(transaction.vsize()).expect("vsize fits in u64")
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
            let synthetic = synthesize(&mut rng, NOW_MS, special_for_index(index));
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

    #[test]
    fn rejection_cycle_is_disjoint_from_the_special_cycles() {
        // Every rejection offset is 13 (mod 50), which is neither 0 (bip110)
        // nor 25 (data-protocol), so the cycle can never claim a special index.
        for offset in REJECTION_SPECIAL_OFFSETS {
            assert_eq!(offset % 50, 13, "offset {offset} must sit at 13 mod 50");
        }
        assert!(is_rejection_index(13));
        assert!(is_rejection_index(63));
        assert!(is_rejection_index(113));
        assert!(!is_rejection_index(0), "a bip110 slot");
        assert!(!is_rejection_index(25), "a protocol slot");
        assert!(!is_rejection_index(50), "a bip110 slot");
        assert!(is_rejection_index(213), "the cycle wraps every 200");

        for index in 0..20_000 {
            assert!(
                !(is_rejection_index(index) && special_for_index(index).is_some()),
                "index {index} claimed by both a rejection and a special cycle",
            );
        }
        // Three of every two hundred transactions are rejected: 1.5%.
        let rejected = (0..20_000)
            .filter(|index| is_rejection_index(*index))
            .count();
        assert_eq!(rejected, 300);
    }

    #[test]
    fn rejected_transactions_split_between_seen_and_unseen_bytes() {
        let events = seed_events(&single_args(500), NOW_MS).expect("events");
        let (mut seen, mut unseen) = (0, 0);
        for window in events.windows(2) {
            let Evidence::MempoolRejected { txid, .. } = &window[1].evidence else {
                continue;
            };
            match &window[0].evidence {
                Evidence::P2pTransaction {
                    txid: sighted_txid, ..
                } if sighted_txid == txid => seen += 1,
                _ => unseen += 1,
            }
        }
        assert!(
            seen > 0,
            "some rejections follow a P2P sighting (classified)"
        );
        assert!(
            unseen > 0,
            "some rejections have no prior bytes (unclassified)"
        );
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

    #[test]
    fn fork_generation_is_deterministic_valid_and_ordered() {
        let first = fork_seeds(&fork_args(500), NOW_MS).expect("fork seeds");
        let second = fork_seeds(&fork_args(500), NOW_MS).expect("fork seeds");
        assert_eq!(first.len(), 3);
        for (left, right) in first.iter().zip(&second) {
            assert_eq!(left, right);
            for event in &left.evidence_events {
                event.validate().expect("fork seed events must be valid");
            }
        }
        // Ordered least to most permissive.
        assert_eq!(first[0].source.as_str(), "knots");
        assert_eq!(first[1].source.as_str(), "core");
        assert_eq!(first[2].source.as_str(), "libre-relay");
        // Each source is its own fresh session.
        for source in &first {
            for event in &source.evidence_events {
                assert_eq!(event.source_id, source.source);
                assert_eq!(event.source_session_id, source.session);
            }
        }
    }

    #[test]
    fn successive_fork_checkpoints_replace_stale_members_without_absent_events() {
        let first = fork_seeds(&fork_args(500), NOW_MS).expect("first fork plan");
        let current: Vec<HashSet<String>> = first.iter().map(membership_txids).collect();
        let second = fork_seeds(&fork_args(75), NOW_MS + 1).expect("second fork plan");
        let desired: Vec<HashSet<String>> = second.iter().map(membership_txids).collect();
        assert!(
            current
                .iter()
                .zip(&desired)
                .all(|(old, new)| old.difference(new).next().is_some()),
            "the smaller second plan must leave stale rows in every source"
        );
        for (index, source) in second.iter().enumerate() {
            let cursor = ReplicaCursor::new(
                SourceEpochId::new(format!("old-epoch-{index}")).expect("epoch"),
                4,
            )
            .expect("cursor");
            let requests = checkpoint_requests(source, Some(cursor.clone())).expect("requests");
            let SourceReplicaCommand::CheckpointBegin(begin) = &requests[0].command else {
                panic!("checkpoint begins first");
            };
            assert_eq!(begin.replaces, Some(cursor));
            assert_eq!(begin.expected_entries, desired[index].len() as u64);
            assert!(source.evidence_events.iter().all(|event| !matches!(
                event.evidence,
                Evidence::MempoolAdded { .. } | Evidence::MempoolReconciled { .. }
            )));
        }
    }

    #[test]
    fn fork_memberships_nest_modulo_the_knots_anomalies() {
        let sources = fork_seeds(&fork_args(2_000), NOW_MS).expect("fork seeds");
        let knots = membership_txids(&sources[0]);
        let core = membership_txids(&sources[1]);
        let libre = membership_txids(&sources[2]);

        // core is a clean subset of libre (the full base population).
        assert!(core.is_subset(&libre));
        // knots is a subset of core except for the injected knots-only
        // anomalies, which are absent from both core and libre.
        let knots_only: HashSet<String> = knots.difference(&core).cloned().collect();
        assert_eq!(knots_only.len() as u64, FORKS_ANOMALY_COUNT);
        assert!(knots_only.iter().all(|txid| !libre.contains(txid)));
        // The stages are genuinely populated: core adds over knots's shared
        // subset, and libre adds over core.
        assert!(knots.len() - knots_only.len() < core.len());
        assert!(core.len() < libre.len());
    }

    #[test]
    fn core_minus_knots_is_dominated_by_data_carrying_transactions() {
        let sources = fork_seeds(&fork_args(3_000), NOW_MS).expect("fork seeds");
        let knots = membership_txids(&sources[0]);
        let core = membership_txids(&sources[1]);
        // libre observed every base txid's bytes, so its P2P sightings recover
        // the raw transactions for classification.
        let observed = observed_transactions(&sources[2]);

        let added: Vec<&String> = core.difference(&knots).collect();
        assert!(
            !added.is_empty(),
            "core must relay transactions knots filters"
        );
        let data_carrying = added
            .iter()
            .filter(|txid| {
                observed
                    .get(**txid)
                    .is_some_and(|transaction| heuristic_class(transaction) == TxClass::Data)
            })
            .count();
        assert!(
            data_carrying * 2 > added.len(),
            "data-carrying transactions ({data_carrying}) must dominate core \\ knots ({})",
            added.len(),
        );
    }

    #[test]
    fn knots_rejects_a_portion_of_what_it_filters_from_core() {
        let sources = fork_seeds(&fork_args(2_000), NOW_MS).expect("fork seeds");
        let rejected = rejected_txids(&sources[0]);
        assert!(
            !rejected.is_empty(),
            "knots must reject some of the transactions it filters",
        );
        // Rejections are point-in-time refusals that never enter knots's
        // membership, yet core relays exactly those transactions.
        let knots_members = membership_txids(&sources[0]);
        let core_members = membership_txids(&sources[1]);
        assert!(rejected.iter().all(|txid| !knots_members.contains(txid)));
        assert!(
            rejected.iter().all(|txid| core_members.contains(txid)),
            "knots rejects transactions core relays",
        );
        // Reasons cycle so the by-reason breakdown renders more than one bucket.
        let reasons: HashSet<&str> = sources[0]
            .evidence_events
            .iter()
            .filter_map(|event| match &event.evidence {
                Evidence::MempoolRejected { reason, .. } => Some(reason.as_str()),
                _ => None,
            })
            .collect();
        assert!(reasons.len() > 1, "rejection reasons must cycle");
    }
}
