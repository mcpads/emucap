use std::collections::VecDeque;

use serde_json::Value;

use super::*;
use crate::live::link::EmulatorIdentity;
use crate::live::runtime::TerminationState;

enum Outcome {
    Ok(Value),
    Timeout,
}

struct SequenceLink {
    caps: Capabilities,
    port: u16,
    outcomes: VecDeque<Outcome>,
}

struct NoPortLink(SequenceLink);

impl SequenceLink {
    fn new(port: u16, launch_id: &str, outcomes: impl IntoIterator<Item = Outcome>) -> Self {
        Self {
            caps: Capabilities {
                protocol_version: 1,
                methods: vec!["status".into()],
                memory_types: vec![],
                memory_regions: vec![],
                breakpoint_kinds: vec![],
                contracts: crate::contracts::ContractAdvertisement::Unreported,
                recording: None,
                features: Default::default(),
                identity: EmulatorIdentity {
                    launch_id: Some(launch_id.into()),
                    ..Default::default()
                },
            },
            port,
            outcomes: outcomes.into_iter().collect(),
        }
    }
}

impl EmulatorLink for SequenceLink {
    fn capabilities(&self) -> &Capabilities {
        &self.caps
    }

    fn call(&mut self, _method: &str, _params: Value) -> Result<Value, LinkError> {
        match self.outcomes.pop_front().expect("test outcome") {
            Outcome::Ok(value) => Ok(value),
            Outcome::Timeout => Err(LinkError::Timeout),
        }
    }

    fn endpoint_port(&self) -> Option<u16> {
        Some(self.port)
    }

    fn has_exclusive_control(&self) -> bool {
        true
    }
}

impl EmulatorLink for NoPortLink {
    fn capabilities(&self) -> &Capabilities {
        self.0.capabilities()
    }

    fn call(&mut self, method: &str, params: Value) -> Result<Value, LinkError> {
        self.0.call(method, params)
    }
}

fn current(store: &RuntimeStore, port: u16) -> CurrentManifest {
    let prepared = store.prepare(port).unwrap();
    let manifest = prepared.manifest(crate::live::runtime::ManifestSpec {
        adapter: "mesen2".into(),
        system: "snes".into(),
        content: "/game.sfc".into(),
        emulator_pid: std::process::id(),
        bridge_pid: None,
        backend_endpoint: None,
        build: Some("test".into()),
    });
    prepared.commit(&manifest).unwrap();
    manifest
}

#[test]
fn continuity_surfaces_durable_requested_stop_without_erasing_other_evidence() {
    let tmp = tempfile::tempdir().unwrap();
    let store = RuntimeStore::new(tmp.path().join("sessions"));
    let current = current(&store, 47819);
    let requested = TerminationRecord::requested(47819, current.launch_id.clone(), None);
    store.write_current_termination(&requested).unwrap();
    let inner = SequenceLink::new(47819, &current.launch_id, []);
    let link = ObservedLink::with_store(inner, store);

    let termination = link.continuity().termination.unwrap();
    assert_eq!(termination.launch_id, current.launch_id);
    assert_eq!(termination.state, TerminationState::Requested);
}

#[test]
fn timeout_preserves_last_good_and_success_recovers_failure() {
    let tmp = tempfile::tempdir().unwrap();
    let store = RuntimeStore::new(tmp.path().join("sessions"));
    let current = current(&store, 47820);
    let mut inner = SequenceLink::new(
        47820,
        &current.launch_id,
        [
            Outcome::Ok(serde_json::json!({"state": "frozen", "pc": 4660})),
            Outcome::Timeout,
            Outcome::Ok(serde_json::json!({"state": "running", "pc": 4662})),
        ],
    );
    inner.caps.identity.session_token = Some("reclaim-must-not-persist".into());
    let mut link = ObservedLink::with_store(inner, store.clone());

    link.call("status", serde_json::json!({})).unwrap();
    let first = link.continuity();
    assert_eq!(first.transport.state, TransportState::Connected);
    assert_eq!(first.execution.state, ExecutionState::Frozen);
    assert_eq!(first.evidence.state, EvidenceState::Live);
    assert_eq!(first.lease.state, LeaseState::Held);

    assert!(matches!(
        link.call("status", serde_json::json!({})),
        Err(LinkError::Timeout)
    ));
    let failed = link.continuity();
    assert_eq!(failed.transport.state, TransportState::Stalled);
    assert_eq!(failed.execution.state, ExecutionState::Unknown);
    assert_eq!(failed.evidence.state, EvidenceState::LastGood);
    assert_eq!(failed.transport.consecutive_timeouts, 1);
    assert_eq!(
        failed.last_failure.as_ref().unwrap().kind,
        "request_timeout"
    );
    assert!(failed.last_failure.as_ref().unwrap().active);
    let persisted: LinkRecord = store
        .read_link_json(47820, &current.launch_id)
        .unwrap()
        .unwrap();
    assert_eq!(persisted.last_status.unwrap()["pc"], 4660);
    let raw_link = std::fs::read_to_string(store.link_path(47820, &current.launch_id)).unwrap();
    assert!(!raw_link.contains("reclaim-must-not-persist"));

    link.call("status", serde_json::json!({})).unwrap();
    let recovered = link.continuity();
    assert_eq!(recovered.transport.state, TransportState::Connected);
    assert_eq!(recovered.execution.state, ExecutionState::Running);
    assert_eq!(recovered.evidence.state, EvidenceState::Live);
    let failure = recovered.last_failure.unwrap();
    assert!(!failure.active);
    assert!(failure.recovered_at_unix_ms.is_some());
}

