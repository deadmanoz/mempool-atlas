//! Bounded binary representations for lazy cross-source conflicting-spend
//! discovery.
//!
//! Fingerprints only identify candidates. A browser must fetch and compare the
//! exact outpoints before presenting a conflict as verified.

use std::sync::Arc;

use bitcoin::OutPoint;
use bitcoin::hashes::{Hash, HashEngine, sha256};
use bytes::Bytes;
use thiserror::Error;

pub(crate) const FINGERPRINT_MAGIC: &[u8; 8] = b"ATLCFP01";
pub(crate) const EXACT_MAGIC: &[u8; 8] = b"ATLCOP01";
const FINGERPRINT_DOMAIN: &[u8] = b"mempool-atlas/conflict-outpoint/v1\0";
const HEADER_BYTES: usize = 24;
const FINGERPRINT_RECORD_BYTES: usize = 12;
const OUTPOINT_BYTES: usize = 36;
const MAX_BODY_BYTES: usize = 64 * 1024 * 1024;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct EncodedConflictFacts {
    pub(crate) body: Bytes,
    pub(crate) content_id: String,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct FingerprintRecord {
    fingerprint: [u8; 8],
    row: u32,
}

#[derive(Debug, Error)]
pub(crate) enum ConflictFactsError {
    #[error("conflict-fact count does not fit in the binary wire format: {0}")]
    CountOverflow(&'static str),
    #[error("conflict-fact body is {actual} bytes, exceeding the {maximum}-byte limit")]
    BodyTooLarge { actual: usize, maximum: usize },
    #[error("failed to reserve conflict-fact response memory")]
    Allocation,
}

pub(crate) fn encode_fingerprints(
    total_rows: u32,
    covered: &[(u32, Arc<[OutPoint]>)],
) -> Result<EncodedConflictFacts, ConflictFactsError> {
    let covered_rows = u32::try_from(covered.len())
        .map_err(|_| ConflictFactsError::CountOverflow("covered rows"))?;
    let record_len = covered.iter().try_fold(0_usize, |total, (_, outpoints)| {
        total
            .checked_add(outpoints.len())
            .ok_or(ConflictFactsError::CountOverflow("fingerprint records"))
    })?;
    let record_count = u32::try_from(record_len)
        .map_err(|_| ConflictFactsError::CountOverflow("fingerprint records"))?;
    let body_bytes = HEADER_BYTES
        .checked_add(record_len.checked_mul(FINGERPRINT_RECORD_BYTES).ok_or(
            ConflictFactsError::BodyTooLarge {
                actual: usize::MAX,
                maximum: MAX_BODY_BYTES,
            },
        )?)
        .ok_or(ConflictFactsError::BodyTooLarge {
            actual: usize::MAX,
            maximum: MAX_BODY_BYTES,
        })?;
    ensure_body_bound(body_bytes)?;
    let mut records = Vec::new();
    records
        .try_reserve_exact(record_len)
        .map_err(|_| ConflictFactsError::Allocation)?;
    for (row, outpoints) in covered {
        records.extend(outpoints.iter().map(|outpoint| FingerprintRecord {
            fingerprint: fingerprint(outpoint),
            row: *row,
        }));
    }
    records.sort_unstable();
    let mut body = Vec::new();
    body.try_reserve_exact(body_bytes)
        .map_err(|_| ConflictFactsError::Allocation)?;
    body.extend_from_slice(FINGERPRINT_MAGIC);
    push_u32(&mut body, total_rows);
    push_u32(&mut body, covered_rows);
    push_u32(&mut body, record_count);
    push_u32(&mut body, 0);
    for record in records {
        body.extend_from_slice(&record.fingerprint);
        push_u32(&mut body, record.row);
    }
    debug_assert_eq!(body.len(), body_bytes);
    Ok(encoded(body))
}

pub(crate) fn encode_exact_outpoints(
    total_rows: u32,
    covered_rows: u32,
    source_row: u32,
    outpoints: &Arc<[OutPoint]>,
) -> Result<EncodedConflictFacts, ConflictFactsError> {
    let outpoint_count = u32::try_from(outpoints.len())
        .map_err(|_| ConflictFactsError::CountOverflow("exact outpoints"))?;
    let body_bytes = HEADER_BYTES
        .checked_add(outpoints.len().checked_mul(OUTPOINT_BYTES).ok_or(
            ConflictFactsError::BodyTooLarge {
                actual: usize::MAX,
                maximum: MAX_BODY_BYTES,
            },
        )?)
        .ok_or(ConflictFactsError::BodyTooLarge {
            actual: usize::MAX,
            maximum: MAX_BODY_BYTES,
        })?;
    ensure_body_bound(body_bytes)?;
    let mut body = Vec::new();
    body.try_reserve_exact(body_bytes)
        .map_err(|_| ConflictFactsError::Allocation)?;
    body.extend_from_slice(EXACT_MAGIC);
    push_u32(&mut body, total_rows);
    push_u32(&mut body, covered_rows);
    push_u32(&mut body, source_row);
    push_u32(&mut body, outpoint_count);
    for outpoint in outpoints.iter() {
        append_outpoint(&mut body, outpoint);
    }
    debug_assert_eq!(body.len(), body_bytes);
    Ok(encoded(body))
}

fn fingerprint(outpoint: &OutPoint) -> [u8; 8] {
    let mut engine = sha256::Hash::engine();
    engine.input(FINGERPRINT_DOMAIN);
    engine.input(outpoint.txid.as_byte_array());
    engine.input(&outpoint.vout.to_le_bytes());
    let digest = sha256::Hash::from_engine(engine).to_byte_array();
    digest[..8]
        .try_into()
        .expect("an eight-byte SHA-256 prefix is fixed width")
}

fn encoded(body: Vec<u8>) -> EncodedConflictFacts {
    let content_id = sha256::Hash::hash(&body).to_string();
    EncodedConflictFacts {
        body: Bytes::from(body),
        content_id,
    }
}

fn ensure_body_bound(actual: usize) -> Result<(), ConflictFactsError> {
    if actual > MAX_BODY_BYTES {
        return Err(ConflictFactsError::BodyTooLarge {
            actual,
            maximum: MAX_BODY_BYTES,
        });
    }
    Ok(())
}

fn push_u32(body: &mut Vec<u8>, value: u32) {
    body.extend_from_slice(&value.to_le_bytes());
}

fn append_outpoint(body: &mut Vec<u8>, outpoint: &OutPoint) {
    body.extend_from_slice(outpoint.txid.as_byte_array());
    body.extend_from_slice(&outpoint.vout.to_le_bytes());
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use bitcoin::consensus::serialize;
    use bitcoin::{OutPoint, Txid};

    use super::*;
    fn outpoint(txid_byte: u8, vout: u32) -> OutPoint {
        OutPoint {
            txid: Txid::from_str(&format!("{txid_byte:02x}").repeat(32)).expect("txid"),
            vout,
        }
    }

    fn read_u32(body: &[u8], offset: usize) -> u32 {
        u32::from_le_bytes(body[offset..offset + 4].try_into().expect("u32"))
    }

    #[test]
    fn fingerprint_body_is_fixed_width_sorted_and_reports_partial_coverage() {
        let outpoints: Arc<[OutPoint]> = vec![outpoint(3, 1), outpoint(2, 0)].into();

        let encoded = encode_fingerprints(2, &[(1, outpoints)]).expect("fingerprints");
        assert_eq!(&encoded.body[..8], FINGERPRINT_MAGIC);
        assert_eq!(read_u32(&encoded.body, 8), 2);
        assert_eq!(read_u32(&encoded.body, 12), 1);
        assert_eq!(read_u32(&encoded.body, 16), 2);
        assert_eq!(read_u32(&encoded.body, 20), 0);
        assert_eq!(
            encoded.body.len(),
            HEADER_BYTES + 2 * FINGERPRINT_RECORD_BYTES
        );
        assert!(encoded.body[24..32] <= encoded.body[36..44]);
        assert_eq!(read_u32(&encoded.body, 32), 1);
        assert_eq!(read_u32(&encoded.body, 44), 1);
    }

    #[test]
    fn exact_body_preserves_consensus_outpoint_order() {
        let outpoints: Arc<[OutPoint]> = vec![outpoint(9, 7), outpoint(8, 6)].into();
        let encoded = encode_exact_outpoints(3, 2, 1, &outpoints).expect("exact facts");
        assert_eq!(&encoded.body[..8], EXACT_MAGIC);
        assert_eq!(read_u32(&encoded.body, 8), 3);
        assert_eq!(read_u32(&encoded.body, 12), 2);
        assert_eq!(read_u32(&encoded.body, 16), 1);
        assert_eq!(read_u32(&encoded.body, 20), 2);
        assert_eq!(&encoded.body[24..60], serialize(&outpoints[0]));
        assert_eq!(&encoded.body[60..96], serialize(&outpoints[1]));
    }

    #[test]
    fn direct_outpoint_encoding_matches_bitcoin_consensus_bytes() {
        let outpoint = outpoint(0xa7, 0x1020_3040);
        let mut direct = Vec::new();
        append_outpoint(&mut direct, &outpoint);
        assert_eq!(direct.len(), OUTPOINT_BYTES);
        assert_eq!(direct, serialize(&outpoint));
    }

    #[test]
    fn retained_fact_budget_keeps_both_binary_responses_below_stage_ceiling() {
        let maximum_outpoints = crate::classification::MAX_CONFLICT_FACT_BYTES
            .saturating_sub(crate::classification::CONFLICT_FACT_TRANSACTION_OVERHEAD_BYTES)
            / std::mem::size_of::<OutPoint>();
        assert!(
            HEADER_BYTES + maximum_outpoints * FINGERPRINT_RECORD_BYTES < MAX_BODY_BYTES,
            "the global fingerprint sidecar must remain comfortably bounded"
        );
        assert!(
            HEADER_BYTES + maximum_outpoints * OUTPOINT_BYTES <= MAX_BODY_BYTES,
            "one covered transaction's exact body must remain bounded"
        );
    }
}
