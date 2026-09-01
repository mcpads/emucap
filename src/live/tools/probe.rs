use super::*;

/// Restore a savestate, advance by exact frame boundaries, and read the target as one operation.
/// Frame-boundary search, regression, and direct agent calls all use this same executor.
pub fn probe(
    link: &mut dyn EmulatorLink,
    state: &str,
    frame: u64,
    memory_type: &str,
    address: u64,
    length: u64,
) -> Result<ToolOutput, LinkError> {
    validate_sync_advance("probe frame", frame)?;
    validate_probe_span(link, memory_type, address, length)?;
    let params = json!({
        "state": state, "frame": frame,
        "memory_type": memory_type, "address": address, "length": length,
    });
    let native_available = link
        .capabilities()
        .methods
        .iter()
        .any(|method| method == "probe");
    if native_available {
        let mut response = link.call("probe", params)?;
        let terminal = link.call("status", json!({}))?;
        verify_frozen_boundary(link, &terminal, "native probe")?;
        validate_native_probe_terminal(&response, frame, length)?;
        let object = response
            .as_object_mut()
            .ok_or_else(|| LinkError::Protocol("native probe response must be an object".into()))?;
        object.insert("state".into(), Value::String("frozen".into()));
        return Ok(ToolOutput::Json(response));
    }
    if !probe_available(link) {
        return Err(LinkError::Emulator {
            kind: "unsupported".into(),
            message: "probe is unavailable: the adapter has neither a native probe nor every operation required for Control composition".into(),
        });
    }

    // The MCP owns one link mutex for the entire tool call. Establishing the frozen boundary here
    // lets adapters with frozen-only state restoration provide the same public operation while
    // admitting no external mutation or guest-time gap between restore, advance, and read.
    let before = link.call("status", json!({}))?;
    let started_running = match before.get("state").and_then(Value::as_str) {
        Some("frozen") => false,
        Some("running") => {
            let paused = link.call("pause", json!({}))?;
            verify_frozen_boundary(link, &paused, "pause")?;
            true
        }
        Some(state) => {
            return Err(LinkError::Protocol(format!(
                "composed probe cannot establish a frozen boundary from state {state}"
            )))
        }
        None => {
            return Err(LinkError::Protocol(
                "composed probe status response did not contain state".into(),
            ))
        }
    };

    // A finite advertised region proves the span without touching the emulator. Legacy adapters
    // that advertise only a memory type get one frozen, non-mutating read preflight. If that
    // preflight fails, restore a running caller before returning: an invalid probe must not leave a
    // previously running guest paused.
    if !probe_span_is_advertised(link, memory_type) {
        let preflight = link
            .call(
                "read_memory",
                json!({"memory_type":memory_type, "address":address, "length":length}),
            )
            .and_then(|response| exact_memory_hex(&response, length, "probe preflight").map(drop));
        if let Err(primary) = preflight {
            let cleanup = if started_running {
                link.call("resume", json!({})).map(|_| ())
            } else {
                Ok(())
            };
            return finish_with_cleanup(Err(primary), cleanup, |primary, cleanup| {
                LinkError::Protocol(match primary {
                    Some(primary) => format!(
                        "probe preflight failed: {primary}; restoring the prior running state also failed: {cleanup}"
                    ),
                    None => format!(
                        "probe preflight cleanup failed while restoring the prior running state: {cleanup}"
                    ),
                })
            });
        }
    }
    let loaded = link.call("load_state", json!({"path":state}))?;
    verify_frozen_boundary(link, &loaded, "load_state")?;

    let advance = if frame == 0 {
        json!({"status":"completed", "state":"frozen", "count":0})
    } else {
        let response = link.call("step", json!({"frames":frame}))?;
        verify_frozen_boundary(link, &response, "step")?;
        response
    };
    let status = advance
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("completed");
    if !matches!(status, "completed" | "interrupted") {
        return Err(LinkError::Protocol(format!(
            "probe frame advance returned unknown terminal status: {status}"
        )));
    }
    let completed = advance
        .get("completed_frames")
        .and_then(Value::as_u64)
        .or_else(|| advance.get("completed").and_then(Value::as_u64))
        .or_else(|| advance.get("advanced").and_then(Value::as_u64))
        .or_else(|| advance.get("count").and_then(Value::as_u64))
        .or_else(|| advance.get("frames_elapsed").and_then(Value::as_u64))
        .unwrap_or(if status == "completed" {
            frame
        } else {
            u64::MAX
        });
    if completed > frame || (status == "completed" && completed != frame) {
        return Err(LinkError::Protocol(format!(
            "probe frame advance reported {completed} of {frame} frames with status {status}"
        )));
    }

    let memory = link.call(
        "read_memory",
        json!({"memory_type":memory_type, "address":address, "length":length}),
    )?;
    let hex = exact_memory_hex(&memory, length, "probe read_memory")?;
    let mut result = json!({
        "status": status,
        "requested_frames": frame,
        "completed_frames": completed,
        "state": "frozen",
        "hex": hex,
        "composition": "load_state_step_read_memory",
    });
    if let (Some(result), Some(advance)) = (result.as_object_mut(), advance.as_object()) {
        for key in [
            "reason",
            "breakpoint_id",
            "event",
            "pc",
            "frame_before",
            "frame",
        ] {
            if let Some(value) = advance.get(key) {
                result.insert(key.into(), value.clone());
            }
        }
    }
    Ok(ToolOutput::Json(result))
}