#[test]
fn live_identity_mismatch_never_overwrites_the_current_capsule() {
    let tmp = tempfile::tempdir().unwrap();
    let store = RuntimeStore::new(tmp.path().join("sessions"));
    let current = current(&store, 47834);
    let mut inner = SequenceLink::new(
        47834,
        &current.launch_id,
        [
            Outcome::Ok(serde_json::json!({"state": "running", "frame": 41})),
            Outcome::Ok(serde_json::json!({"written": 1})),
        ],
    );
    // Shape the live hello like an official fallback launch: authenticated transport, but no
    // generation id, and an identity that clearly is not the old capsule on this port.
    inner.caps.identity = EmulatorIdentity {
        system: Some("snes".into()),
        adapter: Some("mesen2-live".into()),
        content: Some("/fixture.sfc".into()),
        launch_id: None,
        ..Default::default()
    };
    let mut link = ObservedLink::with_store(inner, store.clone());

    link.call("status", serde_json::json!({})).unwrap();
    let snapshot = link.continuity();
    assert_eq!(
        snapshot.runtime_binding.state,
        RuntimeBindingState::Mismatched
    );
    assert_eq!(snapshot.execution.state, ExecutionState::Running);
    assert_eq!(snapshot.transport.state, TransportState::Connected);
    assert_eq!(snapshot.lease.state, LeaseState::Unknown);

    // A fallback-attached emulator remains usable, but its calls do not borrow or mutate the old
    // generation's lease/evidence record.
    link.call("write_memory", serde_json::json!({})).unwrap();
    let persisted: Option<LinkRecord> = store.read_link_json(47834, &current.launch_id).unwrap();
    assert!(persisted.is_none());
    assert_eq!(
        link.failure_context()["link_failure"]["last_status"]["frame"],
        41
    );
}

#[test]
fn stale_adapter_failure_is_reported_but_not_promoted_to_exact() {
    let tmp = tempfile::tempdir().unwrap();
    let store = RuntimeStore::new(tmp.path().join("sessions"));
    let current = current(&store, 47821);
    std::fs::write(
        store.adapter_failure_path(47821, &current.launch_id),
        serde_json::to_vec(&serde_json::json!({
            "launch_id": "launch-stale",
            "reason": "old crash"
        }))
        .unwrap(),
    )
    .unwrap();
    let inner = SequenceLink::new(47821, &current.launch_id, []);
    let mut link = ObservedLink::with_store(inner, store);

    assert_ne!(link.continuity().evidence.state, EvidenceState::Exact);
    let context = link.failure_context();
    assert_eq!(context["adapter_failure"]["stale"], true);
}

#[test]
fn matching_adapter_failure_is_exact_crash_evidence() {
    let tmp = tempfile::tempdir().unwrap();
    let store = RuntimeStore::new(tmp.path().join("sessions"));
    let current = current(&store, 47823);
    std::fs::write(
        store.adapter_failure_path(47823, &current.launch_id),
        serde_json::to_vec(&serde_json::json!({
            "schema_version": 1,
            "launch_id": current.launch_id,
            "kind": "sh4_fatal",
            "observed_at_unix_ms": 1,
            "frame": 2,
            "epc": 0x8c012340u32,
            "incoming_event": 0x180,
            "registers": {"r0": 1, "r15": 2},
            "pc_ring": [0x8c01233eu32, 0x8c012340u32]
        }))
        .unwrap(),
    )
    .unwrap();
    let inner = SequenceLink::new(47823, &current.launch_id, []);
    let mut link = ObservedLink::with_store(inner, store);

    let continuity = link.continuity();
    assert_eq!(continuity.execution.state, ExecutionState::Crashed);
    assert_eq!(continuity.execution.source, "adapter");
    assert_eq!(continuity.evidence.state, EvidenceState::Exact);
    assert!(continuity.evidence.failure_context_available);
    let context = link.failure_context();
    assert_eq!(context["adapter_failure"]["stale"], false);
    assert_eq!(context["adapter_failure"]["epc"], 0x8c012340u32);
}

#[test]
fn active_native_adapter_error_preserves_cause_without_claiming_exact_guest_state() {
    let tmp = tempfile::tempdir().unwrap();
    let store = RuntimeStore::new(tmp.path().join("sessions"));
    let current = current(&store, 47831);
    std::fs::write(
        store.adapter_failure_path(47831, &current.launch_id),
        serde_json::to_vec(&serde_json::json!({
            "schema_version": 1,
            "launch_id": current.launch_id,
            "adapter": "mednafen-native",
            "kind": "adapter_internal_error",
            "operation": "service",
            "reason": "injected failure",
            "observed_at_unix_ms": 1,
            "frame": 2,
            "active": true,
            "execution_state": "unknown"
        }))
        .unwrap(),
    )
    .unwrap();
    let inner = SequenceLink::new(47831, &current.launch_id, []);
    let mut link = ObservedLink::with_store(inner, store);

    let continuity = link.continuity();
    assert_eq!(continuity.execution.state, ExecutionState::Unknown);
    assert_eq!(continuity.execution.source, "adapter");
    assert_eq!(continuity.evidence.state, EvidenceState::Unavailable);
    assert!(continuity.evidence.failure_context_available);
    let context = link.failure_context();
    assert_eq!(context["adapter_failure"]["kind"], "adapter_internal_error");
    assert_eq!(context["adapter_failure"]["operation"], "service");
}

