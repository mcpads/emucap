//! Host-side continuity observation for emulator links.
//!
//! `ObservedLink` records only facts seen while forwarding real tool calls. It deliberately adds no
//! heartbeat: an idle or frozen emulator is not a failure. The durable `link.json` lets a replacement
//! MCP report the last trustworthy status after the socket has timed out or disconnected.

use std::io;

use serde_json::Value;

use super::link::{
    Capabilities, EmulatorIdentity, EmulatorLink, LinkError, ProgressCallControl, ProgressObserver,
};
use super::runtime::{
    capture_process, control_session_key, process_state, CurrentManifest, LeaseRecord, LeaseState,
    LeaseView, ProcessIdentity, ProcessState, RuntimeStore, TerminationRecord,
};

#[path = "continuity_state.rs"]
mod state;
pub use state::{
    ContinuitySnapshot, EvidenceContinuity, EvidenceState, ExecutionContinuity, ExecutionState,
    FailureObservation, LinkRecord, RuntimeBinding, RuntimeBindingState, RuntimeDiagnostic,
    TemporalOperation, TemporalState, TransportContinuity, TransportState,
};

/// Common wrapper for direct and broker links. All durable writes are scoped to the current launch
/// generation; records from another launch id are read as stale evidence and never merged as active.
pub struct ObservedLink<L> {
    inner: L,
    store: RuntimeStore,
    control_key: Option<String>,
    holder: ProcessIdentity,
    current: Option<CurrentManifest>,
    temporal_port: Option<u16>,
    record: Option<LinkRecord>,
    live_record: Option<LinkRecord>,
    runtime_binding: RuntimeBinding,
    adapter_failure: Option<Value>,
    termination: Option<TerminationRecord>,
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
            temporal_port: None,
            record: None,
            live_record: None,
            runtime_binding: RuntimeBinding::default(),
            adapter_failure: None,
            termination: None,
            runtime_diagnostics: Vec::new(),
            snapshot: ContinuitySnapshot::default(),
        };
        observed.refresh_runtime();
        observed
    }

    fn current_temporal(&self) -> Option<&TemporalOperation> {
        let current = self.current.as_ref()?;
        let record = self
            .record
            .as_ref()
            .filter(|record| record.launch_id == current.launch_id)?;
        record
            .temporal_operation
            .as_ref()
            .filter(|operation| operation.key.runtime == current.launch_id)
    }

    fn current_location(&self) -> Option<(u16, &str)> {
        if self.runtime_binding.state != RuntimeBindingState::Bound {
            return None;
        }
        let current = self.current.as_ref()?;
        Some((current.port, current.launch_id.as_str()))
    }

    fn refresh_runtime(&mut self) {
        self.runtime_diagnostics.clear();
        self.adapter_failure = None;
        self.termination = None;
        let resolved = if let Some(port) = self.inner.endpoint_port() {
            self.store
                .read_current(port)
                .map_err(|error| ("current", self.store.current_path(port), error))
        } else if self.inner.has_exclusive_control() {
            match self.inner.capabilities().identity.launch_id.as_deref() {
                Some(launch_id) => self
                    .store
                    .current_for_launch(launch_id)
                    .map_err(|error| ("runtime_lookup", self.store.root().to_path_buf(), error)),
                None => Ok(None),
            }
        } else {
            Ok(None)
        };
        self.current = match resolved {
            Ok(current) => current,
            Err((artifact, path, error)) => {
                self.runtime_diagnostics
                    .push(runtime_diagnostic(artifact, path, &error));
                None
            }
        };
        let Some(port) = self.current.as_ref().map(|current| current.port) else {
            self.record = None;
            self.runtime_binding = runtime_binding(
                None,
                &self.inner.capabilities().identity,
                !self.inner.capabilities().methods.is_empty(),
            );
            self.rebuild_snapshot();
            return;
        };
        self.runtime_binding = runtime_binding(
            self.current.as_ref(),
            &self.inner.capabilities().identity,
            !self.inner.capabilities().methods.is_empty(),
        );
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
        self.termination = match self.current.as_ref() {
            Some(current) => match self.store.read_termination(port, &current.launch_id) {
                Ok(termination) => termination,
                Err(error) => {
                    self.runtime_diagnostics.push(runtime_diagnostic(
                        "termination",
                        self.store.termination_path(port, &current.launch_id),
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
        let current_bound = self.runtime_binding.state == RuntimeBindingState::Bound;
        let active_record = if current_bound {
            self.record.as_ref()
        } else {
            self.live_record.as_ref()
        };
        let adapter_failure = self.adapter_failure.as_ref();
        let adapter_valid =
            self.current
                .as_ref()
                .zip(adapter_failure)
                .is_some_and(|(current, failure)| {
                    current_bound && adapter_failure_is_current(current, failure)
                });
        let adapter_exact = adapter_valid
            && adapter_failure.is_some_and(|failure| {
                failure.get("kind").and_then(Value::as_str) != Some("adapter_internal_error")
            });
        let adapter_active =
            adapter_valid && adapter_failure.is_some_and(adapter_failure_is_active);
        let lease = active_record
            .and_then(|record| record.lease.as_ref())
            .map(|lease| lease_view(lease, &self.holder))
            .unwrap_or_else(LeaseView::unknown);
        let transport = active_record.map_or(
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
        let process = current_bound
            .then(|| self.current.as_ref().map(CurrentManifest::process_state))
            .flatten();
        let last_status = active_record.and_then(|record| record.last_status.as_ref());
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
        let link_failure_available = active_record
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
            runtime_binding: self.runtime_binding.clone(),
            transport,
            execution,
            evidence,
            last_failure: active_record.and_then(|r| r.last_failure.clone()),
            runtime_diagnostics: self.runtime_diagnostics.clone(),
            termination: current_bound.then(|| self.termination.clone()).flatten(),
            lease,
            lease_record_present: active_record
                .and_then(|record| record.lease.as_ref())
                .is_some(),
        };
        self.snapshot.evidence.failure_context_available |= self.current_temporal().is_some();
    }

    fn check_temporal_deadline(&mut self, control: &ProgressCallControl) -> Result<(), LinkError> {
        if control.temporal_stop_ms.is_some()
            && control
                .temporal_deadline
                .is_some_and(|deadline| std::time::Instant::now() >= deadline)
        {
            self.inner.prepare_reconnect();
            return Err(LinkError::Emulator {
                kind: "temporal_unverified".into(),
                message: "temporal deadline expired during runtime ownership admission".into(),
            });
        }
        Ok(())
    }

    fn require_runtime_lookup(&self) -> io::Result<()> {
        if let Some(diagnostic) = self
            .runtime_diagnostics
            .iter()
            .find(|d| d.artifact == "runtime_lookup")
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                diagnostic.reason.clone(),
            ));
        }
        Ok(())
    }

    fn claim_lease(&mut self) -> io::Result<LeaseView> {
        self.refresh_runtime();
        self.require_runtime_lookup()?;
        if self.runtime_binding.state != RuntimeBindingState::Bound {
            return Ok(LeaseView::unknown());
        }
        self.claim_lease_for(None)
    }

    fn claim_lease_for(&mut self, expected_launch_id: Option<&str>) -> io::Result<LeaseView> {
        self.refresh_runtime();
        self.require_runtime_lookup()?;
        if expected_launch_id.is_none() && self.runtime_binding.state != RuntimeBindingState::Bound
        {
            return Ok(LeaseView::unknown());
        }
        let Some(current) = self.current.as_ref() else {
            return Ok(LeaseView::unknown());
        };
        let port = current.port;
        let launch_id = current.launch_id.clone();
        if expected_launch_id.is_some_and(|expected| expected != launch_id) {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "runtime current generation changed before lease acquisition",
            ));
        }
        let updated = claim_generation_lease(
            &self.store,
            port,
            &launch_id,
            self.holder.clone(),
            self.control_key.clone(),
            None,
        );
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

    fn ensure_mutation_lease(&mut self, method: &str, params: &Value) -> Result<(), LinkError> {
        match self
            .claim_lease()
            .map_err(|e| LinkError::Protocol(format!("lease: {e}")))?
            .state
        {
            LeaseState::Held => {
                let Some((port, launch_id)) = self
                    .current_location()
                    .map(|(port, launch_id)| (port, launch_id.to_string()))
                else {
                    return Ok(());
                };
                let record = self
                    .store
                    .read_link_json::<LinkRecord>(port, &launch_id)
                    .map_err(|e| LinkError::Protocol(format!("temporal safety metadata: {e}")))?;
                if let Some(operation) = record.and_then(|record| record.temporal_operation) {
                    let owned = operation.state == TemporalState::Active
                        && operation.key.runtime == launch_id
                        && operation.holder == self.holder
                        && operation.control_session_key == self.control_key
                        && params.get("_temporal_owner")
                            == Some(
                                &serde_json::to_value(&operation.key)
                                    .map_err(|e| LinkError::Protocol(e.to_string()))?,
                            )
                        && matches!(
                            method,
                            "pause"
                                | "set_input"
                                | "step"
                                | "step_instructions"
                                | "begin_temporal_operation"
                                | "finish_temporal_operation"
                        );
                    if !owned {
                        return Err(LinkError::Emulator {
                            kind: "temporal_quarantined".into(),
                            message:
                                "this runtime has a temporal operation without verified cleanup"
                                    .into(),
                        });
                    }
                }
                let capsule = self
                    .store
                    .read_capture_json::<super::capture_capsule::CaptureCapsule>(port, &launch_id)
                    .map_err(|error| {
                        LinkError::Protocol(format!(
                            "recording capture safety metadata is invalid: {error}"
                        ))
                    })?;
                let Some(capsule) = capsule else {
                    return Ok(());
                };
                if capsule.generation_mutation_blocker().is_none() {
                    return Ok(());
                }

                // The recording executor persists Arming before it sends the one matching wire
                // transaction. That exact owner/capture pair is the only mutation admitted while
                // a nonterminal capture exists; every other request must wait for its terminal.
                let owned_arming = method == "record_window"
                    && capsule.state == super::capture_capsule::CaptureState::Arming
                    && params.get("capture_id").and_then(Value::as_str)
                        == Some(capsule.capture_id.as_str())
                    && capsule.lease.holder == self.holder
                    && capsule.lease.control_session_key == self.control_key;
                if owned_arming {
                    return Ok(());
                }
                Err(LinkError::Emulator {
                    kind: "recording_quarantined".into(),
                    message: capsule
                        .generation_mutation_blocker()
                        .unwrap_or_else(|| "recording capture safety is unresolved".into()),
                })
            }
            // Before a launch generation exists there is no lease to guard; the launcher creates
            // the generation and rotates the reclaim capability atomically with its own checks.
            LeaseState::Unknown
                if self.current_location().is_none() && self.inner.has_exclusive_control() =>
            {
                Ok(())
            }
            LeaseState::Occupied | LeaseState::Available | LeaseState::Unknown => {
                Err(LinkError::Busy)
            }
        }
    }

    fn record_success(&mut self, method: &str, result: &Value) {
        self.refresh_runtime();
        if self.current_location().is_some()
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
            let record = self.live_record.get_or_insert_with(|| {
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
        if self.current_location().is_some()
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
                .live_record
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
        let mut value = serde_json::json!({
            "continuity": self.snapshot,
            "link_failure": if self.runtime_binding.state == RuntimeBindingState::Bound {
                self.record.as_ref().map(LinkRecord::public_value)
            } else {
                self.live_record.as_ref().map(LinkRecord::public_value)
            },
            "adapter_failure": adapter_failure,
        });
        if let Some(operation) = self.current_temporal() {
            value["temporal_operation"] = serde_json::json!({
                "launch_id":operation.key.runtime,"operation_id":operation.key.operation_id,"state":operation.state,
            });
        }
        value
    }
}

pub(crate) fn claim_generation_lease(
    store: &RuntimeStore,
    port: u16,
    launch_id: &str,
    holder: ProcessIdentity,
    control_key: Option<String>,
    compatibility_token: Option<String>,
) -> io::Result<LinkRecord> {
    let record_launch_id = launch_id.to_string();
    let compatibility_store = store.clone();
    store.update_current_link_json::<LinkRecord, _>(port, launch_id, move |record| {
        if record
            .as_ref()
            .is_some_and(|r| r.temporal_operation.is_some() && r.launch_id != record_launch_id)
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "temporal marker belongs to a mismatched runtime record",
            ));
        }
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
        if let Some(token) = compatibility_token.as_deref() {
            compatibility_store.write_compatibility_token(port, token)?;
        }
        Ok(record.bounded())
    })
}

