//! Evidence-carrying verdicts.
//!
//! The campaign requires evidence, not booleans: a violation records the byte
//! counts, opcode positions, and depths that prove it, and an `Unknown` records
//! exactly which fact was missing.

use serde::Serialize;

/// The rule-applicability model used to produce a [`TxEvidence`] bundle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EvaluationMode {
    /// BIP-110 consensus, including activation/expiry and UTXO grandfathering.
    Consensus,
    /// Deployed Knots standard-mempool policy, without activation gating or
    /// UTXO grandfathering.
    MempoolPolicy,
}

/// The seven RDTS transaction rules, numbered as in the BIP specification.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RuleId {
    /// Rule 1: output scriptPubKey size (<=34, or `OP_RETURN` <=83). Output
    /// scoped: evaluated on every new output including coinbase, even when all
    /// inputs are grandfathered.
    OutputSize,
    /// Rule 2: pushed data / script-argument witness item size (<=256).
    ElementSize,
    /// Rule 3: spending an undefined witness version or undefined Tapleaf
    /// version is invalid.
    UndefinedVersion,
    /// Rule 4: a Taproot spend containing an annex is invalid.
    TaprootAnnex,
    /// Rule 5: Taproot control block size (<=257, i.e. Merkle depth <=7).
    ControlBlockSize,
    /// Rule 6: a Tapscript containing any `OP_SUCCESS` opcode is invalid.
    OpSuccess,
    /// Rule 7: executing `OP_IF`/`OP_NOTIF` in Tapscript is invalid.
    TapscriptOpIf,
}

impl RuleId {
    /// The BIP rule number (1-7).
    pub fn number(self) -> u8 {
        match self {
            RuleId::OutputSize => 1,
            RuleId::ElementSize => 2,
            RuleId::UndefinedVersion => 3,
            RuleId::TaprootAnnex => 4,
            RuleId::ControlBlockSize => 5,
            RuleId::OpSuccess => 6,
            RuleId::TapscriptOpIf => 7,
        }
    }

    /// Whether the rule is output-scoped (rule 1) rather than input/script
    /// scoped (rules 2-7). Output-scoped rules are always evaluated on new
    /// outputs even when every input is grandfathered.
    pub fn is_output_scoped(self) -> bool {
        matches!(self, RuleId::OutputSize)
    }
}

/// Which script an in-script push was found in (rule 2 evidence).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ScriptKind {
    /// A witness v0 P2WSH witness script.
    WitnessScript,
    /// A Tapscript (leaf version 0xc0).
    Tapscript,
    /// A legacy scriptSig (bare or the non-redeemScript part of P2SH).
    ScriptSig,
    /// A legacy (bare) prevout scriptPubKey executed as a script.
    ScriptPubKey,
}

/// A single proven violation, carrying the numbers that prove it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Violation {
    /// Rule 1: an output scriptPubKey is too large.
    OutputScriptTooLarge {
        /// Output index (`vout`).
        vout: usize,
        /// Actual scriptPubKey length in bytes.
        len: usize,
        /// Whether the script's first opcode is `OP_RETURN` (selects the limit).
        is_op_return: bool,
        /// The applicable byte limit (34 or 83).
        limit: usize,
    },
    /// Rule 2: a script-argument witness item exceeds 256 bytes.
    WitnessItemTooLarge {
        /// Input index.
        input: usize,
        /// Position of the item within the witness stack.
        item_index: usize,
        /// Actual item length in bytes.
        len: usize,
        /// The applicable byte limit (256).
        limit: usize,
    },
    /// Rule 2: a pushed data element inside a decoded script exceeds 256 bytes.
    PushTooLarge {
        /// Input index.
        input: usize,
        /// Which script the push was found in.
        script: ScriptKind,
        /// Byte offset of the push opcode within that script.
        opcode_pos: usize,
        /// Actual pushed-data length in bytes.
        len: usize,
        /// The applicable byte limit (256).
        limit: usize,
    },
    /// Rule 3: spending an undefined witness version.
    UndefinedWitnessVersion {
        /// Input index.
        input: usize,
        /// The witness version number (2-16, or 1 for a non-empty-witness P2A).
        version: u8,
        /// Whether this is the non-empty-witness P2A case that falls through to
        /// the upgradable-witness rejection.
        p2a_non_empty_witness: bool,
    },
    /// Rule 3: spending an undefined Tapleaf version.
    UndefinedTapleafVersion {
        /// Input index.
        input: usize,
        /// The masked leaf version byte (`control[0] & 0xfe`).
        leaf_version: u8,
    },
    /// Rule 4: a Taproot spend carries an annex.
    TaprootAnnexPresent {
        /// Input index.
        input: usize,
        /// Length of the annex element in bytes.
        annex_len: usize,
    },
    /// Rule 5: a Taproot control block is too large.
    ControlBlockTooLarge {
        /// Input index.
        input: usize,
        /// Actual control-block length in bytes.
        len: usize,
        /// Implied Merkle-path depth (`(len - 33) / 32`), when well-formed.
        depth: Option<usize>,
        /// The applicable byte limit (257).
        limit: usize,
    },
    /// Rule 6: a Tapscript contains an `OP_SUCCESS` opcode.
    OpSuccessPresent {
        /// Input index.
        input: usize,
        /// The `OP_SUCCESS` opcode byte.
        opcode: u8,
        /// Byte offset of the opcode within the Tapscript.
        opcode_pos: usize,
    },
    /// Rule 7: a Tapscript executes `OP_IF`/`OP_NOTIF`.
    TapscriptOpIf {
        /// Input index.
        input: usize,
        /// The opcode byte (0x63 `OP_IF` or 0x64 `OP_NOTIF`).
        opcode: u8,
        /// Byte offset of the opcode within the Tapscript.
        opcode_pos: usize,
    },
}