#[test]
fn active_native_adapter_error_demotes_prior_live_status_to_last_good() {
    let tmp = tempfile::tempdir().unwrap();
    let store = RuntimeStore::new(tmp.path().join("sessions"));
    let current = current(&store, 47833);
    let inner = SequenceLink::new(
        47833,
        &current.launch_id,
        [Outcome::Ok(serde_json::json!({"state": "running"}))],
    );
    let mut link = ObservedLink::with_store(inner, store.clone());
    link.call("status", serde_json::json!({})).unwrap();
    std::fs::write(
        store.adapter_failure_path(47833, &current.launch_id),
        serde_json::to_vec(&serde_json::json!({
            "schema_version": 1,
            "launch_id": current.launch_id,
            "adapter": "flycast-native",
            "kind": "adapter_internal_error",
            "operation": "service",
            "reason": "failure after status",
            "observed_at_unix_ms": 2,
            "frame": 3,
            "active": true,
            "execution_state": "unknown"
        }))
        .unwrap(),
    )
    .unwrap();

    link.failure_context();
    let continuity = link.continuity();
    assert_eq!(continuity.execution.state, ExecutionState::Unknown);
    assert_eq!(continuity.execution.source, "adapter");
    assert_eq!(continuity.evidence.state, EvidenceState::LastGood);
    assert!(continuity.evidence.failure_context_available);
}

#[test]
fn recovered_native_adapter_error_remains_context_without_overriding_live_state() {
    let tmp = tempfile::tempdir().unwrap();
    let store = RuntimeStore::new(tmp.path().join("sessions"));
    let current = current(&store, 47832);
    std::fs::write(
        store.adapter_failure_path(47832, &current.launch_id),
        serde_json::to_vec(&serde_json::json!({
            "schema_version": 1,
            "launch_id": current.launch_id,
            "adapter": "flycast-native",
            "kind": "adapter_internal_error",
            "operation": "trace",
            "reason": "injected failure",
            "observed_at_unix_ms": 1,
            "frame": 2,
            "active": false,
            "execution_state": "running"
        }))
        .unwrap(),
    )
    .unwrap();
    let inner = SequenceLink::new(
        47832,
        &current.launch_id,
        [Outcome::Ok(serde_json::json!({"state": "running"}))],
    );
    let mut link = ObservedLink::with_store(inner, store);

    link.call("status", serde_json::json!({})).unwrap();
    let continuity = link.continuity();
    assert_eq!(continuity.execution.state, ExecutionState::Running);
    assert_eq!(continuity.execution.source, "adapter");
    assert_eq!(continuity.evidence.state, EvidenceState::Live);
    assert!(continuity.evidence.failure_context_available);
}

#[test]
fn matching_launch_id_without_exact_snapshot_schema_is_not_promoted() {
    let tmp = tempfile::tempdir().unwrap();
    let store = RuntimeStore::new(tmp.path().join("sessions"));
    let current = current(&store, 47824);
    std::fs::write(
        store.adapter_failure_path(47824, &current.launch_id),
        serde_json::to_vec(&serde_json::json!({
            "schema_version": 1,
            "launch_id": current.launch_id,
            "kind": "sh4_fatal"
        }))
        .unwrap(),
    )
    .unwrap();
    let inner = SequenceLink::new(47824, &current.launch_id, []);
    let link = ObservedLink::with_store(inner, store);

    assert_ne!(link.continuity().execution.state, ExecutionState::Crashed);
    assert_ne!(link.continuity().evidence.state, EvidenceState::Exact);
}

#[test]
fn failure_context_refreshes_an_adapter_snapshot_written_after_link_creation() {
    let tmp = tempfile::tempdir().unwrap();
    let store = RuntimeStore::new(tmp.path().join("sessions"));
    let current = current(&store, 47825);
    let inner = SequenceLink::new(47825, &current.launch_id, []);
    let mut link = ObservedLink::with_store(inner, store.clone());
    assert_ne!(link.continuity().evidence.state, EvidenceState::Exact);

    std::fs::write(
        store.adapter_failure_path(47825, &current.launch_id),
        serde_json::to_vec(&serde_json::json!({
            "schema_version": 1,
            "launch_id": current.launch_id,
            "kind": "sh4_fatal",
            "observed_at_unix_ms": 1,
            "frame": 2,
            "epc": 0x8c012340u32,
            "incoming_event": 0x180,
            "registers": {"r0": 1},
            "pc_ring": [0x8c012340u32]
        }))
        .unwrap(),
    )
    .unwrap();

    let context = link.failure_context();
    assert_eq!(context["continuity"]["execution"]["state"], "crashed");
    assert_eq!(context["continuity"]["evidence"]["state"], "exact");
    assert_eq!(context["adapter_failure"]["stale"], false);
}

#[test]
fn corrupt_current_manifest_is_reported_as_runtime_diagnostic() {
    let tmp = tempfile::tempdir().unwrap();
    let store = RuntimeStore::new(tmp.path().join("sessions"));
    let path = store.current_path(47827);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, b"{not-json").unwrap();
    let inner = SequenceLink::new(47827, "launch-unreadable", []);
    let mut link = ObservedLink::with_store(inner, store);

    let snapshot = link.continuity();
    assert_eq!(snapshot.evidence.state, EvidenceState::Unavailable);
    assert_eq!(snapshot.runtime_diagnostics.len(), 1);
    let diagnostic = &snapshot.runtime_diagnostics[0];
    assert_eq!(diagnostic.artifact, "current");
    assert_eq!(diagnostic.kind, "invalid");
    assert_eq!(diagnostic.path, path.display().to_string());
    assert!(diagnostic.blocks_generation_transition);
    assert!(link.failure_context()["adapter_failure"].is_null());
}

#[test]
fn corrupt_link_is_reported_without_discarding_current_manifest() {
    let tmp = tempfile::tempdir().unwrap();
    let store = RuntimeStore::new(tmp.path().join("sessions"));
    let current = current(&store, 47828);
    let path = store.link_path(47828, &current.launch_id);
    std::fs::write(&path, b"{not-json").unwrap();
    let inner = SequenceLink::new(47828, &current.launch_id, []);
    let link = ObservedLink::with_store(inner, store);

    let snapshot = link.continuity();
    assert_eq!(snapshot.runtime_diagnostics.len(), 1);
    assert_eq!(snapshot.runtime_diagnostics[0].artifact, "link");
    assert_eq!(snapshot.runtime_diagnostics[0].kind, "invalid");
    assert_eq!(
        snapshot.runtime_diagnostics[0].path,
        path.display().to_string()
    );
    assert_eq!(snapshot.execution.state, ExecutionState::Unknown);
}

