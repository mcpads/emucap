use std::fs;
use std::io::Write;

use serde_json::json;
use sha2::{Digest, Sha256};

use super::event::{ClockPoint, EventEnvelope};
use super::manifest::{parse_manifest, BundleManifest};
use super::publish::*;
use super::recording::{ProducerTerminalReport, RecordingValidationInput};
use super::recording_manifest::*;
use crate::event_contracts::EventContractRegistry;

fn request(frames: u64) -> RecordingRequest {
    RecordingRequest {
        frames,
        warmup_frames: 0,
        event_classes: EventContractRegistry::builtin()
            .unwrap()
            .identities(["frame_boundary"])
            .unwrap(),
        event_filters: vec![],
        event_arming: vec![],
        limits: RecordingLimits {
            max_frames: 300,
            max_events: 1000,
            max_bytes: 1024 * 1024,
            max_line_bytes: 4096,
            max_host_ms: 30_000,
            progress_interval_ms: 100,
        },
        input_movie: None,
        stop_on: None,
        start_on: None,
        initial_snapshots: vec![],
        terminal_snapshots: vec![],
        terminal_state: None,
    }
}

fn record(sequence: u64, frame: u64) -> Vec<u8> {
    let identity = request(1).event_classes.remove(0);
    let mut output = serde_json::to_vec(&EventEnvelope {
        sequence,
        class: identity.id,
        contract_sha256: identity.contract_sha256,
        clock: ClockPoint {
            domain: "frame".into(),
            tick: frame,
        },
        frame,
        payload: json!({}),
    })
    .unwrap();
    output.push(b'\n');
    output
}

fn runtime() -> RuntimeIdentity {
    RuntimeIdentity {
        system: "snes".into(),
        adapter_id: "mesen2".into(),
        server_build: "server-build".into(),
        adapter_build: "adapter-build".into(),
        emulator_id: "Mesen".into(),
        emulator_build: "emulator-build".into(),
        emulator_upstream_revision: "upstream-revision".into(),
        emulator_patchset_sha256: "11".repeat(32),
        launch_id: "launch-01test".into(),
        capability_revision: "22".repeat(32),
        content: ContentIdentity {
            sha1: Some("33".repeat(20)),
            sha256: None,
            bytes: 524288,
            path_hint: None,
        },
    }
}

fn complete_input(capture_id: &str, frames: u64, bytes: u64) -> RecordingBundleInput {
    RecordingBundleInput {
        capture_id: capture_id.into(),
        created_at_unix_ms: 100,
        request_digest_sha256: "44".repeat(32),
        runtime: runtime(),
        event_order: None,
        validation: RecordingValidationInput {
            request: request(frames),
            origin: RecordingOrigin::NextFrameBoundary,
            f_start: 10,
            f_end: 10 + frames,
            observation_start: None,
            terminal: ProducerTerminalReport {
                operation_outcome: OperationOutcome::Completed,
                execution_outcome: ExecutionOutcome::TargetReached,
                claimed_integrity: Integrity::Complete,
                final_execution_state: FinalExecutionState::Frozen,
                final_frame: 10 + frames,
                f_origin: None,
                counters: RecordingCounters {
                    frames,
                    events: frames,
                    bytes,
                    dropped: 0,
                },
                loss: LossFacts {
                    dropped: 0,
                    truncated: false,
                    first_sequence_gap: None,
                },
                cleanup: CleanupFacts {
                    hooks: CleanupState::NotAcquired,
                    transient_input: CleanupState::NotAcquired,
                    sink: CleanupState::Released,
                },
                event_classes: Vec::new(),
                stop_event: None,
                reason: None,
            },
        },
    }
}

fn write_exact(staging: &mut RecordingStaging, frames: u64) -> u64 {
    let mut writer = staging.open_event_writer(1000, 1024 * 1024, 4096).unwrap();
    for offset in 0..frames {
        writer.write_record(&record(offset, 10 + offset)).unwrap();
    }
    let bytes = writer.bytes();
    writer.finish().unwrap();
    bytes
}

#[test]
fn publishes_and_reverifies_a_canonical_input_movie_member() {
    let root = tempfile::tempdir().unwrap();
    let mut staging = RecordingStaging::prepare(root.path(), "capture-movie").unwrap();
    let movie = b"0:right\n1:a,b\n";
    let identity = InputMovieIdentity {
        format: crate::input_movie::INPUT_MOVIE_FORMAT.into(),
        port: 0,
        frames: 2,
        bytes: movie.len() as u64,
        sha256: hex::encode(Sha256::digest(movie)),
    };
    staging.write_input_movie(movie, &identity).unwrap();
    let bytes = write_exact(&mut staging, 2);
    let mut input = complete_input("capture-movie", 2, bytes);
    input.validation.request.input_movie = Some(identity.clone());
    let published = staging
        .publish(&EventContractRegistry::builtin().unwrap(), input)
        .unwrap();
    let verified = verify_published_recording(
        &published.bundle_path,
        &EventContractRegistry::builtin().unwrap(),
    )
    .unwrap();
    assert_eq!(verified.manifest.request.input_movie, Some(identity));
    assert_eq!(
        fs::read(published.bundle_path.join("input.movie")).unwrap(),
        movie
    );
    assert!(verified
        .manifest
        .members
        .iter()
        .any(|member| member.role == MemberRole::InputMovie));
}

