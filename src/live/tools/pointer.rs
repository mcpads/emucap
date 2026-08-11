use super::*;

pub fn move_pointer(
    link: &mut dyn EmulatorLink,
    port: u64,
    dx: i64,
    dy: i64,
    frames: u64,
) -> Result<ToolOutput, LinkError> {
    validate_positive_sync_advance("move_pointer frame", frames)?;
    if dx == 0 && dy == 0 {
        return Err(bad_params(
            "move_pointer requires a non-zero relative delta",
        ));
    }
    link.call("pause", json!({}))?;
    let launch_id = link.capabilities().identity.launch_id.clone();
    let outcome = link.call(
        "move_pointer",
        json!({ "port": port, "dx": dx, "dy": dy, "frames": frames }),
    );
    match finish_frozen(link, launch_id.as_deref(), outcome) {
        Ok(value) => Ok(ToolOutput::Json(value)),
        Err(error) => Err(error),
    }
}

fn canonical_pointer_button(button: &str) -> Result<String, LinkError> {
    match button.trim().to_ascii_lowercase().as_str() {
        "left" | "mouse_left" | "left_click" => Ok("mouse_left".into()),
        "right" | "mouse_right" | "right_click" => Ok("mouse_right".into()),
        "middle" | "mouse_middle" | "middle_click" => Ok("mouse_middle".into()),
        _ => Err(bad_params("pointer button must be left, right, or middle")),
    }
}

pub fn click_pointer(
    link: &mut dyn EmulatorLink,
    port: u64,
    button: &str,
    press_frames: u64,
    after_frames: u64,
) -> Result<ToolOutput, LinkError> {
    validate_positive_sync_advance("click_pointer press frame", press_frames)?;
    let button = canonical_pointer_button(button)?;
    let output = tap(
        link,
        port,
        std::slice::from_ref(&button),
        press_frames,
        after_frames,
    )?;
    let ToolOutput::Json(mut result) = output else {
        return Err(LinkError::Protocol(
            "tap returned a non-JSON pointer click result".into(),
        ));
    };
    if let Some(object) = result.as_object_mut() {
        let interrupted = object.get("status").and_then(Value::as_str) == Some("interrupted");
        object.remove("tapped");
        object.insert("operation".into(), json!("click_pointer"));
        object.insert("button".into(), json!(button));
        object.insert("clicked".into(), json!(!interrupted));
        if !interrupted {
            object.insert("status".into(), json!("completed"));
        }
        object.insert("press_frames".into(), json!(press_frames));
        object.insert("after_frames".into(), json!(after_frames));
        object.insert("state".into(), json!("frozen"));
    }
    Ok(ToolOutput::Json(result))
}

#[allow(clippy::too_many_arguments)]
pub fn drag_pointer(
    link: &mut dyn EmulatorLink,
    port: u64,
    button: &str,
    dx: i64,
    dy: i64,
    move_frames: u64,
    after_frames: u64,
) -> Result<ToolOutput, LinkError> {
    validate_positive_sync_advance("drag_pointer movement frame", move_frames)?;
    validate_sync_advance("drag_pointer trailing frame", after_frames)?;
    if dx == 0 && dy == 0 {
        return Err(bad_params(
            "drag_pointer requires a non-zero relative delta",
        ));
    }
    let button = canonical_pointer_button(button)?;
    link.call("pause", json!({}))?;
    let launch_id = link.capabilities().identity.launch_id.clone();
    if let Err(primary) = link.call("set_input", json!({ "port": port, "buttons": [&button] })) {
        return finish_transient_input(link, port, launch_id.as_deref(), Err(primary));
    }
    let outcome = link.call(
        "move_pointer",
        json!({ "port": port, "dx": dx, "dy": dy, "frames": move_frames }),
    );
    let movement = finish_transient_input(link, port, launch_id.as_deref(), outcome)?;
    if movement.get("status").and_then(Value::as_str) == Some("interrupted") {
        let mut interrupted = movement;
        if let Some(object) = interrupted.as_object_mut() {
            object.insert("operation".into(), json!("drag_pointer"));
            object.insert("phase".into(), json!("movement"));
            object.insert("button".into(), json!(button));
            object.insert("released".into(), json!(true));
            object.insert("state".into(), json!("frozen"));
        }
        return Ok(ToolOutput::Json(interrupted));
    }
    let release_edge = link.call("step", json!({ "frames": 1 }));
    let release_edge = finish_frozen(link, launch_id.as_deref(), release_edge)?;
    if release_edge.get("status").and_then(Value::as_str) == Some("interrupted") {
        let mut interrupted = release_edge;
        if let Some(object) = interrupted.as_object_mut() {
            object.insert("operation".into(), json!("drag_pointer"));
            object.insert("phase".into(), json!("release_edge"));
            object.insert("button".into(), json!(button));
            object.insert("released".into(), json!(true));
            object.insert("state".into(), json!("frozen"));
        }
        return Ok(ToolOutput::Json(interrupted));
    }
    if after_frames > 0 {
        let trailing = link.call("step", json!({ "frames": after_frames }));
        let trailing = finish_frozen(link, launch_id.as_deref(), trailing)?;
        if trailing.get("status").and_then(Value::as_str) == Some("interrupted") {
            let mut interrupted = trailing;
            if let Some(object) = interrupted.as_object_mut() {
                object.insert("operation".into(), json!("drag_pointer"));
                object.insert("phase".into(), json!("after_release"));
                object.insert("button".into(), json!(button));
                object.insert("released".into(), json!(true));
                object.insert("state".into(), json!("frozen"));
            }
            return Ok(ToolOutput::Json(interrupted));
        }
    }
    Ok(ToolOutput::Json(json!({
        "status": "completed",
        "dragged": true,
        "button": button,
        "dx": dx,
        "dy": dy,
        "move_frames": move_frames,
        "after_frames": after_frames,
        "state": "frozen",
    })))
}
