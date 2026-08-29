use std::fs;

use super::recording_capability::{RecordingStateLoadAlignment, RecordingStateLoadCapability};
use super::recording_state::{
    acquire_managed_recording_state, acquire_recording_state, preserve_state_snapshot,
    RecordingStateError,
};
use super::runtime::RuntimeStore;
use crate::bundle::recording_manifest::{ContentIdentity, RuntimeIdentity, StateSnapshotBoundary};

fn capability() -> RecordingStateLoadCapability {
    RecordingStateLoadCapability {
        format: "mesen-savestate".into(),
        max_bytes: 16,
        alignment: RecordingStateLoadAlignment::RestoredFrameBoundary,
        requires_input_movie: true,
    }
}

fn runtime(launch_id: &str) -> RuntimeIdentity {
    RuntimeIdentity {
        system: "snes".into(),
        adapter_id: "mesen2".into(),
        server_build: "server".into(),
        adapter_build: "adapter".into(),
        emulator_id: "mesen".into(),
        emulator_build: "binary".into(),
        emulator_upstream_revision: "upstream".into(),
        emulator_patchset_sha256: "patchset".into(),
        launch_id: launch_id.into(),
        capability_revision: "capability".into(),
        content: ContentIdentity {
            sha1: Some("sha1".into()),
            sha256: Some("sha256".into()),
            bytes: 4,
            path_hint: Some("game.sfc".into()),
        },
    }
}

#[test]
fn acquires_exact_state_bytes_and_enforces_the_optional_digest() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("anchor.mss");
    fs::write(&path, b"saved-state").unwrap();

    let acquired = acquire_recording_state(&path, None, &capability()).unwrap();
    assert_eq!(acquired.bytes, b"saved-state");
    assert_eq!(acquired.identity.bytes, 11);
    assert_eq!(acquired.identity.format, "mesen-savestate");

    let matching = acquire_recording_state(
        &path,
        Some(&acquired.identity.sha256.to_ascii_uppercase()),
        &capability(),
    )
    .unwrap();
    assert_eq!(matching.identity, acquired.identity);
    assert!(matches!(
        acquire_recording_state(&path, Some(&"00".repeat(32)), &capability()),
        Err(RecordingStateError::DigestMismatch { .. })
    ));
}

#[test]
fn rejects_relative_empty_oversized_and_symlinked_state_inputs() {
    assert!(matches!(
        acquire_recording_state(std::path::Path::new("relative.mss"), None, &capability()),
        Err(RecordingStateError::RelativePath)
    ));

    let directory = tempfile::tempdir().unwrap();
    let empty = directory.path().join("empty.mss");
    fs::write(&empty, []).unwrap();
    assert!(matches!(
        acquire_recording_state(&empty, None, &capability()),
        Err(RecordingStateError::Empty)
    ));

    let oversized = directory.path().join("oversized.mss");
    fs::write(&oversized, [0_u8; 17]).unwrap();
    assert!(matches!(
        acquire_recording_state(&oversized, None, &capability()),
        Err(RecordingStateError::ByteLimit(16))
    ));

    #[cfg(unix)]
    {
        let target = directory.path().join("target.mss");
        let link = directory.path().join("link.mss");
        fs::write(&target, b"state").unwrap();
        std::os::unix::fs::symlink(&target, &link).unwrap();
        assert!(matches!(
            acquire_recording_state(&link, None, &capability()),
            Err(RecordingStateError::UnsafeFile)
        ));
    }
}

#[test]
fn producer_managed_receipts_bind_exact_generation_boundary_and_bytes() {
    let directory = tempfile::tempdir().unwrap();
    let store = RuntimeStore::new(directory.path().join("sessions"));
    let source_path = directory.path().join("source.mss");
    fs::write(&source_path, b"saved-state").unwrap();
    let state = acquire_recording_state(&source_path, None, &capability()).unwrap();
    let source = runtime("launch-test");
    let receipt = preserve_state_snapshot(
        &store,
        47800,
        source.clone(),
        state,
        120,
        StateSnapshotBoundary::FrameBoundary,
    )
    .unwrap();

    let acquired = acquire_managed_recording_state(
        &store,
        47800,
        "launch-test",
        &receipt.snapshot_id,
        &source,
        &capability(),
    )
    .unwrap();
    assert_eq!(acquired.bytes, b"saved-state");
    assert_eq!(acquired.receipt, receipt);

    let managed_state = store
        .generation_dir(47800, "launch-test")
        .join("recording-snapshots")
        .join(&receipt.snapshot_id)
        .join("state.bin");
    fs::write(&managed_state, b"tampered").unwrap();
    assert!(matches!(
        acquire_managed_recording_state(
            &store,
            47800,
            "launch-test",
            &receipt.snapshot_id,
            &source,
            &capability(),
        ),
        Err(RecordingStateError::DigestMismatch { .. })
            | Err(RecordingStateError::InvalidReceipt(_))
    ));

    assert!(matches!(
        acquire_managed_recording_state(
            &store,
            47800,
            "launch-other",
            &receipt.snapshot_id,
            &runtime("launch-other"),
            &capability(),
        ),
        Err(RecordingStateError::Io(_)) | Err(RecordingStateError::RuntimeMismatch)
    ));
    assert!(matches!(
        acquire_managed_recording_state(
            &store,
            47800,
            "launch-test",
            "../escape",
            &source,
            &capability(),
        ),
        Err(RecordingStateError::InvalidSnapshotId)
    ));
}

#[test]
fn instruction_boundary_receipts_are_preserved_but_not_admitted_as_frame_windows() {
    let directory = tempfile::tempdir().unwrap();
    let store = RuntimeStore::new(directory.path().join("sessions"));
    let source_path = directory.path().join("source.mss");
    fs::write(&source_path, b"saved-state").unwrap();
    let state = acquire_recording_state(&source_path, None, &capability()).unwrap();
    let source = runtime("launch-test");
    let receipt = preserve_state_snapshot(
        &store,
        47800,
        source.clone(),
        state,
        7,
        StateSnapshotBoundary::InstructionBoundary,
    )
    .unwrap();
    assert!(matches!(
        acquire_managed_recording_state(
            &store,
            47800,
            "launch-test",
            &receipt.snapshot_id,
            &source,
            &capability(),
        ),
        Err(RecordingStateError::UnsafeBoundary)
    ));
}