fn publish_terminal_snapshot(root: &std::path::Path, capture_id: &str) -> PublishedRecording {
    let mut staging = RecordingStaging::prepare(root, capture_id).unwrap();
    let snapshot = TerminalSnapshotRequest {
        label: "wram-end".into(),
        memory_type: "workram".into(),
        address: 4,
        length: 4,
    };
    staging
        .write_terminal_snapshot(&snapshot, &[4, 5, 6, 7])
        .unwrap();
    let bytes = write_exact(&mut staging, 1);
    let mut input = complete_input(capture_id, 1, bytes);
    input.validation.request.terminal_snapshots = vec![snapshot];
    staging
        .publish(&EventContractRegistry::builtin().unwrap(), input)
        .unwrap()
}

#[test]
fn publishes_and_reverifies_exact_terminal_snapshot_members() {
    let root = tempfile::tempdir().unwrap();
    let published = publish_terminal_snapshot(root.path(), "capture-snapshot");
    let verified = verify_published_recording(
        &published.bundle_path,
        &EventContractRegistry::builtin().unwrap(),
    )
    .unwrap();
    assert_eq!(verified.manifest.request.terminal_snapshots.len(), 1);
    assert!(verified.manifest.members.iter().any(|member| {
        member.role == MemberRole::TerminalSnapshot
            && member.path == "snapshots/wram-end.bin"
            && member.sha256 == hex::encode(Sha256::digest([4, 5, 6, 7]))
    }));
}

#[test]
fn publishes_and_reverifies_event_aligned_initial_snapshot_members() {
    let root = tempfile::tempdir().unwrap();
    let capture_id = "capture-initial-snapshot";
    let mut staging = RecordingStaging::prepare(root.path(), capture_id).unwrap();
    let snapshot = InitialSnapshotRequest {
        label: "wram-start".into(),
        memory_type: "snesWorkRam".into(),
        address: 0,
        length: 4,
    };
    staging
        .write_initial_snapshot(&snapshot, &[1, 2, 3, 4])
        .unwrap();
    let bytes = write_exact(&mut staging, 1);
    let mut input = complete_input(capture_id, 1, bytes);
    let identity = input.validation.request.event_classes[0].clone();
    input.validation.request.start_on = Some(EventStartCondition {
        event_class: identity.id.clone(),
    });
    input.validation.request.event_arming = vec![EventClassArming {
        id: identity.id.clone(),
        scope: EventArmingScope::Observation,
    }];
    input.validation.request.initial_snapshots = vec![snapshot];
    input.validation.observation_start = Some(ObservationStartFacts {
        sequence: 0,
        event_class: identity.id.clone(),
        contract_sha256: identity.contract_sha256,
        frame: 10,
        clock_domain: "frame".into(),
        clock_tick: 10,
    });
    input.validation.terminal.f_origin = Some(10);
    input.validation.terminal.event_classes = vec![EventClassTerminalFacts {
        id: identity.id,
        armed: true,
        armed_interval: Some(FrameInterval {
            f_start: 10,
            f_end: 11,
        }),
        observed: 1,
        dropped: 0,
    }];
    let published = staging
        .publish(&EventContractRegistry::builtin().unwrap(), input)
        .unwrap();
    let verified = verify_published_recording(
        &published.bundle_path,
        &EventContractRegistry::builtin().unwrap(),
    )
    .unwrap();
    assert!(verified.manifest.members.iter().any(|member| {
        member.role == MemberRole::InitialSnapshot
            && member.path == "initial-snapshots/wram-start.bin"
            && member.sha256 == hex::encode(Sha256::digest([1, 2, 3, 4]))
    }));
}

