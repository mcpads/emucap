use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::bundle::recording_manifest::EventClassIdentity;

const BUILTIN: &str = include_str!("../contracts/event-classes.json");

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
struct RegistryFile {
    registry: String,
    classes: Vec<EventContract>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EventContract {
    pub id: String,
    pub revision: u32,
    pub contract: String,
    pub contract_sha256: String,
    pub clock_domain: String,
    pub clock_order: ClockOrder,
    pub frame_relation: FrameRelation,
    pub payload_kind: PayloadKind,
    #[serde(default)]
    pub payload_fields: Vec<PayloadField>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PayloadField {
    pub path: String,
    #[serde(rename = "type")]
    pub value_type: PayloadValueType,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PayloadValueType {
    U64,
    Bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClockOrder {
    Strict,
    Nondecreasing,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FrameRelation {
    Tick,
    PreviousTick,
    Independent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PayloadKind {
    EmptyObject,
    Object,
}

#[derive(Debug, Clone)]
pub struct EventContractRegistry {
    revision: String,
    classes: BTreeMap<String, EventContract>,
}

#[derive(Debug, thiserror::Error)]
pub enum EventContractError {
    #[error("event contract registry parse failed: {0}")]
    Parse(#[from] serde_json::Error),
    #[error("unsupported event contract registry: {0}")]
    UnsupportedRegistry(String),
    #[error("duplicate event class: {0}")]
    DuplicateClass(String),
    #[error("invalid event class id: {0}")]
    InvalidClassId(String),
    #[error("invalid payload contract for {id}: {reason}")]
    InvalidPayloadContract { id: String, reason: String },
    #[error("event contract digest mismatch for {id}: expected {expected}, got {actual}")]
    DigestMismatch {
        id: String,
        expected: String,
        actual: String,
    },
    #[error("unknown event class: {0}")]
    UnknownClass(String),
}

impl EventContractRegistry {
    pub fn builtin() -> Result<Self, EventContractError> {
        Self::parse(BUILTIN)
    }

    pub fn parse(json: &str) -> Result<Self, EventContractError> {
        let file: RegistryFile = serde_json::from_str(json)?;
        if file.registry != "emucap-event-classes/v1" {
            return Err(EventContractError::UnsupportedRegistry(file.registry));
        }
        let mut classes = BTreeMap::new();
        for contract in file.classes {
            if contract.id.is_empty()
                || !contract
                    .id
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
            {
                return Err(EventContractError::InvalidClassId(contract.id));
            }
            validate_payload_contract(&contract)?;
            let actual = hex::encode(Sha256::digest(contract.contract.as_bytes()));
            if actual != contract.contract_sha256 {
                return Err(EventContractError::DigestMismatch {
                    id: contract.id,
                    expected: contract.contract_sha256,
                    actual,
                });
            }
            let id = contract.id.clone();
            if classes.insert(id.clone(), contract).is_some() {
                return Err(EventContractError::DuplicateClass(id));
            }
        }
        let mut digest = Sha256::new();
        for (id, contract) in &classes {
            digest.update(id.as_bytes());
            digest.update([0]);
            digest.update(contract.contract_sha256.as_bytes());
            digest.update([0]);
        }
        Ok(Self {
            revision: hex::encode(digest.finalize()),
            classes,
        })
    }

    pub fn revision(&self) -> &str {
        &self.revision
    }

    pub fn get(&self, id: &str) -> Result<&EventContract, EventContractError> {
        self.classes
            .get(id)
            .ok_or_else(|| EventContractError::UnknownClass(id.to_string()))
    }

    pub fn identities(
        &self,
        ids: impl IntoIterator<Item = impl AsRef<str>>,
    ) -> Result<Vec<EventClassIdentity>, EventContractError> {
        let mut seen = BTreeSet::new();
        let mut result = Vec::new();
        for id in ids {
            let contract = self.get(id.as_ref())?;
            if seen.insert(contract.id.clone()) {
                result.push(EventClassIdentity {
                    id: contract.id.clone(),
                    contract_sha256: contract.contract_sha256.clone(),
                });
            }
        }
        Ok(result)
    }

    pub fn validate_identity(
        &self,
        identity: &EventClassIdentity,
    ) -> Result<&EventContract, EventContractError> {
        let contract = self.get(&identity.id)?;
        if contract.contract_sha256 != identity.contract_sha256 {
            return Err(EventContractError::DigestMismatch {
                id: identity.id.clone(),
                expected: contract.contract_sha256.clone(),
                actual: identity.contract_sha256.clone(),
            });
        }
        Ok(contract)
    }
}

fn validate_payload_contract(contract: &EventContract) -> Result<(), EventContractError> {
    if contract.payload_kind == PayloadKind::EmptyObject && !contract.payload_fields.is_empty() {
        return Err(EventContractError::InvalidPayloadContract {
            id: contract.id.clone(),
            reason: "empty-object payload cannot declare fields".into(),
        });
    }
    if contract.payload_kind == PayloadKind::Object && contract.payload_fields.is_empty() {
        return Err(EventContractError::InvalidPayloadContract {
            id: contract.id.clone(),
            reason: "object payload requires declarative fields".into(),
        });
    }
    let mut paths = BTreeSet::new();
    for field in &contract.payload_fields {
        if field.path.is_empty()
            || field.path.split('.').any(|part| {
                part.is_empty()
                    || !part.bytes().all(|byte| {
                        byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_'
                    })
            })
            || !paths.insert(field.path.as_str())
        {
            return Err(EventContractError::InvalidPayloadContract {
                id: contract.id.clone(),
                reason: format!("invalid or duplicate field path {}", field.path),
            });
        }
        match field.value_type {
            PayloadValueType::U64 if field.min.unwrap_or(0) <= field.max.unwrap_or(u64::MAX) => {}
            PayloadValueType::Bool if field.min.is_none() && field.max.is_none() => {}
            _ => {
                return Err(EventContractError::InvalidPayloadContract {
                    id: contract.id.clone(),
                    reason: format!("invalid bounds for {}", field.path),
                });
            }
        }
    }
    Ok(())
}