#[test]
fn oversized_adapter_failure_is_diagnostic_not_exact_evidence() {
    let tmp = tempfile::tempdir().unwrap();
    let store = RuntimeStore::new(tmp.path().join("sessions"));
    let current = current(&store, 47829);
    let path = store.adapter_failure_path(47829, &current.launch_id);
    std::fs::write(
        &path,
        vec![b'x'; crate::live::runtime::MAX_CAPSULE_FILE_BYTES as usize + 1],
    )
    .unwrap();
    let inner = SequenceLink::new(47829, &current.launch_id, []);
    let mut link = ObservedLink::with_store(inner, store);

    let snapshot = link.continuity();
    assert_ne!(snapshot.evidence.state, EvidenceState::Exact);
    assert_ne!(snapshot.execution.state, ExecutionState::Crashed);
    assert_eq!(snapshot.runtime_diagnostics[0].artifact, "adapter_failure");
    assert_eq!(snapshot.runtime_diagnostics[0].kind, "oversized");
    assert!(!snapshot.runtime_diagnostics[0].blocks_generation_transition);
    let context = link.failure_context();
    assert!(context["adapter_failure"].is_null());
    assert_eq!(
        context["continuity"]["runtime_diagnostics"][0]["path"],
        path.display().to_string()
    );
}

#[test]
fn malformed_adapter_failure_is_nonblocking_degraded_evidence() {
    let tmp = tempfile::tempdir().unwrap();
    let store = RuntimeStore::new(tmp.path().join("sessions"));
    let current = current(&store, 47835);
    let path = store.adapter_failure_path(47835, &current.launch_id);
    std::fs::write(&path, b"{not-json").unwrap();
    let inner = SequenceLink::new(47835, &current.launch_id, []);
    let mut link = ObservedLink::with_store(inner, store);

    let snapshot = link.continuity();
    assert_ne!(snapshot.evidence.state, EvidenceState::Exact);
    assert_ne!(snapshot.execution.state, ExecutionState::Crashed);
    assert_eq!(snapshot.runtime_diagnostics.len(), 1);
    assert_eq!(snapshot.runtime_diagnostics[0].artifact, "adapter_failure");
    assert_eq!(snapshot.runtime_diagnostics[0].kind, "invalid");
    assert!(!snapshot.runtime_diagnostics[0].blocks_generation_transition);
    assert!(link.failure_context()["adapter_failure"].is_null());
}

#[test]
fn status_without_an_execution_state_does_not_imply_running() {
    let tmp = tempfile::tempdir().unwrap();
    let store = RuntimeStore::new(tmp.path().join("sessions"));
    let current = current(&store, 47826);
    let inner = SequenceLink::new(
        47826,
        &current.launch_id,
        [Outcome::Ok(serde_json::json!({"frame": 7}))],
    );
    let mut link = ObservedLink::with_store(inner, store);

    link.call("status", serde_json::json!({})).unwrap();
    assert_eq!(link.continuity().execution.state, ExecutionState::Unknown);
}

#[test]
fn link_record_public_value_redacts_control_session_key() {
    let mut record = LinkRecord::new("launch-test".into());
    record.lease = Some(LeaseRecord {
        control_session_key: Some("control-secret".into()),
        holder: capture_process(std::process::id()),
        acquired_at_unix_ms: 1,
        refreshed_at_unix_ms: 2,
    });

    let public = record.public_value().to_string();
    assert!(!public.contains("control-secret"));
    assert!(!public.contains("control_session_key"));
}

#[test]
fn broker_shaped_link_keeps_last_good_in_memory_without_capsule_port() {
    let tmp = tempfile::tempdir().unwrap();
    let store = RuntimeStore::new(tmp.path().join("sessions"));
    let inner = NoPortLink(SequenceLink::new(
        0,
        "launch-broker",
        [
            Outcome::Ok(serde_json::json!({"state": "running", "frame": 7})),
            Outcome::Timeout,
        ],
    ));
    let mut link = ObservedLink::with_store(inner, store);

    link.call("status", serde_json::json!({})).unwrap();
    assert!(matches!(
        link.call("status", serde_json::json!({})),
        Err(LinkError::Timeout)
    ));
    let snapshot = link.continuity();
    assert_eq!(snapshot.transport.state, TransportState::Stalled);
    assert_eq!(snapshot.evidence.state, EvidenceState::LastGood);
    assert_eq!(
        link.failure_context()["link_failure"]["last_status"]["frame"],
        7
    );
}

#[test]
fn shared_link_without_an_exclusive_attachment_rejects_mutation() {
    let tmp = tempfile::tempdir().unwrap();
    let store = RuntimeStore::new(tmp.path().join("sessions"));
    let inner = NoPortLink(SequenceLink::new(
        0,
        "launch-shared",
        [Outcome::Ok(serde_json::json!({"unexpected": true}))],
    ));
    let mut link = ObservedLink::with_store(inner, store);

    assert!(matches!(
        link.call("write_memory", serde_json::json!({})),
        Err(LinkError::Busy)
    ));
    assert_eq!(
        link.inner.0.outcomes.len(),
        1,
        "inner mutation must not run"
    );
}

