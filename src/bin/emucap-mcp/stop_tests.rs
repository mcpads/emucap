use super::*;

use std::sync::{Arc, Mutex};

use emucap::live::continuity::ContinuitySnapshot;
use emucap::live::link::{Capabilities, LinkError};
use emucap::live::runtime::{LeaseView, ManifestSpec, PreparedGeneration};

struct StopLink {
    caps: Capabilities,
    port: Option<u16>,
    lease: LeaseState,
    acquired: Arc<Mutex<Vec<String>>>,
    replace_on_acquire: Option<(PreparedGeneration, emucap::live::runtime::CurrentManifest)>,
}

impl StopLink {
    fn new(port: Option<u16>, lease: LeaseState) -> Self {
        Self {
            caps: Capabilities::empty(),
            port,
            lease,
            acquired: Arc::new(Mutex::new(Vec::new())),
            replace_on_acquire: None,
        }
    }
}

impl EmulatorLink for StopLink {
    fn capabilities(&self) -> &Capabilities {
        &self.caps
    }

    fn call(
        &mut self,
        _method: &str,
        _params: serde_json::Value,
    ) -> Result<serde_json::Value, LinkError> {
        Err(LinkError::NotConnected)
    }

    fn endpoint_port(&self) -> Option<u16> {
        self.port
    }

    fn acquire_control_lease(&mut self, expected_launch_id: &str) -> Result<LeaseView, LinkError> {
        self.acquired
            .lock()
            .unwrap()
            .push(expected_launch_id.to_string());
        if let Some((prepared, manifest)) = self.replace_on_acquire.take() {
            prepared.commit(&manifest).unwrap();
        }
        Ok(LeaseView {
            state: self.lease,
            holder_pid: Some(std::process::id()),
        })
    }

    fn continuity(&self) -> ContinuitySnapshot {
        ContinuitySnapshot {
            lease: LeaseView {
                state: self.lease,
                holder_pid: Some(std::process::id()),
            },
            ..ContinuitySnapshot::default()
        }
    }
}

#[cfg(unix)]
fn spawn_reaped_sleep() -> (u32, std::thread::JoinHandle<std::process::ExitStatus>) {
    let mut child = std::process::Command::new("/bin/sleep")
        .arg("30")
        .spawn()
        .unwrap();
    let pid = child.id();
    let waiter = std::thread::spawn(move || child.wait().unwrap());
    (pid, waiter)
}

#[cfg(unix)]
fn pid_alive(pid: u32) -> bool {
    unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
}

fn commit_manifest(
    store: &RuntimeStore,
    port: u16,
    emulator_pid: u32,
    bridge_pid: Option<u32>,
) -> emucap::live::runtime::CurrentManifest {
    let prepared = store.prepare(port).unwrap();
    let manifest = prepared.manifest(ManifestSpec {
        adapter: "test-adapter".into(),
        system: "test-system".into(),
        content: "/games/test.rom".into(),
        emulator_pid,
        bridge_pid,
        backend_endpoint: None,
        build: Some("test-build".into()),
    });
    prepared.commit(&manifest).unwrap();
    manifest
}

#[test]
fn stop_without_a_managed_port_fails_without_lease_or_signal() {
    let tmp = tempfile::tempdir().unwrap();
    let store = RuntimeStore::new(tmp.path().join("sessions"));
    let acquired = Arc::new(Mutex::new(Vec::new()));
    let mut link = StopLink::new(None, LeaseState::Held);
    link.acquired = acquired.clone();

    let result = make_stop_with_store(
        &mut link,
        &StopArgs {
            launch_id: "launch-missing".into(),
        },
        &store,
    );

    assert_eq!(result["stopped"], false);
    assert!(acquired.lock().unwrap().is_empty());
}

#[test]
fn stop_on_an_unused_listener_fails_without_acquiring_a_lease() {
    let tmp = tempfile::tempdir().unwrap();
    let store = RuntimeStore::new(tmp.path().join("sessions"));
    let acquired = Arc::new(Mutex::new(Vec::new()));
    let mut link = StopLink::new(Some(47919), LeaseState::Held);
    link.acquired = acquired.clone();

    let result = make_stop_with_store(
        &mut link,
        &StopArgs {
            launch_id: "launch-missing".into(),
        },
        &store,
    );

    assert_eq!(result["stopped"], false);
    assert_eq!(
        result["reason"],
        "no current managed runtime generation exists on this listener port"
    );
    assert!(acquired.lock().unwrap().is_empty());
}

#[cfg(unix)]
#[test]
fn stale_launch_id_and_occupied_lease_never_signal_the_current_process() {
    let tmp = tempfile::tempdir().unwrap();
    let store = RuntimeStore::new(tmp.path().join("sessions"));
    let port = 47920;
    let (pid, waiter) = spawn_reaped_sleep();
    let current = commit_manifest(&store, port, pid, None);

    let mut stale = StopLink::new(Some(port), LeaseState::Held);
    let stale_result = make_stop_with_store(
        &mut stale,
        &StopArgs {
            launch_id: "launch-stale".into(),
        },
        &store,
    );
    assert_eq!(stale_result["stopped"], false);
    assert_eq!(current.process_state(), ProcessState::Alive);
    assert!(stale.acquired.lock().unwrap().is_empty());

    let mut occupied = StopLink::new(Some(port), LeaseState::Occupied);
    let occupied_result = make_stop_with_store(
        &mut occupied,
        &StopArgs {
            launch_id: current.launch_id.clone(),
        },
        &store,
    );
    assert_eq!(occupied_result["stopped"], false);
    assert_eq!(current.process_state(), ProcessState::Alive);
    assert!(!store.termination_path(port, &current.launch_id).exists());

    emucap::launch::terminate_detached(pid).unwrap();
    waiter.join().unwrap();
}

