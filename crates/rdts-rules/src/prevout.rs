//! Per-input prevout facts supplied by the caller.
//!
//! The evaluator never touches a chain or a UTXO set. For each transaction
//! input the caller supplies the spent output's scriptPubKey and the height at
//! which that output was created (needed for grandfathering). Either fact may
//! be absent; absence yields an explicit `Unknown` verdict for the input-scoped
//! rules rather than a guess.

use bitcoin::ScriptBuf;

/// Facts about a single spent prevout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrevoutFacts {
    /// The spent output's scriptPubKey. Needed to classify the spend (segwit
    /// version, Taproot, P2A, ...) for rules 2-7.
    pub script_pubkey: ScriptBuf,

    /// The block height at which the spent output was created (confirmed). Used
    /// for consensus grandfathering: an input is exempt from rules 2-7 iff this
    /// is strictly below the branch activation height. `None` when the caller
    /// could not determine it. Mempool-policy evaluation ignores this field.
    pub creation_height: Option<u32>,
}

impl PrevoutFacts {
    /// Construct prevout facts.
    pub fn new(script_pubkey: ScriptBuf, creation_height: Option<u32>) -> Self {
        Self {
            script_pubkey,
            creation_height,
        }
    }
}

/// The full set of per-input prevout facts for one transaction, indexed by
/// input position. An entry may be `None` when the caller has no facts at all
/// for that input (distinct from having the scriptPubKey but not the height).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PrevoutSet {
    facts: Vec<Option<PrevoutFacts>>,
}

impl PrevoutSet {
    /// Build from a vector indexed by input position.
    pub fn from_vec(facts: Vec<Option<PrevoutFacts>>) -> Self {
        Self { facts }
    }

    /// Facts for input `index`, if any were supplied.
    pub fn get(&self, index: usize) -> Option<&PrevoutFacts> {
        self.facts.get(index).and_then(|f| f.as_ref())
    }

    /// Number of inputs described.
    pub fn len(&self) -> usize {
        self.facts.len()
    }

    /// Whether no facts are present.
    pub fn is_empty(&self) -> bool {
        self.facts.is_empty()
    }
}

impl FromIterator<Option<PrevoutFacts>> for PrevoutSet {
    fn from_iter<T: IntoIterator<Item = Option<PrevoutFacts>>>(iter: T) -> Self {
        Self {
            facts: iter.into_iter().collect(),
        }
    }
}
