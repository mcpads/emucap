use std::path::Path;

use serde_json::{json, Value};

use super::link::{EmulatorLink, LinkError};
use super::runtime::RuntimeStore;
use super::tools::ToolOutput;
use crate::bundle::recording_manifest::StateSnapshotBoundary;

pub fn save_state(link: &mut dyn EmulatorLink, path: &str) -> Result<ToolOutput, LinkError> {
    Ok(ToolOutput::Json(
        link.call("save_state", json!({ "path": path }))?,
    ))
}

pub fn save_state_for_recording(
    link: &mut dyn EmulatorLink,
    path: &str,
) -> Result<ToolOutput, LinkError> {
    save_state_in_store(link, path, RuntimeStore::discover())
}

pub(crate) fn save_state_in_store(
    link: &mut dyn EmulatorLink,
    path: &str,
    store: RuntimeStore,
) -> Result<ToolOutput, LinkError> {
    let recording =
        link.capabilities().recording.clone().ok_or_else(|| {
            LinkError::Protocol("recording is not advertised by this runtime".into())
        })?;
    let state_load = recording
        .state_load
        .clone()
        .ok_or_else(|| LinkError::Protocol("state-backed recording is not advertised".into()))?;
    let identity = link.capabilities().identity.clone();
    let port = link.endpoint_port().ok_or_else(|| {
        LinkError::Protocol("a managed direct runtime is required for a recording receipt".into())
    })?;
    if !Path::new(path).is_absolute() {
        return Err(LinkError::Protocol(
            "save_state requires an absolute path for a producer-managed snapshot receipt".into(),
        ));
    }
    let current = store
        .read_current(port)
        .map_err(|error| LinkError::Protocol(format!("runtime identity read failed: {error}")))?
        .ok_or_else(|| LinkError::Protocol("runtime generation is missing".into()))?;
    if identity.launch_id.as_deref() != Some(current.launch_id.as_str())
        || identity.content.as_deref() != Some(current.content.as_str())
    {
        return Err(LinkError::Protocol(
            "save_state receipt requires an exactly bound live generation".into(),
        ));
    }
    let source = super::recording_request::runtime_identity(&identity, &current, &recording)
        .map_err(|error| LinkError::Protocol(format!("runtime identity failed: {error}")))?;
    let mut result = link.call("save_state", json!({ "path": path }))?;
    if result.get("state").and_then(Value::as_str) != Some("frozen") {
        return Err(LinkError::Protocol(
            "save_state did not prove the frozen state required for a recording receipt".into(),
        ));
    }
    let state = super::recording_state::acquire_recording_state(Path::new(path), None, &state_load)
        .map_err(|error| {
            LinkError::Protocol(format!(
                "save_state completed but its exact receipt could not be acquired: {error}"
            ))
        })?;
    let frame = result
        .get("frame")
        .and_then(Value::as_u64)
        .ok_or_else(|| LinkError::Protocol("frozen save_state response lacks its frame".into()))?;
    let boundary = match result.get("boundary").and_then(Value::as_str) {
        Some("frame_boundary") => StateSnapshotBoundary::FrameBoundary,
        Some("instruction_boundary") => StateSnapshotBoundary::InstructionBoundary,
        _ => {
            return Err(LinkError::Protocol(
                "frozen save_state response lacks its boundary kind".into(),
            ));
        }
    };
    let receipt = super::recording_state::preserve_state_snapshot(
        &store, port, source, state, frame, boundary,
    )
    .map_err(|error| {
        LinkError::Protocol(format!(
            "save_state completed but its managed receipt could not be preserved: {error}"
        ))
    })?;
    let object = result
        .as_object_mut()
        .ok_or_else(|| LinkError::Protocol("save_state returned a non-object result".into()))?;
    object.insert("snapshot_receipt".into(), json!(receipt));
    Ok(ToolOutput::Json(result))
}
