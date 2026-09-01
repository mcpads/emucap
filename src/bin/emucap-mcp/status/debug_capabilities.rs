use emucap::live::link::EmulatorIdentity;

pub(super) fn enrich_debug_selection(value: &mut serde_json::Value, identity: &EmulatorIdentity) {
    let Some(object) = value.as_object_mut() else {
        return;
    };
    if !object
        .get("connected")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(true)
    {
        return;
    }

    // Hello is the generation-binding handshake and has already passed strict validation. Never
    // let a later status payload replace that catalog with unvalidated or stale selector metadata.
    object.remove("state_groups");
    object.remove("cpu_targets");
    if !identity.state_groups.is_empty() {
        object.insert(
            "state_groups".into(),
            serde_json::json!(identity.state_groups),
        );
    }
    if !identity.cpu_targets.is_empty() {
        object.insert(
            "cpu_targets".into(),
            serde_json::json!(identity.cpu_targets),
        );
    }
}
