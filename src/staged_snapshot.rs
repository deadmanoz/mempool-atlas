//! Canonical v2 staged-snapshot encoding.
//!
//! Stage bodies are serialized from structs rather than maps so field order is
//! stable. Classifier-result and BIP-110-assessment dictionaries use
//! deterministic first-seen order and retain each domain vector's original
//! order inside the exact dictionary tuple. The publication preimage
//! length-frames canonical JSON for catalog and summary structs; their field
//! order and `BTreeMap` key order are stable. Bitsets are row-aligned and use
//! the least-significant bit first within each byte.

use std::collections::HashMap;
use std::io::{self, Read, Write};

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use bitcoin::hashes::{Hash, sha256};
use bytes::Bytes;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::model::{
    Bip110Assessment, Bip110Summary, ChainTip, ClassificationResult, ClassificationResultState,
    ClassifierDescriptor, ClassifierSummary, KNOTS_BIP110_CLASSIFIER_ID, MempoolEntry,
    MempoolSnapshot, SourceAvailability, SourceSummary,
};

mod validation;

use validation::validate_input;

const SCHEMA_VERSION: u64 = 2;
const CLASSIFICATION_SET_DOMAIN: &[u8] = b"mempool-atlas/v2/classification-set\0";
const PUBLICATION_DOMAIN: &[u8] = b"mempool-atlas/v2/publication\0";
const MAX_ENCODED_STAGE_BYTES: usize = 64 * 1024 * 1024;
const MAX_ENCODED_PUBLICATION_BYTES: usize = 192 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct StagedSnapshotLimits {
    pub(crate) max_stage_bytes: usize,
    pub(crate) max_publication_bytes: usize,
}

