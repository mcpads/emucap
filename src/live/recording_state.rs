use std::fs::{self, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use super::recording_capability::RecordingStateLoadCapability;
use super::runtime::RuntimeStore;
use crate::bundle::recording_manifest::{
    FinalExecutionState, RuntimeIdentity, StateArtifactIdentity, StateSnapshotBoundary,
    StateSnapshotFrozenFacts, StateSnapshotReceipt,
};

const MAX_RECEIPT_BYTES: u64 = 128 * 1024;

#[derive(Debug, Clone)]
pub struct AcquiredStateFile {
    pub bytes: Vec<u8>,
    pub identity: StateArtifactIdentity,
}

#[derive(Debug, Clone)]
pub struct AcquiredRecordingState {
    pub bytes: Vec<u8>,
    pub receipt: StateSnapshotReceipt,
}

#[derive(Debug, thiserror::Error)]
pub enum RecordingStateError {
    #[error("initial state path must be absolute")]
    RelativePath,
    #[error("initial state must be a real regular file")]
    UnsafeFile,
    #[error("initial state must contain at least one byte")]
    Empty,
    #[error("initial state exceeds {0} bytes")]
    ByteLimit(u64),
    #[error("initial state expected_sha256 must be a SHA-256")]
    InvalidExpectedDigest,
    #[error("initial state SHA-256 mismatch: expected {expected}, got {actual}")]
    DigestMismatch { expected: String, actual: String },
    #[error("initial state changed while it was acquired")]
    Changed,
    #[error("snapshot_id is invalid")]
    InvalidSnapshotId,
    #[error("managed snapshot receipt is invalid: {0}")]
    InvalidReceipt(String),
    #[error("managed snapshot belongs to a different runtime generation")]
    RuntimeMismatch,
    #[error("state-backed recording requires a producer receipt at a frame boundary")]
    UnsafeBoundary,
    #[error("managed snapshot already exists")]
    AlreadyExists,
    #[error("initial state I/O failed: {0}")]
    Io(#[from] io::Error),
}

#[cfg(unix)]
fn same_identity(left: &fs::Metadata, right: &fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;

    left.dev() == right.dev()
        && left.ino() == right.ino()
        && left.len() == right.len()
        && left.mtime() == right.mtime()
        && left.mtime_nsec() == right.mtime_nsec()
}

#[cfg(not(unix))]
fn same_identity(left: &fs::Metadata, right: &fs::Metadata) -> bool {
    left.len() == right.len() && left.modified().ok() == right.modified().ok()
}

pub fn acquire_recording_state(
    path: &Path,
    expected_sha256: Option<&str>,
    capability: &RecordingStateLoadCapability,
) -> Result<AcquiredStateFile, RecordingStateError> {
    if !path.is_absolute() {
        return Err(RecordingStateError::RelativePath);
    }
    let expected_sha256 = expected_sha256
        .map(|digest| {
            if digest.len() == 64 && digest.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                Ok(digest.to_ascii_lowercase())
            } else {
                Err(RecordingStateError::InvalidExpectedDigest)
            }
        })
        .transpose()?;

    let path_before = fs::symlink_metadata(path)?;
    if path_before.file_type().is_symlink() || !path_before.is_file() {
        return Err(RecordingStateError::UnsafeFile);
    }
    if path_before.len() == 0 {
        return Err(RecordingStateError::Empty);
    }
    if path_before.len() > capability.max_bytes {
        return Err(RecordingStateError::ByteLimit(capability.max_bytes));
    }

    let mut file = crate::path_safety::open_regular_file_no_follow(path)?;
    let handle_before = file.metadata()?;
    if !same_identity(&path_before, &handle_before) {
        return Err(RecordingStateError::Changed);
    }
    let mut bytes = Vec::with_capacity(usize::try_from(handle_before.len()).unwrap_or(0));
    std::io::Read::by_ref(&mut file)
        .take(capability.max_bytes.saturating_add(1))
        .read_to_end(&mut bytes)?;
    if bytes.is_empty() {
        return Err(RecordingStateError::Empty);
    }
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > capability.max_bytes {
        return Err(RecordingStateError::ByteLimit(capability.max_bytes));
    }

    let handle_after = file.metadata()?;
    let path_after = fs::symlink_metadata(path)?;
    if path_after.file_type().is_symlink()
        || !path_after.is_file()
        || !same_identity(&handle_before, &handle_after)
        || !same_identity(&handle_after, &path_after)
    {
        return Err(RecordingStateError::Changed);
    }

    let sha256 = hex::encode(Sha256::digest(&bytes));
    if let Some(expected) = expected_sha256 {
        if expected != sha256 {
            return Err(RecordingStateError::DigestMismatch {
                expected,
                actual: sha256,
            });
        }
    }
    Ok(AcquiredStateFile {
        identity: StateArtifactIdentity {
            format: capability.format.clone(),
            bytes: u64::try_from(bytes.len()).unwrap_or(u64::MAX),
            sha256,
        },
        bytes,
    })
}

fn valid_snapshot_id(value: &str) -> bool {
    value.starts_with("snapshot-") && crate::path_safety::is_hyphenated_ascii_id(value, 96)
}

fn snapshots_root(store: &RuntimeStore, port: u16, launch_id: &str) -> PathBuf {
    store
        .generation_dir(port, launch_id)
        .join("recording-snapshots")
}

fn write_private_state(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut options = OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    file.write_all(bytes)?;
    file.sync_all()
}

pub fn preserve_state_snapshot(
    store: &RuntimeStore,
    port: u16,
    source: RuntimeIdentity,
    state: AcquiredStateFile,
    frame: u64,
    boundary: StateSnapshotBoundary,
) -> Result<StateSnapshotReceipt, RecordingStateError> {
    let snapshot_id = format!(
        "snapshot-{}",
        ulid::Ulid::generate().to_string().to_ascii_lowercase()
    );
    let receipt = StateSnapshotReceipt {
        snapshot_id: snapshot_id.clone(),
        snapshot: state.identity,
        source,
        frozen: StateSnapshotFrozenFacts {
            state: FinalExecutionState::Frozen,
            frame,
            boundary,
        },
    };
    let root = snapshots_root(store, port, &receipt.source.launch_id);
    store.create_managed_dir(&root)?;
    let destination = root.join(&snapshot_id);
    if destination.exists() {
        return Err(RecordingStateError::AlreadyExists);
    }
    let staging = root.join(format!(".{snapshot_id}.staging"));
    if staging.exists() {
        return Err(RecordingStateError::AlreadyExists);
    }
    store.create_managed_dir(&staging)?;
    let result = (|| {
        write_private_state(&staging.join("state.bin"), &state.bytes)?;
        super::runtime::write_atomic_json(&staging.join("receipt.json"), &receipt)?;
        fs::rename(&staging, &destination)?;
        super::runtime::sync_parent(&root)
    })();
    if result.is_err() {
        let _ = fs::remove_dir_all(&staging);
    }
    result?;
    Ok(receipt)
}

fn read_receipt(path: &Path) -> Result<StateSnapshotReceipt, RecordingStateError> {
    let mut file = crate::path_safety::open_regular_file_no_follow(path)?;
    let mut bytes = Vec::new();
    std::io::Read::by_ref(&mut file)
        .take(MAX_RECEIPT_BYTES.saturating_add(1))
        .read_to_end(&mut bytes)?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > MAX_RECEIPT_BYTES {
        return Err(RecordingStateError::InvalidReceipt(
            "receipt exceeds its byte limit".into(),
        ));
    }
    serde_json::from_slice(&bytes)
        .map_err(|error| RecordingStateError::InvalidReceipt(error.to_string()))
}

fn same_runtime_generation(saved: &RuntimeIdentity, live: &RuntimeIdentity) -> bool {
    saved.system == live.system
        && saved.adapter_id == live.adapter_id
        && saved.adapter_build == live.adapter_build
        && saved.emulator_id == live.emulator_id
        && saved.emulator_build == live.emulator_build
        && saved.emulator_upstream_revision == live.emulator_upstream_revision
        && saved.emulator_patchset_sha256 == live.emulator_patchset_sha256
        && saved.launch_id == live.launch_id
        && saved.content == live.content
}

pub fn acquire_managed_recording_state(
    store: &RuntimeStore,
    port: u16,
    launch_id: &str,
    snapshot_id: &str,
    live: &RuntimeIdentity,
    capability: &RecordingStateLoadCapability,
) -> Result<AcquiredRecordingState, RecordingStateError> {
    if !valid_snapshot_id(snapshot_id) {
        return Err(RecordingStateError::InvalidSnapshotId);
    }
    if live.launch_id != launch_id {
        return Err(RecordingStateError::RuntimeMismatch);
    }
    let root = snapshots_root(store, port, launch_id);
    let relative_receipt = format!("{snapshot_id}/receipt.json");
    let receipt_path = crate::path_safety::regular_member_path(&root, &relative_receipt)?;
    let receipt = read_receipt(&receipt_path)?;
    if receipt.snapshot_id != snapshot_id {
        return Err(RecordingStateError::InvalidReceipt(
            "receipt snapshot_id does not match its managed directory".into(),
        ));
    }
    if receipt.frozen.state != FinalExecutionState::Frozen
        || receipt.frozen.boundary != StateSnapshotBoundary::FrameBoundary
    {
        return Err(RecordingStateError::UnsafeBoundary);
    }
    if !same_runtime_generation(&receipt.source, live) {
        return Err(RecordingStateError::RuntimeMismatch);
    }
    let relative_state = format!("{snapshot_id}/state.bin");
    let state_path = crate::path_safety::regular_member_path(&root, &relative_state)?;
    let state = acquire_recording_state(&state_path, Some(&receipt.snapshot.sha256), capability)?;
    if state.identity != receipt.snapshot {
        return Err(RecordingStateError::InvalidReceipt(
            "receipt does not match the managed state bytes".into(),
        ));
    }
    Ok(AcquiredRecordingState {
        bytes: state.bytes,
        receipt,
    })
}
