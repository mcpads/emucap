use emucap::live::link::EmulatorLink;

pub(crate) fn enrich_continuity(v: &mut serde_json::Value, link: &dyn EmulatorLink) {
    let continuity = link.continuity();
    let Some(object) = v.as_object_mut() else {
        return;
    };
    object.insert(
        "continuity".into(),
        serde_json::to_value(&continuity).unwrap_or_else(|_| serde_json::json!({})),
    );
    if continuity
        .runtime_diagnostics
        .iter()
        .any(|diagnostic| diagnostic.blocks_generation_transition)
    {
        object.insert(
            "next_safe_action".into(),
            serde_json::json!(
                "inspect the reported runtime artifact; do not replace a live emulator until ownership is proven"
            ),
        );
    }
    let candidates = link.runtime_candidates();
    if !candidates.is_empty() {
        object.insert(
            "runtime_candidates".into(),
            serde_json::Value::Array(candidates),
        );
        object.insert(
            "next_safe_action".into(),
            serde_json::json!("select an explicit runtime candidate; automatic attach refused"),
        );
    }

    let store = emucap::live::runtime::RuntimeStore::discover();
    let refreshed_current = link
        .endpoint_port()
        .and_then(|port| store.read_current(port).ok().flatten());
    if let (Some(port), Some(current)) = (link.endpoint_port(), refreshed_current.as_ref()) {
        match store.read_capture_json::<emucap::live::capture_capsule::CaptureCapsule>(
            port,
            &current.launch_id,
        ) {
            Ok(Some(capsule)) => {
                let field = if matches!(
                    continuity.runtime_binding.state,
                    emucap::live::continuity::RuntimeBindingState::Mismatched
                        | emucap::live::continuity::RuntimeBindingState::Unmanaged
                ) {
                    "stale_recording_capture"
                } else {
                    "recording_capture"
                };
                object.insert(field.into(), recording_capture_projection(&capsule));
            }
            Ok(None) => {}
            Err(_) => {
                object.insert(
                    "recording_capture".into(),
                    serde_json::json!({
                        "metadata_state": "invalid",
                        "launch_id": current.launch_id,
                        "next_safe_action": "inspect failure context; do not edit runtime metadata by hand"
                    }),
                );
            }
        }
    }

    let current = refreshed_current.map(|current| {
        let mut value = current.public_value_with_lease(&continuity.lease);
        if let (Some(runtime), Some(termination)) =
            (value.as_object_mut(), continuity.termination.as_ref())
        {
            runtime.insert(
                "termination".into(),
                serde_json::to_value(termination).unwrap_or_else(|_| serde_json::json!({})),
            );
        }
        value
    });
    enrich_runtime_instance(object, &continuity, current);
}

pub(crate) fn recording_capture_projection(
    capsule: &emucap::live::capture_capsule::CaptureCapsule,
) -> serde_json::Value {
    const MAX_REASON_BYTES: usize = 512;
    const MAX_BUNDLE_PATH_BYTES: usize = 4096;

    fn bounded_text(value: &str, max_bytes: usize) -> String {
        if value.len() <= max_bytes {
            return value.to_string();
        }
        let mut end = max_bytes;
        while !value.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}...", &value[..end])
    }

    let terminal = capsule.terminal.as_ref().map(|terminal| {
        let bundle_path = terminal
            .bundle_path
            .as_deref()
            .filter(|path| path.len() <= MAX_BUNDLE_PATH_BYTES);
        serde_json::json!({
            "operation_outcome": terminal.operation_outcome,
            "execution_outcome": terminal.execution_outcome,
            "integrity": terminal.integrity,
            "publication": terminal.publication,
            "final_execution_state": terminal.final_execution_state,
            "final_frame": terminal.final_frame,
            "counters": terminal.counters,
            "cleanup": terminal.cleanup,
            "stop_event": terminal.stop_event,
            "bundle_path": bundle_path,
            "bundle_path_omitted": terminal.bundle_path.is_some() && bundle_path.is_none(),
            "manifest_sha256": terminal.manifest_sha256,
            "reason": terminal.reason.as_deref().map(|reason| bounded_text(reason, MAX_REASON_BYTES)),
        })
    });
    serde_json::json!({
        "capture_id": capsule.capture_id,
        "launch_id": capsule.launch_id,
        "state": capsule.state,
        "progress": capsule.progress,
        "terminal": terminal,
        "updated_at_unix_ms": capsule.updated_at_unix_ms,
    })
}

pub(super) fn enrich_runtime_instance(
    object: &mut serde_json::Map<String, serde_json::Value>,
    continuity: &emucap::live::continuity::ContinuitySnapshot,
    current: Option<serde_json::Value>,
) {
    if let Some(current) = current {
        if matches!(
            continuity.runtime_binding.state,
            emucap::live::continuity::RuntimeBindingState::Mismatched
                | emucap::live::continuity::RuntimeBindingState::Unmanaged
        ) {
            object.remove("runtime_instance");
            object.insert("stale_runtime_instance".into(), current);
            object.insert(
                "next_safe_action".into(),
                serde_json::json!(
                    "use the live emulator identity for observation; do not treat the stale capsule as ownership evidence or edit runtime files"
                ),
            );
        } else {
            object.remove("stale_runtime_instance");
            object.insert("runtime_instance".into(), current);
        }
    } else if let Some(runtime) = object
        .get_mut("runtime_instance")
        .and_then(serde_json::Value::as_object_mut)
    {
        runtime.insert(
            "lease".into(),
            serde_json::to_value(&continuity.lease)
                .unwrap_or_else(|_| serde_json::json!({"state": "unknown"})),
        );
    }
}
