use serde_json::{json, Value};

use super::capture_capsule::{CaptureCapsuleRepository, CaptureState};
use super::continuity::RuntimeBindingState;
use super::link::{EmulatorLink, RequestCancellation};
use super::recording::{terminal_summary, RecordingError};
use super::runtime::{ProcessState, RuntimeStore};
use crate::bundle::publish::RecordingStaging;
use crate::bundle::recording::RecordingValidationInput;
use crate::bundle::recording_manifest::{
    Integrity, OperationOutcome, PublicationOutcome, TerminalSnapshotRequest, TerminalStateRequest,
};
use crate::live::recording_capability::RecordingTerminalStateCapability;

pub(super) struct CapturedTerminalSnapshot {
    pub(super) request: TerminalSnapshotRequest,
    pub(super) bytes: Vec<u8>,
}

pub(super) struct CapturedTerminalState {
    pub(super) request: TerminalStateRequest,
    pub(super) bytes: Vec<u8>,
}

pub(super) struct TerminalSnapshotReadout<'a> {
    pub(super) store: &'a RuntimeStore,
    pub(super) port: u16,
    pub(super) launch_id: &'a str,
    pub(super) capability_revision: &'a str,
    pub(super) final_frame: u64,
    pub(super) cancellation: &'a RequestCancellation,
}

pub(super) fn capture_terminal_snapshots(
    link: &mut dyn EmulatorLink,
    requests: &[TerminalSnapshotRequest],
    readout: &TerminalSnapshotReadout<'_>,
) -> Result<Vec<CapturedTerminalSnapshot>, RecordingError> {
    verify_terminal_snapshot_boundary(link, readout)?;
    let mut captured = Vec::with_capacity(requests.len());
    for request in requests {
        if readout.cancellation.is_cancelled() {
            return Err(RecordingError::Snapshot(
                "request was cancelled during frozen readout".into(),
            ));
        }
        let region = link
            .capabilities()
            .memory_regions
            .iter()
            .find(|region| region.memory_type == request.memory_type)
            .ok_or_else(|| {
                RecordingError::Snapshot(format!(
                    "memory region {} disappeared before frozen readout",
                    request.memory_type
                ))
            })?;
        if request.address > region.size
            || request.length > region.size.saturating_sub(request.address)
        {
            return Err(RecordingError::Snapshot(format!(
                "memory region {} changed before frozen readout",
                request.memory_type
            )));
        }
        let result = link.call(
            "read_memory",
            json!({
                "memory_type": request.memory_type,
                "address": request.address,
                "length": request.length,
            }),
        )?;
        let encoded = result.get("hex").and_then(Value::as_str).ok_or_else(|| {
            RecordingError::Snapshot(format!(
                "terminal snapshot {} returned no hex payload",
                request.label
            ))
        })?;
        let bytes = hex::decode(encoded).map_err(|error| {
            RecordingError::Snapshot(format!(
                "terminal snapshot {} returned invalid hex: {error}",
                request.label
            ))
        })?;
        if u64::try_from(bytes.len()).unwrap_or(u64::MAX) != request.length {
            return Err(RecordingError::Snapshot(format!(
                "terminal snapshot {} returned the wrong byte count",
                request.label
            )));
        }
        captured.push(CapturedTerminalSnapshot {
            request: request.clone(),
            bytes,
        });
    }
    verify_terminal_snapshot_boundary(link, readout)?;
    if readout.cancellation.is_cancelled() {
        return Err(RecordingError::Snapshot(
            "request was cancelled during frozen readout".into(),
        ));
    }
    Ok(captured)
}