fn runtime_binding(
    current: Option<&CurrentManifest>,
    identity: &EmulatorIdentity,
    live_identity_observed: bool,
) -> RuntimeBinding {
    let current_launch_id = current.map(|manifest| manifest.launch_id.clone());
    let live_launch_id = identity.launch_id.clone();
    if !live_identity_observed {
        return RuntimeBinding {
            state: RuntimeBindingState::Unobserved,
            current_launch_id,
            live_launch_id,
            reason: "no live adapter identity has been observed".into(),
        };
    }
    let Some(current) = current else {
        return RuntimeBinding {
            state: RuntimeBindingState::Unmanaged,
            current_launch_id: None,
            live_launch_id,
            reason: "the live adapter has no current runtime capsule".into(),
        };
    };
    if let Some(live) = identity.launch_id.as_deref() {
        let bound = live == current.launch_id;
        return RuntimeBinding {
            state: if bound {
                RuntimeBindingState::Bound
            } else {
                RuntimeBindingState::Mismatched
            },
            current_launch_id: Some(current.launch_id.clone()),
            live_launch_id: Some(live.to_string()),
            reason: if bound {
                "live adapter launch_id matches the current runtime capsule".into()
            } else {
                "live adapter launch_id does not match the current runtime capsule".into()
            },
        };
    }

    let identity_disagrees = [
        (identity.system.as_deref(), Some(current.system.as_str())),
        (identity.adapter.as_deref(), Some(current.adapter.as_str())),
        (identity.content.as_deref(), Some(current.content.as_str())),
    ]
    .into_iter()
    .any(|(live, capsule)| {
        live.zip(capsule)
            .is_some_and(|(live, capsule)| live != capsule)
    });
    RuntimeBinding {
        state: if identity_disagrees {
            RuntimeBindingState::Mismatched
        } else {
            RuntimeBindingState::Unmanaged
        },
        current_launch_id: Some(current.launch_id.clone()),
        live_launch_id: None,
        reason: if identity_disagrees {
            "live adapter identity disagrees with the current runtime capsule".into()
        } else {
            "live adapter did not advertise a launch_id, so capsule ownership is unproven".into()
        },
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
        // adapter-failure.json is bounded failure evidence, not an ownership capsule. Losing it
        // degrades crash proof but must not strand an otherwise verifiable exited generation.
        // Unknown future artifacts remain blocking by default.
        blocks_generation_transition: artifact != "adapter_failure",
    }
}

