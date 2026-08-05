use std::fs;

use super::capture_capsule::*;
use super::continuity::LinkRecord;
use super::runtime::{capture_process, LeaseRecord, ManifestSpec, RuntimeStore};
use crate::bundle::recording_manifest::*;

fn setup() -> (
    tempfile::TempDir,
    RuntimeStore,
    u16,
    String,
    CaptureLeaseIdentity,
) {
    let temp = tempfile::tempdir().unwrap();
    let store = RuntimeStore::new(temp.path().join("runtime"));
    let port = 47800;
    let prepared = store.prepare(port).unwrap();
    let launch_id = prepared.launch_id().to_string();
    let current = prepared.manifest(ManifestSpec {
        adapter: "synthetic".into(),
        system: "test".into(),
        content: "/content/test.rom".into(),
        emulator_pid: std::process::id(),
        bridge_pid: None,
        backend_endpoint: None,
        build: Some("test-build".into()),
    });
    prepared.commit(&current).unwrap();
    let lease = CaptureLeaseIdentity::current();
    let lease_for_record = lease.clone();
    let launch_for_record = launch_id.clone();
    store
        .update_link_json(port, &launch_id, move |_: Option<LinkRecord>| {
            let mut record = LinkRecord::new(launch_for_record);
            record.lease = Some(LeaseRecord {
                control_session_key: lease_for_record.control_session_key,
                holder: lease_for_record.holder,
                acquired_at_unix_ms: 1,
                refreshed_at_unix_ms: 1,
            });
            Ok(record)
        })
        .unwrap();
    (temp, store, port, launch_id, lease)
}

fn preparation(root: &std::path::Path, lease: CaptureLeaseIdentity) -> CapturePreparation {
    let capture_id = "capture-test";
    let staging = root.join(format!(".{capture_id}.staging-test"));
    fs::create_dir(&staging).unwrap();
    CapturePreparation {
        capture_id: capture_id.into(),
        request_digest_sha256: "11".repeat(32),
        capability_revision: "22".repeat(32),
        output_root: root.to_path_buf(),
        destination_path: root.join(capture_id),
        staging_path: staging,
        lease,
    }
}

fn failed_terminal() -> CaptureTerminalSummary {
    CaptureTerminalSummary {
        operation_outcome: OperationOutcome::Failed,
        execution_outcome: ExecutionOutcome::AdapterError,
        integrity: Integrity::Unverifiable,
        publication: PublicationOutcome::Failed,
        final_execution_state: FinalExecutionState::Frozen,
        final_frame: 10,
        counters: RecordingCounters {
            frames: 1,
            events: 1,
            bytes: 100,
            dropped: 0,
        },
        cleanup: CleanupFacts {
            hooks: CleanupState::Released,
            transient_input: CleanupState::NotAcquired,
            sink: CleanupState::Released,
        },
        stop_event: None,
        bundle_path: None,
        manifest_sha256: None,
        reason: Some("injected".into()),
    }
}

#[test]
fn persists_only_valid_typed_transitions_and_monotonic_progress() {
    let (_temp, store, port, launch_id, lease) = setup();
    let output = tempfile::tempdir().unwrap();
    let repository = CaptureCapsuleRepository::new(store, port, &launch_id);
    let created = repository
        .create(preparation(output.path(), lease))
        .unwrap();
    assert_eq!(created.state, CaptureState::Prepared);
    repository
        .transition(
            "capture-test",
            CaptureState::Prepared,
            CaptureState::Arming,
            None,
        )
        .unwrap();
    repository
        .transition(
            "capture-test",
            CaptureState::Arming,
            CaptureState::Armed,
            None,
        )
        .unwrap();
    repository
        .transition(
            "capture-test",
            CaptureState::Armed,
            CaptureState::Recording,
            None,
        )
        .unwrap();
    repository
        .update_progress(
            "capture-test",
            CaptureProgress {
                sequence: 0,
                frame: 10,
                frames: Some(1),
                events: 1,
                bytes: 100,
                observed_at_unix_ms: 1,
            },
        )
        .unwrap();
    assert!(repository
        .update_progress(
            "capture-test",
            CaptureProgress {
                sequence: 0,
                frame: 9,
                frames: Some(0),
                events: 0,
                bytes: 0,
                observed_at_unix_ms: 2,
            },
        )
        .is_err());
    assert!(matches!(
        repository.transition(
            "capture-test",
            CaptureState::Recording,
            CaptureState::Published,
            Some(failed_terminal())
        ),
        Err(CaptureCapsuleError::InvalidTransition { .. })
    ));
}

#[test]
fn rejects_a_second_capture_until_the_exact_first_capsule_is_terminal() {
    let (_temp, store, port, launch_id, lease) = setup();
    let output = tempfile::tempdir().unwrap();
    let repository = CaptureCapsuleRepository::new(store, port, &launch_id);
    repository
        .create(preparation(output.path(), lease.clone()))
        .unwrap();
    let second_root = tempfile::tempdir().unwrap();
    assert!(matches!(
        repository.create(preparation(second_root.path(), lease)),
        Err(CaptureCapsuleError::ActiveCapture { .. })
    ));
}