#[cfg(unix)]
#[test]
fn live_foreign_lease_rejects_mutation_without_calling_inner_link() {
    let tmp = tempfile::tempdir().unwrap();
    let store = RuntimeStore::new(tmp.path().join("sessions"));
    let current = current(&store, 47822);
    let mut holder = std::process::Command::new("sleep")
        .arg("5")
        .spawn()
        .unwrap();
    let mut record = LinkRecord::new(current.launch_id.clone());
    record.lease = Some(LeaseRecord {
        control_session_key: Some("control-foreign".into()),
        holder: capture_process(holder.id()),
        acquired_at_unix_ms: 1,
        refreshed_at_unix_ms: 1,
    });
    store
        .update_link_json(47822, &current.launch_id, |_| Ok(record))
        .unwrap();
    let inner = SequenceLink::new(
        47822,
        &current.launch_id,
        [Outcome::Ok(serde_json::json!({"unexpected": true}))],
    );
    let mut link = ObservedLink::with_store(inner, store);

    assert!(matches!(
        link.call("write_memory", serde_json::json!({})),
        Err(LinkError::Busy)
    ));
    assert_eq!(link.continuity().lease.state, LeaseState::Occupied);
    assert_eq!(link.inner.outcomes.len(), 1, "inner mutation must not run");

    let _ = holder.kill();
    let _ = holder.wait();
}

#[cfg(unix)]
#[test]
fn exited_anonymous_lease_is_available_without_editing_link_record() {
    let mut holder = std::process::Command::new("sleep")
        .arg("30")
        .spawn()
        .unwrap();
    let holder_identity = capture_process(holder.id());
    holder.kill().unwrap();
    holder.wait().unwrap();
    assert_eq!(process_state(&holder_identity), ProcessState::Exited);

    let lease = LeaseRecord {
        control_session_key: None,
        holder: holder_identity,
        acquired_at_unix_ms: 1,
        refreshed_at_unix_ms: 1,
    };
    let current_holder = capture_process(std::process::id());
    assert_eq!(
        lease_view(&lease, &current_holder).state,
        LeaseState::Available
    );
}

#[test]
fn lease_acquisition_is_bound_to_the_expected_current_generation() {
    let tmp = tempfile::tempdir().unwrap();
    let store = RuntimeStore::new(tmp.path().join("sessions"));
    let first = current(&store, 47829);
    let inner = SequenceLink::new(47829, &first.launch_id, []);
    let mut link = ObservedLink::with_store(inner, store.clone());

    let second = current(&store, 47829);
    let error = link
        .acquire_control_lease(&first.launch_id)
        .expect_err("a changed current generation must not lend its lease to stale cleanup");

    assert!(
        matches!(error, LinkError::Protocol(message) if message.contains("current generation changed"))
    );
    let second_record: Option<LinkRecord> = store.read_link_json(47829, &second.launch_id).unwrap();
    assert!(
        second_record.and_then(|record| record.lease).is_none(),
        "the newer generation's lease must remain untouched"
    );
}

#[test]
fn unresolved_recording_cleanup_blocks_mutation_without_calling_the_adapter() {
    use crate::bundle::recording_manifest::{
        CleanupFacts, CleanupState, ExecutionOutcome, FinalExecutionState, Integrity,
        OperationOutcome, PublicationOutcome, RecordingCounters,
    };
    use crate::live::capture_capsule::{
        CaptureCapsuleRepository, CaptureLeaseIdentity, CapturePreparation, CaptureState,
        CaptureTerminalSummary,
    };

    let tmp = tempfile::tempdir().unwrap();
    let store = RuntimeStore::new(tmp.path().join("sessions"));
    let current = current(&store, 47831);
    let inner = SequenceLink::new(
        47831,
        &current.launch_id,
        [Outcome::Ok(serde_json::json!({"unexpected": true}))],
    );
    let mut link = ObservedLink::with_store(inner, store.clone());
    link.acquire_control_lease(&current.launch_id).unwrap();

    let output = tempfile::tempdir().unwrap();
    let staging = output.path().join(".capture-quarantine.staging-test");
    std::fs::create_dir(&staging).unwrap();
    let repository = CaptureCapsuleRepository::new(store, 47831, &current.launch_id);
    repository
        .create(CapturePreparation {
            capture_id: "capture-quarantine".into(),
            request_digest_sha256: "11".repeat(32),
            capability_revision: "22".repeat(32),
            output_root: output.path().to_path_buf(),
            destination_path: output.path().join("capture-quarantine"),
            staging_path: staging,
            lease: CaptureLeaseIdentity::current(),
        })
        .unwrap();
    repository
        .transition(
            "capture-quarantine",
            CaptureState::Prepared,
            CaptureState::PublicationFailed,
            Some(CaptureTerminalSummary {
                operation_outcome: OperationOutcome::Failed,
                execution_outcome: ExecutionOutcome::AdapterError,
                integrity: Integrity::Unverifiable,
                publication: PublicationOutcome::Failed,
                final_execution_state: FinalExecutionState::Unknown,
                final_frame: 0,
                counters: RecordingCounters {
                    frames: 0,
                    events: 0,
                    bytes: 0,
                    dropped: 0,
                },
                cleanup: CleanupFacts {
                    hooks: CleanupState::Unverifiable,
                    transient_input: CleanupState::Unverifiable,
                    sink: CleanupState::Released,
                },
                stop_event: None,
                bundle_path: None,
                manifest_sha256: None,
                reason: Some("connection closed before cleanup was observed".into()),
            }),
        )
        .unwrap();

    assert!(matches!(
        link.call("write_memory", serde_json::json!({})),
        Err(LinkError::Emulator { kind, .. }) if kind == "recording_quarantined"
    ));
    assert_eq!(link.inner.outcomes.len(), 1, "inner mutation must not run");
}

fn temporal_key(runtime: &str) -> crate::live::reconnect::cancellation::OperationKey {
    crate::live::reconnect::cancellation::OperationKey {
        runtime: runtime.into(),
        owner_id: "attachment-a".into(),
        operation_id: "parent-operation".into(),
    }
}

