//! Shared witness-structure inference over raw witness elements.
//!
//! Mempool evidence carries no prevout scripts, so packs that reason about
//! witness contents infer the spend interpretation from structure alone.
//! These helpers implement the two structural steps the BIP-110 conformance
//! pack documents and the data-protocol pack reuses: stripping one trailing
//! BIP-341 annex candidate, and recognizing a Taproot script-path-shaped
//! stack whose last element has the control block layout. An inference is a
//! plausible interpretation of the bytes, never proof of the spend type.

/// BIP-341 annex marker: the first byte of the last witness element.
pub(crate) const ANNEX_MARKER: u8 = 0x50;
/// BIP-341 control block layout: one leaf-version-and-parity byte plus a
/// 32-byte internal key, then 32 bytes per merkle-path step.
const CONTROL_BLOCK_BASE_LEN: usize = 33;
const CONTROL_BLOCK_STEP_LEN: usize = 32;
/// BIP-341 structural bound on control blocks: `33 + 32 * 128`.
const CONTROL_BLOCK_STRUCTURAL_MAX_LEN: usize = 4129;

/// Whether one witness element has the BIP-341 control block layout: a
/// leaf-version-and-parity byte plus a 32-byte internal key, then whole
/// 32-byte merkle-path steps up to the structural maximum of 128.
pub(crate) fn is_control_block_shaped(element: &[u8]) -> bool {
    element.len() >= CONTROL_BLOCK_BASE_LEN
        && element.len() <= CONTROL_BLOCK_STRUCTURAL_MAX_LEN
        && (element.len() - CONTROL_BLOCK_BASE_LEN).is_multiple_of(CONTROL_BLOCK_STEP_LEN)
}

/// Strips one trailing `0x50`-led annex candidate when at least two elements
/// are present, returning the remaining stack and the stripped candidate.
/// Whether the candidate is really an annex depends on the stack being
/// Taproot-shaped; callers decide that.
pub(crate) fn split_annex_candidate<'stack, 'element>(
    elements: &'stack [&'element [u8]],
) -> (&'stack [&'element [u8]], Option<&'element [u8]>) {
    if elements.len() >= 2 && elements[elements.len() - 1].first() == Some(&ANNEX_MARKER) {
        (
            &elements[..elements.len() - 1],
            Some(elements[elements.len() - 1]),
        )
    } else {
        (elements, None)
    }
}

/// The Taproot script-path decomposition of one annex-stripped witness stack.
pub(crate) struct ScriptPathShape<'element> {
    /// Every element below the tapscript: script arguments under this
    /// interpretation.
    pub arguments: Vec<&'element [u8]>,
    pub tapscript: &'element [u8],
    pub control_block: &'element [u8],
}

/// Infers a Taproot script-path spend from an annex-stripped stack that ends
/// with a control-block-shaped element and has a tapscript below it.
pub(crate) fn script_path_shape<'element>(
    stack: &[&'element [u8]],
) -> Option<ScriptPathShape<'element>> {
    if stack.len() >= 2 && is_control_block_shaped(stack[stack.len() - 1]) {
        Some(ScriptPathShape {
            arguments: stack[..stack.len() - 2].to_vec(),
            tapscript: stack[stack.len() - 2],
            control_block: stack[stack.len() - 1],
        })
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn control_block_shapes_require_the_bip341_layout() {
        assert!(is_control_block_shaped(&[0xc0; 33]));
        assert!(is_control_block_shaped(&[0xc0; 33 + 32 * 128]));
        assert!(!is_control_block_shaped(&[0xc0; 32]));
        assert!(!is_control_block_shaped(&[0xc0; 34]));
        assert!(!is_control_block_shaped(&[0xc0; 33 + 32 * 129]));
    }

    #[test]
    fn annex_candidates_need_a_marker_and_a_second_element() {
        let signature = [0x01_u8; 64];
        let annex = [0x50_u8, 0xaa];
        let both: Vec<&[u8]> = vec![&signature, &annex];
        let (stack, candidate) = split_annex_candidate(&both);
        assert_eq!(stack.len(), 1);
        assert_eq!(candidate, Some(annex.as_slice()));

        let lone: Vec<&[u8]> = vec![&annex];
        let (stack, candidate) = split_annex_candidate(&lone);
        assert_eq!(stack.len(), 1);
        assert_eq!(candidate, None);
    }

    #[test]
    fn script_path_shapes_split_arguments_tapscript_and_control_block() {
        let argument = [0xaa_u8; 10];
        let tapscript = [0x51_u8];
        let control_block = [0xc0_u8; 33];
        let stack: Vec<&[u8]> = vec![&argument, &tapscript, &control_block];
        let shape = script_path_shape(&stack).expect("script-path shaped");
        assert_eq!(shape.arguments, vec![argument.as_slice()]);
        assert_eq!(shape.tapscript, tapscript.as_slice());
        assert_eq!(shape.control_block, control_block.as_slice());

        let short: Vec<&[u8]> = vec![&control_block];
        assert!(script_path_shape(&short).is_none());
        let unshaped: Vec<&[u8]> = vec![&argument, &tapscript];
        assert!(script_path_shape(&unshaped).is_none());
    }
}
