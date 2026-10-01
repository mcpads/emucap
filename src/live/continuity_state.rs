//! Serializable continuity state and bounded public projections.

use crate::live::link::EmulatorIdentity;
use crate::live::runtime::{LeaseRecord, LeaseView, ProcessIdentity, TerminationRecord};
use serde::{Deserialize, Serialize};
use serde_json::Value;

const LINK_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TransportState {
    Connected,
    Stalled,
    Disconnected,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionState {
    Running,
    Frozen,
    Crashed,
    Exited,
    Unknown,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceState {
    Live,
    Exact,
    LastGood,
    Unavailable,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeBindingState {
    Bound,
    Mismatched,
    Unmanaged,
    Unobserved,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RuntimeBinding {
    pub state: RuntimeBindingState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_launch_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub live_launch_id: Option<String>,
    pub reason: String,
}

impl Default for RuntimeBinding {
    fn default() -> Self {
        Self {
            state: RuntimeBindingState::Unobserved,
            current_launch_id: None,
            live_launch_id: None,
            reason: "no live adapter identity has been observed".into(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TransportContinuity {
    pub state: TransportState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_response_unix_ms: Option<u64>,
    pub consecutive_timeouts: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExecutionContinuity {
    pub state: ExecutionState,
    pub source: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EvidenceContinuity {
    pub state: EvidenceState,
    pub failure_context_available: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FailureObservation {
    pub active: bool,
    pub kind: String,
    pub operation: String,
    pub observed_at_unix_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recovered_at_unix_ms: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RuntimeDiagnostic {
    pub artifact: String,
    pub path: String,
    pub kind: String,
    pub reason: String,
    /// False only for an evidence artifact whose bytes are never used to establish process,
    /// generation, or lease ownership. Older serialized diagnostics default to fail-closed.
    #[serde(default = "runtime_diagnostic_blocks_transition_by_default")]
    pub blocks_generation_transition: bool,
}

const fn runtime_diagnostic_blocks_transition_by_default() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ContinuitySnapshot {
    pub runtime_binding: RuntimeBinding,
    pub transport: TransportContinuity,
    pub execution: ExecutionContinuity,
    pub evidence: EvidenceContinuity,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_failure: Option<FailureObservation>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub runtime_diagnostics: Vec<RuntimeDiagnostic>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub termination: Option<TerminationRecord>,
    /// Kept out of the `continuity` JSON; status surfaces it under `runtime_instance.lease`.
    #[serde(skip)]
    pub lease: LeaseView,
    /// Distinguishes a missing lease record from a present record whose holder is unverifiable.
    #[serde(skip)]
    pub lease_record_present: bool,
}

impl Default for ContinuitySnapshot {
    fn default() -> Self {
        Self {
            runtime_binding: RuntimeBinding::default(),
            transport: TransportContinuity {
                state: TransportState::Disconnected,
                last_response_unix_ms: None,
                consecutive_timeouts: 0,
            },
            execution: ExecutionContinuity {
                state: ExecutionState::Unknown,
                source: "host".into(),
            },
            evidence: EvidenceContinuity {
                state: EvidenceState::Unavailable,
                failure_context_available: false,
            },
            last_failure: None,
            runtime_diagnostics: Vec::new(),
            termination: None,
            lease: LeaseView::unknown(),
            lease_record_present: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TemporalState {
    Active,
    Unverified,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TemporalOperation {
    pub key: crate::live::reconnect::cancellation::OperationKey,
    pub holder: ProcessIdentity,
    pub control_session_key: Option<String>,
    pub state: TemporalState,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LinkRecord {
    pub schema_version: u32,
    pub launch_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temporal_operation: Option<TemporalOperation>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lease: Option<LeaseRecord>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_identity: Option<EmulatorIdentity>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_response_unix_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_method: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_status: Option<Value>,
    pub transport_state: TransportState,
    pub consecutive_timeouts: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_failure: Option<FailureObservation>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub truncated: bool,
    pub updated_at_unix_ms: u64,
}

impl LinkRecord {
    pub(crate) fn new(launch_id: String) -> Self {
        Self {
            schema_version: LINK_SCHEMA_VERSION,
            launch_id,
            temporal_operation: None,
            lease: None,
            last_identity: None,
            last_response_unix_ms: None,
            last_method: None,
            last_status: None,
            transport_state: TransportState::Disconnected,
            consecutive_timeouts: 0,
            last_failure: None,
            truncated: false,
            updated_at_unix_ms: crate::live::runtime::now_unix_ms(),
        }
    }

    pub(crate) fn bounded(mut self) -> Self {
        if let Some(identity) = self.last_identity.as_mut() {
            // Reclaim capability is transport auth, never diagnostic evidence.
            identity.session_token = None;
        }
        let size = serde_json::to_vec(&self)
            .map(|v| v.len())
            .unwrap_or(usize::MAX);
        if size as u64 <= crate::live::runtime::MAX_CAPSULE_FILE_BYTES {
            return self;
        }
        self.truncated = true;
        self.last_status = Some(serde_json::json!({
            "truncated": true,
            "reason": "last_status exceeded runtime capsule limit"
        }));
        if let Some(identity) = self.last_identity.as_mut() {
            for value in [
                &mut identity.system,
                &mut identity.adapter,
                &mut identity.build,
                &mut identity.name,
                &mut identity.content,
                &mut identity.launch_id,
            ] {
                if let Some(value) = value.as_mut() {
                    value.truncate(1024);
                }
            }
        }
        if let Some(failure) = self.last_failure.as_mut() {
            failure.kind.truncate(128);
            failure.operation.truncate(128);
        }
        if serde_json::to_vec(&self)
            .map(|bytes| bytes.len() as u64 > crate::live::runtime::MAX_CAPSULE_FILE_BYTES)
            .unwrap_or(true)
        {
            self.last_identity = None;
        }
        self
    }

    pub fn public_value(&self) -> Value {
        let mut identity = self.last_identity.clone();
        if let Some(identity) = identity.as_mut() {
            identity.session_token = None;
        }
        serde_json::json!({
            "launch_id": self.launch_id,
            "temporal_operation": self.temporal_operation.as_ref().map(|operation| serde_json::json!({
                "operation_id": operation.key.operation_id, "state": operation.state,
            })),
            "last_identity": identity,
            "last_response_unix_ms": self.last_response_unix_ms,
            "last_method": self.last_method,
            "last_status": self.last_status,
            "transport_state": self.transport_state,
            "consecutive_timeouts": self.consecutive_timeouts,
            "last_failure": self.last_failure,
            "truncated": self.truncated,
            "updated_at_unix_ms": self.updated_at_unix_ms,
        })
    }
}

fn is_false(value: &bool) -> bool {
    !*value
}