#[cfg(unix)]
#[test]
fn published_verification_rejects_tampered_or_extra_snapshot_files() {
    use std::os::unix::fs::PermissionsExt;
    let registry = EventContractRegistry::builtin().unwrap();

    let tampered_root = tempfile::tempdir().unwrap();
    let tampered = publish_terminal_snapshot(tampered_root.path(), "capture-snapshot-tampered");
    let path = tampered.bundle_path.join("snapshots/wram-end.bin");
    let mut permissions = std::fs::metadata(&path).unwrap().permissions();
    permissions.set_mode(0o600);
    std::fs::set_permissions(&path, permissions).unwrap();
    std::fs::write(&path, [9, 9, 9, 9]).unwrap();
    assert!(verify_published_recording(&tampered.bundle_path, &registry).is_err());

    let extra_root = tempfile::tempdir().unwrap();
    let extra = publish_terminal_snapshot(extra_root.path(), "capture-snapshot-extra");
    std::fs::write(extra.bundle_path.join("snapshots/unrequested.bin"), [0]).unwrap();
    assert!(verify_published_recording(&extra.bundle_path, &registry).is_err());
}

fn quarantine_count(root: &std::path::Path, capture_id: &str) -> usize {
    fs::read_dir(root)
        .unwrap()
        .filter_map(Result::ok)
        .filter(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .starts_with(&format!(".{capture_id}.invalid-"))
        })
        .count()
}

#[test]
fn publishes_validated_recording_atomically_with_member_and_manifest_hashes() {
    let root = tempfile::tempdir().unwrap();
    let capture_id = "capture-atomic";
    let mut staging = RecordingStaging::prepare(root.path(), capture_id).unwrap();
    let bytes = write_exact(&mut staging, 3);
    let published = staging
        .publish(
            &EventContractRegistry::builtin().unwrap(),
            complete_input(capture_id, 3, bytes),
        )
        .unwrap();

    assert_eq!(
        published.bundle_path,
        fs::canonicalize(root.path()).unwrap().join(capture_id)
    );
    assert!(published.bundle_path.join("manifest.json").is_file());
    assert!(published
        .bundle_path
        .join("events/segment-000.ndjson")
        .is_file());
    assert_eq!(published.manifest.terminal.integrity, Integrity::Complete);
    assert_eq!(published.manifest.members[0].bytes, bytes);
    let manifest_bytes = fs::read(published.bundle_path.join("manifest.json")).unwrap();
    assert_eq!(
        published.manifest_sha256,
        hex::encode(Sha256::digest(&manifest_bytes))
    );
    assert!(matches!(
        parse_manifest(std::str::from_utf8(&manifest_bytes).unwrap()).unwrap(),
        BundleManifest::Recording(_)
    ));
    let verified = verify_published_recording(
        &published.bundle_path,
        &EventContractRegistry::builtin().unwrap(),
    )
    .unwrap();
    assert_eq!(verified.manifest_sha256, published.manifest_sha256);
    assert_eq!(verified.manifest.capture_id, capture_id);
}

#[test]
fn published_verification_rejects_member_tampering() {
    let root = tempfile::tempdir().unwrap();
    let capture_id = "capture-tampered";
    let mut staging = RecordingStaging::prepare(root.path(), capture_id).unwrap();
    let bytes = write_exact(&mut staging, 2);
    let published = staging
        .publish(
            &EventContractRegistry::builtin().unwrap(),
            complete_input(capture_id, 2, bytes),
        )
        .unwrap();
    fs::OpenOptions::new()
        .append(true)
        .open(published.bundle_path.join("events/segment-000.ndjson"))
        .unwrap()
        .write_all(b"{}\n")
        .unwrap();

    assert!(verify_published_recording(
        &published.bundle_path,
        &EventContractRegistry::builtin().unwrap(),
    )
    .is_err());
}

#[test]
fn bounded_writer_rejects_before_starting_the_over_limit_record() {
    let root = tempfile::tempdir().unwrap();
    let mut staging = RecordingStaging::prepare(root.path(), "capture-bounds").unwrap();
    let first = record(0, 10);
    let mut writer = staging
        .open_event_writer(1, first.len() as u64, 4096)
        .unwrap();
    writer.write_record(&first).unwrap();
    let before = fs::metadata(staging.events_path()).unwrap().len();
    assert!(matches!(
        writer.write_record(&record(1, 11)),
        Err(super::error::PublishError::EventLimit(1))
    ));
    assert_eq!(fs::metadata(staging.events_path()).unwrap().len(), before);
    writer.finish().unwrap();
    staging.quarantine().unwrap();
}

#[test]
fn validates_output_root_capture_id_and_existing_destination_before_mutation() {
    assert!(matches!(
        RecordingStaging::prepare(std::path::Path::new("relative"), "capture-test"),
        Err(super::error::PublishError::InvalidOutputRoot(_))
    ));
    let root = tempfile::tempdir().unwrap();
    assert!(matches!(
        RecordingStaging::prepare(root.path(), "../escape"),
        Err(super::error::PublishError::InvalidCaptureId(_))
    ));
    assert!(matches!(
        RecordingStaging::prepare(root.path(), "capture_escape"),
        Err(super::error::PublishError::InvalidCaptureId(_))
    ));
    fs::create_dir(root.path().join("capture-existing")).unwrap();
    assert!(matches!(
        RecordingStaging::prepare(root.path(), "capture-existing"),
        Err(super::error::PublishError::DestinationExists(_))
    ));
}