impl<L: EmulatorLink> EmulatorLink for ObservedLink<L> {
    fn capabilities(&self) -> &Capabilities {
        self.inner.capabilities()
    }

    fn call(&mut self, method: &str, params: Value) -> Result<Value, LinkError> {
        if !is_read_only(method) {
            self.ensure_mutation_lease(method, &params)?;
        }
        let result = self.inner.call(method, params);
        match &result {
            Ok(value) => self.record_success(method, value),
            Err(error) => self.record_failure(method, error),
        }
        result
    }

    fn call_with_progress(
        &mut self,
        method: &str,
        params: Value,
        observer: &mut ProgressObserver<'_>,
        control: &ProgressCallControl,
    ) -> Result<Value, LinkError> {
        self.check_temporal_deadline(control)?;
        if !is_read_only(method) {
            self.ensure_mutation_lease(method, &params)?;
        }
        self.check_temporal_deadline(control)?;
        let result = self
            .inner
            .call_with_progress(method, params, observer, control);
        match &result {
            Ok(value) => self.record_success(method, value),
            Err(error) => self.record_failure(method, error),
        }
        result
    }

    fn begin_temporal_control(
        &mut self,
        key: &super::reconnect::cancellation::OperationKey,
    ) -> Result<(), LinkError> {
        key.validate()
            .map_err(|e| LinkError::Protocol(e.to_string()))?;
        self.ensure_mutation_lease("begin_temporal_control", &serde_json::json!({}))?;
        let (port, runtime) = self.current_location().ok_or_else(|| LinkError::Emulator {
            kind: "unsupported".into(),
            message: "temporal control requires a durable bound runtime".into(),
        })?;
        if runtime != key.runtime {
            return Err(LinkError::Protocol("temporal runtime mismatch".into()));
        }
        let key = key.clone();
        let holder = self.holder.clone();
        let control_session_key = self.control_key.clone();
        let updated = self
            .store
            .update_current_link_json::<LinkRecord, _>(port, &key.runtime.clone(), move |record| {
                let mut record =
                    record.ok_or_else(|| io::Error::other("runtime lease record missing"))?;
                if record.launch_id != key.runtime
                    || record.temporal_operation.is_some()
                    || !record.lease.as_ref().is_some_and(|lease| {
                        lease.holder == holder && lease.control_session_key == control_session_key
                    })
                {
                    return Err(io::Error::other("temporal admission ownership changed"));
                }
                record.temporal_operation = Some(TemporalOperation {
                    key,
                    holder,
                    control_session_key,
                    state: TemporalState::Active,
                });
                record.updated_at_unix_ms = super::runtime::now_unix_ms();
                Ok(record.bounded())
            })
            .map_err(|e| LinkError::Protocol(format!("persist temporal admission: {e}")))?;
        self.record = Some(updated);
        self.temporal_port = Some(port);
        self.rebuild_snapshot();
        Ok(())
    }