#[test]
fn temporal_quarantine_survives_status_and_replacement_controller() {
    let tmp = tempfile::tempdir().unwrap();
    let store = RuntimeStore::new(tmp.path().join("sessions"));
    let current = current(&store, 47901);
    let key = temporal_key(&current.launch_id);
    let inner = SequenceLink::new(
        47901,
        &current.launch_id,
        [Outcome::Ok(serde_json::json!({"state":"frozen"}))],
    );
    let mut link = ObservedLink::with_store(inner, store.clone());
    link.begin_temporal_control(&key).unwrap();
    assert!(
        matches!(link.call("reset",serde_json::json!({})),Err(LinkError::Emulator { ref kind,.. }) if kind=="temporal_quarantined")
    );
    link.finish_temporal_control(&key, false).unwrap();
    assert_eq!(
        link.acquire_control_lease(&current.launch_id)
            .unwrap()
            .state,
        LeaseState::Held
    );
    link.call("status", serde_json::json!({})).unwrap();
    assert!(link.continuity().evidence.failure_context_available);
    assert_eq!(
        link.failure_context()["temporal_operation"]["state"],
        "unverified"
    );
    assert_eq!(
        store
            .read_link_json::<LinkRecord>(47901, &current.launch_id)
            .unwrap()
            .unwrap()
            .temporal_operation
            .unwrap()
            .state,
        TemporalState::Unverified
    );
    drop(link);
    let mut replacement = ObservedLink::with_store(
        SequenceLink::new(47901, &current.launch_id, []),
        store.clone(),
    );
    replacement.control_key = Some("replacement-controller".into());
    assert!(
        matches!(replacement.call("pause",serde_json::json!({"_temporal_owner":key})),Err(LinkError::Emulator { ref kind,.. }) if kind=="temporal_quarantined")
    );
    assert!(replacement.finish_temporal_control(&key, true).is_err());
    assert!(store
        .read_link_json::<LinkRecord>(47901, &current.launch_id)
        .unwrap()
        .unwrap()
        .temporal_operation
        .is_some());
}

#[test]
fn abandoned_active_temporal_owner_blocks_new_work_until_verified_completion() {
    let tmp = tempfile::tempdir().unwrap();
    let store = RuntimeStore::new(tmp.path().join("sessions"));
    let current = current(&store, 47902);
    let key = temporal_key(&current.launch_id);
    let mut link = ObservedLink::with_store(
        SequenceLink::new(47902, &current.launch_id, []),
        store.clone(),
    );
    link.begin_temporal_control(&key).unwrap();
    drop(link); // No terminal finalizer, as after a lost controller.
    let mut replacement = ObservedLink::with_store(
        SequenceLink::new(47902, &current.launch_id, []),
        store.clone(),
    );
    assert!(
        matches!(replacement.call("step",serde_json::json!({"frames":1})),Err(LinkError::Emulator { ref kind,.. }) if kind=="temporal_quarantined")
    );
    let mut wrong_key = key.clone();
    wrong_key.operation_id = "later-operation".into();
    assert!(replacement.begin_temporal_control(&wrong_key).is_err());
    assert!(replacement
        .finish_temporal_control(&wrong_key, true)
        .is_err());
    let record = store
        .read_link_json::<LinkRecord>(47902, &current.launch_id)
        .unwrap()
        .unwrap();
    assert_eq!(record.temporal_operation.unwrap().key, key);
}

#[test]
fn matching_temporal_cleanup_clears_marker_before_next_mutation() {
    let tmp = tempfile::tempdir().unwrap();
    let store = RuntimeStore::new(tmp.path().join("sessions"));
    let current = current(&store, 47903);
    let key = temporal_key(&current.launch_id);
    let inner = SequenceLink::new(
        47903,
        &current.launch_id,
        [
            Outcome::Ok(serde_json::json!({"state":"frozen"})),
            Outcome::Ok(serde_json::json!({"status":"completed"})),
        ],
    );
    let mut link = ObservedLink::with_store(inner, store.clone());
    link.begin_temporal_control(&key).unwrap();
    link.call("pause", serde_json::json!({"_temporal_owner":key}))
        .unwrap();
    assert!(store
        .read_link_json::<LinkRecord>(47903, &current.launch_id)
        .unwrap()
        .unwrap()
        .temporal_operation
        .is_some());
    link.finish_temporal_control(&key, true).unwrap();
    link.call("step", serde_json::json!({"frames":1})).unwrap();
    assert!(store
        .read_link_json::<LinkRecord>(47903, &current.launch_id)
        .unwrap()
        .unwrap()
        .temporal_operation
        .is_none());
}

#[test]
fn temporal_guard_never_clears_a_replacement_generation_or_exposes_owner_secret() {
    let tmp = tempfile::tempdir().unwrap();
    let store = RuntimeStore::new(tmp.path().join("sessions"));
    let old = current(&store, 47904);
    let key = temporal_key(&old.launch_id);
    let mut link =
        ObservedLink::with_store(SequenceLink::new(47904, &old.launch_id, []), store.clone());
    link.control_key = Some("private-controller-key".into());
    link.begin_temporal_control(&key).unwrap();
    let record = store
        .read_link_json::<LinkRecord>(47904, &old.launch_id)
        .unwrap()
        .unwrap();
    let public = record.public_value().to_string();
    assert!(!public.contains("private-controller-key"));
    assert!(!public.contains("attachment-a"));
    let new = current(&store, 47904);
    assert_ne!(old.launch_id, new.launch_id);
    assert!(link.finish_temporal_control(&key, true).is_err());
    assert!(store
        .read_link_json::<LinkRecord>(47904, &new.launch_id)
        .unwrap()
        .is_none());
}

