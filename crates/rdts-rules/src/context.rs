//! Consensus constants and the evaluation context shared by mempool-time and
//! block-time callers.
//!
//! Every constant below is mirrored byte-for-byte from the enforcement client
//! (`bitcoin-bip110-client`, commit
//! f41f01e1e6de7025d52a865bef97f2a67277f0f3).
//! Source file and line are cited on each constant so a future reader can
//! re-check them against the client.

/// Maximum size (bytes) of a non-`OP_RETURN` output scriptPubKey while RDTS is
/// active. Rule 1.
///
/// Client: `src/consensus/consensus.h:37` `MAX_OUTPUT_SCRIPT_SIZE{34}`.
pub const MAX_OUTPUT_SCRIPT_SIZE: usize = 34;

/// Maximum size (bytes) of an `OP_RETURN` output scriptPubKey while RDTS is
/// active. Rule 1. This is total script size, not payload size.
///
/// Client: `src/consensus/consensus.h:38` `MAX_OUTPUT_DATA_SIZE{83}`.
pub const MAX_OUTPUT_DATA_SIZE: usize = 83;

/// Maximum size (bytes) of a pushed data element / script-argument witness item
/// while RDTS is active. Rule 2.
///
/// Client: `src/script/script.h:29` `MAX_SCRIPT_ELEMENT_SIZE_REDUCED = 256`.
pub const MAX_SCRIPT_ELEMENT_SIZE_REDUCED: usize = 256;

/// The ordinary (non-RDTS) maximum pushed data element size, kept only for
/// documentation / contrast.
///
/// Client: `src/script/script.h:28` `MAX_SCRIPT_ELEMENT_SIZE = 520`.
pub const MAX_SCRIPT_ELEMENT_SIZE: usize = 520;

/// Base size (bytes) of a Taproot control block (leaf-version byte plus the
/// 32-byte internal key).
///
/// Client: `src/script/interpreter.h:247` `TAPROOT_CONTROL_BASE_SIZE = 33`.
pub const TAPROOT_CONTROL_BASE_SIZE: usize = 33;

/// Size (bytes) of one Merkle-path node inside a Taproot control block.
///
/// Client: `src/script/interpreter.h:248` `TAPROOT_CONTROL_NODE_SIZE = 32`.
pub const TAPROOT_CONTROL_NODE_SIZE: usize = 32;

/// Maximum Merkle-path depth (number of control-block nodes) while RDTS is
/// active. Rule 5.
///
/// Client: `src/script/interpreter.h:251` `TAPROOT_CONTROL_MAX_NODE_COUNT_REDUCED = 7`.
pub const TAPROOT_CONTROL_MAX_NODE_COUNT_REDUCED: usize = 7;

/// Maximum Taproot control-block size (bytes) while RDTS is active. Rule 5.
/// `33 + 32 * 7 = 257`.
///
/// Client: `src/script/interpreter.h:252` `TAPROOT_CONTROL_MAX_SIZE_REDUCED`.
pub const TAPROOT_CONTROL_MAX_SIZE_REDUCED: usize =
    TAPROOT_CONTROL_BASE_SIZE + TAPROOT_CONTROL_NODE_SIZE * TAPROOT_CONTROL_MAX_NODE_COUNT_REDUCED;

/// Tag byte that marks a Taproot annex as the last witness element. Rule 4.
///
/// Client: `src/script/interpreter.cpp` uses `ANNEX_TAG` (0x50) in the annex
/// detection at line 1961.
pub const ANNEX_TAG: u8 = 0x50;

/// Mask applied to the control-block leaf-version byte to recover the leaf
/// version (the low bit is the parity/negation flag).
///
/// Client: `src/script/interpreter.h:245` `TAPROOT_LEAF_MASK = 0xfe`.
pub const TAPROOT_LEAF_MASK: u8 = 0xfe;

/// The defined Tapscript leaf version. Any other (masked) leaf version is an
/// undefined Tapleaf version and is rejected while RDTS is active. Rule 3.
///
/// Client: `src/script/interpreter.h:246` `TAPROOT_LEAF_TAPSCRIPT = 0xc0`.
pub const TAPROOT_LEAF_TAPSCRIPT: u8 = 0xc0;

/// `OP_RETURN` opcode value; used to select the rule 1 output-size limit.
pub const OP_RETURN: u8 = 0x6a;

/// Number of blocks for which the RDTS transaction rules stay active on a
/// branch, from the activation height (inclusive) to the expiry height
/// (exclusive). Mainnet value.
///
/// Client: `src/kernel/chainparams.cpp:126`
/// `DEPLOYMENT_REDUCED_DATA.active_duration = 52416`.
pub const ACTIVE_DURATION: u32 = 52_416;

/// Shared evaluation context for a single transaction.
///
/// The same context type is used at mempool time (where `spend_height` is the
/// height of the next block) and at block time (where `spend_height` is the
/// height of the block being connected). It never reads a chain; the caller
/// supplies the two heights.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EvaluationContext {
    /// Height at which RDTS became `ACTIVE` on the branch being evaluated, i.e.
    /// the client's `reduced_data_start_height`
    /// (`src/validation.cpp:2921-2924`). `None` means the deployment has not
    /// activated on this branch, so none of the seven rules apply as consensus.
    ///
    /// This single value drives both applicability (is RDTS active at
    /// `spend_height`) and grandfathering (an input is exempt iff its prevout
    /// was created strictly below this height).
    pub activation_height: Option<u32>,

    /// Height at which the transaction is being evaluated. Block-time: the
    /// height of the block being connected. Mempool-time: the height of the
    /// next block.
    pub spend_height: u32,
}

impl EvaluationContext {
    /// Construct a context.
    pub fn new(activation_height: Option<u32>, spend_height: u32) -> Self {
        Self {
            activation_height,
            spend_height,
        }
    }

    /// Whether the RDTS transaction rules are active as *consensus* at
    /// `spend_height`.
    ///
    /// Mirrors the client's `DeploymentActiveAt(...DEPLOYMENT_REDUCED_DATA)`
    /// gate: rules apply only inside the active window
    /// `[activation_height, activation_height + ACTIVE_DURATION)`.
    ///
    /// Knots mempool policy is intentionally separate and is exposed by
    /// [`crate::evaluate_mempool_policy`]. Callers must not fabricate an
    /// activation height to approximate policy.
    pub fn rdts_active(&self) -> bool {
        match self.activation_height {
            Some(h) => {
                self.spend_height >= h && self.spend_height < h.saturating_add(ACTIVE_DURATION)
            }
            None => false,
        }
    }

    /// Whether an input spending a prevout created at `creation_height` is
    /// grandfathered (exempt from the per-input script rules 2-7).
    ///
    /// Client: `src/validation.cpp:2976`
    /// `prevheights[j] < reduced_data_start_height`.
    /// The comparison is strict: a prevout created *at* the activation height is
    /// not exempt.
    pub fn is_grandfathered(&self, creation_height: u32) -> Option<bool> {
        self.activation_height.map(|h| creation_height < h)
    }
}