/// Whether this connection can execute the public atomic probe directly or as one MCP-owned
/// frozen transaction. This is shared by the public tool and regression/bisection consumers so
/// capability promotion cannot drift away from execution.
pub fn probe_available(link: &dyn EmulatorLink) -> bool {
    probe_available_for_methods(&link.capabilities().methods, true)
}

pub fn probe_available_for_methods(methods: &[String], frame_step_available: bool) -> bool {
    let has = |wanted: &str| methods.iter().any(|method| method == wanted);
    has("probe")
        || (frame_step_available
            && [
                "status",
                "pause",
                "resume",
                "load_state",
                "step",
                "read_memory",
            ]
            .iter()
            .all(|method| has(method)))
}

fn validate_probe_span(
    link: &dyn EmulatorLink,
    memory_type: &str,
    address: u64,
    length: u64,
) -> Result<(), LinkError> {
    if memory_type.is_empty() {
        return Err(bad_debug_parameter(
            "probe memory_type must not be empty".into(),
        ));
    }
    if length == 0 {
        return Err(bad_debug_parameter(
            "probe length must be greater than zero".into(),
        ));
    }
    let end = address
        .checked_add(length)
        .ok_or_else(|| bad_debug_parameter("probe address range overflows u64".into()))?;
    let capabilities = link.capabilities();
    if !capabilities.memory_types.is_empty()
        && !capabilities
            .memory_types
            .iter()
            .any(|candidate| candidate == memory_type)
    {
        return Err(bad_debug_parameter(format!(
            "unknown probe memory_type '{memory_type}'; available: {}",
            capabilities.memory_types.join(", ")
        )));
    }
    if let Some(region) = capabilities
        .memory_regions
        .iter()
        .find(|region| region.memory_type == memory_type)
    {
        if end > region.size {
            return Err(bad_debug_parameter(format!(
                "probe range {address:#x}..{end:#x} exceeds {memory_type} size {:#x}",
                region.size
            )));
        }
    }
    Ok(())
}

fn probe_span_is_advertised(link: &dyn EmulatorLink, memory_type: &str) -> bool {
    link.capabilities()
        .memory_regions
        .iter()
        .any(|region| region.memory_type == memory_type)
}

fn exact_memory_hex(response: &Value, length: u64, operation: &str) -> Result<String, LinkError> {
    let encoded = response
        .get("hex")
        .and_then(Value::as_str)
        .ok_or_else(|| LinkError::Protocol(format!("{operation} response did not contain hex")))?;
    let bytes = hex::decode(encoded)
        .map_err(|_| LinkError::Protocol(format!("{operation} returned invalid hex")))?;
    if bytes.len() as u64 != length {
        return Err(LinkError::Protocol(format!(
            "{operation} returned {} bytes, expected {length}",
            bytes.len()
        )));
    }
    Ok(encoded.to_string())
}

fn validate_native_probe_terminal(
    response: &Value,
    requested_frames: u64,
    length: u64,
) -> Result<(), LinkError> {
    let status = response
        .get("status")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            LinkError::Protocol("native probe response did not contain status".into())
        })?;
    match status {
        "completed" => {
            let reported_requested = response
                .get("requested_frames")
                .and_then(Value::as_u64)
                .ok_or_else(|| {
                    LinkError::Protocol(
                        "completed native probe did not report requested_frames".into(),
                    )
                })?;
            let completed = response
                .get("completed_frames")
                .and_then(Value::as_u64)
                .ok_or_else(|| {
                    LinkError::Protocol(
                        "completed native probe did not report completed_frames".into(),
                    )
                })?;
            if reported_requested != requested_frames {
                return Err(LinkError::Protocol(format!(
                    "native probe echoed {reported_requested} requested frames, expected {requested_frames}"
                )));
            }
            if completed != requested_frames {
                return Err(LinkError::Protocol(format!(
                    "native probe reported {completed} of {requested_frames} frames with status completed"
                )));
            }
            exact_memory_hex(response, length, "native probe")?;
            Ok(())
        }
        "interrupted" => {
            if let Some(reported_requested) =
                response.get("requested_frames").and_then(Value::as_u64)
            {
                if reported_requested != requested_frames {
                    return Err(LinkError::Protocol(format!(
                        "interrupted native probe echoed {reported_requested} requested frames, expected {requested_frames}"
                    )));
                }
            }
            if let Some(completed) = response.get("completed_frames").and_then(Value::as_u64) {
                if completed > requested_frames {
                    return Err(LinkError::Protocol(format!(
                        "interrupted native probe reported {completed} of {requested_frames} frames"
                    )));
                }
            }
            if response.get("hex").is_some() {
                exact_memory_hex(response, length, "interrupted native probe")?;
            }
            Ok(())
        }
        other => Err(LinkError::Protocol(format!(
            "native probe returned unknown terminal status: {other}"
        ))),
    }
}

fn verify_frozen_boundary(
    link: &mut dyn EmulatorLink,
    response: &Value,
    operation: &str,
) -> Result<(), LinkError> {
    let observed = match response.get("state").and_then(Value::as_str) {
        Some(state) => state.to_string(),
        None => link
            .call("status", json!({}))?
            .get("state")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_string(),
    };
    if observed != "frozen" {
        return Err(LinkError::Protocol(format!(
            "composed probe requires {operation} to terminate frozen, observed {observed}"
        )));
    }
    Ok(())
}
