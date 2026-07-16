//! Required MVP contract for in-process classifier rule packs.

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ClassifierManifest {
    pub id: String,
    pub version: String,
    pub required_facts: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ClassificationStatus {
    Complete,
    Partial,
    Unknown,
    NotApplicable,
    Error,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ClassificationResult {
    pub classifier_id: String,
    pub classifier_version: String,
    pub status: ClassificationStatus,
    pub value: Value,
    pub evidence: Value,
}

pub struct ClassificationInput<'a> {
    pub txid: &'a str,
    pub wtxid: Option<&'a str>,
    pub raw_transaction: Option<&'a [u8]>,
}

pub trait Classifier: Send + Sync {
    fn manifest(&self) -> ClassifierManifest;
    fn classify(&self, input: &ClassificationInput<'_>) -> ClassificationResult;
}