#[cfg(unix)]
#[test]
fn stop_terminates_emulator_and_bridge_records_completion_and_is_idempotent() {
    let tmp = tempfile::tempdir().unwrap();
    let store = RuntimeStore::new(tmp.path().join("sessions"));
    let port = 47921;
    let (emulator_pid, emulator_waiter) = spawn_reaped_sleep();
    let (bridge_pid, bridge_waiter) = spawn_reaped_sleep();
    let current = commit_manifest(&store, port, emulator_pid, Some(bridge_pid));
    let failure_path = store.adapter_failure_path(port, &current.launch_id);
    std::fs::write(&failure_path, b"{\"preserved\":true}").unwrap();

    let mut link = StopLink::new(Some(port), LeaseState::Held);
    let first = make_stop_with_store(
        &mut link,
        &StopArgs {
            launch_id: current.launch_id.clone(),
        },
        &store,
    );

    assert_eq!(first["stopped"], true, "{first}");
    assert_eq!(first["status"], "completed");
    assert_eq!(first["processes"]["completed"], true);
    assert_eq!(current.process_state(), ProcessState::Exited);
    assert_eq!(current.bridge_process_state(), Some(ProcessState::Exited));
    assert_eq!(
        store
            .read_termination(port, &current.launch_id)
            .unwrap()
            .unwrap()
            .state,
        TerminationState::Completed
    );
    assert_eq!(
        std::fs::read(&failure_path).unwrap(),
        b"{\"preserved\":true}"
    );
    emulator_waiter.join().unwrap();
    bridge_waiter.join().unwrap();

    let second = make_stop_with_store(
        &mut link,
        &StopArgs {
            launch_id: current.launch_id.clone(),
        },
        &store,
    );
    assert_eq!(second["stopped"], true, "{second}");
    assert_eq!(second["processes"]["emulator"]["method"], "already_exited");
    assert_eq!(second["processes"]["bridge"]["method"], "already_exited");
}

#[cfg(unix)]
#[test]
fn unknown_process_identity_rejects_stop_before_record_or_signal() {
    let tmp = tempfile::tempdir().unwrap();
    let store = RuntimeStore::new(tmp.path().join("sessions"));
    let port = 47922;
    let (pid, waiter) = spawn_reaped_sleep();
    let prepared = store.prepare(port).unwrap();
    let mut current = prepared.manifest(ManifestSpec {
        adapter: "test-adapter".into(),
        system: "test-system".into(),
        content: "/games/test.rom".into(),
        emulator_pid: pid,
        bridge_pid: None,
        backend_endpoint: None,
        build: None,
    });
    current.emulator.start_identity = None;
    prepared.commit(&current).unwrap();
    assert_eq!(current.process_state(), ProcessState::Unknown);

    let mut link = StopLink::new(Some(port), LeaseState::Held);
    let result = make_stop_with_store(
        &mut link,
        &StopArgs {
            launch_id: current.launch_id.clone(),
        },
        &store,
    );
    assert_eq!(result["stopped"], false, "{result}");
    assert!(!store.termination_path(port, &current.launch_id).exists());
    assert!(pid_alive(pid));

    emucap::launch::terminate_detached(pid).unwrap();
    waiter.join().unwrap();
}

#[cfg(unix)]
#[test]
fn generation_change_during_lease_acquisition_rejects_without_signal() {
    let tmp = tempfile::tempdir().unwrap();
    let store = RuntimeStore::new(tmp.path().join("sessions"));
    let port = 47923;
    let (pid, waiter) = spawn_reaped_sleep();
    let first = commit_manifest(&store, port, pid, None);
    let replacement = store.prepare(port).unwrap();
    let replacement_manifest = replacement.manifest(ManifestSpec {
        adapter: "test-adapter".into(),
        system: "test-system".into(),
        content: "/games/replacement.rom".into(),
        emulator_pid: pid,
        bridge_pid: None,
        backend_endpoint: None,
        build: None,
    });

    let mut link = StopLink::new(Some(port), LeaseState::Held);
    link.replace_on_acquire = Some((replacement, replacement_manifest.clone()));
    let result = make_stop_with_store(
        &mut link,
        &StopArgs {
            launch_id: first.launch_id,
        },
        &store,
    );

    assert_eq!(result["stopped"], false, "{result}");
    assert_eq!(
        store.read_current(port).unwrap().unwrap().launch_id,
        replacement_manifest.launch_id
    );
    assert_eq!(replacement_manifest.process_state(), ProcessState::Alive);
    assert!(!store
        .termination_path(port, &replacement_manifest.launch_id)
        .exists());

    emucap::launch::terminate_detached(pid).unwrap();
    waiter.join().unwrap();
}