pub(super) fn capture_terminal_state(
    link: &mut dyn EmulatorLink,
    request: &TerminalStateRequest,
    capability: &RecordingTerminalStateCapability,
    readout: &TerminalSnapshotReadout<'_>,
) -> Result<CapturedTerminalState, RecordingError> {
    verify_terminal_snapshot_boundary(link, readout)?;
    if readout.cancellation.is_cancelled() {
        return Err(RecordingError::Snapshot(
            "request was cancelled during frozen state readout".into(),
        ));
    }
    let profile = capability
        .profiles
        .iter()
        .find(|profile| {
            profile.id == request.profile && profile.contract_sha256 == request.contract_sha256
        })
        .ok_or_else(|| {
            RecordingError::Snapshot("terminal state profile changed before frozen readout".into())
        })?;
    let result = link.call("get_state", json!({ "groups": profile.groups }))?;
    let state = result.get("state").ok_or_else(|| {
        RecordingError::Snapshot("terminal state query returned no state payload".into())
    })?;
    let object = state
        .as_object()
        .filter(|object| !object.is_empty())
        .ok_or_else(|| {
            RecordingError::Snapshot("terminal state query returned an empty non-object".into())
        })?;
    if object.keys().any(|key| {
        let group = key.split(['.', '[']).next().unwrap_or_default();
        !profile.groups.iter().any(|expected| expected == group)
    }) {
        return Err(RecordingError::Snapshot(
            "terminal state query returned fields outside the advertised profile".into(),
        ));
    }
    let bytes = crate::track::observe::canonical_json(state).into_bytes();
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > capability.max_bytes {
        return Err(RecordingError::Snapshot(format!(
            "terminal state exceeds the advertised {} byte bound",
            capability.max_bytes
        )));
    }
    verify_terminal_snapshot_boundary(link, readout)?;
    if readout.cancellation.is_cancelled() {
        return Err(RecordingError::Snapshot(
            "request was cancelled during frozen state readout".into(),
        ));
    }
    Ok(CapturedTerminalState {
        request: request.clone(),
        bytes,
    })
}

fn verify_terminal_snapshot_boundary(
    link: &mut dyn EmulatorLink,
    readout: &TerminalSnapshotReadout<'_>,
) -> Result<(), RecordingError> {
    let status = link.call("status", json!({}))?;
    if link.capabilities().identity.launch_id.as_deref() != Some(readout.launch_id)
        || link
            .capabilities()
            .recording
            .as_ref()
            .is_none_or(|capability| capability.revision != readout.capability_revision)
        || link.continuity().runtime_binding.state != RuntimeBindingState::Bound
    {
        return Err(RecordingError::Snapshot(
            "live generation or recording capability changed during frozen readout".into(),
        ));
    }
    let current = readout
        .store
        .read_current(readout.port)?
        .ok_or_else(|| RecordingError::Snapshot("runtime generation disappeared".into()))?;
    if current.launch_id != readout.launch_id || current.process_state() != ProcessState::Alive {
        return Err(RecordingError::Snapshot(
            "runtime generation changed during frozen readout".into(),
        ));
    }
    if status.get("state").and_then(Value::as_str) != Some("frozen")
        || status.get("frame").and_then(Value::as_u64) != Some(readout.final_frame)
    {
        return Err(RecordingError::Snapshot(format!(
            "terminal boundary is not frozen at frame {}",
            readout.final_frame
        )));
    }
    Ok(())
}

pub(super) fn terminalize_snapshot_failure(
    repository: &CaptureCapsuleRepository,
    capture_id: &str,
    staging: &mut Option<RecordingStaging>,
    validation: &mut RecordingValidationInput,
    reason: String,
) -> Result<(), RecordingError> {
    validation.terminal.operation_outcome = OperationOutcome::Failed;
    validation.terminal.claimed_integrity = Integrity::Unverifiable;
    validation.terminal.reason = Some(reason.clone());
    repository.transition(
        capture_id,
        CaptureState::FrozenReadout,
        CaptureState::Finalizing,
        None,
    )?;
    let terminal = terminal_summary(
        &validation.terminal,
        PublicationOutcome::Failed,
        None,
        None,
        Some(reason),
    );
    repository.transition(
        capture_id,
        CaptureState::Finalizing,
        CaptureState::PublicationFailed,
        Some(terminal),
    )?;
    if let Some(staging) = staging.take() {
        let _ = staging.quarantine();
    }
    Ok(())
}