/// A missing fact that prevents a definite verdict for an input-scoped rule.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "missing", rename_all = "snake_case")]
pub enum Missing {
    /// No prevout scriptPubKey supplied for this input, so its spend cannot be
    /// classified.
    ScriptPubKey {
        /// Input index.
        input: usize,
    },
    /// The prevout scriptPubKey is known and the input would violate the rule,
    /// but the prevout creation height is unknown, so grandfathering cannot be
    /// decided.
    CreationHeight {
        /// Input index.
        input: usize,
    },
    /// The spend form is recognized but not modeled by this evaluator (today:
    /// P2SH-wrapped spends, whose nested redeemScript / witness dispatch is a
    /// `VERIFY(client)` open item). The evaluator refuses to guess rather than
    /// emit a possibly-wrong verdict.
    UnsupportedSpend {
        /// Input index.
        input: usize,
        /// Short machine-readable reason.
        reason: &'static str,
    },
}

/// The verdict for one rule over the whole transaction.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "verdict", rename_all = "snake_case")]
pub enum RuleVerdict {
    /// The rule was evaluated and no input/output violates it.
    Pass,
    /// At least one input/output definitely violates the rule.
    Violate {
        /// Every proven violation for this rule.
        evidence: Vec<Violation>,
        /// Facts that prevented classification of other inputs for this rule.
        /// They do not weaken the definite transaction-level violation.
        #[serde(skip_serializing_if = "Vec::is_empty")]
        missing: Vec<Missing>,
    },
    /// The rule could not be decided because a required fact is missing. Only
    /// possible for input-scoped rules (2-7).
    Unknown {
        /// Every missing fact that blocked a decision.
        missing: Vec<Missing>,
    },
}

/// The deterministic first rejecting rule and its evidence.
///
/// Output-size failures precede input script failures. Input failures are
/// ordered by input index and then by the enforcement client's check order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PrimaryViolation {
    /// The first rejecting rule.
    pub rule: RuleId,
    /// The evidence for that specific rejection.
    #[serde(flatten)]
    pub evidence: Violation,
}

impl RuleVerdict {
    /// Whether this verdict is `Pass`.
    pub fn is_pass(&self) -> bool {
        matches!(self, RuleVerdict::Pass)
    }
    /// Whether this verdict is `Violate`.
    pub fn is_violate(&self) -> bool {
        matches!(self, RuleVerdict::Violate { .. })
    }
    /// Whether this verdict is `Unknown`.
    pub fn is_unknown(&self) -> bool {
        matches!(self, RuleVerdict::Unknown { .. })
    }
}

/// The verdict for one rule, tagged with which rule it is.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RuleOutcome {
    /// Which rule.
    pub rule: RuleId,
    /// The BIP rule number (1-7), included for convenience in the JSON.
    pub number: u8,
    /// The verdict.
    #[serde(flatten)]
    pub verdict: RuleVerdict,
}

/// The full per-transaction evidence bundle: one outcome per rule.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TxEvidence {
    /// Which applicability model produced this bundle.
    pub evaluation_mode: EvaluationMode,
    /// Whether the rules were applied. This can be `false` only for consensus
    /// evaluation outside the deployment's active window.
    pub rules_applied: bool,
    /// Whether the transaction is a coinbase (only rule 1 is evaluated).
    pub is_coinbase: bool,
    /// The deterministic first rejection, when all earlier inputs were
    /// classifiable. `None` means there was no violation or missing facts made
    /// the first rejection indeterminate.
    pub primary_violation: Option<PrimaryViolation>,
    /// One outcome per rule, rules 1 through 7 in order.
    pub rules: Vec<RuleOutcome>,
}

impl TxEvidence {
    /// The outcome for a specific rule.
    pub fn rule(&self, rule: RuleId) -> &RuleOutcome {
        self.rules
            .iter()
            .find(|o| o.rule == rule)
            .expect("every rule is always present in TxEvidence")
    }

    /// Whether any rule is `Violate`.
    pub fn any_violation(&self) -> bool {
        self.rules.iter().any(|o| o.verdict.is_violate())
    }

    /// Whether any rule is `Unknown`.
    pub fn any_unknown(&self) -> bool {
        self.rules.iter().any(|o| o.verdict.is_unknown())
    }
}
