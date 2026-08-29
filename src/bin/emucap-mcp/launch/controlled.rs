use emucap::live::recording_capability::RecordingCapabilityOrigin;
use emucap::live::runtime::ExecutionProfileIdentity;

pub(super) fn controlled_launch_contract_error(
    controlled_start: bool,
    repeatable: bool,
    ready_status: &serde_json::Value,
    execution_profile: Option<&ExecutionProfileIdentity>,
    capabilities: &emucap::live::link::Capabilities,
) -> Option<String> {
    if !controlled_start {
        return None;
    }
    if ready_status
        .get("state")
        .and_then(serde_json::Value::as_str)
        != Some("frozen")
    {
        return Some("controlled launch connected without a frozen guest entry boundary".into());
    }
    let launch_start = ready_status.get("launch_start");
    if launch_start
        .and_then(|value| value.get("controlled"))
        .and_then(serde_json::Value::as_bool)
        != Some(true)
        || launch_start
            .and_then(|value| value.get("boundary"))
            .and_then(serde_json::Value::as_str)
            != Some("pre_first_instruction")
    {
        return Some("controlled launch host did not prove its guest entry boundary".into());
    }

    let required_feature = if repeatable {
        "repeatable_recording"
    } else {
        "controlled_start"
    };
    if !capabilities
        .identity
        .host_features
        .iter()
        .any(|feature| feature == required_feature)
    {
        return Some(format!("controlled launch host lacks {required_feature}"));
    }
    if !repeatable {
        return None;
    }

    let Some(expected_profile) = execution_profile else {
        return Some("repeatable launch outcome lacks the selected execution identity".into());
    };
    let live = capabilities
        .recording
        .as_ref()
        .and_then(|recording| recording.repeatability.as_ref());
    if live.is_none_or(|live| {
        live.profile.as_str() != expected_profile.id.as_str()
            || live.conditions_sha256.as_str() != expected_profile.conditions_sha256.as_str()
            || !live
                .origins
                .contains(&RecordingCapabilityOrigin::ResetRelease)
            || !live.requires_input_movie
    }) {
        return Some(
            "repeatable launch host did not advertise the exact selected recording conditions"
                .into(),
        );
    }
    None
}
