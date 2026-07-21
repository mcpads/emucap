//! Host-side continuity observation for emulator links.
//!
//! `ObservedLink` records only facts seen while forwarding real tool calls. It deliberately adds no
//! heartbeat: an idle or frozen emulator is not a failure. The durable `link.json` lets a replacement
//! MCP report the last trustworthy status after the socket has timed out or disconnected.

use std::io;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::link::{Capabilities, EmulatorIdentity, EmulatorLink, LinkError};
use super::runtime::{
    capture_process, control_session_key, process_state, CurrentManifest, LeaseRecord, LeaseState,
    LeaseView, ProcessIdentity, ProcessState, RuntimeStore,
};

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
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ContinuitySnapshot {
    pub transport: TransportContinuity,
    pub execution: ExecutionContinuity,
    pub evidence: EvidenceContinuity,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_failure: Option<FailureObservation>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub runtime_diagnostics: Vec<RuntimeDiagnostic>,
    /// Kept out of the `continuity` JSON; status surfaces it under `runtime_instance.lease`.
    #[serde(skip)]
    pub lease: LeaseView,
}

impl Default for ContinuitySnapshot {
    fn default() -> Self {
        Self {
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
            lease: LeaseView::unknown(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LinkRecord {
    pub schema_version: u32,
    pub launch_id: String,
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
            lease: None,
            last_identity: None,
            last_response_unix_ms: None,
            last_method: None,
            last_status: None,
            transport_state: TransportState::Disconnected,
            consecutive_timeouts: 0,
            last_failure: None,
            truncated: false,
            updated_at_unix_ms: super::runtime::now_unix_ms(),
        }
    }

    fn bounded(mut self) -> Self {
        if let Some(identity) = self.last_identity.as_mut() {
            // Reclaim capability is transport auth, never diagnostic evidence.
            identity.session_token = None;
        }
        let size = serde_json::to_vec(&self)
            .map(|v| v.len())
            .unwrap_or(usize::MAX);
        if size as u64 <= super::runtime::MAX_CAPSULE_FILE_BYTES {
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
            .map(|bytes| bytes.len() as u64 > super::runtime::MAX_CAPSULE_FILE_BYTES)
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

/// Common wrapper for direct and broker links. All durable writes are scoped to the current launch
/// generation; records from another launch id are read as stale evidence and never merged as active.
pub struct ObservedLink<L> {
    inner: L,
    store: RuntimeStore,
    control_key: Option<String>,
    holder: ProcessIdentity,
    current: Option<CurrentManifest>,
    record: Option<LinkRecord>,
    adapter_failure: Option<Value>,
    runtime_diagnostics: Vec<RuntimeDiagnostic>,
    snapshot: ContinuitySnapshot,
}

pub fn observed<L: EmulatorLink>(inner: L) -> ObservedLink<L> {
    ObservedLink::new(inner)
}

impl<L: EmulatorLink> ObservedLink<L> {
    pub fn new(inner: L) -> Self {
        Self::with_store(inner, RuntimeStore::discover())
    }

    pub fn with_store(inner: L, store: RuntimeStore) -> Self {
        let mut observed = Self {
            inner,
            store,
            control_key: control_session_key(),
            holder: capture_process(std::process::id()),
            current: None,
            record: None,
            adapter_failure: None,
            runtime_diagnostics: Vec::new(),
            snapshot: ContinuitySnapshot::default(),
        };
        observed.refresh_runtime();
        observed
    }

    fn current_location(&self) -> Option<(u16, &str)> {
        let current = self.current.as_ref()?;
        Some((current.port, current.launch_id.as_str()))
    }

    fn refresh_runtime(&mut self) {
        self.runtime_diagnostics.clear();
        self.adapter_failure = None;
        let Some(port) = self.inner.endpoint_port() else {
            self.rebuild_snapshot();
            return;
        };
        self.current = match self.store.read_current(port) {
            Ok(current) => current,
            Err(error) => {
                self.runtime_diagnostics.push(runtime_diagnostic(
                    "current",
                    self.store.current_path(port),
                    &error,
                ));
                None
            }
        };
        self.record = match self.current.as_ref() {
            Some(current) => {
                match self
                    .store
                    .read_link_json::<LinkRecord>(port, &current.launch_id)
                {
                    Ok(record) => record.filter(|record| record.launch_id == current.launch_id),
                    Err(error) => {
                        self.runtime_diagnostics.push(runtime_diagnostic(
                            "link",
                            self.store.link_path(port, &current.launch_id),
                            &error,
                        ));
                        None
                    }
                }
            }
            None => None,
        };
        self.adapter_failure = match self.current.as_ref() {
            Some(current) => match self.store.read_adapter_failure(port, &current.launch_id) {
                Ok(failure) => failure,
                Err(error) => {
                    self.runtime_diagnostics.push(runtime_diagnostic(
                        "adapter_failure",
                        self.store.adapter_failure_path(port, &current.launch_id),
                        &error,
                    ));
                    None
                }
            },
            None => None,
        };
        self.rebuild_snapshot();
    }

    fn rebuild_snapshot(&mut self) {
        let adapter_failure = self.adapter_failure.as_ref();
        let adapter_valid = self
            .current
            .as_ref()
            .zip(adapter_failure)
            .is_some_and(|(current, failure)| adapter_failure_is_current(current, failure));
        let adapter_exact = adapter_valid
            && adapter_failure.is_some_and(|failure| {
                failure.get("kind").and_then(Value::as_str) != Some("adapter_internal_error")
            });
        let adapter_active =
            adapter_valid && adapter_failure.is_some_and(adapter_failure_is_active);
        let lease = self
            .record
            .as_ref()
            .and_then(|record| record.lease.as_ref())
            .map(|lease| lease_view(lease, &self.holder))
            .unwrap_or_else(LeaseView::unknown);
        let transport = self.record.as_ref().map_or(
            TransportContinuity {
                state: TransportState::Disconnected,
                last_response_unix_ms: None,
                consecutive_timeouts: 0,
            },
            |record| TransportContinuity {
                state: record.transport_state,
                last_response_unix_ms: record.last_response_unix_ms,
                consecutive_timeouts: record.consecutive_timeouts,
            },
        );
        let process = self.current.as_ref().map(CurrentManifest::process_state);
        let last_status = self
            .record
            .as_ref()
            .and_then(|record| record.last_status.as_ref());
        let execution = if process == Some(ProcessState::Exited) {
            ExecutionContinuity {
                state: ExecutionState::Exited,
                source: "host".into(),
            }
        } else if adapter_active {
            ExecutionContinuity {
                state: adapter_failure
                    .map(adapter_failure_execution)
                    .unwrap_or(ExecutionState::Unknown),
                source: "adapter".into(),
            }
        } else if transport.state == TransportState::Connected {
            status_execution(last_status).unwrap_or(ExecutionContinuity {
                state: ExecutionState::Unknown,
                source: "host".into(),
            })
        } else {
            ExecutionContinuity {
                state: ExecutionState::Unknown,
                source: "host".into(),
            }
        };
        let link_failure_available = self
            .record
            .as_ref()
            .and_then(|record| record.last_failure.as_ref())
            .is_some();
        let evidence = if adapter_exact && adapter_active {
            EvidenceContinuity {
                state: EvidenceState::Exact,
                failure_context_available: true,
            }
        } else if adapter_active {
            EvidenceContinuity {
                state: if last_status.is_some() {
                    EvidenceState::LastGood
                } else {
                    EvidenceState::Unavailable
                },
                failure_context_available: true,
            }
        } else if transport.state == TransportState::Connected && last_status.is_some() {
            EvidenceContinuity {
                state: EvidenceState::Live,
                failure_context_available: adapter_valid || link_failure_available,
            }
        } else if last_status.is_some() {
            EvidenceContinuity {
                state: EvidenceState::LastGood,
                failure_context_available: true,
            }
        } else {
            EvidenceContinuity {
                state: EvidenceState::Unavailable,
                failure_context_available: adapter_valid || link_failure_available,
            }
        };
        self.snapshot = ContinuitySnapshot {
            transport,
            execution,
            evidence,
            last_failure: self.record.as_ref().and_then(|r| r.last_failure.clone()),
            runtime_diagnostics: self.runtime_diagnostics.clone(),
            lease,
        };
    }

    fn claim_lease(&mut self) -> io::Result<LeaseView> {
        self.claim_lease_for(None)
    }

    fn claim_lease_for(&mut self, expected_launch_id: Option<&str>) -> io::Result<LeaseView> {
        self.refresh_runtime();
        let Some((port, launch_id)) = self
            .current_location()
            .map(|(port, id)| (port, id.to_string()))
        else {
            return Ok(LeaseView::unknown());
        };
        if expected_launch_id.is_some_and(|expected| expected != launch_id) {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "runtime current generation changed before lease acquisition",
            ));
        }
        let holder = self.holder.clone();
        let control_key = self.control_key.clone();
        let record_launch_id = launch_id.clone();
        let updated =
            self.store
                .update_link_json::<LinkRecord, _>(port, &launch_id, move |record| {
                    let mut record = record
                        .filter(|record| record.launch_id == record_launch_id)
                        .unwrap_or_else(|| LinkRecord::new(record_launch_id.clone()));
                    let now = super::runtime::now_unix_ms();
                    match record.lease.as_mut() {
                        Some(lease) if lease.holder == holder => {
                            lease.refreshed_at_unix_ms = now;
                        }
                        Some(lease) => match process_state(&lease.holder) {
                            ProcessState::Alive => {
                                return Err(io::Error::new(
                                    io::ErrorKind::PermissionDenied,
                                    "runtime lease is held by a live controller",
                                ));
                            }
                            ProcessState::Exited => {
                                *lease = LeaseRecord {
                                    control_session_key: control_key.clone(),
                                    holder: holder.clone(),
                                    acquired_at_unix_ms: now,
                                    refreshed_at_unix_ms: now,
                                };
                            }
                            ProcessState::Unknown => {
                                return Err(io::Error::new(
                                    io::ErrorKind::PermissionDenied,
                                    "runtime lease cannot be reclaimed safely",
                                ));
                            }
                        },
                        None => {
                            record.lease = Some(LeaseRecord {
                                control_session_key: control_key.clone(),
                                holder: holder.clone(),
                                acquired_at_unix_ms: now,
                                refreshed_at_unix_ms: now,
                            });
                        }
                    }
                    record.updated_at_unix_ms = now;
                    Ok(record.bounded())
                });
        let updated = match updated {
            Ok(updated) => updated,
            Err(error) if error.kind() == io::ErrorKind::PermissionDenied => {
                self.refresh_runtime();
                return Ok(self
                    .record
                    .as_ref()
                    .and_then(|record| record.lease.as_ref())
                    .map(|lease| lease_view(lease, &self.holder))
                    .unwrap_or_else(LeaseView::unknown));
            }
            Err(error) => return Err(error),
        };
        let view = updated
            .lease
            .as_ref()
            .map(|lease| lease_view(lease, &self.holder))
            .unwrap_or_else(LeaseView::unknown);
        self.record = Some(updated);
        self.rebuild_snapshot();
        Ok(view)
    }

    fn ensure_mutation_lease(&mut self) -> Result<(), LinkError> {
        match self
            .claim_lease()
            .map_err(|e| LinkError::Protocol(format!("lease: {e}")))?
            .state
        {
            LeaseState::Held => Ok(()),
            // Before a launch generation exists there is no lease to guard; the launcher creates
            // the generation and rotates the reclaim capability atomically with its own checks.
            LeaseState::Unknown if self.current.is_none() => Ok(()),
            LeaseState::Occupied | LeaseState::Available | LeaseState::Unknown => {
                Err(LinkError::Busy)
            }
        }
    }

    fn record_success(&mut self, method: &str, result: &Value) {
        self.refresh_runtime();
        if self.current.is_some()
            && self
                .claim_lease()
                .map(|lease| lease.state != LeaseState::Held)
                .unwrap_or(true)
        {
            return;
        }
        let Some((port, launch_id)) = self
            .current_location()
            .map(|(port, id)| (port, id.to_string()))
        else {
            let now = super::runtime::now_unix_ms();
            let mut identity = self.inner.capabilities().identity.clone();
            identity.session_token = None;
            let record = self.record.get_or_insert_with(|| {
                LinkRecord::new(
                    identity
                        .launch_id
                        .clone()
                        .unwrap_or_else(|| "unmanaged".into()),
                )
            });
            record.last_identity = Some(identity);
            record.last_response_unix_ms = Some(now);
            record.last_method = Some(method.into());
            if method == "status" {
                record.last_status = Some(result.clone());
            }
            record.transport_state = TransportState::Connected;
            record.consecutive_timeouts = 0;
            if let Some(failure) = record.last_failure.as_mut() {
                if failure.active {
                    failure.active = false;
                    failure.recovered_at_unix_ms = Some(now);
                }
            }
            record.updated_at_unix_ms = now;
            self.rebuild_snapshot();
            return;
        };
        let mut identity = self.inner.capabilities().identity.clone();
        identity.session_token = None;
        let method = method.to_string();
        let result = result.clone();
        let record_launch_id = launch_id.clone();
        if let Ok(updated) =
            self.store
                .update_link_json::<LinkRecord, _>(port, &launch_id, move |record| {
                    let mut record = record
                        .filter(|record| record.launch_id == record_launch_id)
                        .unwrap_or_else(|| LinkRecord::new(record_launch_id.clone()));
                    let now = super::runtime::now_unix_ms();
                    record.last_identity = Some(identity);
                    record.last_response_unix_ms = Some(now);
                    record.last_method = Some(method.clone());
                    if method == "status" {
                        record.last_status = Some(result);
                    }
                    record.transport_state = TransportState::Connected;
                    record.consecutive_timeouts = 0;
                    if let Some(failure) = record.last_failure.as_mut() {
                        if failure.active {
                            failure.active = false;
                            failure.recovered_at_unix_ms = Some(now);
                        }
                    }
                    record.updated_at_unix_ms = now;
                    Ok(record.bounded())
                })
        {
            self.record = Some(updated);
        }
        self.refresh_runtime();
    }

    fn record_failure(&mut self, method: &str, error: &LinkError) {
        if matches!(
            error,
            LinkError::IdentityMismatch { .. }
                | LinkError::PortBusy { .. }
                | LinkError::Busy
                | LinkError::NoSuchEmulator { .. }
                | LinkError::Ambiguous { .. }
        ) {
            return;
        }
        self.refresh_runtime();
        if self.current.is_some()
            && self
                .claim_lease()
                .map(|lease| lease.state != LeaseState::Held)
                .unwrap_or(true)
        {
            return;
        }
        let connected_caps = !self.inner.capabilities().methods.is_empty();
        let transport = if matches!(error, LinkError::Timeout) && connected_caps {
            TransportState::Stalled
        } else {
            TransportState::Disconnected
        };
        let Some((port, launch_id)) = self
            .current_location()
            .map(|(port, id)| (port, id.to_string()))
        else {
            let now = super::runtime::now_unix_ms();
            let unmanaged_id = self
                .inner
                .capabilities()
                .identity
                .launch_id
                .clone()
                .unwrap_or_else(|| "unmanaged".into());
            let record = self
                .record
                .get_or_insert_with(|| LinkRecord::new(unmanaged_id));
            record.transport_state = transport;
            if matches!(error, LinkError::Timeout) {
                record.consecutive_timeouts = record.consecutive_timeouts.saturating_add(1);
            }
            record.last_failure = Some(FailureObservation {
                active: true,
                kind: error.kind().into(),
                operation: method.into(),
                observed_at_unix_ms: now,
                recovered_at_unix_ms: None,
            });
            record.updated_at_unix_ms = now;
            self.rebuild_snapshot();
            return;
        };
        let method = method.to_string();
        let kind = error.kind().to_string();
        let record_launch_id = launch_id.clone();
        if let Ok(updated) =
            self.store
                .update_link_json::<LinkRecord, _>(port, &launch_id, move |record| {
                    let mut record = record
                        .filter(|record| record.launch_id == record_launch_id)
                        .unwrap_or_else(|| LinkRecord::new(record_launch_id.clone()));
                    let now = super::runtime::now_unix_ms();
                    record.transport_state = transport;
                    if kind == "request_timeout" {
                        record.consecutive_timeouts = record.consecutive_timeouts.saturating_add(1);
                    }
                    record.last_failure = Some(FailureObservation {
                        active: true,
                        kind,
                        operation: method,
                        observed_at_unix_ms: now,
                        recovered_at_unix_ms: None,
                    });
                    record.updated_at_unix_ms = now;
                    Ok(record.bounded())
                })
        {
            self.record = Some(updated);
        }
        self.refresh_runtime();
    }

    fn failure_context_value(&self) -> Value {
        let adapter_failure = self
            .current
            .as_ref()
            .zip(self.adapter_failure.as_ref())
            .map(|(current, value)| {
                let mut value = value.clone();
                let stale = value.get("launch_id").and_then(Value::as_str)
                    != Some(current.launch_id.as_str());
                if let Some(object) = value.as_object_mut() {
                    object.insert("stale".into(), Value::Bool(stale));
                }
                value
            });
        serde_json::json!({
            "continuity": self.snapshot,
            "link_failure": self.record.as_ref().map(LinkRecord::public_value),
            "adapter_failure": adapter_failure,
        })
    }
}

fn runtime_diagnostic(
    artifact: &str,
    path: std::path::PathBuf,
    error: &io::Error,
) -> RuntimeDiagnostic {
    let reason = error.to_string();
    let kind = match error.kind() {
        io::ErrorKind::FileTooLarge => "oversized",
        io::ErrorKind::InvalidData => "invalid",
        io::ErrorKind::PermissionDenied => "permission_denied",
        io::ErrorKind::NotFound => "missing",
        _ => "io_error",
    };
    RuntimeDiagnostic {
        artifact: artifact.into(),
        path: path.display().to_string(),
        kind: kind.into(),
        reason,
    }
}

impl<L: EmulatorLink> EmulatorLink for ObservedLink<L> {
    fn capabilities(&self) -> &Capabilities {
        self.inner.capabilities()
    }

    fn call(&mut self, method: &str, params: Value) -> Result<Value, LinkError> {
        if !is_read_only(method) {
            self.ensure_mutation_lease()?;
        }
        let result = self.inner.call(method, params);
        match &result {
            Ok(value) => self.record_success(method, value),
            Err(error) => self.record_failure(method, error),
        }
        result
    }

    fn supports_session_reconnect(&self) -> bool {
        self.inner.supports_session_reconnect()
    }

    fn prepare_reconnect(&mut self) {
        self.inner.prepare_reconnect();
    }

    fn endpoint_port(&self) -> Option<u16> {
        self.inner.endpoint_port()
    }

    fn session_token(&self) -> Option<&str> {
        self.inner.session_token()
    }

    fn replace_reclaim_token(&mut self, token: &str) -> Result<bool, LinkError> {
        self.inner.replace_reclaim_token(token)
    }

    fn acquire_control_lease(&mut self, expected_launch_id: &str) -> Result<LeaseView, LinkError> {
        self.claim_lease_for(Some(expected_launch_id))
            .map_err(|error| LinkError::Protocol(format!("lease: {error}")))
    }

    fn continuity(&self) -> ContinuitySnapshot {
        self.snapshot.clone()
    }

    fn failure_context(&mut self) -> Value {
        self.refresh_runtime();
        self.failure_context_value()
    }

    fn runtime_candidates(&self) -> Vec<Value> {
        self.inner.runtime_candidates()
    }
}

fn is_read_only(method: &str) -> bool {
    matches!(
        method,
        "hello"
            | "status"
            | "get_state"
            | "read_memory"
            | "screenshot"
            | "get_trace"
            | "call_stack"
            | "disassemble"
            | "find_pattern"
            | "get_rom_info"
            | "poll_events"
            | "get_video_state"
            | "resolve_tile"
    )
}

fn status_execution(status: Option<&Value>) -> Option<ExecutionContinuity> {
    let status = status?;
    let state = status.get("state").and_then(Value::as_str)?;
    Some(ExecutionContinuity {
        state: match state {
            "running" => ExecutionState::Running,
            "crashed" | "fatal" => ExecutionState::Crashed,
            "frozen" | "paused" | "stopped" => ExecutionState::Frozen,
            _ => ExecutionState::Unknown,
        },
        source: "adapter".into(),
    })
}

fn adapter_failure_is_current(current: &CurrentManifest, failure: &Value) -> bool {
    let base_valid = failure.get("schema_version").and_then(Value::as_u64) == Some(1)
        && failure.get("launch_id").and_then(Value::as_str) == Some(current.launch_id.as_str())
        && failure
            .get("kind")
            .and_then(Value::as_str)
            .is_some_and(|kind| !kind.is_empty())
        && failure
            .get("observed_at_unix_ms")
            .and_then(Value::as_u64)
            .is_some()
        && failure.get("frame").and_then(Value::as_u64).is_some();
    if !base_valid {
        return false;
    }
    if failure.get("kind").and_then(Value::as_str) == Some("adapter_internal_error") {
        return failure
            .get("adapter")
            .and_then(Value::as_str)
            .is_some_and(|adapter| !adapter.is_empty())
            && failure
                .get("operation")
                .and_then(Value::as_str)
                .is_some_and(|operation| !operation.is_empty())
            && failure.get("reason").and_then(Value::as_str).is_some()
            && failure.get("active").and_then(Value::as_bool).is_some()
            && failure
                .get("execution_state")
                .and_then(Value::as_str)
                .is_some_and(|state| {
                    matches!(state, "running" | "frozen" | "crashed" | "unknown")
                });
    }
    failure.get("epc").and_then(Value::as_u64).is_some()
        && failure
            .get("incoming_event")
            .and_then(Value::as_u64)
            .is_some()
        && failure
            .get("registers")
            .and_then(Value::as_object)
            .is_some()
        && failure.get("pc_ring").and_then(Value::as_array).is_some()
}

fn adapter_failure_is_active(failure: &Value) -> bool {
    failure
        .get("active")
        .and_then(Value::as_bool)
        .unwrap_or(true)
}

fn adapter_failure_execution(failure: &Value) -> ExecutionState {
    match failure.get("execution_state").and_then(Value::as_str) {
        Some("running") => ExecutionState::Running,
        Some("frozen") => ExecutionState::Frozen,
        Some("crashed") => ExecutionState::Crashed,
        Some("unknown") => ExecutionState::Unknown,
        _ => ExecutionState::Crashed,
    }
}

fn lease_view(lease: &LeaseRecord, holder: &ProcessIdentity) -> LeaseView {
    let state = if &lease.holder == holder {
        LeaseState::Held
    } else {
        match process_state(&lease.holder) {
            ProcessState::Alive => LeaseState::Occupied,
            ProcessState::Exited => LeaseState::Available,
            ProcessState::Unknown => LeaseState::Unknown,
        }
    };
    LeaseView {
        state,
        holder_pid: Some(lease.holder.pid),
    }
}

fn is_false(value: &bool) -> bool {
    !*value
}

#[cfg(test)]
#[path = "continuity_tests.rs"]
mod tests;