#[test]
fn invalid_temporal_metadata_cannot_be_erased_by_lease_refresh() {
    let tmp = tempfile::tempdir().unwrap();
    let store = RuntimeStore::new(tmp.path().join("sessions"));
    let current = current(&store, 47906);
    let key = temporal_key(&current.launch_id);
    let mut link = ObservedLink::with_store(
        SequenceLink::new(47906, &current.launch_id, []),
        store.clone(),
    );
    link.begin_temporal_control(&key).unwrap();
    store
        .update_link_json::<LinkRecord, _>(47906, &current.launch_id, |record| {
            let mut record = record.unwrap();
            record.launch_id = "wrong-runtime".into();
            Ok(record)
        })
        .unwrap();
    assert!(link.call("reset", serde_json::json!({})).is_err());
    let record = store
        .read_link_json::<LinkRecord>(47906, &current.launch_id)
        .unwrap()
        .unwrap();
    assert_eq!(record.launch_id, "wrong-runtime");
    assert!(record.temporal_operation.is_some());
    assert!(link.finish_temporal_control(&key, true).is_err());
}

struct ExclusiveBrokerLink(SequenceLink);
impl EmulatorLink for ExclusiveBrokerLink {
    fn capabilities(&self) -> &Capabilities {
        self.0.capabilities()
    }
    fn call(&mut self, method: &str, params: Value) -> Result<Value, LinkError> {
        self.0.call(method, params)
    }
    fn has_exclusive_control(&self) -> bool {
        !self.0.caps.methods.is_empty()
    }
}

#[test]
fn broker_temporal_ownership_uses_the_exact_local_capsule_without_an_endpoint_port() {
    let temp = tempfile::tempdir().unwrap();
    let store = RuntimeStore::new(temp.path().join("sessions"));
    let manifest = current(&store, 47920);
    let key = temporal_key(&manifest.launch_id);
    let mut link = ObservedLink::with_store(
        ExclusiveBrokerLink(SequenceLink::new(0, &manifest.launch_id, [])),
        store.clone(),
    );
    assert_eq!(link.endpoint_port(), None);
    link.begin_temporal_control(&key).unwrap();
    let record: LinkRecord = store
        .read_link_json(47920, &manifest.launch_id)
        .unwrap()
        .unwrap();
    assert_eq!(record.temporal_operation.unwrap().key, key);
    link.finish_temporal_control(&key, true).unwrap();
    let record: LinkRecord = store
        .read_link_json(47920, &manifest.launch_id)
        .unwrap()
        .unwrap();
    assert!(record.temporal_operation.is_none());
    assert_eq!(link.endpoint_port(), None);
}

#[test]
fn broker_disconnect_retains_quarantine_at_the_admitted_location() {
    let temp = tempfile::tempdir().unwrap();
    let store = RuntimeStore::new(temp.path().join("sessions"));
    let manifest = current(&store, 47921);
    let key = temporal_key(&manifest.launch_id);
    let mut link = ObservedLink::with_store(
        ExclusiveBrokerLink(SequenceLink::new(0, &manifest.launch_id, [])),
        store.clone(),
    );
    link.begin_temporal_control(&key).unwrap();
    link.inner.0.caps = Capabilities::empty();
    link.finish_temporal_control(&key, false).unwrap();
    let record: LinkRecord = store
        .read_link_json(47921, &manifest.launch_id)
        .unwrap()
        .unwrap();
    assert_eq!(
        record.temporal_operation.unwrap().state,
        TemporalState::Unverified
    );
    let mut replacement = ObservedLink::with_store(
        ExclusiveBrokerLink(SequenceLink::new(0, &manifest.launch_id, [])),
        store,
    );
    assert!(
        matches!(replacement.call("reset",serde_json::json!({})),Err(LinkError::Emulator { ref kind,.. }) if kind=="temporal_quarantined")
    );
}

#[test]
fn ambiguous_or_corrupt_broker_location_never_falls_back_to_unmanaged_mutation() {
    for duplicate in [false, true] {
        let temp = tempfile::tempdir().unwrap();
        let store = RuntimeStore::new(temp.path().join("sessions"));
        let manifest = current(&store, 47922);
        std::fs::create_dir_all(store.session_dir(47923)).unwrap();
        if duplicate {
            let mut other = manifest.clone();
            other.port = 47923;
            std::fs::write(
                store.current_path(47923),
                serde_json::to_vec(&other).unwrap(),
            )
            .unwrap();
        } else {
            std::fs::write(store.current_path(47923), b"corrupt").unwrap();
        }
        let inner = ExclusiveBrokerLink(SequenceLink::new(
            0,
            &manifest.launch_id,
            [Outcome::Ok(serde_json::json!({"state":"frozen"}))],
        ));
        let mut link = ObservedLink::with_store(inner, store.clone());
        assert!(link
            .begin_temporal_control(&temporal_key(&manifest.launch_id))
            .is_err());
        assert!(link.call("reset", serde_json::json!({})).is_err());
        assert_eq!(link.inner.0.outcomes.len(), 1);
        link.call("status", serde_json::json!({})).unwrap();
        assert!(store
            .read_link_json::<LinkRecord>(47922, &manifest.launch_id)
            .unwrap()
            .is_none());
    }
}

#[test]
fn broker_completion_never_clears_a_replacement_generation() {
    let temp = tempfile::tempdir().unwrap();
    let store = RuntimeStore::new(temp.path().join("sessions"));
    let first = current(&store, 47924);
    let key = temporal_key(&first.launch_id);
    let mut link = ObservedLink::with_store(
        ExclusiveBrokerLink(SequenceLink::new(0, &first.launch_id, [])),
        store.clone(),
    );
    link.begin_temporal_control(&key).unwrap();
    let next = current(&store, 47924);
    assert!(link.finish_temporal_control(&key, true).is_err());
    assert!(store
        .read_link_json::<LinkRecord>(47924, &next.launch_id)
        .unwrap()
        .is_none());
}