#[test]
fn terminal_cleanup_safety_distinguishes_prearm_rejection_from_quarantine() {
    let (_temp, store, port, launch_id, lease) = setup();
    let output = tempfile::tempdir().unwrap();
    let repository = CaptureCapsuleRepository::new(store.clone(), port, &launch_id);
    repository
        .create(preparation(output.path(), lease.clone()))
        .unwrap();

    let mut prearm = failed_terminal();
    prearm.final_execution_state = FinalExecutionState::Unknown;
    prearm.final_frame = 0;
    prearm.counters = RecordingCounters {
        frames: 0,
        events: 0,
        bytes: 0,
        dropped: 0,
    };
    prearm.cleanup = CleanupFacts {
        hooks: CleanupState::NotAcquired,
        transient_input: CleanupState::NotAcquired,
        sink: CleanupState::NotAcquired,
    };
    repository
        .transition(
            "capture-test",
            CaptureState::Prepared,
            CaptureState::PublicationFailed,
            Some(prearm),
        )
        .unwrap();
    assert!(repository
        .read()
        .unwrap()
        .unwrap()
        .generation_mutation_blocker()
        .is_none());

    let second_root = tempfile::tempdir().unwrap();
    repository
        .create(preparation(second_root.path(), lease))
        .unwrap();
}

#[test]
fn terminal_with_unverifiable_cleanup_blocks_capture_and_generation_replacement() {
    let (_temp, store, port, launch_id, lease) = setup();
    let output = tempfile::tempdir().unwrap();
    let repository = CaptureCapsuleRepository::new(store.clone(), port, &launch_id);
    repository
        .create(preparation(output.path(), lease.clone()))
        .unwrap();

    let mut unsafe_terminal = failed_terminal();
    unsafe_terminal.cleanup.hooks = CleanupState::Unverifiable;
    repository
        .transition(
            "capture-test",
            CaptureState::Prepared,
            CaptureState::PublicationFailed,
            Some(unsafe_terminal),
        )
        .unwrap();
    let capsule = repository.read().unwrap().unwrap();
    assert!(capsule.generation_mutation_blocker().is_some());

    let second_root = tempfile::tempdir().unwrap();
    assert!(matches!(
        repository.create(preparation(second_root.path(), lease)),
        Err(CaptureCapsuleError::ActiveCapture { .. })
    ));

    let replacement = store.prepare(port).unwrap();
    let replacement_manifest = replacement.manifest(ManifestSpec {
        adapter: "synthetic".into(),
        system: "test".into(),
        content: "/content/replacement.rom".into(),
        emulator_pid: std::process::id(),
        bridge_pid: None,
        backend_endpoint: None,
        build: Some("test-build".into()),
    });
    assert_eq!(
        replacement
            .commit(&replacement_manifest)
            .unwrap_err()
            .kind(),
        std::io::ErrorKind::WouldBlock
    );
}

#[test]
fn abandoned_recovery_requires_old_holder_exit_and_exact_staging_identity() {
    let (_temp, store, port, launch_id, lease) = setup();
    let output = tempfile::tempdir().unwrap();
    let repository = CaptureCapsuleRepository::new(store.clone(), port, &launch_id);
    repository
        .create(preparation(output.path(), lease.clone()))
        .unwrap();

    assert!(matches!(
        repository.reconcile(&lease, None, Some(failed_terminal())),
        Err(CaptureCapsuleError::RecoveryBlocked(_))
    ));

    store
        .update_capture_json(port, &launch_id, |current: Option<CaptureCapsule>| {
            let mut current = current.unwrap();
            current.lease.holder = capture_process(u32::MAX - 1);
            Ok(current)
        })
        .unwrap();
    let mut adapter_terminal = failed_terminal();
    adapter_terminal.operation_outcome = OperationOutcome::Completed;
    adapter_terminal.execution_outcome = ExecutionOutcome::TargetReached;
    adapter_terminal.integrity = Integrity::Complete;
    assert_eq!(
        repository
            .reconcile(&lease, None, Some(adapter_terminal))
            .unwrap(),
        ReconcileOutcome::Quarantined
    );
    let recovered = repository.read().unwrap().unwrap();
    assert_eq!(recovered.state, CaptureState::PublicationFailed);
    let terminal = recovered.terminal.unwrap();
    assert_eq!(terminal.operation_outcome, OperationOutcome::Completed);
    assert_eq!(terminal.execution_outcome, ExecutionOutcome::TargetReached);
    assert_eq!(terminal.integrity, Integrity::Unverifiable);
    assert_eq!(terminal.publication, PublicationOutcome::Failed);
    assert!(fs::read_dir(output.path()).unwrap().any(|entry| entry
        .unwrap()
        .file_name()
        .to_string_lossy()
        .starts_with(".capture-test.invalid-")));
}

#[test]
fn recovery_never_mutates_a_replaced_staging_directory() {
    let (_temp, store, port, launch_id, lease) = setup();
    let output = tempfile::tempdir().unwrap();
    let repository = CaptureCapsuleRepository::new(store.clone(), port, &launch_id);
    repository
        .create(preparation(output.path(), lease.clone()))
        .unwrap();
    let capsule = repository.read().unwrap().unwrap();
    fs::remove_dir(&capsule.staging_path).unwrap();
    fs::create_dir(&capsule.staging_path).unwrap();
    store
        .update_capture_json(port, &launch_id, |current: Option<CaptureCapsule>| {
            let mut current = current.unwrap();
            current.lease.holder = capture_process(u32::MAX - 1);
            Ok(current)
        })
        .unwrap();
    assert!(matches!(
        repository.reconcile(&lease, None, Some(failed_terminal())),
        Err(CaptureCapsuleError::RecoveryBlocked(_))
    ));
    assert!(PathBuf::from(&capsule.staging_path).is_dir());
}

use std::path::PathBuf;