    fn finish_temporal_control(
        &mut self,
        key: &super::reconnect::cancellation::OperationKey,
        verified: bool,
    ) -> Result<(), LinkError> {
        self.refresh_runtime();
        if verified {
            self.require_runtime_lookup()
                .map_err(|e| LinkError::Protocol(e.to_string()))?;
        }
        let port = self
            .temporal_port
            .or_else(|| self.current_location().map(|(port, _)| port))
            .ok_or_else(|| LinkError::Protocol("temporal runtime location missing".into()))?;
        if verified && self.inner.capabilities().identity.launch_id.as_deref() != Some(&key.runtime)
        {
            return Err(LinkError::Protocol(
                "verified temporal cleanup belongs to another runtime".into(),
            ));
        }
        let holder = self.holder.clone();
        let control_key = self.control_key.clone();
        let updated = self
            .store
            .update_current_link_json::<LinkRecord, _>(port, &key.runtime, |record| {
                let mut record =
                    record.ok_or_else(|| io::Error::other("temporal record missing"))?;
                if record.launch_id != key.runtime
                    || !record.lease.as_ref().is_some_and(|lease| {
                        lease.holder == holder && lease.control_session_key == control_key
                    })
                {
                    return Err(io::Error::other("temporal completion lease changed"));
                }
                let operation = record
                    .temporal_operation
                    .as_mut()
                    .ok_or_else(|| io::Error::other("temporal owner missing"))?;
                if operation.key != *key
                    || operation.holder != holder
                    || operation.control_session_key != control_key
                    || operation.state != TemporalState::Active
                {
                    return Err(io::Error::other("temporal completion ownership changed"));
                }
                if verified {
                    record.temporal_operation = None;
                } else {
                    operation.state = TemporalState::Unverified;
                }
                record.updated_at_unix_ms = super::runtime::now_unix_ms();
                Ok(record.bounded())
            })
            .map_err(|e| LinkError::Protocol(format!("persist temporal completion: {e}")))?;
        self.record = Some(updated);
        if verified {
            self.temporal_port = None;
        }
        self.rebuild_snapshot();
        Ok(())
    }

