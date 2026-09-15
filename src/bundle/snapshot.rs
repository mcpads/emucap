//! Producer-issued instruction snapshots. This evidence class is independent of recordings.
use super::recording_manifest::{RuntimeIdentity, StateArtifactIdentity};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const MAX_SNAPSHOT_BYTES: u64 = 1024 * 1024;
pub const MAX_RECEIPT_BYTES: u64 = 128 * 1024;
pub const CAPABILITY: &str = "instruction_snapshot_capture";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Clock {
    pub value: String,
    pub domain: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Halt {
    pub cpu: String,
    pub kind: String,
    pub boundary: String,
    pub pc: u64,
    pub program_bank: Option<u64>,
    pub frame: Clock,
    pub cycle: Clock,
}
impl Halt {
    pub fn validate(&self) -> Result<(), String> {
        if self.cpu != "main"
            || self.kind != "main_cpu_instruction"
            || self.boundary != "instruction_boundary"
            || self.frame.domain.is_empty()
            || self.cycle.domain.is_empty()
        {
            return Err("unsupported instruction halt facts".into());
        }
        for clock in [&self.frame, &self.cycle] {
            let n = clock
                .value
                .parse::<u64>()
                .map_err(|_| "invalid clock integer")?;
            if n.to_string() != clock.value {
                return Err("noncanonical clock integer".into());
            }
        }
        Ok(())
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Snapshot {
    pub kind: String,
    pub snapshot_id: String,
    pub source: RuntimeIdentity,
    pub snapshot: StateArtifactIdentity,
    pub halt: Halt,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Receipt {
    pub sha256: String,
    pub body: Snapshot,
}
impl Receipt {
    pub fn issue(body: Snapshot) -> Result<Self, String> {
        let bytes = serde_json::to_vec(&body).map_err(|e| e.to_string())?;
        let value: serde_json::Value = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
        // serde_json's default map is sorted; the typed schema contains no floating-point values.
        let canonical = serde_json::to_vec(&value).map_err(|e| e.to_string())?;
        let mut hash = Sha256::new();
        hash.update(b"emucap-instruction-snapshot\n");
        hash.update(canonical);
        let receipt = Self {
            sha256: hex::encode(hash.finalize()),
            body,
        };
        if serde_json::to_vec(&receipt)
            .map_err(|e| e.to_string())?
            .len() as u64
            > MAX_RECEIPT_BYTES
        {
            return Err("receipt exceeds metadata bound".into());
        }
        Ok(receipt)
    }
    pub fn verify(&self, bytes: &[u8]) -> Result<(), String> {
        self.body.halt.validate()?;
        if self.body.kind != "instruction_snapshot"
            || self.body.snapshot.format.is_empty()
            || bytes.is_empty()
            || bytes.len() as u64 > MAX_SNAPSHOT_BYTES
            || self.body.snapshot.bytes != bytes.len() as u64
            || self.body.snapshot.sha256 != hex::encode(Sha256::digest(bytes))
            || Self::issue(self.body.clone())?.sha256 != self.sha256
        {
            return Err("receipt or snapshot integrity mismatch".into());
        }
        Ok(())
    }
}
