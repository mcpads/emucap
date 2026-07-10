use std::collections::VecDeque;

use serde_json::Value;

use super::*;
use crate::live::link::EmulatorIdentity;

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