impl Default for StagedSnapshotLimits {
    fn default() -> Self {
        Self {
            max_stage_bytes: MAX_ENCODED_STAGE_BYTES,
            max_publication_bytes: MAX_ENCODED_PUBLICATION_BYTES,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StageKind {
    Population,
    Membership,
    Structure,
    Classifier,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct StageDescriptor {
    pub kind: StageKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub classifier_id: Option<String>,
    pub content_id: String,
    pub uncompressed_bytes: u64,
    pub row_count: u64,
    pub dependency_ids: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct StagedSnapshotManifest {
    pub schema_version: u64,
    pub source: SourceSummary,
    pub source_id: String,
    pub source_label: String,
    pub collection_started_at_ms: u64,
    pub collection_completed_at_ms: u64,
    pub collection_duration_ms: u64,
    pub observed_at_ms: u64,
    pub classification_revision: u64,
    pub chain_tip: ChainTip,
    pub transaction_count: u64,
    pub total_vsize: u64,
    pub classifier_catalog: Vec<ClassifierDescriptor>,
    pub classification_summaries: Vec<ClassifierSummary>,
    pub bip110_summary: Bip110Summary,
    pub row_count: u64,
    pub population_id: String,
    pub classification_set_id: String,
    pub publication_id: String,
    pub stages: Vec<StageDescriptor>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EncodedManifest {
    pub content_id: String,
    pub bytes: Bytes,
    pub value: StagedSnapshotManifest,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EncodedStage {
    pub descriptor: StageDescriptor,
    pub bytes: Bytes,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StagedSnapshotBundle {
    pub manifest: EncodedManifest,
    pub population: EncodedStage,
    pub membership: EncodedStage,
    pub structure: EncodedStage,
    /// Classifier stages retain classifier-catalog order.
    pub classifier_stages: Vec<EncodedStage>,
}

#[derive(Debug, Error)]
pub enum StagedSnapshotError {
    #[error("invalid staged snapshot: {0}")]
    Invalid(String),
    #[error("staged snapshot count does not fit in the v2 wire format: {0}")]
    CountOverflow(&'static str),
    #[error("unsupported fixed-width integer column width {0}")]
    UnsupportedWidth(usize),
    #[error("encoded {body} is {actual} bytes, exceeding the {maximum}-byte limit")]
    EncodedBodyTooLarge {
        body: String,
        actual: usize,
        maximum: usize,
    },
    #[error("encoded publication is {actual} bytes, exceeding the {maximum}-byte total limit")]
    EncodedPublicationTooLarge { actual: usize, maximum: usize },
    #[error("failed to reserve memory while encoding {body}: {message}")]
    Allocation { body: String, message: String },
    #[error("failed to serialize staged snapshot: {0}")]
    Serialize(#[from] serde_json::Error),
}

#[derive(Clone, Debug)]
struct Digest {
    bytes: [u8; 32],
    hex: String,
}

impl Digest {
    fn of(bytes: &[u8]) -> Self {
        let hash = sha256::Hash::hash(bytes);
        Self {
            bytes: hash.to_byte_array(),
            hex: hash.to_string(),
        }
    }
}

#[derive(Serialize)]
struct UnsignedColumn {
    width_bytes: u8,
    values_base64: String,
}

#[derive(Serialize)]
struct SignedColumn {
    width_bytes: u8,
    values_base64: String,
}

#[derive(Serialize)]
struct PopulationBody {
    schema_version: u64,
    kind: StageKind,
    row_count: u64,
    dependency_ids: Vec<String>,
    txids_base64: String,
    vsize: UnsignedColumn,
}

#[derive(Deserialize)]
struct RetainedPopulationIdentity<'a> {
    txids_base64: &'a str,
}

#[derive(Serialize)]
struct MembershipBody {
    schema_version: u64,
    kind: StageKind,
    row_count: u64,
    dependency_ids: Vec<String>,
    population_id: String,
    differing_wtxid_bitset_base64: String,
    differing_wtxids_base64: String,
    weight: UnsignedColumn,
    fee_sats: UnsignedColumn,
    entered_at_ms: UnsignedColumn,
    ancestor_count: UnsignedColumn,
    ancestor_vsize: UnsignedColumn,
    ancestor_fee_sats: SignedColumn,
    descendant_count: UnsignedColumn,
    descendant_vsize: UnsignedColumn,
    replaceable_bitset_base64: String,
}

#[derive(Serialize)]
struct StructureBody {
    schema_version: u64,
    kind: StageKind,
    row_count: u64,
    dependency_ids: Vec<String>,
    population_id: String,
    classification_set_id: String,
    presence_bitset_base64: String,
    input_count: UnsignedColumn,
    output_count: UnsignedColumn,
    op_return_bytes: UnsignedColumn,
    output_sats: UnsignedColumn,
    witness_bytes: UnsignedColumn,
}

#[derive(Serialize)]
struct ClassifierBody {
    schema_version: u64,
    kind: StageKind,
    row_count: u64,
    dependency_ids: Vec<String>,
    population_id: String,
    classifier_id: String,
    result_dictionary: Vec<ClassifierResultTuple>,
    result_codes: UnsignedColumn,
    #[serde(skip_serializing_if = "Option::is_none")]
    bip110: Option<Bip110Columns>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct ClassifierResultTuple {
    state: ClassificationResultState,
    primary_label: Option<String>,
    labels: Vec<String>,
    missing_facts: Vec<String>,
}

impl From<&ClassificationResult> for ClassifierResultTuple {
    fn from(result: &ClassificationResult) -> Self {
        Self {
            state: result.state,
            primary_label: result.primary_label.clone(),
            labels: result.labels.clone(),
            missing_facts: result.missing_facts.clone(),
        }
    }
}

#[derive(Serialize)]
struct Bip110Columns {
    assessment_dictionary: Vec<Bip110Assessment>,
    assessment_codes: UnsignedColumn,
}

/// Encode one complete, current v2 publication bundle.
pub fn encode_staged_snapshot(
    source: &SourceSummary,
    snapshot: &MempoolSnapshot,
) -> Result<StagedSnapshotBundle, StagedSnapshotError> {
    encode_staged_snapshot_with_limits(source, snapshot, None, StagedSnapshotLimits::default())
}

pub(crate) fn encode_staged_snapshot_with_limits(
    source: &SourceSummary,
    snapshot: &MempoolSnapshot,
    reused_membership: Option<(EncodedStage, EncodedStage)>,
    limits: StagedSnapshotLimits,
) -> Result<StagedSnapshotBundle, StagedSnapshotError> {
    validate_input(source, snapshot)?;
    let row_count = count_u64(snapshot.transactions.len(), "row_count")?;
    let mut budget = PublicationBudget::new(limits)?;

    let (population_stage, membership_stage) =
        if let Some((population, membership)) = reused_membership {
            validate_reused_membership(&population, &membership, snapshot, row_count)?;
            budget.charge("population stage", population.bytes.len())?;
            budget.charge("membership stage", membership.bytes.len())?;
            (population, membership)
        } else {
            let population_body = population_body(snapshot, row_count)?;
            let population_bytes = encode_json(
                &population_body,
                "population stage",
                budget.next_body_limit(),
            )?;
            budget.charge("population stage", population_bytes.len())?;
            let population_digest = Digest::of(&population_bytes);
            let population_descriptor = stage_descriptor(
                StageKind::Population,
                None,
                &population_digest,
                population_bytes.len(),
                row_count,
                Vec::new(),
            )?;
            let population = EncodedStage {
                descriptor: population_descriptor,
                bytes: population_bytes,
            };

            let membership_body =
                membership_body(snapshot, row_count, &population.descriptor.content_id)?;
            let membership_bytes = encode_json(
                &membership_body,
                "membership stage",
                budget.next_body_limit(),
            )?;
            budget.charge("membership stage", membership_bytes.len())?;
            let membership_digest = Digest::of(&membership_bytes);
            let membership_descriptor = stage_descriptor(
                StageKind::Membership,
                None,
                &membership_digest,
                membership_bytes.len(),
                row_count,
                vec![population.descriptor.content_id.clone()],
            )?;
            let membership = EncodedStage {
                descriptor: membership_descriptor,
                bytes: membership_bytes,
            };
            (population, membership)
        };
    let population_id = population_stage.descriptor.content_id.clone();

    let mut classifier_parts = Vec::with_capacity(snapshot.classifier_catalog.len());
    for descriptor in &snapshot.classifier_catalog {
        let body = classifier_body(snapshot, descriptor, row_count, &population_id)?;
        let body_name = format!("classifier {} stage", descriptor.id);
        let bytes = encode_json(&body, &body_name, budget.next_body_limit())?;
        budget.charge(&body_name, bytes.len())?;
        let digest = Digest::of(&bytes);
        classifier_parts.push((descriptor, bytes, digest));
    }

    let classification_set_digest = classification_set_digest(&classifier_parts)?;
    let structure_body = structure_body(
        snapshot,
        row_count,
        &population_id,
        &classification_set_digest.hex,
    )?;
    let structure_bytes =
        encode_json(&structure_body, "structure stage", budget.next_body_limit())?;
    budget.charge("structure stage", structure_bytes.len())?;
    let structure_digest = Digest::of(&structure_bytes);

    let structure_descriptor = stage_descriptor(
        StageKind::Structure,
        None,
        &structure_digest,
        structure_bytes.len(),
        row_count,
        vec![population_id.clone(), classification_set_digest.hex.clone()],
    )?;
    let classifier_descriptors = classifier_parts
        .iter()
        .map(|(catalog, bytes, digest)| {
            stage_descriptor(
                StageKind::Classifier,
                Some(catalog.id.clone()),
                digest,
                bytes.len(),
                row_count,
                vec![population_id.clone()],
            )
        })
        .collect::<Result<Vec<_>, _>>()?;

    let mut ordered_descriptors = Vec::with_capacity(3 + classifier_descriptors.len());
    ordered_descriptors.push(population_stage.descriptor.clone());
    ordered_descriptors.push(membership_stage.descriptor.clone());
    ordered_descriptors.push(structure_descriptor.clone());
    ordered_descriptors.extend(classifier_descriptors.iter().cloned());

    let mut manifest_value = StagedSnapshotManifest {
        schema_version: SCHEMA_VERSION,
        source: source.clone(),
        source_id: snapshot.source_id.clone(),
        source_label: snapshot.source_label.clone(),
        collection_started_at_ms: snapshot.collection_started_at_ms,
        collection_completed_at_ms: snapshot.collection_completed_at_ms,
        collection_duration_ms: snapshot.collection_duration_ms,
        observed_at_ms: snapshot.observed_at_ms,
        classification_revision: snapshot.classification_revision,
        chain_tip: snapshot.chain_tip.clone(),
        transaction_count: snapshot.transaction_count,
        total_vsize: snapshot.total_vsize,
        classifier_catalog: snapshot.classifier_catalog.clone(),
        classification_summaries: snapshot.classification_summaries.clone(),
        bip110_summary: snapshot.bip110_summary.clone(),
        row_count,
        population_id,
        classification_set_id: classification_set_digest.hex,
        publication_id: String::new(),
        stages: ordered_descriptors,
    };
    manifest_value.publication_id = publication_digest(&manifest_value)?.hex;
    let manifest_bytes = encode_json(&manifest_value, "manifest", budget.next_body_limit())?;
    budget.charge("manifest", manifest_bytes.len())?;
    let manifest_digest = Digest::of(&manifest_bytes);

    let classifier_stages = classifier_parts
        .into_iter()
        .zip(classifier_descriptors)
        .map(|((_, bytes, _), descriptor)| EncodedStage { descriptor, bytes })
        .collect();

    Ok(StagedSnapshotBundle {
        manifest: EncodedManifest {
            content_id: manifest_digest.hex,
            bytes: manifest_bytes,
            value: manifest_value,
        },
        population: population_stage,
        membership: membership_stage,
        structure: EncodedStage {
            descriptor: structure_descriptor,
            bytes: structure_bytes,
        },
        classifier_stages,
    })
}

/// Rebuild only the manifest for a metadata-only source-state change.
///
/// The retained manifest supplies the complete snapshot identity and stage
/// graph. This operation replaces only its source summary, derives a new
/// publication root over that retained graph, and leaves every stage body and
/// content ID untouched.
pub fn reencode_manifest_for_source(
    retained: &StagedSnapshotManifest,
    source: &SourceSummary,
) -> Result<EncodedManifest, StagedSnapshotError> {
    reencode_manifest_for_source_with_limits(retained, source, StagedSnapshotLimits::default())
}

pub(crate) fn reencode_manifest_for_source_with_limits(
    retained: &StagedSnapshotManifest,
    source: &SourceSummary,
    limits: StagedSnapshotLimits,
) -> Result<EncodedManifest, StagedSnapshotError> {
    validate_retained_source(source, retained)?;
    let mut budget = PublicationBudget::new(limits)?;
    for descriptor in &retained.stages {
        let bytes = usize::try_from(descriptor.uncompressed_bytes)
            .map_err(|_| StagedSnapshotError::CountOverflow("retained stage byte length"))?;
        budget.charge("retained stage", bytes)?;
    }
    let mut value = retained.clone();
    value.source = source.clone();
    value.publication_id.clear();
    value.publication_id = publication_digest(&value)?.hex;
    let bytes = encode_json(&value, "manifest", budget.next_body_limit())?;
    budget.charge("manifest", bytes.len())?;
    let content_id = Digest::of(&bytes).hex;
    Ok(EncodedManifest {
        content_id,
        bytes,
        value,
    })
}

fn validate_reused_membership(
    population: &EncodedStage,
    membership: &EncodedStage,
    snapshot: &MempoolSnapshot,
    row_count: u64,
) -> Result<(), StagedSnapshotError> {
    if population.descriptor.kind != StageKind::Population
        || population.descriptor.classifier_id.is_some()
        || population.descriptor.row_count != row_count
        || !population.descriptor.dependency_ids.is_empty()
        || population.descriptor.uncompressed_bytes
            != count_u64(population.bytes.len(), "population stage byte length")?
    {
        return Err(invalid("retained population stage is inconsistent"));
    }
    if membership.descriptor.kind != StageKind::Membership
        || membership.descriptor.classifier_id.is_some()
        || membership.descriptor.row_count != row_count
        || membership.descriptor.dependency_ids.len() != 1
        || membership.descriptor.dependency_ids.first() != Some(&population.descriptor.content_id)
        || membership.descriptor.uncompressed_bytes
            != count_u64(membership.bytes.len(), "membership stage byte length")?
    {
        return Err(invalid("retained membership stage is inconsistent"));
    }
    validate_reused_population_txids(population, snapshot)?;
    #[cfg(debug_assertions)]
    {
        if population.descriptor.content_id != Digest::of(&population.bytes).hex {
            return Err(invalid("retained population stage digest is inconsistent"));
        }
        if membership.descriptor.content_id != Digest::of(&membership.bytes).hex {
            return Err(invalid("retained membership stage digest is inconsistent"));
        }
    }
    Ok(())
}

fn validate_reused_population_txids(
    population: &EncodedStage,
    snapshot: &MempoolSnapshot,
) -> Result<(), StagedSnapshotError> {
    let identity = serde_json::from_slice::<RetainedPopulationIdentity>(&population.bytes)
        .map_err(|_| invalid("retained population stage txids are inconsistent"))?;
    let mut decoded =
        base64::read::DecoderReader::new(identity.txids_base64.as_bytes(), &BASE64_STANDARD);
    let mut retained_txid = [0_u8; 32];
    for transaction in &snapshot.transactions {
        decoded
            .read_exact(&mut retained_txid)
            .map_err(|_| invalid("retained population stage txids are inconsistent"))?;
        if retained_txid != parse_display_hash(&transaction.txid, "txid")? {
            return Err(invalid(
                "retained population stage does not match snapshot txids",
            ));
        }
    }
    let mut trailing = [0_u8; 1];
    if decoded
        .read(&mut trailing)
        .map_err(|_| invalid("retained population stage txids are inconsistent"))?
        != 0
    {
        return Err(invalid("retained population stage has excess txids"));
    }
    Ok(())
}

struct PublicationBudget {
    limits: StagedSnapshotLimits,
    encoded_bytes: usize,
}

impl PublicationBudget {
    fn new(limits: StagedSnapshotLimits) -> Result<Self, StagedSnapshotError> {
        if limits.max_stage_bytes == 0 || limits.max_publication_bytes == 0 {
            return Err(invalid("encoded publication limits must be positive"));
        }
        Ok(Self {
            limits,
            encoded_bytes: 0,
        })
    }

    fn next_body_limit(&self) -> usize {
        self.limits.max_stage_bytes.min(
            self.limits
                .max_publication_bytes
                .saturating_sub(self.encoded_bytes),
        )
    }

    fn charge(&mut self, body: &str, bytes: usize) -> Result<(), StagedSnapshotError> {
        if bytes > self.limits.max_stage_bytes {
            return Err(StagedSnapshotError::EncodedBodyTooLarge {
                body: body.to_owned(),
                actual: bytes,
                maximum: self.limits.max_stage_bytes,
            });
        }
        let total = self.encoded_bytes.checked_add(bytes).ok_or(
            StagedSnapshotError::EncodedPublicationTooLarge {
                actual: usize::MAX,
                maximum: self.limits.max_publication_bytes,
            },
        )?;
        if total > self.limits.max_publication_bytes {
            return Err(StagedSnapshotError::EncodedPublicationTooLarge {
                actual: total,
                maximum: self.limits.max_publication_bytes,
            });
        }
        self.encoded_bytes = total;
        Ok(())
    }
}

struct BoundedJsonWriter {
    body: String,
    limit: usize,
    bytes: Vec<u8>,
    failure: Option<StagedSnapshotError>,
}

impl BoundedJsonWriter {
    fn new(body: &str, limit: usize) -> Self {
        Self {
            body: body.to_owned(),
            limit,
            bytes: Vec::new(),
            failure: None,
        }
    }

    fn finish(
        self,
        serialization: Result<(), serde_json::Error>,
    ) -> Result<Bytes, StagedSnapshotError> {
        match serialization {
            Ok(()) => Ok(Bytes::from(self.bytes)),
            Err(error) => Err(self
                .failure
                .unwrap_or(StagedSnapshotError::Serialize(error))),
        }
    }
}

impl Write for BoundedJsonWriter {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        let Some(new_len) = self.bytes.len().checked_add(buffer.len()) else {
            self.failure = Some(StagedSnapshotError::EncodedBodyTooLarge {
                body: self.body.clone(),
                actual: usize::MAX,
                maximum: self.limit,
            });
            return Err(io::Error::other("encoded body length overflow"));
        };
        if new_len > self.limit {
            self.failure = Some(StagedSnapshotError::EncodedBodyTooLarge {
                body: self.body.clone(),
                actual: new_len,
                maximum: self.limit,
            });
            return Err(io::Error::other("encoded body exceeds limit"));
        }
        if let Err(error) = self.bytes.try_reserve(buffer.len()) {
            self.failure = Some(StagedSnapshotError::Allocation {
                body: self.body.clone(),
                message: error.to_string(),
            });
            return Err(io::Error::other("encoded body allocation failed"));
        }
        self.bytes.extend_from_slice(buffer);
        Ok(buffer.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn encode_json(
    value: &impl Serialize,
    body: &str,
    limit: usize,
) -> Result<Bytes, StagedSnapshotError> {
    let mut writer = BoundedJsonWriter::new(body, limit);
    let serialization = serde_json::to_writer(&mut writer, value);
    writer.finish(serialization)
}

fn validate_retained_source(
    source: &SourceSummary,
    retained: &StagedSnapshotManifest,
) -> Result<(), StagedSnapshotError> {
    if source.source_id != retained.source_id || source.source_label != retained.source_label {
        return Err(invalid(
            "replacement source summary does not identify the retained snapshot",
        ));
    }
    if source.snapshot_observed_at_ms != Some(retained.observed_at_ms)
        || source.chain_tip.as_ref() != Some(&retained.chain_tip)
        || source.transaction_count != Some(retained.transaction_count)
        || source.total_vsize != Some(retained.total_vsize)
        || source
            .classification
            .as_ref()
            .map(|classification| classification.revision)
            != Some(retained.classification_revision)
    {
        return Err(invalid(
            "replacement source summary snapshot identity does not match the retained manifest",
        ));
    }
    let expected_classified = retained.bip110_summary.compatible_count
        + retained.bip110_summary.violating_count
        + retained.bip110_summary.indeterminate_count;
    if source.classification.as_ref().is_none_or(|classification| {
        classification.classified_count != expected_classified
            || classification.unclassified_count != retained.bip110_summary.unclassified_count
    }) {
        return Err(invalid(
            "replacement source summary classification counts do not match the retained manifest",
        ));
    }
    Ok(())
}

fn population_body(
    snapshot: &MempoolSnapshot,
    row_count: u64,
) -> Result<PopulationBody, StagedSnapshotError> {
    let txid_bytes = snapshot
        .transactions
        .len()
        .checked_mul(32)
        .ok_or(StagedSnapshotError::CountOverflow("txid bytes"))?;
    let mut txids = try_vec_with_capacity(txid_bytes, "population txids")?;
    for transaction in &snapshot.transactions {
        txids.extend_from_slice(&parse_display_hash(&transaction.txid, "txid")?);
    }
    Ok(PopulationBody {
        schema_version: SCHEMA_VERSION,
        kind: StageKind::Population,
        row_count,
        dependency_ids: Vec::new(),
        txids_base64: base64_string(&txids, "population txids")?,
        vsize: unsigned_column(snapshot.transactions.iter().map(|entry| entry.vsize))?,
    })
}

fn membership_body(
    snapshot: &MempoolSnapshot,
    row_count: u64,
    population_id: &str,
) -> Result<MembershipBody, StagedSnapshotError> {
    let mut differing = bitset(snapshot.transactions.len())?;
    let differing_count = snapshot
        .transactions
        .iter()
        .filter(|transaction| transaction.wtxid != transaction.txid)
        .count();
    let differing_bytes = differing_count
        .checked_mul(32)
        .ok_or(StagedSnapshotError::CountOverflow("differing wtxid bytes"))?;
    let mut differing_wtxids = try_vec_with_capacity(differing_bytes, "differing wtxids")?;
    let mut replaceable = bitset(snapshot.transactions.len())?;
    for (row, transaction) in snapshot.transactions.iter().enumerate() {
        if transaction.wtxid != transaction.txid {
            set_bit(&mut differing, row);
            differing_wtxids.extend_from_slice(&parse_display_hash(&transaction.wtxid, "wtxid")?);
        }
        if transaction.replaceable {
            set_bit(&mut replaceable, row);
        }
    }
    Ok(MembershipBody {
        schema_version: SCHEMA_VERSION,
        kind: StageKind::Membership,
        row_count,
        dependency_ids: vec![population_id.to_owned()],
        population_id: population_id.to_owned(),
        differing_wtxid_bitset_base64: base64_string(&differing, "differing wtxid bitset")?,
        differing_wtxids_base64: base64_string(&differing_wtxids, "differing wtxids")?,
        weight: unsigned_column(snapshot.transactions.iter().map(|entry| entry.weight))?,
        fee_sats: unsigned_column(snapshot.transactions.iter().map(|entry| entry.fee_sats))?,
        entered_at_ms: unsigned_column(
            snapshot
                .transactions
                .iter()
                .map(|entry| entry.entered_at_ms),
        )?,
        ancestor_count: unsigned_column(
            snapshot
                .transactions
                .iter()
                .map(|entry| entry.ancestor_count),
        )?,
        ancestor_vsize: unsigned_column(
            snapshot
                .transactions
                .iter()
                .map(|entry| entry.ancestor_vsize),
        )?,
        ancestor_fee_sats: signed_column(
            snapshot
                .transactions
                .iter()
                .map(|entry| entry.ancestor_fee_sats),
        )?,
        descendant_count: unsigned_column(
            snapshot
                .transactions
                .iter()
                .map(|entry| entry.descendant_count),
        )?,
        descendant_vsize: unsigned_column(
            snapshot
                .transactions
                .iter()
                .map(|entry| entry.descendant_vsize),
        )?,
        replaceable_bitset_base64: base64_string(&replaceable, "replaceable bitset")?,
    })
}

fn structure_body(
    snapshot: &MempoolSnapshot,
    row_count: u64,
    population_id: &str,
    classification_set_id: &str,
) -> Result<StructureBody, StagedSnapshotError> {
    let mut presence = bitset(snapshot.transactions.len())?;
    for (row, entry) in snapshot.transactions.iter().enumerate() {
        entry.structure.inspect(|_| {
            set_bit(&mut presence, row);
        });
    }
    Ok(StructureBody {
        schema_version: SCHEMA_VERSION,
        kind: StageKind::Structure,
        row_count,
        dependency_ids: vec![population_id.to_owned(), classification_set_id.to_owned()],
        population_id: population_id.to_owned(),
        classification_set_id: classification_set_id.to_owned(),
        presence_bitset_base64: base64_string(&presence, "structure presence bitset")?,
        input_count: unsigned_column(
            snapshot
                .transactions
                .iter()
                .filter_map(|entry| entry.structure.map(|value| value.input_count)),
        )?,
        output_count: unsigned_column(
            snapshot
                .transactions
                .iter()
                .filter_map(|entry| entry.structure.map(|value| value.output_count)),
        )?,
        op_return_bytes: unsigned_column(
            snapshot
                .transactions
                .iter()
                .filter_map(|entry| entry.structure.map(|value| value.op_return_bytes)),
        )?,
        output_sats: unsigned_column(
            snapshot
                .transactions
                .iter()
                .filter_map(|entry| entry.structure.map(|value| value.output_sats)),
        )?,
        witness_bytes: unsigned_column(
            snapshot
                .transactions
                .iter()
                .filter_map(|entry| entry.structure.map(|value| value.witness_bytes)),
        )?,
    })
}

fn classifier_body(
    snapshot: &MempoolSnapshot,
    descriptor: &ClassifierDescriptor,
    row_count: u64,
    population_id: &str,
) -> Result<ClassifierBody, StagedSnapshotError> {
    let (result_dictionary, result_codes) =
        exact_dictionary_codes(snapshot.transactions.iter().map(|entry| {
            classifier_result(entry, &descriptor.id).map(ClassifierResultTuple::from)
        }))?;

    let bip110 = if descriptor.id == KNOTS_BIP110_CLASSIFIER_ID {
        Some(bip110_columns(snapshot)?)
    } else {
        None
    };
    Ok(ClassifierBody {
        schema_version: SCHEMA_VERSION,
        kind: StageKind::Classifier,
        row_count,
        dependency_ids: vec![population_id.to_owned()],
        population_id: population_id.to_owned(),
        classifier_id: descriptor.id.clone(),
        result_dictionary,
        result_codes,
        bip110,
    })
}

fn bip110_columns(snapshot: &MempoolSnapshot) -> Result<Bip110Columns, StagedSnapshotError> {
    let (assessment_dictionary, assessment_codes) = exact_dictionary_codes(
        snapshot
            .transactions
            .iter()
            .map(|transaction| transaction.bip110.clone()),
    )?;
    Ok(Bip110Columns {
        assessment_dictionary,
        assessment_codes,
    })
}

fn classifier_result<'a>(
    entry: &'a MempoolEntry,
    classifier_id: &str,
) -> Option<&'a ClassificationResult> {
    entry
        .classifications
        .iter()
        .find(|result| result.classifier_id == classifier_id)
}

fn exact_dictionary_codes<T>(
    values: impl IntoIterator<Item = Option<T>>,
) -> Result<(Vec<T>, UnsignedColumn), StagedSnapshotError>
where
    T: Serialize,
{
    const INITIAL_DICTIONARY_CAPACITY: usize = 16;

    let values = values.into_iter();
    let expected = values.size_hint().1.unwrap_or(values.size_hint().0);
    // Classifier taxonomies have low cardinality even when a mempool has many rows. Keep the
    // eager allocation bounded, then let these collections grow if a future classifier needs it.
    let dictionary_capacity = expected.min(INITIAL_DICTIONARY_CAPACITY);
    let mut dictionary = try_vec_with_capacity(dictionary_capacity, "classifier dictionary")?;
    let mut indexes = HashMap::new();
    indexes
        .try_reserve(dictionary_capacity)
        .map_err(|error| allocation("classifier dictionary index", error))?;
    let mut codes = try_vec_with_capacity(expected, "classifier result codes")?;
    for value in values {
        let Some(value) = value else {
            codes.push(0);
            continue;
        };
        let key = serde_json::to_vec(&value)?;
        let code = if let Some(code) = indexes.get(&key) {
            *code
        } else {
            dictionary
                .try_reserve(1)
                .map_err(|error| allocation("classifier dictionary", error))?;
            indexes
                .try_reserve(1)
                .map_err(|error| allocation("classifier dictionary index", error))?;
            dictionary.push(value);
            let code = count_u64(dictionary.len(), "dictionary code")?;
            indexes.insert(key, code);
            code
        };
        codes.push(code);
    }
    Ok((dictionary, unsigned_column(codes)?))
}

fn unsigned_column(
    values: impl IntoIterator<Item = u64>,
) -> Result<UnsignedColumn, StagedSnapshotError> {
    let values = values.into_iter();
    let expected = values.size_hint().1.unwrap_or(values.size_hint().0);
    let mut collected = try_vec_with_capacity(expected, "unsigned column values")?;
    collected.extend(values);
    let values = collected;
    let width = unsigned_width(values.iter().copied().max().unwrap_or(0));
    let byte_len = values
        .len()
        .checked_mul(width)
        .ok_or(StagedSnapshotError::CountOverflow("unsigned column bytes"))?;
    let mut bytes = try_vec_with_capacity(byte_len, "unsigned column bytes")?;
    for value in values {
        bytes.extend_from_slice(&value.to_le_bytes()[..width]);
    }
    Ok(UnsignedColumn {
        width_bytes: u8::try_from(width)
            .map_err(|_| StagedSnapshotError::UnsupportedWidth(width))?,
        values_base64: base64_string(&bytes, "unsigned column")?,
    })
}

fn signed_column(
    values: impl IntoIterator<Item = i64>,
) -> Result<SignedColumn, StagedSnapshotError> {
    let values = values.into_iter();
    let expected = values.size_hint().1.unwrap_or(values.size_hint().0);
    let mut collected = try_vec_with_capacity(expected, "signed column values")?;
    collected.extend(values);
    let values = collected;
    let width = signed_width(&values)?;
    let byte_len = values
        .len()
        .checked_mul(width)
        .ok_or(StagedSnapshotError::CountOverflow("signed column bytes"))?;
    let mut bytes = try_vec_with_capacity(byte_len, "signed column bytes")?;
    for value in values {
        bytes.extend_from_slice(&value.to_le_bytes()[..width]);
    }
    Ok(SignedColumn {
        width_bytes: u8::try_from(width)
            .map_err(|_| StagedSnapshotError::UnsupportedWidth(width))?,
        values_base64: base64_string(&bytes, "signed column")?,
    })
}

fn unsigned_width(maximum: u64) -> usize {
    let significant_bits = (u64::BITS - maximum.leading_zeros()).max(1);
    significant_bits.div_ceil(8) as usize
}

fn signed_width(values: &[i64]) -> Result<usize, StagedSnapshotError> {
    for width in 1..=8 {
        let bits = width * 8;
        let minimum = -(1_i128 << (bits - 1));
        let maximum = (1_i128 << (bits - 1)) - 1;
        if values
            .iter()
            .all(|value| i128::from(*value) >= minimum && i128::from(*value) <= maximum)
        {
            return Ok(width);
        }
    }
    Err(StagedSnapshotError::UnsupportedWidth(9))
}

fn classification_set_digest(
    classifiers: &[(&ClassifierDescriptor, Bytes, Digest)],
) -> Result<Digest, StagedSnapshotError> {
    let mut preimage = Vec::new();
    preimage.extend_from_slice(CLASSIFICATION_SET_DOMAIN);
    append_u64(
        &mut preimage,
        count_u64(classifiers.len(), "classifier count")?,
    );
    for (descriptor, _, digest) in classifiers {
        append_string(&mut preimage, &descriptor.id)?;
        append_string(&mut preimage, &descriptor.version)?;
        preimage.extend_from_slice(&digest.bytes);
    }
    Ok(Digest::of(&preimage))
}

fn publication_digest(manifest: &StagedSnapshotManifest) -> Result<Digest, StagedSnapshotError> {
    let mut preimage = Vec::new();
    preimage.extend_from_slice(PUBLICATION_DOMAIN);
    append_source_summary(&mut preimage, &manifest.source)?;
    append_string(&mut preimage, &manifest.source_id)?;
    append_string(&mut preimage, &manifest.source_label)?;
    for value in [
        manifest.collection_started_at_ms,
        manifest.collection_completed_at_ms,
        manifest.collection_duration_ms,
        manifest.observed_at_ms,
        manifest.classification_revision,
        manifest.chain_tip.height,
        manifest.transaction_count,
        manifest.total_vsize,
    ] {
        append_u64(&mut preimage, value);
    }
    append_string(&mut preimage, &manifest.chain_tip.hash)?;
    append_canonical_json(&mut preimage, &manifest.classifier_catalog)?;
    append_canonical_json(&mut preimage, &manifest.classification_summaries)?;
    append_canonical_json(&mut preimage, &manifest.bip110_summary)?;
    preimage.extend_from_slice(&parse_digest(&manifest.classification_set_id)?);
    append_u64(
        &mut preimage,
        count_u64(manifest.stages.len(), "descriptor count")?,
    );
    for descriptor in &manifest.stages {
        preimage.push(match descriptor.kind {
            StageKind::Population => 1,
            StageKind::Membership => 2,
            StageKind::Structure => 3,
            StageKind::Classifier => 4,
        });
        append_optional_string(&mut preimage, descriptor.classifier_id.as_deref())?;
        preimage.extend_from_slice(&parse_digest(&descriptor.content_id)?);
        append_u64(&mut preimage, descriptor.uncompressed_bytes);
        append_u64(&mut preimage, descriptor.row_count);
        append_u64(
            &mut preimage,
            count_u64(descriptor.dependency_ids.len(), "dependency count")?,
        );
        for dependency in &descriptor.dependency_ids {
            preimage.extend_from_slice(&parse_digest(dependency)?);
        }
    }
    Ok(Digest::of(&preimage))
}

fn append_source_summary(
    output: &mut Vec<u8>,
    source: &SourceSummary,
) -> Result<(), StagedSnapshotError> {
    append_string(output, &source.source_id)?;
    append_string(output, &source.source_label)?;
    output.push(match source.availability {
        SourceAvailability::Waiting => 0,
        SourceAvailability::Ready => 1,
        SourceAvailability::Stale => 2,
        SourceAvailability::Error => 3,
    });
    append_u64(output, source.poll_interval_seconds);
    append_optional_u64(output, source.last_poll_started_at_ms);
    append_optional_u64(output, source.snapshot_observed_at_ms);
    if let Some(tip) = &source.chain_tip {
        output.push(1);
        append_u64(output, tip.height);
        append_string(output, &tip.hash)?;
    } else {
        output.push(0);
    }
    append_optional_u64(output, source.transaction_count);
    append_optional_u64(output, source.total_vsize);
    if let Some(classification) = &source.classification {
        output.push(1);
        output.push(match classification.state {
            crate::model::ClassificationState::Classifying => 0,
            crate::model::ClassificationState::Complete => 1,
            crate::model::ClassificationState::Paused => 2,
        });
        append_u64(output, classification.revision);
        append_u64(output, classification.classified_count);
        append_u64(output, classification.unclassified_count);
    } else {
        output.push(0);
    }
    append_optional_string(output, source.last_error.as_deref())
}

fn stage_descriptor(
    kind: StageKind,
    classifier_id: Option<String>,
    digest: &Digest,
    byte_len: usize,
    row_count: u64,
    dependency_ids: Vec<String>,
) -> Result<StageDescriptor, StagedSnapshotError> {
    Ok(StageDescriptor {
        kind,
        classifier_id,
        content_id: digest.hex.clone(),
        uncompressed_bytes: count_u64(byte_len, "stage byte length")?,
        row_count,
        dependency_ids,
    })
}

fn append_u64(output: &mut Vec<u8>, value: u64) {
    output.extend_from_slice(&value.to_le_bytes());
}

fn append_optional_u64(output: &mut Vec<u8>, value: Option<u64>) {
    match value {
        Some(value) => {
            output.push(1);
            append_u64(output, value);
        }
        None => output.push(0),
    }
}

fn append_string(output: &mut Vec<u8>, value: &str) -> Result<(), StagedSnapshotError> {
    append_u64(output, count_u64(value.len(), "string byte length")?);
    output.extend_from_slice(value.as_bytes());
    Ok(())
}

fn append_optional_string(
    output: &mut Vec<u8>,
    value: Option<&str>,
) -> Result<(), StagedSnapshotError> {
    match value {
        Some(value) => {
            output.push(1);
            append_string(output, value)
        }
        None => {
            output.push(0);
            Ok(())
        }
    }
}

fn append_canonical_json(
    output: &mut Vec<u8>,
    value: &impl Serialize,
) -> Result<(), StagedSnapshotError> {
    let bytes = serde_json::to_vec(value)?;
    append_u64(
        output,
        count_u64(bytes.len(), "canonical JSON byte length")?,
    );
    output.extend_from_slice(&bytes);
    Ok(())
}

fn try_vec_with_capacity<T>(capacity: usize, body: &str) -> Result<Vec<T>, StagedSnapshotError> {
    let mut values = Vec::new();
    values
        .try_reserve_exact(capacity)
        .map_err(|error| allocation(body, error))?;
    Ok(values)
}

fn base64_string(bytes: &[u8], body: &str) -> Result<String, StagedSnapshotError> {
    let encoded_len = bytes
        .len()
        .checked_add(2)
        .and_then(|length| length.checked_div(3))
        .and_then(|length| length.checked_mul(4))
        .ok_or(StagedSnapshotError::CountOverflow("base64 byte length"))?;
    let mut encoded = String::new();
    encoded
        .try_reserve_exact(encoded_len)
        .map_err(|error| allocation(body, error))?;
    BASE64_STANDARD.encode_string(bytes, &mut encoded);
    Ok(encoded)
}

fn allocation(body: &str, error: impl std::fmt::Display) -> StagedSnapshotError {
    StagedSnapshotError::Allocation {
        body: body.to_owned(),
        message: error.to_string(),
    }
}

fn bitset(row_count: usize) -> Result<Vec<u8>, StagedSnapshotError> {
    let bytes = row_count
        .checked_add(7)
        .ok_or(StagedSnapshotError::CountOverflow("bitset rows"))?
        / 8;
    let mut bitset = try_vec_with_capacity(bytes, "row bitset")?;
    bitset.resize(bytes, 0);
    Ok(bitset)
}

fn set_bit(bitset: &mut [u8], row: usize) {
    bitset[row / 8] |= 1 << (row % 8);
}

fn parse_display_hash(value: &str, field: &str) -> Result<[u8; 32], StagedSnapshotError> {
    if value.len() != 64 {
        return Err(invalid(format!(
            "{field} must contain 64 lowercase hex characters"
        )));
    }
    let bytes = value.as_bytes();
    let mut decoded = [0_u8; 32];
    for (index, pair) in bytes.chunks_exact(2).enumerate() {
        decoded[index] = (hex_nibble(pair[0], field)? << 4) | hex_nibble(pair[1], field)?;
    }
    Ok(decoded)
}

fn parse_digest(value: &str) -> Result<[u8; 32], StagedSnapshotError> {
    parse_display_hash(value, "content id")
}

fn hex_nibble(value: u8, field: &str) -> Result<u8, StagedSnapshotError> {
    match value {
        b'0'..=b'9' => Ok(value - b'0'),
        b'a'..=b'f' => Ok(value - b'a' + 10),
        _ => Err(invalid(format!(
            "{field} must contain 64 lowercase hex characters"
        ))),
    }
}

fn count_u64(value: usize, field: &'static str) -> Result<u64, StagedSnapshotError> {
    u64::try_from(value).map_err(|_| StagedSnapshotError::CountOverflow(field))
}

fn invalid(message: impl Into<String>) -> StagedSnapshotError {
    StagedSnapshotError::Invalid(message.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{
        Bip110Assessment, Bip110RuleId, Bip110Status, ClassificationProgress, ClassificationState,
        MembershipFacts, TransactionStructure, classifier_catalog,
    };

    mod hot_paths;
    mod input_validation;

    fn hash(byte: u8) -> String {
        format!("{byte:02x}").repeat(32)
    }

    fn result(
        classifier_id: &str,
        state: ClassificationResultState,
        primary_label: Option<&str>,
        labels: &[&str],
        missing_facts: &[&str],
    ) -> ClassificationResult {
        ClassificationResult {
            classifier_id: classifier_id.to_owned(),
            state,
            primary_label: primary_label.map(str::to_owned),
            labels: labels.iter().map(|value| (*value).to_owned()).collect(),
            missing_facts: missing_facts
                .iter()
                .map(|value| (*value).to_owned())
                .collect(),
            evidence: None,
        }
    }

    fn entry(byte: u8, differing_wtxid: Option<u8>, classified: bool) -> MempoolEntry {
        let mut entry = MempoolEntry::new_variant(
            hash(byte),
            hash(differing_wtxid.unwrap_or(byte)),
            100 + u64::from(byte),
            200 + u64::from(byte),
            1_700_000_000_000 + u64::from(byte),
            MembershipFacts {
                weight: 400,
                ancestor_count: 1,
                ancestor_vsize: 100 + u64::from(byte),
                ancestor_fee_sats: if byte == 2 {
                    -12
                } else {
                    200 + i64::from(byte)
                },
                descendant_count: 1,
                descendant_vsize: 100 + u64::from(byte),
                replaceable: byte == 2,
            },
        )
        .expect("entry");
        if classified {
            entry.structure = Some(TransactionStructure::new(2, 3, 4, 500, 6).expect("structure"));
            entry.classifications = vec![
                result(
                    "transaction_properties",
                    ClassificationResultState::Complete,
                    None,
                    &["p2wpkh", "version_2"],
                    &[],
                ),
                result(
                    "transaction_shape",
                    ClassificationResultState::Partial,
                    Some("other_shape"),
                    &["other_shape"],
                    &["raw_transaction", "input_script_pubkeys"],
                ),
                result(
                    "data_protocols",
                    ClassificationResultState::Complete,
                    Some("no_detected_protocol"),
                    &["no_detected_protocol"],
                    &[],
                ),
                result(
                    KNOTS_BIP110_CLASSIFIER_ID,
                    ClassificationResultState::Partial,
                    Some("violating"),
                    &["violating"],
                    &["policy_facts"],
                ),
            ];
            entry.bip110 = Some(Bip110Assessment {
                status: Bip110Status::Violating,
                primary_rule: Some(Bip110RuleId::ElementSize),
                violated_rules: vec![Bip110RuleId::ElementSize, Bip110RuleId::OutputSize],
                unknown_rules: vec![Bip110RuleId::OpSuccess, Bip110RuleId::TaprootAnnex],
            });
        }
        entry
    }

    fn fixture() -> (SourceSummary, MempoolSnapshot) {
        let snapshot = MempoolSnapshot::new_with_collection_window(
            "alpha".to_owned(),
            "Alpha".to_owned(),
            1_700_000_000_000,
            1_700_000_000_025,
            25,
            ChainTip {
                height: 900_000,
                hash: hash(0xaa),
            },
            vec![
                entry(1, Some(0x91), true),
                entry(2, None, false),
                entry(3, Some(0x93), true),
            ],
        )
        .expect("snapshot")
        .with_classification_revision(7)
        .expect("revision");
        let source = SourceSummary {
            source_id: snapshot.source_id.clone(),
            source_label: snapshot.source_label.clone(),
            availability: SourceAvailability::Ready,
            poll_interval_seconds: 300,
            last_poll_started_at_ms: Some(snapshot.collection_started_at_ms),
            snapshot_observed_at_ms: Some(snapshot.observed_at_ms),
            chain_tip: Some(snapshot.chain_tip.clone()),
            transaction_count: Some(snapshot.transaction_count),
            total_vsize: Some(snapshot.total_vsize),
            classification: Some(ClassificationProgress {
                state: ClassificationState::Classifying,
                revision: snapshot.classification_revision,
                classified_count: 2,
                unclassified_count: 1,
            }),
            last_error: None,
        };
        (source, snapshot)
    }

    fn body_json(stage: &EncodedStage) -> serde_json::Value {
        serde_json::from_slice(&stage.bytes).expect("stage JSON")
    }

    fn decoded(value: &serde_json::Value, field: &str) -> Vec<u8> {
        BASE64_STANDARD
            .decode(value[field].as_str().expect("base64 string"))
            .expect("base64")
    }

    fn decoded_column(value: &serde_json::Value, field: &str) -> Vec<u64> {
        let width = value[field]["width_bytes"].as_u64().expect("width") as usize;
        let bytes = BASE64_STANDARD
            .decode(
                value[field]["values_base64"]
                    .as_str()
                    .expect("column base64"),
            )
            .expect("column base64");
        bytes
            .chunks_exact(width)
            .map(|chunk| {
                let mut encoded = [0_u8; 8];
                encoded[..width].copy_from_slice(chunk);
                u64::from_le_bytes(encoded)
            })
            .collect()
    }

    #[test]
    fn repeat_encoding_is_byte_for_byte_deterministic() {
        let (source, snapshot) = fixture();
        let first = encode_staged_snapshot(&source, &snapshot).expect("first encoding");
        let second = encode_staged_snapshot(&source, &snapshot).expect("second encoding");
        assert_eq!(first, second);
    }

    #[test]
    fn retained_membership_buffers_are_reused_without_copying() {
        let (source, snapshot) = fixture();
        let first = encode_staged_snapshot(&source, &snapshot).expect("first encoding");
        let population = first.population.clone();
        let membership = first.membership.clone();

        let second = encode_staged_snapshot_with_limits(
            &source,
            &snapshot,
            Some((population, membership)),
            StagedSnapshotLimits::default(),
        )
        .expect("reused encoding");

        assert_eq!(
            first.population.bytes.as_ptr(),
            second.population.bytes.as_ptr()
        );
        assert_eq!(
            first.membership.bytes.as_ptr(),
            second.membership.bytes.as_ptr()
        );
        assert_eq!(first.population.descriptor, second.population.descriptor);
        assert_eq!(first.membership.descriptor, second.membership.descriptor);
    }

    #[test]
    fn encoded_stage_and_total_limits_fail_recoverably() {
        let (source, snapshot) = fixture();
        let stage_error = encode_staged_snapshot_with_limits(
            &source,
            &snapshot,
            None,
            StagedSnapshotLimits {
                max_stage_bytes: 1,
                max_publication_bytes: 1024,
            },
        )
        .expect_err("population must exceed the stage limit");
        assert!(matches!(
            stage_error,
            StagedSnapshotError::EncodedBodyTooLarge { .. }
        ));

        let baseline = encode_staged_snapshot(&source, &snapshot).expect("baseline");
        let retained_bytes = baseline.population.bytes.len() + baseline.membership.bytes.len();
        let total_error = encode_staged_snapshot_with_limits(
            &source,
            &snapshot,
            Some((baseline.population.clone(), baseline.membership.clone())),
            StagedSnapshotLimits {
                max_stage_bytes: MAX_ENCODED_STAGE_BYTES,
                max_publication_bytes: retained_bytes - 1,
            },
        )
        .expect_err("retained stages must exceed the total limit");
        assert!(matches!(
            total_error,
            StagedSnapshotError::EncodedPublicationTooLarge { .. }
        ));
    }

    #[test]
    fn differing_wtxids_follow_ascending_set_bit_rows() {
        let (source, snapshot) = fixture();
        let bundle = encode_staged_snapshot(&source, &snapshot).expect("encoding");
        let membership = body_json(&bundle.membership);
        assert_eq!(
            decoded(&membership, "differing_wtxid_bitset_base64"),
            vec![0b0000_0101]
        );
        let differing = decoded(&membership, "differing_wtxids_base64");
        assert_eq!(&differing[..32], &[0x91; 32]);
        assert_eq!(&differing[32..], &[0x93; 32]);
    }

    #[test]
    fn unclassified_rows_have_zero_dictionary_codes_and_absent_structure() {
        let (source, snapshot) = fixture();
        let bundle = encode_staged_snapshot(&source, &snapshot).expect("encoding");
        let structure = body_json(&bundle.structure);
        assert_eq!(
            decoded(&structure, "presence_bitset_base64"),
            vec![0b0000_0101]
        );
        assert_eq!(decoded_column(&structure, "input_count"), vec![2, 2]);

        let lane = bundle
            .classifier_stages
            .iter()
            .find(|stage| stage.descriptor.classifier_id.as_deref() == Some("transaction_shape"))
            .expect("shape lane");
        let lane = body_json(lane);
        assert_eq!(decoded_column(&lane, "result_codes"), vec![1, 0, 1]);
        assert_eq!(
            lane["result_dictionary"]
                .as_array()
                .expect("dictionary")
                .len(),
            1
        );
        assert_eq!(lane["result_dictionary"][0]["state"], "partial");
    }

    #[test]
    fn result_and_assessment_dictionaries_reconstruct_exact_ordered_vectors() {
        let (source, mut snapshot) = fixture();
        snapshot.transactions[2]
            .classifications
            .iter_mut()
            .find(|result| result.classifier_id == "transaction_shape")
            .expect("shape result")
            .missing_facts
            .reverse();
        let assessment = snapshot.transactions[2]
            .bip110
            .as_mut()
            .expect("assessment");
        assessment.violated_rules.reverse();
        assessment.unknown_rules.reverse();
        let bundle = encode_staged_snapshot(&source, &snapshot).expect("encoding");
        let properties = body_json(
            bundle
                .classifier_stages
                .iter()
                .find(|stage| {
                    stage.descriptor.classifier_id.as_deref() == Some("transaction_properties")
                })
                .expect("properties lane"),
        );
        assert_eq!(decoded_column(&properties, "result_codes"), vec![1, 0, 1]);
        assert_eq!(
            properties["result_dictionary"],
            serde_json::json!([{
                "state": "complete",
                "primary_label": null,
                "labels": ["p2wpkh", "version_2"],
                "missing_facts": []
            }])
        );

        let shape = body_json(
            bundle
                .classifier_stages
                .iter()
                .find(|stage| {
                    stage.descriptor.classifier_id.as_deref() == Some("transaction_shape")
                })
                .expect("shape lane"),
        );
        assert_eq!(decoded_column(&shape, "result_codes"), vec![1, 0, 2]);
        assert_eq!(
            shape["result_dictionary"][0]["missing_facts"],
            serde_json::json!(["raw_transaction", "input_script_pubkeys"])
        );
        assert_eq!(
            shape["result_dictionary"][1]["missing_facts"],
            serde_json::json!(["input_script_pubkeys", "raw_transaction"])
        );

        let bip110 = body_json(
            bundle
                .classifier_stages
                .iter()
                .find(|stage| {
                    stage.descriptor.classifier_id.as_deref() == Some(KNOTS_BIP110_CLASSIFIER_ID)
                })
                .expect("BIP-110 lane"),
        );
        let bip110 = &bip110["bip110"];
        assert_eq!(decoded_column(bip110, "assessment_codes"), vec![1, 0, 2]);
        assert_eq!(
            bip110["assessment_dictionary"][0]["violated_rules"],
            serde_json::json!(["element_size", "output_size"])
        );
        assert_eq!(
            bip110["assessment_dictionary"][1]["violated_rules"],
            serde_json::json!(["output_size", "element_size"])
        );
        assert_eq!(
            bip110["assessment_dictionary"][0]["unknown_rules"],
            serde_json::json!(["op_success", "taproot_annex"])
        );
        assert_eq!(
            bip110["assessment_dictionary"][1]["unknown_rules"],
            serde_json::json!(["taproot_annex", "op_success"])
        );
    }

    #[test]
    fn changed_classification_row_changes_dependent_ids_and_publication() {
        let (source, snapshot) = fixture();
        let before = encode_staged_snapshot(&source, &snapshot).expect("before");
        let mut changed = snapshot.clone();
        let result = changed.transactions[0]
            .classifications
            .iter_mut()
            .find(|result| result.classifier_id == "transaction_shape")
            .expect("shape result");
        result.missing_facts.reverse();
        let after = encode_staged_snapshot(&source, &changed).expect("after");

        assert_eq!(
            before.population.descriptor.content_id,
            after.population.descriptor.content_id
        );
        assert_eq!(
            before.membership.descriptor.content_id,
            after.membership.descriptor.content_id
        );
        assert_ne!(
            before
                .classifier_stages
                .iter()
                .find(|stage| stage.descriptor.classifier_id.as_deref() == Some("transaction_shape"))
                .expect("before lane")
                .descriptor
                .content_id,
            after
                .classifier_stages
                .iter()
                .find(|stage| stage.descriptor.classifier_id.as_deref() == Some("transaction_shape"))
                .expect("after lane")
                .descriptor
                .content_id
        );
        assert_ne!(
            before.manifest.value.classification_set_id,
            after.manifest.value.classification_set_id
        );
        assert_ne!(
            before.structure.descriptor.content_id,
            after.structure.descriptor.content_id
        );
        assert_ne!(
            before.manifest.value.publication_id,
            after.manifest.value.publication_id
        );
    }

    #[test]
    fn publication_authenticates_catalog_and_summary_semantics() {
        let (source, snapshot) = fixture();
        let baseline = encode_staged_snapshot(&source, &snapshot).expect("baseline");

        let mut changed_catalog = snapshot.clone();
        changed_catalog.classifier_catalog[0]
            .title
            .push_str(" revised");
        let catalog_bundle =
            encode_staged_snapshot(&source, &changed_catalog).expect("changed catalog");
        assert_eq!(
            baseline.manifest.value.classification_set_id,
            catalog_bundle.manifest.value.classification_set_id
        );
        assert_ne!(
            baseline.manifest.value.publication_id,
            catalog_bundle.manifest.value.publication_id
        );

        let mut changed_summary = snapshot.clone();
        *changed_summary.classification_summaries[0]
            .label_counts
            .get_mut("p2wpkh")
            .expect("label count") += 1;
        let summary_bundle =
            encode_staged_snapshot(&source, &changed_summary).expect("changed summary");
        assert_ne!(
            baseline.manifest.value.publication_id,
            summary_bundle.manifest.value.publication_id
        );

        let mut changed_bip110_summary = snapshot.clone();
        changed_bip110_summary.bip110_summary.evaluator_version = "different".to_owned();
        let bip110_bundle = encode_staged_snapshot(&source, &changed_bip110_summary)
            .expect("changed BIP-110 summary");
        assert_ne!(
            baseline.manifest.value.publication_id,
            bip110_bundle.manifest.value.publication_id
        );
    }

    #[test]
    fn manifest_descriptors_match_body_digests_and_lengths() {
        let (source, snapshot) = fixture();
        let bundle = encode_staged_snapshot(&source, &snapshot).expect("encoding");
        let stages = std::iter::once(&bundle.population)
            .chain(std::iter::once(&bundle.membership))
            .chain(std::iter::once(&bundle.structure))
            .chain(bundle.classifier_stages.iter())
            .collect::<Vec<_>>();
        assert_eq!(bundle.manifest.value.stages.len(), stages.len());
        for (manifest_descriptor, stage) in bundle.manifest.value.stages.iter().zip(stages) {
            assert_eq!(manifest_descriptor, &stage.descriptor);
            assert_eq!(manifest_descriptor.content_id, Digest::of(&stage.bytes).hex);
            assert_eq!(
                manifest_descriptor.uncompressed_bytes,
                u64::try_from(stage.bytes.len()).expect("length")
            );
        }
        assert_eq!(
            bundle.manifest.content_id,
            Digest::of(&bundle.manifest.bytes).hex
        );
    }

    #[test]
    fn metadata_only_reencoding_retains_the_complete_stage_graph() {
        let (source, snapshot) = fixture();
        let bundle = encode_staged_snapshot(&source, &snapshot).expect("encoding");
        let stage_bytes = std::iter::once(&bundle.population)
            .chain(std::iter::once(&bundle.membership))
            .chain(std::iter::once(&bundle.structure))
            .chain(bundle.classifier_stages.iter())
            .map(|stage| (stage.descriptor.clone(), stage.bytes.clone()))
            .collect::<Vec<_>>();

        let mut stale = source;
        stale.availability = SourceAvailability::Stale;
        stale.last_poll_started_at_ms = Some(1_700_000_000_100);
        stale.last_error = Some("node unavailable".to_owned());
        let replacement =
            reencode_manifest_for_source(&bundle.manifest.value, &stale).expect("reencoding");

        assert_eq!(replacement.value.source, stale);
        assert_eq!(
            replacement.value.stages, bundle.manifest.value.stages,
            "the retained descriptors are the complete stage graph"
        );
        assert_eq!(
            replacement.value.population_id,
            bundle.manifest.value.population_id
        );
        assert_eq!(
            replacement.value.classification_set_id,
            bundle.manifest.value.classification_set_id
        );
        assert_eq!(
            replacement.value.observed_at_ms,
            bundle.manifest.value.observed_at_ms
        );
        assert_eq!(
            replacement.value.classification_revision,
            bundle.manifest.value.classification_revision
        );
        assert_ne!(
            replacement.value.publication_id,
            bundle.manifest.value.publication_id
        );
        assert_ne!(replacement.content_id, bundle.manifest.content_id);
        assert_eq!(
            stage_bytes,
            std::iter::once(&bundle.population)
                .chain(std::iter::once(&bundle.membership))
                .chain(std::iter::once(&bundle.structure))
                .chain(bundle.classifier_stages.iter())
                .map(|stage| (stage.descriptor.clone(), stage.bytes.clone()))
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn rejects_invalid_hex_and_catalog_result_mismatch() {
        let (source, mut snapshot) = fixture();
        snapshot.transactions[0].txid = "AA".repeat(32);
        assert!(matches!(
            encode_staged_snapshot(&source, &snapshot),
            Err(StagedSnapshotError::Invalid(_))
        ));

        let (source, mut snapshot) = fixture();
        snapshot.transactions[0].classifications[0].classifier_id = "unknown".to_owned();
        assert!(matches!(
            encode_staged_snapshot(&source, &snapshot),
            Err(StagedSnapshotError::Invalid(_))
        ));

        let (mut source, snapshot) = fixture();
        source.total_vsize = Some(snapshot.total_vsize + 1);
        assert!(matches!(
            encode_staged_snapshot(&source, &snapshot),
            Err(StagedSnapshotError::Invalid(_))
        ));

        let (source, mut snapshot) = fixture();
        snapshot.transactions[0].classifications.swap(0, 1);
        assert!(matches!(
            encode_staged_snapshot(&source, &snapshot),
            Err(StagedSnapshotError::Invalid(_))
        ));

        let (source, mut snapshot) = fixture();
        snapshot.transactions[0].classifications.pop();
        snapshot.transactions[0].bip110 = None;
        assert!(matches!(
            encode_staged_snapshot(&source, &snapshot),
            Err(StagedSnapshotError::Invalid(_))
        ));

        let (source, mut snapshot) = fixture();
        snapshot.transactions[0].classifications[0].evidence =
            Some(serde_json::json!({ "not": "compact" }));
        assert!(matches!(
            encode_staged_snapshot(&source, &snapshot),
            Err(StagedSnapshotError::Invalid(_))
        ));
    }

    #[test]
    fn catalog_order_is_the_classifier_stage_order() {
        let (source, mut snapshot) = fixture();
        snapshot.classifier_catalog = classifier_catalog();
        let bundle = encode_staged_snapshot(&source, &snapshot).expect("encoding");
        assert_eq!(
            bundle
                .classifier_stages
                .iter()
                .map(|stage| stage
                    .descriptor
                    .classifier_id
                    .as_deref()
                    .expect("classifier id"))
                .collect::<Vec<_>>(),
            snapshot
                .classifier_catalog
                .iter()
                .map(|descriptor| descriptor.id.as_str())
                .collect::<Vec<_>>()
        );
    }
}
