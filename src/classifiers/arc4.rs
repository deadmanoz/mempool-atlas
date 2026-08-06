//! Minimal ARC4 stream cipher, private to the classifier lenses.
//!
//! Counterparty and Bitcoin Stamps obfuscate their embedded payloads with
//! ARC4 keyed by the spending transaction's first input txid. Recognizing
//! either fingerprint therefore requires running the keystream. The cipher is
//! implemented here rather than pulled in as a dependency because it is used
//! only to deobfuscate data-carrier bytes; Atlas never uses it for security.

/// Applies the ARC4 keystream to `data`. ARC4 is symmetric, so this is both
/// encryption and decryption.
pub(super) fn apply(key: &[u8], data: &[u8]) -> Vec<u8> {
    if key.is_empty() {
        return data.to_vec();
    }
    let mut state = [0_u8; 256];
    for (index, cell) in state.iter_mut().enumerate() {
        *cell = u8::try_from(index).unwrap_or(0);
    }
    let mut swap_index = 0_u8;
    for index in 0..256_usize {
        swap_index = swap_index
            .wrapping_add(state[index])
            .wrapping_add(key[index % key.len()]);
        state.swap(index, usize::from(swap_index));
    }

    let mut output = Vec::with_capacity(data.len());
    let mut i = 0_u8;
    let mut j = 0_u8;
    for byte in data {
        i = i.wrapping_add(1);
        j = j.wrapping_add(state[usize::from(i)]);
        state.swap(usize::from(i), usize::from(j));
        let keystream =
            state[usize::from(state[usize::from(i)].wrapping_add(state[usize::from(j)]))];
        output.push(byte ^ keystream);
    }
    output
}

#[cfg(test)]
mod tests {
    use super::apply;

    #[test]
    fn matches_published_arc4_test_vectors() {
        // RFC 6229 style vectors: short ASCII keys over "Plaintext" style
        // inputs, taken from the original published ARC4 description.
        assert_eq!(
            apply(b"Key", b"Plaintext"),
            [0xBB, 0xF3, 0x16, 0xE8, 0xD9, 0x40, 0xAF, 0x0A, 0xD3],
        );
        assert_eq!(apply(b"Wiki", b"pedia"), [0x10, 0x21, 0xBF, 0x04, 0x20],);
        assert_eq!(
            apply(b"Secret", b"Attack at dawn"),
            [
                0x45, 0xA0, 0x1F, 0x64, 0x5F, 0xC3, 0x5B, 0x38, 0x35, 0x52, 0x54, 0x4B, 0x9B, 0xF5
            ],
        );
    }

    #[test]
    fn is_symmetric_for_a_thirty_two_byte_key() {
        let key = [0x5a_u8; 32];
        let plaintext = b"CNTRPRTY payload bytes";
        let ciphertext = apply(&key, plaintext);
        assert_ne!(ciphertext, plaintext.to_vec());
        assert_eq!(apply(&key, &ciphertext), plaintext.to_vec());
    }

    #[test]
    fn an_empty_key_leaves_the_data_unchanged() {
        assert_eq!(apply(&[], b"abc"), b"abc".to_vec());
    }
}