    fn attachment_id(&self) -> Option<&str> {
        self.inner.attachment_id()
    }

    fn supports_session_reconnect(&self) -> bool {
        self.inner.supports_session_reconnect()
    }

    fn prepare_reconnect(&mut self) {
        self.inner.prepare_reconnect();
    }

    fn reattach_runtime(&mut self, expected_launch_id: &str) -> Result<Value, LinkError> {
        let result = self.inner.reattach_runtime(expected_launch_id);
        self.refresh_runtime();
        result
    }

    fn has_exclusive_control(&self) -> bool {
        self.inner.has_exclusive_control()
    }

    fn base_port(&self) -> Option<u16> {
        self.inner.base_port()
    }

    fn endpoint_port(&self) -> Option<u16> {
        self.inner.endpoint_port()
    }

    fn session_token(&self) -> Option<&str> {
        self.inner.session_token()
    }

    fn stage_reclaim_token(&mut self, token: &str) -> Result<bool, LinkError> {
        self.inner.stage_reclaim_token(token)
    }

    fn commit_staged_reclaim_token(&mut self, token: &str) -> Result<bool, LinkError> {
        self.inner.commit_staged_reclaim_token(token)
    }

    fn abort_staged_reclaim_token(&mut self, token: &str) -> Result<bool, LinkError> {
        self.inner.abort_staged_reclaim_token(token)
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

    fn runtime_reservations(&self) -> Vec<Value> {
        self.inner.runtime_reservations()
    }
}

pub(crate) fn is_read_only(method: &str) -> bool {
    matches!(
        method,
        "hello"
            | "status"
            | "get_state"
            | "read_memory"
            | "read_memory_batch"
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

pub(super) fn lease_view(lease: &LeaseRecord, holder: &ProcessIdentity) -> LeaseView {
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

#[cfg(test)]
#[path = "continuity_tests.rs"]
mod tests;
