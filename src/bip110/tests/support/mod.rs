use bitcoin::ScriptBuf;
use bitcoin::hashes::{Hash, hash160};

pub(crate) fn p2sh_spk(redeem_script: &[u8]) -> ScriptBuf {
    let mut script = vec![0xa9u8, 0x14];
    script.extend(hash160::Hash::hash(redeem_script).to_byte_array());
    script.push(0x87);
    ScriptBuf::from_bytes(script)
}

pub(crate) fn push_data(data: &[u8]) -> Vec<u8> {
    let mut encoded = Vec::with_capacity(data.len() + 5);
    match data.len() {
        len if len < 0x4c => encoded.push(len as u8),
        len if len <= u8::MAX as usize => encoded.extend([0x4c, len as u8]),
        len if len <= u16::MAX as usize => {
            encoded.extend([0x4d, len as u8, (len >> 8) as u8]);
        }
        len => encoded.extend([
            0x4e,
            len as u8,
            (len >> 8) as u8,
            (len >> 16) as u8,
            (len >> 24) as u8,
        ]),
    }
    encoded.extend_from_slice(data);
    encoded
}