#[test]
fn broker_with_only_a_local_unpublished_generation_cannot_mutate() {
    let temp = tempfile::tempdir().unwrap();
    let store = RuntimeStore::new(temp.path().join("sessions"));
    let prepared = store.prepare(47925).unwrap();
    let mut link = ObservedLink::with_store(
        ExclusiveBrokerLink(SequenceLink::new(0, prepared.launch_id(), [])),
        store,
    );
    assert!(link
        .begin_temporal_control(&temporal_key(prepared.launch_id()))
        .is_err());
    assert!(link.call("pause", serde_json::json!({})).is_err());
}

#[test]
fn actual_broker_link_binds_local_generation_for_durable_ownership() {
    use std::io::{BufRead, BufReader, Write};
    use std::net::TcpListener;
    use std::time::Duration;
    let temp = tempfile::tempdir().unwrap();
    let store = RuntimeStore::new(temp.path().join("sessions"));
    let manifest = current(&store, 47926);
    let identity = manifest.launch_id.clone();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap().to_string();
    let worker = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        let attach: Value = serde_json::from_str(&line).unwrap();
        writeln!(stream,"{}",serde_json::json!({"id":attach["id"],"ok":true,"result":{
            "attached_name":"owned", "broker_registration_id":1,"methods":["pause"],"launch_id":identity}})).unwrap();
        line.clear();
        reader.read_line(&mut line).unwrap();
        let pause: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(pause["method"], "pause");
        assert_eq!(pause["params"]["_temporal_owner"]["runtime"], identity);
        writeln!(
            stream,
            "{}",
            serde_json::json!({"id":pause["id"],"ok":true,"result":{"state":"frozen"}})
        )
        .unwrap();
    });
    let native = crate::live::broker_link::connect(&address, None, Duration::from_secs(2)).unwrap();
    let mut link = ObservedLink::with_store(native, store.clone());
    let key = temporal_key(&manifest.launch_id);
    link.begin_temporal_control(&key).unwrap();
    assert_eq!(link.endpoint_port(), None);
    link.call("pause", serde_json::json!({"_temporal_owner":key}))
        .unwrap();
    link.finish_temporal_control(&key, true).unwrap();
    let record: LinkRecord = store
        .read_link_json(47926, &manifest.launch_id)
        .unwrap()
        .unwrap();
    assert!(record.temporal_operation.is_none());
    worker.join().unwrap();
}

#[test]
fn broker_lease_wait_cannot_restart_a_temporal_dispatch_deadline() {
    use std::sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc,
    };
    use std::time::{Duration, Instant};
    struct DelayedLink {
        caps: Capabilities,
        observed: Arc<AtomicUsize>,
        closed: Arc<AtomicBool>,
    }
    impl EmulatorLink for DelayedLink {
        fn capabilities(&self) -> &Capabilities {
            self.observed.fetch_add(1, Ordering::SeqCst);
            &self.caps
        }
        fn has_exclusive_control(&self) -> bool {
            true
        }
        fn call(&mut self, _: &str, _: Value) -> Result<Value, LinkError> {
            panic!("expired request reached native dispatch")
        }
        fn call_with_progress(
            &mut self,
            _: &str,
            _: Value,
            _: &mut ProgressObserver<'_>,
            _: &ProgressCallControl,
        ) -> Result<Value, LinkError> {
            panic!("expired request reached native dispatch")
        }
        fn prepare_reconnect(&mut self) {
            self.closed.store(true, Ordering::SeqCst);
        }
    }
    let temp = tempfile::tempdir().unwrap();
    let store = RuntimeStore::new(temp.path().join("sessions"));
    let manifest = current(&store, 47927);
    let observed = Arc::new(AtomicUsize::new(0));
    let closed = Arc::new(AtomicBool::new(false));
    let caps = SequenceLink::new(0, &manifest.launch_id, []).caps;
    let mut link = ObservedLink::with_store(
        DelayedLink {
            caps,
            observed: observed.clone(),
            closed: closed.clone(),
        },
        store.clone(),
    );
    let key = temporal_key(&manifest.launch_id);
    link.begin_temporal_control(&key).unwrap();
    let lock = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(
            store
                .generation_dir(47927, &manifest.launch_id)
                .join(".link.lock"),
        )
        .unwrap();
    fs2::FileExt::lock_exclusive(&lock).unwrap();
    observed.store(0, Ordering::SeqCst);
    let deadline = Instant::now() + Duration::from_millis(100);
    let control = ProgressCallControl {
        max_host_ms: Some(2000),
        temporal_stop_ms: Some(500),
        temporal_deadline: Some(deadline),
        ..Default::default()
    };
    let worker = std::thread::spawn(move || {
        link.call_with_progress(
            "pause",
            serde_json::json!({"_temporal_owner":key}),
            &mut |_| Ok(()),
            &control,
        )
    });
    let wait_until = Instant::now() + Duration::from_secs(3);
    while observed.load(Ordering::SeqCst) == 0 {
        assert!(Instant::now() < wait_until);
        std::thread::yield_now();
    }
    // The first deadline check passed and runtime resolution entered. Hold the
    // actual generation lock past the deadline, then permit ownership refresh.
    std::thread::sleep(
        deadline.saturating_duration_since(Instant::now()) + Duration::from_millis(5),
    );
    fs2::FileExt::unlock(&lock).unwrap();
    let result = worker.join().unwrap();
    assert!(
        matches!(result,Err(LinkError::Emulator { ref kind,.. }) if kind=="temporal_unverified")
    );
    assert!(closed.load(Ordering::SeqCst));
}