#[cfg(unix)]
#[test]
fn rejects_symlink_output_root_and_member() {
    use std::os::unix::fs::symlink;

    let parent = tempfile::tempdir().unwrap();
    let real = parent.path().join("real");
    fs::create_dir(&real).unwrap();
    let root_link = parent.path().join("root-link");
    symlink(&real, &root_link).unwrap();
    assert!(matches!(
        RecordingStaging::prepare(&root_link, "capture-link"),
        Err(super::error::PublishError::SymlinkOutputRoot(_))
    ));

    let root = tempfile::tempdir().unwrap();
    let staging = RecordingStaging::prepare(root.path(), "capture-member-link").unwrap();
    let outside = parent.path().join("outside");
    fs::write(&outside, b"outside").unwrap();
    fs::remove_file(staging.events_path()).unwrap();
    symlink(&outside, staging.events_path()).unwrap();
    assert!(matches!(
        staging.publish(
            &EventContractRegistry::builtin().unwrap(),
            complete_input("capture-member-link", 1, 0)
        ),
        Err(super::error::PublishError::UnsafeMember(_))
    ));
    assert!(!root.path().join("capture-member-link").exists());
}

#[test]
fn missing_member_and_destination_replacement_fail_without_authoritative_manifest() {
    let root = tempfile::tempdir().unwrap();
    let staging = RecordingStaging::prepare(root.path(), "capture-missing").unwrap();
    fs::remove_file(staging.events_path()).unwrap();
    assert!(matches!(
        staging.publish(
            &EventContractRegistry::builtin().unwrap(),
            complete_input("capture-missing", 1, 0)
        ),
        Err(super::error::PublishError::MemberMissing(_))
    ));
    assert!(!root.path().join("capture-missing").exists());

    let mut staging = RecordingStaging::prepare(root.path(), "capture-replaced").unwrap();
    let bytes = write_exact(&mut staging, 1);
    fs::create_dir(root.path().join("capture-replaced")).unwrap();
    assert!(matches!(
        staging.publish(
            &EventContractRegistry::builtin().unwrap(),
            complete_input("capture-replaced", 1, bytes)
        ),
        Err(super::error::PublishError::DestinationExists(_))
    ));
    assert!(!root.path().join("capture-replaced/manifest.json").exists());
}

#[test]
fn write_sync_short_write_and_rename_faults_quarantine_and_allow_retry() {
    for (index, fault) in [
        PublishFault::ManifestWrite,
        PublishFault::ManifestShortWrite,
        PublishFault::ManifestSync,
        PublishFault::Rename,
    ]
    .into_iter()
    .enumerate()
    {
        let root = tempfile::tempdir().unwrap();
        let capture_id = format!("capture-fault-{index}");
        let mut staging = RecordingStaging::prepare(root.path(), &capture_id).unwrap();
        let bytes = write_exact(&mut staging, 1);
        assert!(staging
            .publish_with_fault(
                &EventContractRegistry::builtin().unwrap(),
                complete_input(&capture_id, 1, bytes),
                fault,
            )
            .is_err());
        assert!(!root.path().join(&capture_id).exists());
        assert_eq!(quarantine_count(root.path(), &capture_id), 1);
        let retry = RecordingStaging::prepare(root.path(), &capture_id).unwrap();
        retry.quarantine().unwrap();
    }
}

#[test]
fn partial_evidence_can_publish_only_as_unverifiable() {
    let root = tempfile::tempdir().unwrap();
    let capture_id = "capture-partial";
    let staging = RecordingStaging::prepare(root.path(), capture_id).unwrap();
    let first = record(0, 10);
    let mut raw = first.clone();
    raw.extend_from_slice(br#"{"sequence":1"#);
    fs::write(staging.events_path(), &raw).unwrap();
    let mut input = complete_input(capture_id, 2, first.len() as u64);
    input.validation.terminal.operation_outcome = OperationOutcome::Failed;
    input.validation.terminal.execution_outcome = ExecutionOutcome::AdapterError;
    input.validation.terminal.claimed_integrity = Integrity::Unverifiable;
    input.validation.terminal.final_frame = 11;
    input.validation.terminal.counters.events = 1;
    input.validation.terminal.cleanup.sink = CleanupState::Unverifiable;
    let published = staging
        .publish(&EventContractRegistry::builtin().unwrap(), input)
        .unwrap();
    assert_eq!(
        published.manifest.terminal.integrity,
        Integrity::Unverifiable
    );
    assert_eq!(published.manifest.members[0].bytes, raw.len() as u64);
    assert_eq!(published.manifest.members[0].records, Some(1));
}
