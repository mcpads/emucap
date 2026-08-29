use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use super::error::PublishError;
use super::publish::{regular_owned_member, valid_hex_digest, valid_profile_id};
use super::recording_manifest::{
    FinalExecutionState, MemberDescriptor, MemberRole, RecordingRequest, RuntimeIdentity,
    StateArtifactIdentity, StateSnapshotBoundary, StateSnapshotReceipt,
};

const INITIAL_STATE_MEMBER: &str = "initial.state";

pub(super) fn write_initial_state(
    staging_path: &Path,
    already_written: bool,
    bytes: &[u8],
    identity: &StateArtifactIdentity,
) -> Result<PathBuf, PublishError> {
    if already_written
        || identity.bytes != u64::try_from(bytes.len()).unwrap_or(u64::MAX)
        || identity.bytes == 0
        || identity.bytes > crate::live::recording_capability::CORE_MAX_RECORDING_STATE_BYTES
        || !valid_profile_id(&identity.format)
        || !valid_hex_digest(&identity.sha256, 32)
        || identity.sha256 != hex::encode(Sha256::digest(bytes))
    {
        return Err(PublishError::InvalidIdentity(
            "initial state identity or staging state mismatch".into(),
        ));
    }
    let path = staging_path.join(INITIAL_STATE_MEMBER);
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o400);
    }
    let mut file = options.open(&path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    drop(file);
    let mut permissions = fs::metadata(&path)?.permissions();
    permissions.set_readonly(true);
    fs::set_permissions(&path, permissions)?;
    Ok(path)
}

pub(super) fn validate_initial_state_member(
    bundle_path: &Path,
    request: &RecordingRequest,
    runtime: &RuntimeIdentity,
) -> Result<(), PublishError> {
    let path = bundle_path.join(INITIAL_STATE_MEMBER);
    let Some(receipt) = &request.initial_state else {
        if fs::symlink_metadata(&path).is_ok() {
            return Err(PublishError::InvalidIdentity(
                "unexpected initial state member".into(),
            ));
        }
        return Ok(());
    };
    let identity = &receipt.snapshot;
    if !crate::path_safety::is_hyphenated_ascii_id(&receipt.snapshot_id, 96)
        || !receipt.snapshot_id.starts_with("snapshot-")
        || receipt.frozen.state != FinalExecutionState::Frozen
        || receipt.frozen.boundary != StateSnapshotBoundary::FrameBoundary
        || !valid_profile_id(&identity.format)
        || identity.bytes == 0
        || identity.bytes > crate::live::recording_capability::CORE_MAX_RECORDING_STATE_BYTES
        || !valid_hex_digest(&identity.sha256, 32)
    {
        return Err(PublishError::InvalidIdentity(
            "initial state request identity is invalid".into(),
        ));
    }
    let source = &receipt.source;
    if source.system != runtime.system
        || source.adapter_id != runtime.adapter_id
        || source.adapter_build != runtime.adapter_build
        || source.emulator_id != runtime.emulator_id
        || source.emulator_build != runtime.emulator_build
        || source.emulator_upstream_revision != runtime.emulator_upstream_revision
        || source.emulator_patchset_sha256 != runtime.emulator_patchset_sha256
        || source.launch_id != runtime.launch_id
        || source.content != runtime.content
    {
        return Err(PublishError::InvalidIdentity(
            "initial state receipt belongs to a different runtime generation".into(),
        ));
    }
    regular_owned_member(&path)?;
    let metadata = fs::metadata(&path)?;
    if metadata.len() != identity.bytes {
        return Err(PublishError::InvalidIdentity(
            "initial state member size mismatch".into(),
        ));
    }
    let bytes = fs::read(&path)?;
    if identity.sha256 != hex::encode(Sha256::digest(&bytes)) {
        return Err(PublishError::InvalidIdentity(
            "initial state member digest mismatch".into(),
        ));
    }
    Ok(())
}

pub(super) fn member_descriptor(receipt: &StateSnapshotReceipt) -> MemberDescriptor {
    MemberDescriptor {
        role: MemberRole::InitialState,
        path: INITIAL_STATE_MEMBER.into(),
        sha256: receipt.snapshot.sha256.clone(),
        bytes: receipt.snapshot.bytes,
        records: None,
    }
}

pub(super) fn descriptor_matches(
    members: &[MemberDescriptor],
    receipt: &StateSnapshotReceipt,
) -> bool {
    let state_members: Vec<_> = members
        .iter()
        .filter(|member| member.role == MemberRole::InitialState)
        .collect();
    state_members.len() == 1 && state_members[0] == &member_descriptor(receipt)
}
